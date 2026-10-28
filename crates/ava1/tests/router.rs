//! Data-plane routing over sessions and lanes (SPEC.md §11.1, §12.1).
mod common;

use std::sync::Arc;
use std::time::Duration;

use ava1::conn::Frame;
use ava1::gen::{self, Chunk, Credit, JobOpen, JobOpenAck, Received};
use ava1::router::{Inbound, JobHost, JobLink};
use ava1::session::connect;
use ava1::wire::{FrameMessage, Message};

/// Upload-style host: acknowledges every chunk it receives.
struct Echo;
impl JobHost for Echo {
    fn accept(&self, mut link: JobLink, first: Frame, _peer: [u8; 32]) {
        tokio::spawn(async move {
            let open: JobOpen = first.decode().unwrap();
            link.control
                .send(&JobOpenAck {
                    job_id: open.job_id,
                    credit: 64 << 20,
                    ..Default::default()
                })
                .await
                .unwrap();
            while let Some(ev) = link.rx.recv().await {
                match ev {
                    Inbound::Lane { lane, frame } => {
                        let c: Chunk = frame.decode().unwrap();
                        let job_id = c.job_id;
                        link.control
                            .send(&Received {
                                job_id,
                                lane,
                                seq: frame.channel,
                            })
                            .await
                            .unwrap();
                        link.control
                            .send(&Credit {
                                job_id,
                                bytes: c.data.len() as u64,
                            })
                            .await
                            .unwrap();
                    }
                    Inbound::Closed(_) => break,
                    _ => {}
                }
            }
        });
    }
}

/// Download-style host: sends one chunk on every lane it learns about.
struct Pusher;
impl JobHost for Pusher {
    fn accept(&self, mut link: JobLink, first: Frame, _peer: [u8; 32]) {
        tokio::spawn(async move {
            let open: JobOpen = first.decode().unwrap();
            while let Some(ev) = link.rx.recv().await {
                if let Inbound::LaneUp(id) = ev {
                    let body = Chunk {
                        job_id: open.job_id,
                        file_id: id as u32,
                        offset: 0,
                        data: vec![1; 1 << 20],
                    }
                    .to_bytes()
                    .unwrap();
                    link.lane(id)
                        .unwrap()
                        .tx
                        .send_raw(Chunk::TYPE, 0, 1, body)
                        .await
                        .unwrap();
                }
            }
        });
    }
}

async fn next(link: &mut JobLink) -> Inbound {
    tokio::time::timeout(Duration::from_secs(5), link.rx.recv())
        .await
        .expect("no event within 5 s")
        .expect("job channel closed")
}

async fn next_control(link: &mut JobLink) -> Frame {
    loop {
        if let Inbound::Control(f) = next(link).await {
            return f;
        }
    }
}

#[tokio::test]
async fn an_eight_mib_chunk_crosses_a_lane_and_is_acknowledged() {
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(Arc::new(Echo))).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    assert_ne!(s.peer_caps() & gen::CAP_DATA_PLANE, 0);
    let job = [9u8; 16];
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
    let body = Chunk {
        job_id: job,
        file_id: 1,
        offset: 0,
        data: vec![0x5a; 8 << 20],
    }
    .to_bytes()
    .unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Chunk::TYPE, 0, 77, body)
        .await
        .unwrap();
    let r: Received = next_control(&mut link).await.decode().unwrap();
    assert_eq!((r.lane, r.seq), (lane, 77));
    let c: Credit = next_control(&mut link).await.decode().unwrap();
    assert_eq!(c.bytes, 8 << 20);
}

#[tokio::test]
async fn the_server_pushes_data_on_lanes_the_client_opened() {
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(Arc::new(Pusher))).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let job = [3u8; 16];
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
        if let Inbound::Lane { lane: l, frame, .. } = next(&mut link).await {
            assert_eq!(l, lane);
            let c: Chunk = frame.decode().unwrap();
            assert_eq!(c.data.len(), 1 << 20);
            break;
        }
    }
}

#[tokio::test]
async fn a_closed_lane_is_reported_to_its_jobs() {
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(Arc::new(Echo))).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([1; 16]);
    let opener = link.opener().unwrap().clone();
    let lane = opener.open().await.unwrap();
    opener.close(lane);
    loop {
        if let Inbound::LaneDown(l) = next(&mut link).await {
            assert_eq!(l, lane);
            break;
        }
    }
    assert!(link.lane(lane).is_none());
}

#[tokio::test]
async fn a_late_frame_for_an_unknown_job_does_not_end_the_session() {
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(Arc::new(Echo))).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let link = s.job([4; 16]);
    link.control
        .send(&Credit {
            job_id: [4; 16],
            bytes: 1,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!s.is_closed());
    s.node_info().await.unwrap();
}

#[tokio::test]
async fn a_server_without_jobs_does_not_advertise_the_data_plane() {
    let (addr, _ctx, id, peers) = common::paired().await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    assert_eq!(s.peer_caps() & gen::CAP_DATA_PLANE, 0);
}

#[tokio::test]
async fn dropping_the_session_tells_every_job() {
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(Arc::new(Echo))).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::fast())
        .await
        .unwrap();
    let mut link = s.job([5; 16]);
    drop(s);
    loop {
        if let Inbound::Closed(_) = next(&mut link).await {
            break;
        }
    }
}
