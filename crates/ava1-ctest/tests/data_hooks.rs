#![cfg(unix)]
mod common;

use std::time::Duration;

use ava1::conn::Frame;
use ava1::gen::{self, Chunk, Credit, JobOpen, JobOpenAck, Received};
use ava1::router::{Inbound, JobLink};
use ava1::session::connect;
use ava1::wire::{FrameMessage, Message};
use ava1_ctest::CServer;
use common::*;

async fn next(link: &mut JobLink) -> Inbound {
    tokio::time::timeout(Duration::from_secs(5), link.rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn next_control(link: &mut JobLink) -> Frame {
    loop {
        if let Inbound::Control(f) = next(link).await {
            return f;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_without_hooks_advertises_no_data_plane_cap() {
    // SPEC.md §10: a capability is a promise. The plain test server deliberately asks for
    // CAP_DATA_PLANE with no data hooks (a mistaken embedder); the server masks it off, so
    // a client never opens lanes against a server that would close them.
    let d = dir("hooks-none");
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start(SECRET, &peers, 5, 100, 500, 500);
    let s = connect(&srv.addr(), me, mine, "rust", fast())
        .await
        .unwrap();
    assert_eq!(s.peer_caps() & gen::CAP_DATA_PLANE, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chunk_reaches_the_c_data_hooks_and_is_acknowledged() {
    let d = dir("hooks-up");
    let peers = d.join("peers");
    // The C server loads its peers file once, at start: pair first (P1 pattern).
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_echo(SECRET, &peers, 100, 500, 500);
    let s = connect(&srv.addr(), me, mine, "rust", fast())
        .await
        .unwrap();
    assert_ne!(s.peer_caps() & gen::CAP_DATA_PLANE, 0);
    let job = [6u8; 16];
    let mut link = s.job(job);
    link.control
        .send(&JobOpen {
            job_id: job,
            kind: gen::JOB_UPLOAD,
            ..Default::default()
        })
        .await
        .unwrap();
    let ack: JobOpenAck = next_control(&mut link).await.decode().unwrap();
    assert_eq!(ack.credit, 64 << 20);
    let lane = link.opener().unwrap().open().await.unwrap();
    for (seq, mib) in [(1u32, 8usize), (2, 1), (3, 9)] {
        let body = Chunk {
            job_id: job,
            file_id: 0,
            offset: 0,
            data: vec![0xa5; mib << 20],
        }
        .to_bytes()
        .unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Chunk::TYPE, 0, seq, body)
            .await
            .unwrap();
        let r: Received = next_control(&mut link).await.decode().unwrap();
        assert_eq!((r.lane, r.seq), (lane, seq));
        let c: Credit = next_control(&mut link).await.decode().unwrap();
        assert_eq!(c.bytes, (mib << 20) as u64);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_c_server_pushes_a_frame_on_a_lane() {
    let d = dir("hooks-down");
    let peers = d.join("peers");
    // The C server loads its peers file once, at start: pair first (P1 pattern).
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_echo(SECRET, &peers, 100, 500, 500);
    let s = connect(&srv.addr(), me, mine, "rust", fast())
        .await
        .unwrap();
    let job = [8u8; 16];
    let mut link = s.job(job);
    let lane = link.opener().unwrap().open().await.unwrap();
    link.control
        .send(&JobOpen {
            job_id: job,
            kind: gen::JOB_DOWNLOAD,
            ..Default::default()
        })
        .await
        .unwrap();
    loop {
        if let Inbound::Lane { lane: l, frame } = next(&mut link).await {
            assert_eq!(l, lane);
            assert_eq!(frame.channel, 1);
            let c: Chunk = frame.decode().unwrap();
            assert_eq!((c.job_id, c.data.len()), (job, 1 << 20));
            assert!(c.data.iter().all(|b| *b == 0x3c));
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_over_the_admitted_size_closes_only_its_lane() {
    let d = dir("hooks-admit");
    let peers = d.join("peers");
    // The C server loads its peers file once, at start: pair first (P1 pattern).
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_echo(SECRET, &peers, 100, 500, 500);
    let s = connect(&srv.addr(), me, mine, "rust", fast())
        .await
        .unwrap();
    let job = [2u8; 16];
    let mut link = s.job(job);
    let lane = link.opener().unwrap().open().await.unwrap();
    // The echo hooks admit at most 12 MiB per frame.
    let body = Chunk {
        job_id: job,
        file_id: 0,
        offset: 0,
        data: vec![0; 13 << 20],
    }
    .to_bytes()
    .unwrap();
    let _ = link
        .lane(lane)
        .unwrap()
        .tx
        .send_raw(Chunk::TYPE, 0, 1, body)
        .await;
    loop {
        if let Inbound::LaneDown(l) = next(&mut link).await {
            assert_eq!(l, lane);
            break;
        }
    }
    assert!(!s.is_closed());
    s.node_info().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_frame_on_control_closes_the_session() {
    let d = dir("hooks-ctl");
    let peers = d.join("peers");
    // The C server loads its peers file once, at start: pair first (P1 pattern).
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_echo(SECRET, &peers, 100, 500, 500);
    let s = connect(&srv.addr(), me, mine, "rust", fast())
        .await
        .unwrap();
    let link = s.job([1; 16]);
    let body = Chunk {
        job_id: [1; 16],
        data: vec![0; 70_000],
        ..Default::default()
    }
    .to_bytes()
    .unwrap();
    let _ = link.control.send_raw(Chunk::TYPE, 0, 0, body).await;
    tokio::time::timeout(Duration::from_secs(3), s.closed())
        .await
        .unwrap();
}

#[test]
fn the_c_post_queue_is_bounded_and_frames_land_whole() {
    assert_eq!(ava1_ctest::c_post_queue([0x24; 32]), 0);
}
