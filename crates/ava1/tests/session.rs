mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ava1::gen;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::ServerCtx;
use ava1::session::{connect, connect_expecting};
use ava1::Ava1Error;
use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test]
async fn paired_devices_connect_and_call_node_info() {
    let (addr, _ctx, me, peers) = paired().await;
    let s = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(!s.pairing_pending());
    assert_eq!(s.peer_name(), "Rust test server");
    assert_eq!(s.node_info().await.unwrap().name, "Rust test server");
    let r = s.rpc(999, &[]).await.unwrap();
    assert_eq!(r.status, gen::ERR_UNKNOWN_METHOD);
}

#[tokio::test]
async fn heartbeats_keep_an_idle_session_alive_and_measure_rtt() {
    let (addr, _ctx, me, peers) = paired().await;
    let s = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await; // 3× dead_after with no traffic of ours
    assert!(!s.is_closed());
    assert!(s.rtt().is_some());
    s.node_info().await.unwrap();
}

#[tokio::test]
async fn an_unknown_client_is_refused_when_pairing_is_closed() {
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "s",
        PeerStore::in_memory(),
        node_info_rpc("s"),
    );
    let (addr, _ctx) = start(ctx.with_timing(fast())).await;
    let r = connect(
        &addr.to_string(),
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "c",
        fast(),
    )
    .await;
    assert!(matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_PAIRING_CLOSED));
}

#[tokio::test]
async fn pairing_shows_the_same_code_and_persists_on_both_sides() {
    let dir = temp_dir("pair");
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "console",
        PeerStore::load(&dir.join("server-peers")).unwrap(),
        node_info_rpc("console"),
    )
    .with_timing(fast())
    .with_notify(Box::new(move |r| *seen2.lock().unwrap() = Some(r.code)));
    ctx.open_pairing(Duration::from_secs(60));
    let (addr, _ctx) = start(ctx).await;
    let me = Arc::new(Identity::generate().unwrap());
    let peers = Arc::new(Mutex::new(
        PeerStore::load(&dir.join("client-peers")).unwrap(),
    ));

    let mut s = connect(
        &addr.to_string(),
        me.clone(),
        peers.clone(),
        "laptop",
        fast(),
    )
    .await
    .unwrap();
    assert!(s.pairing_pending());
    wait_for(Duration::from_secs(5), || seen.lock().unwrap().is_some())
        .await
        .expect("the server shows a code");
    let code = seen.lock().unwrap().unwrap();
    // Until the user has confirmed the code the server is unverified: the client sends
    // it nothing but PairConfirm, and the server answers anything else ERR_NOT_PAIRED.
    assert!(matches!(
        s.rpc(gen::METHOD_NODE_INFO, &[]).await,
        Err(Ava1Error::NotPaired)
    ));
    assert!(matches!(s.node_info().await, Err(Ava1Error::NotPaired)));
    assert!(matches!(
        s.open_pairing(60).await,
        Err(Ava1Error::NotPaired)
    ));
    assert_eq!(
        s.rpc_unchecked_for_test(gen::METHOD_NODE_INFO, &[])
            .await
            .unwrap()
            .status,
        gen::ERR_NOT_PAIRED
    );
    s.confirm_pairing(code).await.unwrap();
    assert_eq!(s.node_info().await.unwrap().name, "console");

    let server_file = std::fs::read_to_string(dir.join("server-peers")).unwrap();
    assert!(
        server_file.contains(&ava1::hex::encode(&me.public())),
        "{server_file}"
    );
    assert!(server_file.trim_end().ends_with(" laptop"));
    // A new connection needs no pairing on either side.
    let again = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(!again.pairing_pending());
}

#[tokio::test]
async fn a_server_that_declines_the_pairing_is_reported() {
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "console",
        PeerStore::in_memory(),
        node_info_rpc("console"),
    )
    .with_timing(fast())
    .with_approve(Box::new(|_| false));
    ctx.open_pairing(Duration::from_secs(60));
    let (addr, ctx) = start(ctx).await;
    let id = Arc::new(Identity::generate().unwrap());
    let mut s = connect(
        &addr.to_string(),
        id.clone(),
        Arc::new(Mutex::new(PeerStore::in_memory())),
        "c",
        fast(),
    )
    .await
    .unwrap();
    let code = console_code(&ctx, &id.public()).await;
    let r = s.confirm_pairing(code).await;
    assert!(matches!(r, Err(Ava1Error::Refused { .. })), "{r:?}");
}

#[tokio::test]
async fn a_silent_half_handshake_is_dropped() {
    // Review focus 1.
    let (addr, ctx, me, peers) = paired().await;
    let mut raw = TcpStream::connect(addr).await.unwrap();
    raw.write_all(b"A1\x01\x00\x00").await.unwrap(); // 5 of 16 header bytes, then silence
    let t = Instant::now();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(3), raw.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0, "server closed the half-open handshake");
    assert!(t.elapsed() < Duration::from_millis(2500)); // handshake 500 ms + headroom
                                                        // Everyone else was served meanwhile.
    let s = connect(&addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap();
    wait_for(Duration::from_secs(5), || ctx.connections() <= 1)
        .await
        .expect("the half-open connection was released");
    drop(s);
}

#[tokio::test]
async fn a_connection_storm_is_refused_then_recovers() {
    // Review focus 2. One address may hold only 12 connections by default; lift that
    // here so the global limit is what refuses.
    let (addr, ctx, me, peers) = paired_opts(fast(), roomy()).await;
    let mut held = Vec::new();
    for _ in 0..ava1::server::MAX_CONNS {
        held.push(TcpStream::connect(addr).await.unwrap());
    }
    wait_for(Duration::from_secs(5), || {
        ctx.connections() == ava1::server::MAX_CONNS
    })
    .await
    .expect("every held connection was accepted");
    let r = connect(&addr.to_string(), me.clone(), peers.clone(), "c", fast()).await;
    assert!(
        matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_BUSY),
        "{r:?}"
    );
    drop(held);
    let t = Instant::now();
    while ctx.connections() > 0 && t.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    connect(&addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap();
}

#[tokio::test]
async fn closing_a_session_says_goodbye() {
    let (addr, ctx, me, peers) = paired().await;
    let s = connect(&addr.to_string(), me, peers, "c", fast())
        .await
        .unwrap();
    assert_eq!(ctx.sessions(), 1);
    s.close().await;
    let t = Instant::now();
    while ctx.sessions() > 0 && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(ctx.sessions(), 0);
}

#[tokio::test]
async fn a_paired_device_opens_the_pairing_window_for_another() {
    let (addr, ctx, me, peers) = paired().await;
    assert!(
        !ctx.open_pairing_if_unpaired(Duration::from_secs(60)),
        "a paired node keeps its window shut"
    );
    let stranger = Arc::new(Identity::generate().unwrap());
    let none = Arc::new(Mutex::new(PeerStore::in_memory()));
    let r = connect(
        &addr.to_string(),
        stranger.clone(),
        none.clone(),
        "phone",
        fast(),
    )
    .await;
    assert!(matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_PAIRING_CLOSED));
    let s = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    s.open_pairing(60).await.unwrap();
    let p = connect(&addr.to_string(), stranger, none, "phone", fast())
        .await
        .unwrap();
    assert!(p.pairing_pending());
}

#[tokio::test]
async fn an_unpaired_server_opens_its_own_window() {
    let ctx = ServerCtx::new(
        Identity::generate().unwrap(),
        "s",
        PeerStore::in_memory(),
        node_info_rpc("s"),
    );
    assert!(ctx.open_pairing_if_unpaired(Duration::from_secs(60)));
    assert!(ctx.pairing_open());
}

#[tokio::test]
async fn a_slow_call_does_not_stop_liveness_or_other_calls() {
    // Design-review flaw 4: a 1.5 s call on a 500 ms dead_after session.
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut sp = PeerStore::in_memory();
    sp.add(c_id.public(), "c").unwrap();
    let mut cp = PeerStore::in_memory();
    cp.add(s_id.public(), "s").unwrap();
    let rpc: ava1::server::RpcHandler = Box::new(|method, _| {
        if method == 77 {
            std::thread::sleep(Duration::from_millis(1500));
        }
        ava1::session::RpcReply {
            status: gen::STATUS_OK,
            body: vec![method as u8],
        }
    });
    let (addr, _ctx) = start(ServerCtx::new(s_id, "s", sp, rpc).with_timing(fast())).await;
    let s = Arc::new(
        connect(
            &addr.to_string(),
            c_id,
            Arc::new(Mutex::new(cp)),
            "c",
            fast(),
        )
        .await
        .unwrap(),
    );
    let s2 = s.clone();
    let slow = tokio::spawn(async move { s2.rpc(77, &[]).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        s.rpc(5, &[]).await.unwrap().body,
        vec![5],
        "a fast call is not stuck behind the slow one"
    );
    assert_eq!(slow.await.unwrap().unwrap().body, vec![77]);
    assert!(!s.is_closed(), "liveness held through the slow call");
}

/// A paired client and a server whose handler holds method 77 until `release` is set and
/// answers method 78 with `len` bytes (len = the request's first two bytes, LE).
async fn gated_server() -> (
    Arc<ava1::session::Session>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut sp = PeerStore::in_memory();
    sp.add(c_id.public(), "c").unwrap();
    let mut cp = PeerStore::in_memory();
    cp.add(s_id.public(), "s").unwrap();
    let release = Arc::new(AtomicBool::new(false));
    let r2 = release.clone();
    let rpc: ava1::server::RpcHandler = Box::new(move |method, body| {
        if method == 77 {
            while !r2.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if method == 78 {
            let n = u32::from_le_bytes([body[0], body[1], body[2], 0]) as usize;
            return ava1::session::RpcReply {
                status: gen::STATUS_OK,
                body: vec![0xAB; n],
            };
        }
        ava1::session::RpcReply {
            status: gen::STATUS_OK,
            body: vec![method as u8],
        }
    });
    let (addr, _ctx) = start(ServerCtx::new(s_id, "s", sp, rpc).with_timing(fast())).await;
    let s = connect(
        &addr.to_string(),
        c_id,
        Arc::new(Mutex::new(cp)),
        "c",
        fast(),
    )
    .await
    .unwrap();
    (Arc::new(s), release)
}

#[tokio::test]
async fn eight_calls_run_in_flight_and_the_ninth_is_busy() {
    // SPEC.md §7.4: a session has at most 8 requests in flight.
    assert_eq!(ava1::server::RPC_WORKERS, 8);
    let (s, release) = gated_server().await;
    let mut held = Vec::new();
    for _ in 0..8 {
        let s2 = s.clone();
        held.push(tokio::spawn(async move { s2.rpc(77, &[]).await }));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(s.rpc(5, &[]).await.unwrap().status, gen::ERR_BUSY);
    release.store(true, std::sync::atomic::Ordering::SeqCst);
    for h in held {
        assert_eq!(h.await.unwrap().unwrap().status, gen::STATUS_OK);
    }
    assert_eq!(s.rpc(5, &[]).await.unwrap().status, gen::STATUS_OK);
}

#[tokio::test]
async fn a_256_kib_reply_goes_through_and_a_larger_one_is_refused_not_clipped() {
    // SPEC.md §7.4: a reply body is at most 256 KiB; a handler that exceeds it gets
    // ERR_INTERNAL with a cause, never a clipped `ok`.
    assert_eq!(ava1::server::RPC_REPLY_MAX, 256 * 1024);
    let (s, _release) = gated_server().await;
    let ask = |n: u32| n.to_le_bytes()[..3].to_vec();
    let r = s.rpc(78, &ask(256 * 1024)).await.unwrap();
    assert_eq!((r.status, r.body.len()), (gen::STATUS_OK, 256 * 1024));
    let r = s.rpc(78, &ask(256 * 1024 + 1)).await.unwrap();
    assert_eq!(r.status, gen::ERR_INTERNAL);
    assert_eq!(r.body, b"reply exceeds the 256 KiB RPC cap");
}

#[tokio::test]
async fn a_request_over_56_kib_is_refused_with_a_cause() {
    // SPEC.md §7.4: a request body is at most 56 KiB.
    assert_eq!(ava1::server::RPC_REQUEST_MAX, 56 * 1024);
    let (s, _release) = gated_server().await;
    let r = s.rpc(5, &vec![0u8; 56 * 1024]).await.unwrap();
    assert_eq!(r.status, gen::STATUS_OK);
    let r = s.rpc(5, &vec![0u8; 56 * 1024 + 1]).await.unwrap();
    assert_eq!(r.status, gen::ERR_PROTOCOL);
    assert_eq!(r.body, b"request exceeds the 56 KiB RPC cap");
    assert!(!s.is_closed());
}

#[test]
fn identity_file_of_wrong_length_is_an_error_not_a_new_key() {
    // Review focus 4 (covered in keys.rs too; pinned here at the integration boundary).
    let d = temp_dir("id");
    let p = d.join("identity");
    std::fs::write(&p, [1u8; 31]).unwrap();
    assert!(Identity::load_or_create(&p).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), vec![1u8; 31]);
}

#[test]
fn peer_file_skips_garbage_lines() {
    // Review focus 4.
    let d = temp_dir("peers");
    let p = d.join("peers");
    let good = "ab".repeat(32);
    std::fs::write(
        &p,
        format!(
            "garbage\n{good} 1700000000 Pro\nzz{} 1 x\n\u{0}\u{ff}\n{good}1 2 short\n",
            "a".repeat(62)
        ),
    )
    .unwrap();
    let s = PeerStore::load(&p).unwrap();
    assert_eq!(s.list().len(), 1);
    assert_eq!(s.list()[0].name, "Pro");
    assert!(s.contains(&[0xab; 32]));
}

#[test]
fn a_peer_removal_that_cannot_be_saved_changes_nothing() {
    let d = temp_dir("remove-rollback");
    let path = d.join("sub").join("peers");
    let mut store = PeerStore::load(&path).unwrap();
    store.add([1; 32], "one").unwrap();
    store.add([2; 32], "two").unwrap();
    // The directory turns into a file: nothing can be written there any more.
    std::fs::remove_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("sub"), b"in the way").unwrap();
    assert!(store.remove(&[1; 32]).is_err());
    assert!(
        store.contains(&[1; 32]),
        "forgotten in memory though the file still lists it"
    );
    assert_eq!(store.list().len(), 2);
    // Once the file can be written again, the removal goes through.
    std::fs::remove_file(d.join("sub")).unwrap();
    // A write starts from the file as it is on disk (it is the truth, another process may
    // have changed it): the wiped file lists nobody, so there is nothing to remove until
    // the peers are back.
    assert!(!store.remove(&[1; 32]).unwrap());
    let mut again = PeerStore::load(&path).unwrap();
    again.add([1; 32], "one").unwrap();
    again.add([2; 32], "two").unwrap();
    assert!(store.remove(&[1; 32]).unwrap());
    assert!(!PeerStore::load(&path).unwrap().contains(&[1; 32]));
}

#[test]
fn peer_store_keeps_at_most_32_and_replaces_by_key() {
    let d = temp_dir("cap");
    let mut s = PeerStore::load(&d.join("peers")).unwrap();
    for i in 0..40u8 {
        s.add([i; 32], &format!("n{i}")).unwrap();
    }
    assert_eq!(s.list().len(), ava1::peers::MAX_PEERS);
    assert!(!s.contains(&[0; 32]) && s.contains(&[39; 32]));
    s.add([39; 32], "renamed\nline").unwrap();
    let again = PeerStore::load(&d.join("peers")).unwrap();
    assert_eq!(again.list().last().unwrap().name, "renamed line");
    assert_eq!(again.list().len(), ava1::peers::MAX_PEERS);
}

/// A relay between client and server whose client-to-server direction can be stalled
/// (it stops reading the client socket, so the client's send buffer fills).
async fn stallable_relay(
    server: std::net::SocketAddr,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let stall = Arc::new(AtomicBool::new(false));
    let flag = stall.clone();
    tokio::spawn(async move {
        let (c, _) = l.accept().await.unwrap();
        let s = TcpStream::connect(server).await.unwrap();
        let (mut cr, mut cw) = c.into_split();
        let (mut sr, mut sw) = s.into_split();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                while flag.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                match cr.read(&mut buf).await {
                    Ok(n) if n > 0 => {
                        if sw.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            }
        });
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match sr.read(&mut buf).await {
                Ok(n) if n > 0 => {
                    if cw.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
                _ => return,
            }
        }
    });
    (addr, stall)
}

#[tokio::test]
async fn a_cancelled_call_does_not_break_the_session() {
    use std::sync::atomic::Ordering;
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut sp = PeerStore::in_memory();
    sp.add(c_id.public(), "c").unwrap();
    let mut cp = PeerStore::in_memory();
    cp.add(s_id.public(), "s").unwrap();
    // The stalled relay stops the client's pings reaching the server; allow that.
    let timing = ava1::session::Timing {
        dead_after: Duration::from_secs(20),
        ..fast()
    };
    let (addr, _ctx) =
        start(ServerCtx::new(s_id, "s", sp, node_info_rpc("s")).with_timing(timing)).await;
    let (relay, stall) = stallable_relay(addr).await;
    let s = connect(
        &relay.to_string(),
        c_id,
        Arc::new(Mutex::new(cp)),
        "c",
        timing,
    )
    .await
    .unwrap();

    stall.store(true, Ordering::SeqCst);
    let big = vec![7u8; 60_000];
    // Each call is cancelled after 5 ms: first while waiting for a reply that the stalled
    // relay never delivers, then, once the socket buffers are full, while its send is
    // blocked mid-frame. 300 x 60 KB is far more than loopback buffers hold.
    for _ in 0..300 {
        let _ = tokio::time::timeout(Duration::from_millis(5), s.rpc(5, &big)).await;
    }
    stall.store(false, Ordering::SeqCst);
    // The frames queued before the cancellations still reach the server and occupy its
    // workers for a moment (answered ERR_BUSY past four): wait for a free worker rather
    // than for a fixed time. What matters is that the stream was never torn.
    let t = Instant::now();
    let info = loop {
        match tokio::time::timeout(Duration::from_secs(10), s.node_info()).await {
            Ok(Err(Ava1Error::Refused { code, .. })) if code == gen::ERR_BUSY => {
                assert!(t.elapsed() < Duration::from_secs(10), "workers never freed");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(r) => break r.unwrap(),
            Err(_) => panic!("node.info hung after the stall"),
        }
    };
    assert_eq!(info.name, "s");
    assert!(!s.is_closed(), "{}", s.closed().await);
}

#[tokio::test]
async fn close_returns_even_when_the_peer_stopped_reading() {
    use std::sync::atomic::Ordering;
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut sp = PeerStore::in_memory();
    sp.add(c_id.public(), "c").unwrap();
    let mut cp = PeerStore::in_memory();
    cp.add(s_id.public(), "s").unwrap();
    let timing = ava1::session::Timing {
        dead_after: Duration::from_secs(20),
        ..fast()
    };
    let (addr, _ctx) =
        start(ServerCtx::new(s_id, "s", sp, node_info_rpc("s")).with_timing(timing)).await;
    let (relay, stall) = stallable_relay(addr).await;
    let s = connect(
        &relay.to_string(),
        c_id,
        Arc::new(Mutex::new(cp)),
        "c",
        timing,
    )
    .await
    .unwrap();
    stall.store(true, Ordering::SeqCst);
    // Fill the socket buffers and the send queue: these calls can never be written out.
    let big = vec![7u8; 60_000];
    for _ in 0..300 {
        let _ = tokio::time::timeout(Duration::from_millis(5), s.rpc(5, &big)).await;
    }
    let t = Instant::now();
    tokio::time::timeout(Duration::from_secs(5), s.close())
        .await
        .expect("close() hung on a stuck write");
    assert!(
        t.elapsed() < Duration::from_millis(2500),
        "{:?}",
        t.elapsed()
    );
}

#[tokio::test]
async fn the_session_limit_holds_under_a_handshake_storm() {
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut sp = PeerStore::in_memory();
    sp.add(c_id.public(), "c").unwrap();
    let mut cp = PeerStore::in_memory();
    cp.add(s_id.public(), "s").unwrap();
    let cp = Arc::new(Mutex::new(cp));
    let timing = ava1::session::Timing {
        handshake: Duration::from_secs(5),
        ..fast()
    };
    let (addr, ctx) = start(
        ServerCtx::new(s_id, "s", sp, node_info_rpc("s"))
            .with_timing(timing)
            .with_limits(roomy()),
    )
    .await;
    let n = ava1::server::MAX_SESSIONS + 4;
    let a = addr.to_string();
    let results = futures_join_all((0..n).map(|_| {
        let (a, c_id, cp) = (a.clone(), c_id.clone(), cp.clone());
        async move { connect(&a, c_id, cp, "c", timing).await }
    }))
    .await;
    let (ok, bad): (Vec<_>, Vec<_>) = results.into_iter().partition(|r| r.is_ok());
    assert_eq!(ok.len(), ava1::server::MAX_SESSIONS);
    for r in bad {
        assert!(
            matches!(r, Err(Ava1Error::Refused { code, .. }) if code == gen::ERR_BUSY),
            "{r:?}"
        );
    }
    assert!(ctx.sessions() <= ava1::server::MAX_SESSIONS);
    for r in ok {
        r.unwrap().close().await;
    }
    let t = Instant::now();
    while ctx.sessions() > 0 && t.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    connect(&a, c_id, cp, "c", timing).await.unwrap();
}

async fn futures_join_all<F: std::future::Future + Send + 'static>(
    it: impl Iterator<Item = F>,
) -> Vec<F::Output>
where
    F::Output: Send + 'static,
{
    let hs: Vec<_> = it.map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in hs {
        out.push(h.await.unwrap());
    }
    out
}

/// Reads frames until one that is not a heartbeat.
async fn next_non_heartbeat(r: &mut RawReader) -> ava1::conn::Frame {
    use ava1::wire::FrameMessage;
    loop {
        let f = tokio::time::timeout(Duration::from_secs(5), r.recv())
            .await
            .expect("a reply")
            .unwrap();
        if f.ty != gen::Ping::TYPE && f.ty != gen::Pong::TYPE {
            return f;
        }
    }
}

#[tokio::test]
async fn a_malformed_rpc_request_is_answered_with_an_error_before_closing() {
    let (addr, ctx, me, _peers) = paired().await;
    let (mut r, mut w) = raw_session(addr, &me).await;
    w.send(0x60, 7, &[0xff]).await.unwrap(); // RpcRequest needs at least 8 bytes
    let e: gen::Error = next_non_heartbeat(&mut r).await.decode().unwrap();
    assert_eq!(e.code, gen::ERR_PROTOCOL);
    wait_for(Duration::from_secs(5), || ctx.sessions() == 0)
        .await
        .expect("then the session is closed");
}

#[tokio::test]
async fn the_handshake_has_one_deadline_from_accept_to_welcome() {
    // 1.4 s of silence, then a valid first message, then silence again: the server must
    // hang up 2 s after the accept, not 2 s after each step (which would be 3.4 s).
    let timing = ava1::session::Timing {
        handshake: Duration::from_secs(2),
        ..fast()
    };
    let (addr, _ctx, me, _peers) = paired_with(timing).await;
    let t = Instant::now();
    let stream = TcpStream::connect(addr).await.unwrap();
    let (rh, wh) = stream.into_split();
    let (mut r, mut w) = (
        ava1::conn::FrameReader::new(rh),
        ava1::conn::FrameWriter::new(wh),
    );
    tokio::time::sleep(Duration::from_millis(1400)).await;
    let mut hs = ava1::keys::Handshake::initiator(&me).unwrap();
    let hello = {
        use ava1::wire::Message;
        gen::HelloInfo {
            version_min: gen::PROTOCOL_VERSION,
            version_max: gen::PROTOCOL_VERSION,
            caps: 0,
        }
        .to_bytes()
        .unwrap()
    };
    w.send_msg(
        0,
        &gen::Hs1 {
            noise: hs.write(&hello).unwrap(),
        },
    )
    .await
    .unwrap();
    let _hs2: gen::Hs2 = r.recv().await.unwrap().decode().unwrap();
    // Never send Hs3.
    let end = tokio::time::timeout(Duration::from_secs(5), r.recv()).await;
    assert!(matches!(end, Ok(Err(_))), "the server hung up: {end:?}");
    let took = t.elapsed();
    assert!(took >= Duration::from_millis(1900), "{took:?}");
    assert!(
        took < Duration::from_millis(2900),
        "one deadline, not two: {took:?}"
    );
}

#[tokio::test]
async fn a_client_expecting_another_device_refuses_before_introducing_itself() {
    let seen = Arc::new(Mutex::new(0usize));
    let seen2 = seen.clone();
    let s_id = Identity::generate().unwrap();
    let s_pub = s_id.public();
    let ctx = ServerCtx::new(s_id, "console", PeerStore::in_memory(), node_info_rpc("s"))
        .with_timing(fast())
        .with_notify(Box::new(move |_| *seen2.lock().unwrap() += 1));
    ctx.open_pairing(Duration::from_secs(60));
    let (addr, ctx) = start(ctx).await;
    let me = Arc::new(Identity::generate().unwrap());
    let peers = Arc::new(Mutex::new(PeerStore::in_memory()));
    // The right address, the wrong console.
    let r = connect_expecting(
        &addr.to_string(),
        Some([0x33; 32]),
        me.clone(),
        peers.clone(),
        "laptop",
        fast(),
    )
    .await;
    assert!(matches!(r, Err(Ava1Error::WrongPeer)), "{r:?}");
    wait_for(Duration::from_secs(5), || ctx.connections() == 0)
        .await
        .expect("the refused connection is gone");
    assert_eq!(ctx.sessions(), 0);
    assert_eq!(
        *seen.lock().unwrap(),
        0,
        "the server never learned who asked"
    );
    // The expected console is accepted as before.
    let s = connect_expecting(&addr.to_string(), Some(s_pub), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert_eq!(s.peer_key(), s_pub);
}
