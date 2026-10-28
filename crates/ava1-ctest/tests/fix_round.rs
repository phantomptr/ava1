#![cfg(unix)]
//! Task 14 fix round 1 (the fix brief): a job can never stay attached to a dead session,
//! control frames are checked against the attached session, the global control-queue cap,
//! Received headroom at volume, re-attach drops the old session's batch, retiring ids.
//! Every network test runs under one outer timeout, so a hang fails in seconds.
mod common;

use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::conn::Frame;
use ava1::gen::{
    self, Bundle, BundleRecord, Chunk, FileRoot, JobDone, JobMap, JobOpen, JobOpenAck, ManifestEnd,
    Received, Resume,
};
use ava1::manifest::{Entry, Manifest};
use ava1::router::{Inbound, JobLink};
use ava1::session::{connect, Session};
use ava1::wire::{FrameMessage, Message};
use ava1_ctest::CServer;
use common::*;

/// The whole body of a network test: anything that hangs fails the test here.
async fn bounded<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(90), f)
        .await
        .expect("the test hung (its outer timeout fired)")
}

/// The next control frame, skipping Status.
async fn next_control(link: &mut JobLink) -> Frame {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), link.rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Inbound::Control(f) if f.ty != gen::Status::TYPE => return f,
            _ => {}
        }
    }
}

/// The next control frame of type `ty`.
async fn next_of(link: &mut JobLink, ty: u8) -> Frame {
    loop {
        let f = next_control(link).await;
        if f.ty == ty {
            return f;
        }
    }
}

/// Every control frame up to and including JobDone (its status last).
async fn until_done(link: &mut JobLink) -> (Vec<Frame>, JobDone) {
    let mut seen = Vec::new();
    let t = Instant::now();
    loop {
        assert!(t.elapsed() < Duration::from_secs(60), "no JobDone");
        let f = next_control(link).await;
        if f.ty == JobDone::TYPE {
            return (seen, f.decode().unwrap());
        }
        seen.push(f);
    }
}

fn file(path: &str, size: u64) -> Entry {
    Entry {
        kind: gen::ENTRY_FILE,
        mode: 0o644,
        size,
        mtime: 1,
        path: path.into(),
        root: None,
    }
}

fn open_msg(job: [u8; 16], root: &Path) -> JobOpen {
    JobOpen {
        job_id: job,
        kind: gen::JOB_UPLOAD,
        root: root.to_str().unwrap().into(),
        ..Default::default()
    }
}

async fn open(link: &mut JobLink, job: [u8; 16], root: &Path) -> JobOpenAck {
    link.control.send(&open_msg(job, root)).await.unwrap();
    next_control(link).await.decode().unwrap()
}

async fn send_manifest(link: &JobLink, job: [u8; 16], m: &Manifest) {
    for p in m.pages(job) {
        link.control.send(&p).await.unwrap();
    }
    link.control
        .send(&ManifestEnd {
            job_id: job,
            files: m.files(),
            bytes: m.bytes(),
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
}

fn chunk(job: [u8; 16], id: u32, off: usize, d: &[u8]) -> Vec<u8> {
    Chunk {
        job_id: job,
        file_id: id,
        offset: off as u64,
        data: d.to_vec(),
    }
    .to_bytes()
    .unwrap()
}

fn bundle(job: [u8; 16], id: u32, d: &[u8]) -> Vec<u8> {
    let rec = BundleRecord {
        file_id: id,
        root: *blake3::hash(d).as_bytes(),
        data: d.to_vec(),
    };
    Bundle {
        job_id: job,
        records: vec![rec],
    }
    .to_bytes()
    .unwrap()
}

type Ids = (
    std::sync::Arc<ava1::keys::Identity>,
    std::sync::Arc<std::sync::Mutex<ava1::peers::PeerStore>>,
);

/// The C server reads its peers file once, at start: pair before starting it.
async fn session(srv: &CServer, ids: &Ids) -> Session {
    connect(&srv.addr(), ids.0.clone(), ids.1.clone(), "rust", calm())
        .await
        .unwrap()
}

fn big_data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 4093) as u8).collect()
}

/// Fix brief, Important 1: a session that ends while a JobOpen's work is still delayed
/// must leave the job parked (never attached to the dead session), and the reaper then
/// collects it after the shortened park age.
///
/// Regression guard only: this also passes against the pre-fix code (the pre-fix open
/// already parked on E_CLOSED once the delayed send failed), so it guards the end-to-end
/// outcome. The red-first evidence for the park-on-session-end fix lives in
/// `an_open_ack_send_failure_parks_the_job` and `a_job_whose_feeder_cannot_start_is_parked`.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_ending_during_a_delayed_open_parks_and_reaps_the_job() {
    bounded(async {
        let d = dir("fix1-sessionend");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("park_ms", 300);
        srv.set_open_delay_ms(1500);
        let job = [0x61u8; 16];
        {
            let s = session(&srv, &ids).await;
            let link = s.job(job);
            link.control
                .send(&open_msg(job, &d.join("dest")))
                .await
                .unwrap();
            s.close().await; /* the session ends while the open is still delayed */
        }
        let t = Instant::now();
        let mut parked = false;
        loop {
            let a = srv.job_attached(job);
            if a == -1 {
                if parked {
                    break; /* created, parked, then reaped */
                }
                assert!(
                    t.elapsed() < Duration::from_secs(5),
                    "the job was never created"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            if a == 0 {
                parked = true; /* the session end parks the open's job; the reap follows */
            } else {
                /* A first sighting may still be attached (the poll caught the open
                 * before the session end parked it); it must not STAY attached. */
                assert!(
                    t.elapsed() < Duration::from_secs(6),
                    "the job stayed attached to the dead session"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
}

/// Fix brief, Important 1: a JobOpenAck whose send fails with anything but E_CLOSED
/// (a broken connection mid-open, the post queue) must park the job all the same.
#[tokio::test(flavor = "multi_thread")]
async fn an_open_ack_send_failure_parks_the_job() {
    bounded(async {
        let d = dir("fix1-ackfail");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("park_ms", 300);
        srv.knob("ack_fail", 8); /* the ack's send "fails" with a non-closed error */
        let s = session(&srv, &ids).await;
        let job = [0x62u8; 16];
        let link = s.job(job);
        link.control
            .send(&open_msg(job, &d.join("dest")))
            .await
            .unwrap();
        let t = Instant::now();
        let mut parked = false;
        loop {
            let a = srv.job_attached(job);
            if a == -1 {
                if parked {
                    break; /* the job was created, parked, then reaped */
                }
                assert!(
                    t.elapsed() < Duration::from_secs(5),
                    "the job was never created"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            if a == 0 {
                parked = true; /* the park follows the failed ack; the reap follows the park */
            } else {
                /* A first sighting may still be attached (the poll caught the attach
                 * before the park); it must not STAY attached. */
                assert!(
                    t.elapsed() < Duration::from_secs(5),
                    "the job stayed attached after the ack failed"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        s.close().await;
    })
    .await
}

/// Fix brief, Important 1: when the feeder cannot start, ava1_job_attach must park the
/// job before answering BUSY — it must not stay attached to the refused session.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_feeder_cannot_start_is_parked() {
    bounded(async {
        let d = dir("fix1-feederfail");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("park_ms", 300);
        srv.knob("feeder_fail", 1);
        let s = session(&srv, &ids).await;
        let job = [0x63u8; 16];
        let mut link = s.job(job);
        let ack = open(&mut link, job, &d.join("dest")).await;
        assert_eq!(ack.status, gen::ERR_BUSY);
        assert_eq!(srv.job_attached(job), 0);
        let t = Instant::now();
        while srv.job_attached(job) != -1 {
            assert!(t.elapsed() < Duration::from_secs(5), "never reaped");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!s.is_closed());
    })
    .await
}

/// Fix brief, Important 2: a session whose open of the job was refused (here: another
/// destination) and that pipelines manifest pages behind it must not reach the job. The
/// job must not adopt that manifest, so a later Resume with its hash is told to open.
#[tokio::test(flavor = "multi_thread")]
async fn pages_pipelined_after_a_refused_open_leave_the_job_alone() {
    bounded(async {
        let d = dir("fix2-route");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        let job = [0x64u8; 16];
        let root = d.join("dest");
        let ma = Manifest {
            entries: vec![file("a0", 1), file("a1", 1), file("a2", 1)],
        };
        {
            let s = session(&srv, &ids).await;
            let mut link = s.job(job);
            assert_eq!(open(&mut link, job, &root).await.status, 0);
        } /* the session ends: the job parks */
        let t = Instant::now();
        while srv.job_attached(job) != 0 {
            assert!(t.elapsed() < Duration::from_secs(5), "never parked");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // A session opens the same id for another destination and pipelines its own
        // (different) manifest right behind the open, without waiting for the refusal.
        {
            let s = session(&srv, &ids).await;
            let mut link = s.job(job);
            link.control
                .send(&open_msg(job, &d.join("dest-a")))
                .await
                .unwrap();
            send_manifest(&link, job, &ma).await;
            let ack: JobOpenAck = next_of(&mut link, JobOpenAck::TYPE).await.decode().unwrap();
            assert_eq!(ack.status, gen::ERR_PROTOCOL);
        }
        tokio::time::sleep(Duration::from_millis(300)).await; /* the refused open's pages routed */
        // The pipelined manifest must not have been adopted: a Resume with its hash is
        // told to open (pre-fix, the pages reached the job's inbox and the manifest was
        // adopted, so the resume answered OK for a manifest no live session ever sent).
        let s = session(&srv, &ids).await;
        let mut link = s.job(job);
        link.control
            .send(&Resume {
                job_id: job,
                manifest_hash: ma.hash(),
            })
            .await
            .unwrap();
        let map: JobMap = next_of(&mut link, JobMap::TYPE).await.decode().unwrap();
        assert_eq!(map.status, gen::ERR_UNKNOWN_JOB, "{:?}", map.message);
    })
    .await
}

/// Fix brief, Minor 1: a held batch the feeder took under an earlier session is dropped
/// when a new session attaches — it names the old manifest and must not be applied.
#[tokio::test(flavor = "multi_thread")]
async fn a_re_attach_drops_the_batch_the_old_session_fed() {
    bounded(async {
        let d = dir("fix-m1-reattach");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("feed_delay_ms", 2000);
        let job = [0x65u8; 16];
        let root = d.join("dest");
        let m = Manifest {
            entries: vec![file("f", 3 << 20)],
        };
        let right = big_data(3 << 20);
        let wrong = vec![0xeeu8; 1 << 20];
        {
            let s = session(&srv, &ids).await;
            let mut link = s.job(job);
            assert_eq!(open(&mut link, job, &root).await.status, 0);
            send_manifest(&link, job, &m).await;
            let _map = next_control(&mut link).await;
            let lane = link.opener().unwrap().open().await.unwrap();
            let tx = link.lane(lane).unwrap().tx;
            for seq in 1..=3u32 {
                let off = (seq - 1) as usize * (1 << 20);
                tx.send_raw(Chunk::TYPE, 0, seq, chunk(job, 0, off, &wrong))
                    .await
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(400)).await; /* the feeder took the batch */
            s.close().await; /* ... and the session ends while it feeds */
        }
        let t = Instant::now();
        while srv.job_attached(job) != 0 {
            assert!(t.elapsed() < Duration::from_secs(5), "never parked");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let s = session(&srv, &ids).await;
        let mut link = s.job(job);
        assert_eq!(open(&mut link, job, &root).await.status, 0);
        send_manifest(&link, job, &m).await;
        let map: JobMap = next_of(&mut link, JobMap::TYPE).await.decode().unwrap();
        assert_eq!(map.status, 0);
        let lane = link.opener().unwrap().open().await.unwrap();
        let tx = link.lane(lane).unwrap().tx;
        for (i, off) in (0..3 << 20).step_by(1 << 20).enumerate() {
            tx.send_raw(
                Chunk::TYPE,
                0,
                4 + i as u32,
                chunk(job, 0, off, &right[off..off + (1 << 20)]),
            )
            .await
            .unwrap();
        }
        link.control
            .send(&FileRoot {
                job_id: job,
                file_id: 0,
                root: *blake3::hash(&right).as_bytes(),
            })
            .await
            .unwrap();
        let (_, done) = until_done(&mut link).await;
        assert_eq!(done.status, 0, "{:?}", done.message);
        assert_eq!(std::fs::read(root.join("f")).unwrap(), right);
        // The old session's batch was dropped, never applied: only the new bytes landed.
        let (applied, _) = srv.job_counts(job);
        assert_eq!(applied, (3 << 20) as u64);
    })
    .await
}

/// Fix brief, Minor 1: a failed ava1_apply_reserve on a live job (Received already went
/// out) ends the job loudly instead of dropping the frame silently and hanging.
#[tokio::test(flavor = "multi_thread")]
async fn a_reserve_failure_ends_the_job_loudly() {
    bounded(async {
        let d = dir("fix-m1-reserve");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("reserve_fail", 1);
        let s = session(&srv, &ids).await;
        let job = [0x66u8; 16];
        let root = d.join("dest");
        let m = Manifest {
            entries: vec![file("x", 2)],
        };
        let mut link = s.job(job);
        assert_eq!(open(&mut link, job, &root).await.status, 0);
        send_manifest(&link, job, &m).await;
        let _map = next_control(&mut link).await;
        let lane = link.opener().unwrap().open().await.unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, b"xx"))
            .await
            .unwrap();
        let (_, done) = until_done(&mut link).await;
        assert_eq!(done.status, gen::ERR_PROTOCOL, "{:?}", done.message);
        assert!(!s.is_closed());
    })
    .await
}

/// Fix brief, Minor 2: a failed calloc for a lane frame ends the job (INTERNAL) instead
/// of dropping the frame silently after its Received went out.
#[tokio::test(flavor = "multi_thread")]
async fn an_alloc_failure_on_a_lane_frame_ends_the_job() {
    bounded(async {
        let d = dir("fix-m2-alloc");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("lane_alloc_fail", 1);
        let s = session(&srv, &ids).await;
        let job = [0x67u8; 16];
        let root = d.join("dest");
        let m = Manifest {
            entries: vec![file("x", 2)],
        };
        let mut link = s.job(job);
        assert_eq!(open(&mut link, job, &root).await.status, 0);
        send_manifest(&link, job, &m).await;
        let _map = next_control(&mut link).await;
        let lane = link.opener().unwrap().open().await.unwrap();
        link.lane(lane)
            .unwrap()
            .tx
            .send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, b"xx"))
            .await
            .unwrap();
        let (_, done) = until_done(&mut link).await;
        assert_eq!(done.status, gen::ERR_INTERNAL, "{:?}", done.message);
        assert!(!s.is_closed());
    })
    .await
}

/// Fix brief, Minor 3: the global control cap covers the frames queued behind a JobOpen
/// that is still opening — past it, the open is refused ERR_BUSY, the session stays.
#[tokio::test(flavor = "multi_thread")]
async fn a_control_flood_past_the_cap_refuses_the_open_busy() {
    bounded(async {
        let d = dir("fix-m3-open");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("ctl_cap", 65536);
        srv.set_open_delay_ms(2000);
        let s = session(&srv, &ids).await;
        let job = [0x68u8; 16];
        let mut link = s.job(job);
        let m = Manifest {
            entries: (0..5000).map(|i| file(&format!("f{i}"), 1)).collect(),
        };
        link.control
            .send(&open_msg(job, &d.join("dest")))
            .await
            .unwrap();
        send_manifest(&link, job, &m).await; /* pipelined: several 60 KiB pages */
        let ack: JobOpenAck = next_of(&mut link, JobOpenAck::TYPE).await.decode().unwrap();
        assert_eq!(ack.status, gen::ERR_BUSY);
        assert!(!s.is_closed());
        assert_eq!(srv.job_attached(job), -1);
        // The cap freed once the refused open's queue drained: a new open goes through.
        srv.set_open_delay_ms(0);
        assert_eq!(open(&mut link, job, &d.join("dest2")).await.status, 0);
    })
    .await
}

/// Fix brief, Minor 3: the same cap bounds a live job's inbox — past it the job ends
/// with ERR_BUSY (a refusal the sender can retry), not silently.
#[tokio::test(flavor = "multi_thread")]
async fn a_control_flood_past_the_cap_ends_the_job_busy() {
    bounded(async {
        let d = dir("fix-m3-inbox");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("ctl_cap", 512); /* the open frame fits, a manifest page does not */
        let s = session(&srv, &ids).await;
        let job = [0x69u8; 16];
        let root = d.join("dest");
        let m = Manifest {
            entries: (0..100).map(|i| file(&format!("f{i}"), 1)).collect(),
        };
        let mut link = s.job(job);
        assert_eq!(open(&mut link, job, &root).await.status, 0);
        send_manifest(&link, job, &m).await;
        let (_, done) = until_done(&mut link).await;
        assert_eq!(done.status, gen::ERR_BUSY, "{:?}", done.message);
        assert!(!s.is_closed());
    })
    .await
}

/// Sends 20,000 tiny bundles for `job` through `s` and returns (JobDone, Received count).
/// A spawned task drains the control frames while the flood runs, so the 64-deep client
/// inbox is never the backpressure.
async fn flood_20k(s: &Session, job: [u8; 16], root: &Path, m: &Manifest) -> (JobDone, u32) {
    let mut link = s.job(job);
    assert_eq!(open(&mut link, job, root).await.status, 0);
    send_manifest(&link, job, m).await;
    let _map = next_control(&mut link).await;
    let lane = s.open_lane().await.unwrap();
    let tx = link.lane(lane.id).unwrap().tx;
    let received = Arc::new(AtomicU32::new(0));
    let got = received.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let drain = tokio::spawn(async move {
        let mut done = None;
        loop {
            // After JobDone, keep draining a moment: Received acks sent through the
            // waiting-send fallback can trail it on the wire, and the count must see them.
            let f = match done {
                None => link.rx.recv().await,
                Some(_) => {
                    match tokio::time::timeout(Duration::from_millis(300), link.rx.recv()).await {
                        Ok(f) => f,
                        Err(_) => break, /* the tail after JobDone drained */
                    }
                }
            };
            match f {
                Some(Inbound::Control(f)) => {
                    if f.ty == Received::TYPE {
                        got.fetch_add(1, Ordering::Relaxed);
                    } else if f.ty == JobDone::TYPE {
                        done = Some(f.decode::<JobDone>().unwrap());
                    }
                }
                Some(_) => {}
                None => break, /* the session closed: report it as a missing JobDone */
            }
        }
        let _ = done_tx.send(done);
    });
    let b = bundle(job, 0, b"xx");
    for seq in 1..=20000u32 {
        if let Err(e) = tx.send_raw(Bundle::TYPE, 0, seq, b.clone()).await {
            panic!(
                "the lane send failed at {seq}: {e:?}; the lane closed: {}",
                lane.closed().await
            );
        }
    }
    let done = tokio::time::timeout(Duration::from_secs(120), done_rx)
        .await
        .unwrap()
        .expect("the session closed before JobDone")
        .expect("no JobDone");
    drain.await.unwrap();
    (done, received.load(Ordering::Relaxed))
}

/// Fix brief, Minor 4: at volume (20,000 tiny bundles) the per-frame Received posts must
/// not fill the connection's bounded post queue and close the session — once the queue
/// runs low, the ack waits for the socket on a short-lived thread instead.
#[tokio::test(flavor = "multi_thread")]
async fn twenty_thousand_small_frames_do_not_close_the_session() {
    bounded(async {
        let d = dir("fix-m4-received");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        let s = session(&srv, &ids).await;
        let job = [0x6au8; 16];
        let root = d.join("dest");
        let m = Manifest {
            entries: vec![file("s", 2)],
        };
        let (done, received) = flood_20k(&s, job, &root, &m).await;
        assert_eq!(done.status, 0, "{:?}", done.message);
        assert_eq!(received, 20000);
        assert!(!s.is_closed());
        assert_eq!(std::fs::read(root.join("s")).unwrap(), b"xx");
    })
    .await
}

/// Fix brief, Minor 4: the same volume with every Received forced through the waiting
/// send (the fallback for a low post queue), so that path is exercised at volume too.
#[tokio::test(flavor = "multi_thread")]
async fn forced_waiting_received_acks_survive_the_volume() {
    bounded(async {
        let d = dir("fix-m4-fbforce");
        let peers = d.join("peers");
        let ids = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        srv.knob("fb_force", 1);
        let s = session(&srv, &ids).await;
        let job = [0x6bu8; 16];
        let root = d.join("dest");
        let m = Manifest {
            entries: vec![file("s", 2)],
        };
        let (done, received) = flood_20k(&s, job, &root, &m).await;
        assert_eq!(done.status, 0, "{:?}", done.message);
        assert_eq!(received, 20000);
        assert!(!s.is_closed());
        assert_eq!(std::fs::read(root.join("s")).unwrap(), b"xx");
    })
    .await
}

/// Fix brief, Minor 5: an id whose job was reaped (unlisted) stays "retiring" until the
/// job's destroy completes — a create of the same id is refused until then.
#[test]
fn retiring_jobs_block_a_reopen() {
    assert_eq!(ava1_ctest::c_retiring_blocks_reopen(), 0);
}
