mod common;

use std::time::{Duration, Instant};

use ava1::gen;
use ava1::session::{connect, Timing};
use ava1_chaos::{ChaosConfig, ChaosProxy};
use common::*;

#[tokio::test]
async fn blackholed_session_is_declared_dead() {
    // Review focus 3: a half-open link (no FIN) on both sides.
    let (addr, ctx, me, peers) = paired().await;
    let proxy = ChaosProxy::start(addr, ChaosConfig::default())
        .await
        .unwrap();
    let s = connect(&proxy.addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap();
    assert_eq!(ctx.sessions(), 1);
    proxy.blackhole(true);
    let t = Instant::now();
    let why = tokio::time::timeout(Duration::from_secs(3), s.closed())
        .await
        .expect("client noticed");
    let took = t.elapsed();
    assert!(why.contains("stopped answering"), "{why}");
    // dead_after is 500 ms and the check runs every 100 ms; the rest is headroom for a
    // loaded machine (the bound is here to catch "never", not to time the scheduler).
    assert!(took < Duration::from_millis(2500), "{took:?}");
    // `s` is still alive here, so the server cannot have learned of death from a FIN.
    while ctx.sessions() > 0 && t.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let server_took = t.elapsed();
    assert_eq!(ctx.sessions(), 0, "server noticed too");
    assert!(
        server_took < Duration::from_millis(2500),
        "server took {server_took:?}"
    );
    drop(s);
}

#[tokio::test]
async fn a_killed_lane_does_not_end_the_session() {
    let (addr, _ctx, me, peers) = paired().await;
    let proxy = ChaosProxy::start(addr, ChaosConfig::default())
        .await
        .unwrap();
    let s = connect(&proxy.addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap();
    let keep = s.open_lane().await.unwrap();
    let doomed = s.open_lane().await.unwrap();
    proxy.kill_newest();
    tokio::time::timeout(Duration::from_secs(2), doomed.closed())
        .await
        .expect("killed lane noticed");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!keep.is_closed() && !s.is_closed());
    s.node_info().await.unwrap();
    drop(doomed);
    s.open_lane().await.unwrap();
}

#[tokio::test]
async fn latency_and_a_bandwidth_cap_do_not_kill_a_session() {
    let (addr, _ctx, me, peers) = paired().await;
    let cfg = ChaosConfig {
        delay: Duration::from_millis(60),
        bytes_per_sec: Some(32 * 1024),
        kill_every: None,
    };
    let proxy = ChaosProxy::start(addr, cfg).await.unwrap();
    // The subject is the 500 ms dead_after; the handshake's several round trips through a
    // 60 ms, 32 KiB/s proxy can outlast fast()'s 500 ms handshake on a loaded CI runner.
    let timing = ava1::session::Timing {
        handshake: Duration::from_secs(5),
        ..fast()
    };
    let s = connect(&proxy.addr.to_string(), me, peers, "c", timing)
        .await
        .unwrap();
    for _ in 0..10 {
        s.node_info().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(!s.is_closed());
    assert!(s.rtt().unwrap() >= Duration::from_millis(100));
}

#[tokio::test]
async fn a_killed_session_reports_why_and_a_new_one_connects() {
    let (addr, _ctx, me, peers) = paired().await;
    let proxy = ChaosProxy::start(addr, ChaosConfig::default())
        .await
        .unwrap();
    let s = connect(
        &proxy.addr.to_string(),
        me.clone(),
        peers.clone(),
        "c",
        fast(),
    )
    .await
    .unwrap();
    proxy.kill_all();
    let why = tokio::time::timeout(Duration::from_secs(2), s.closed())
        .await
        .unwrap();
    assert!(!why.is_empty());
    assert!(s.node_info().await.is_err());
    connect(&proxy.addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap()
        .node_info()
        .await
        .unwrap();
}

fn slow_link() -> Timing {
    Timing {
        ping_every: Duration::from_millis(200),
        dead_after: Duration::from_millis(1000),
        handshake: Duration::from_secs(3),
        ..Timing::default()
    }
}

#[tokio::test]
async fn a_max_size_frame_slower_than_dead_after_keeps_the_session() {
    // Any byte is proof of life: a frame that takes several dead_afters to arrive (a slow
    // link) must not kill the session on either side.
    let t = slow_link();
    let (addr, ctx, me, peers) = paired_with(t).await;
    let cfg = ChaosConfig {
        bytes_per_sec: Some(16 * 1024),
        ..ChaosConfig::default()
    };
    let proxy = ChaosProxy::start(addr, cfg).await.unwrap();
    let s = connect(&proxy.addr.to_string(), me, peers, "c", t)
        .await
        .unwrap();
    // The largest control frame: 56 KiB of body + RpcRequest framing + MAC < 64 KiB.
    let big = vec![0x5a; 56 * 1024];
    let start = Instant::now();
    let r = s.rpc(gen::METHOD_NODE_INFO, &big).await.unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    assert!(
        start.elapsed() > t.dead_after * 3,
        "the frame should outlast dead_after: {:?}",
        start.elapsed()
    );
    assert!(!s.is_closed(), "client side");
    assert_eq!(ctx.sessions(), 1, "server side");
}

#[tokio::test]
async fn a_peer_that_stops_mid_frame_is_dropped() {
    let (addr, ctx, me, _peers) = paired().await;
    let (_r, mut w) = raw_session(addr, &me).await;
    assert_eq!(ctx.sessions(), 1);
    w.send(0x60, 1, &[1u8; 200]).await.unwrap(); // a complete frame, then half of one:
    let mut raw = ava1::frame::Header {
        ty: 0x60,
        flags: ava1::frame::FLAG_SEALED,
        channel: 2,
        body_len: 60_000,
    }
    .encode()
    .to_vec();
    raw.extend_from_slice(&[0u8; 1000]);
    let mut stream = w.into_inner();
    use tokio::io::AsyncWriteExt;
    stream.write_all(&raw).await.unwrap();
    let took = wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
        .await
        .expect("a stalled frame ends the session");
    assert!(took < Duration::from_millis(2000), "{took:?}"); // dead_after 500 ms + slack
}

#[tokio::test]
async fn a_peer_dripping_one_frame_is_dropped_by_the_rate_floor() {
    // One byte every 100 ms is never 500 ms of silence, but the frame would take hours.
    let t = Timing {
        min_frame_rate: 64 * 1024,
        ..fast()
    };
    let (addr, ctx, me, _peers) = paired_with(t).await;
    let (_r, w) = raw_session(addr, &me).await;
    let mut stream = w.into_inner();
    let header = ava1::frame::Header {
        ty: 0x60,
        flags: ava1::frame::FLAG_SEALED,
        channel: 2,
        body_len: 60_000,
    }
    .encode();
    use tokio::io::AsyncWriteExt;
    stream.write_all(&header).await.unwrap();
    let drip = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if stream.write_all(&[0]).await.is_err() {
                return;
            }
        }
    });
    // Budget: dead_after 500 ms + 60,000 B at 64 KiB/s ≈ 1.4 s.
    let took = wait_for(Duration::from_secs(8), || ctx.sessions() == 0)
        .await
        .expect("the rate floor ends a dripping frame");
    assert!(
        took > Duration::from_millis(900),
        "cut by the floor, not idleness: {took:?}"
    );
    drip.abort();
}

#[tokio::test]
async fn a_peer_that_never_reads_its_replies_is_dropped() {
    let (addr, ctx, me, peers) = paired().await;
    let (_r, mut w) = raw_session(addr, &me).await; // _r is never read
    let flood = tokio::spawn(async move {
        let q = gen::RpcRequest {
            method: gen::METHOD_NODE_INFO,
            body: Vec::new(),
        };
        let mut id = 1u32;
        while w.send_msg(id, &q).await.is_ok() {
            id = id.wrapping_add(1);
        }
    });
    wait_for(Duration::from_secs(10), || ctx.sessions() == 0)
        .await
        .expect("the server drops a peer that stopped reading");
    flood.abort();
    // Its slot is free again.
    connect(&addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap()
        .node_info()
        .await
        .unwrap();
}

#[tokio::test]
async fn a_client_whose_link_died_reconnects_at_once_with_all_its_lanes() {
    // The link dies without a FIN (Wi-Fi gone, console asleep): the server still counts
    // the old control connection and its 8 lanes until dead_after. One address may hold
    // 12 connections, so without superseding the old session the new one would get its
    // control connection and two lanes, and ERR_BUSY for the third.
    let t = Timing {
        dead_after: Duration::from_secs(5),
        ..fast()
    };
    let (addr, ctx, me, peers) = paired_with(t).await;
    let proxy = ChaosProxy::start(addr, ChaosConfig::default())
        .await
        .unwrap();
    let old = connect(&proxy.addr.to_string(), me.clone(), peers.clone(), "c", t)
        .await
        .unwrap();
    let mut old_lanes = Vec::new();
    for _ in 0..gen::MAX_LANES {
        old_lanes.push(old.open_lane().await.unwrap());
    }
    assert_eq!(ctx.connections(), 9);
    proxy.blackhole(true);
    let new = connect(&addr.to_string(), me, peers, "c", t).await.unwrap();
    let mut lanes = Vec::new();
    for i in 0..gen::MAX_LANES {
        match new.open_lane().await {
            Ok(l) => lanes.push(l),
            Err(e) => panic!("lane {} of the new session: {e:?}", i + 1),
        }
    }
    new.node_info().await.unwrap();
    assert_eq!(
        ctx.sessions(),
        1,
        "the old session is gone, not waiting to die"
    );
    // The old connections themselves end right away, not after dead_after.
    wait_for(Duration::from_secs(2), || ctx.connections() == 9)
        .await
        .expect("the superseded session's connections are closed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!new.is_closed() && lanes.iter().all(|l| !l.is_closed()));
}
