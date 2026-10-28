#![cfg(unix)]
//! The C receiver on the wire (Task 14): JobOpen → ack → pages → map → chunks/bundles →
//! Received/Credit/Durable → JobDone, credit admission, held frames, parking.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use ava1::conn::Frame;
use ava1::gen::{
    self, Bundle, BundleRecord, Chunk, Credit, FileRoot, JobDone, JobMap, JobOpen, JobOpenAck,
    ManifestEnd, Received, Resume,
};
use ava1::manifest::{Entry, Manifest};
use ava1::router::{Inbound, JobLink};
use ava1::session::{connect, Session};
use ava1::wire::{FrameMessage, Message};
use ava1_ctest::CServer;
use common::*;

/// The next control frame of type `ty`, skipping the others.
async fn next_of(link: &mut JobLink, ty: u8) -> Frame {
    loop {
        let f = next_control(link).await;
        if f.ty == ty {
            return f;
        }
    }
}

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

#[tokio::test(flavor = "multi_thread")]
async fn a_hand_driven_upload_lands_on_the_c_receiver() {
    let d = dir("wire-up");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;
    let job = [0x51u8; 16];
    let big = big_data((2 << 20) + 3);
    let m = Manifest {
        entries: vec![file("b", big.len() as u64), file("s", 2)],
    };
    let root = d.join("dest");
    let mut link = s.job(job);
    let ack = open(&mut link, job, &root).await;
    assert_eq!((ack.status, ack.staged), (0, 1));
    send_manifest(&link, job, &m).await;
    let map: JobMap = next_control(&mut link).await.decode().unwrap();
    assert_eq!((map.status, map.last, map.done.len()), (0, 1, 0));
    let lane = link.opener().unwrap().open().await.unwrap();
    let tx = link.lane(lane).unwrap().tx;
    tx.send_raw(Bundle::TYPE, 0, 1, bundle(job, 1, b"hi"))
        .await
        .unwrap();
    for (seq, off) in [(2u32, 0usize), (3, 1 << 20), (4, 2 << 20)] {
        let end = (off + (1 << 20)).min(big.len());
        tx.send_raw(Chunk::TYPE, 0, seq, chunk(job, 0, off, &big[off..end]))
            .await
            .unwrap();
    }
    link.control
        .send(&FileRoot {
            job_id: job,
            file_id: 0,
            root: *blake3::hash(&big).as_bytes(),
        })
        .await
        .unwrap();
    let (seen, done) = until_done(&mut link).await;
    assert_eq!(done.status, 0, "{:?}", done.message);
    let mut received: Vec<u32> = seen
        .iter()
        .filter(|f| f.ty == Received::TYPE)
        .map(|f| f.decode::<Received>().unwrap().seq)
        .collect();
    received.sort();
    assert_eq!(received, vec![1, 2, 3, 4]);
    assert_eq!(std::fs::read(root.join("s")).unwrap(), b"hi");
    assert_eq!(std::fs::read(root.join("b")).unwrap(), big);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_beyond_the_granted_credit_closes_its_lane() {
    let d = dir("wire-credit");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;
    let job = [0x52u8; 16];
    let mut link = s.job(job);
    let ack = open(&mut link, job, &d.join("dest")).await;
    assert_eq!(ack.status, 0);
    let lane = link.opener().unwrap().open().await.unwrap();
    let tx = link.lane(lane).unwrap().tx;
    // No manifest yet, so nothing is consumed: the credit, then one frame too many.
    let n = (ack.credit / (15 << 20)) as u32 + 1;
    for seq in 1..=n {
        if tx
            .send_raw(Chunk::TYPE, 0, seq, chunk(job, 0, 0, &vec![0; 15 << 20]))
            .await
            .is_err()
        {
            break;
        }
    }
    let t = Instant::now();
    loop {
        assert!(t.elapsed() < Duration::from_secs(20), "the lane stayed up");
        if let Inbound::LaneDown(l) = tokio::time::timeout(Duration::from_secs(10), link.rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            assert_eq!(l, lane);
            break;
        }
    }
    assert!(!s.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_overrun_is_refused_with_err_credit_and_the_job_goes_on_on_another_lane() {
    // SPEC §12.4: the lane that overran gets ERR_CREDIT and closes; the session and the job
    // stay, and frames sent before the map (held) are applied once it is out.
    let d = dir("wire-credit2");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;
    let job = [0x53u8; 16];
    let held = 60usize << 20; // 4 chunks of 15 MiB: 4 MiB of the 64 MiB credit left
    let size = held + (4 << 20);
    let data = big_data(size);
    let m = Manifest {
        entries: vec![file("f", size as u64)],
    };
    let root = d.join("dest");
    let mut link = s.job(job);
    let ack = open(&mut link, job, &root).await;
    assert_eq!((ack.status, ack.credit), (0, 64 << 20));
    let bad = s.open_lane().await.unwrap();
    let tx = link.lane(bad.id).unwrap().tx;
    for (i, off) in (0..held).step_by(15 << 20).enumerate() {
        tx.send_raw(
            Chunk::TYPE,
            0,
            i as u32 + 1,
            chunk(job, 0, off, &data[off..off + (15 << 20)]),
        )
        .await
        .unwrap();
    }
    // One byte past the credit: nothing was applied yet (no manifest), so nothing came back.
    let _ = tx
        .send_raw(Chunk::TYPE, 0, 5, chunk(job, 0, 0, &[1u8; 8 << 20]))
        .await;
    let why = tokio::time::timeout(Duration::from_secs(10), bad.closed())
        .await
        .unwrap();
    assert!(why.contains("error 17"), "{why}");
    assert!(!s.is_closed());
    // The held frames land after the map; the job finishes on the session's other lane.
    send_manifest(&link, job, &m).await;
    let map: JobMap = next_of(&mut link, JobMap::TYPE).await.decode().unwrap();
    assert_eq!(map.status, 0);
    let good = link.opener().unwrap().open().await.unwrap();
    assert_ne!(good, bad.id);
    // The tail (4 MiB and its header) is more than the 4 MiB left: wait for the held
    // frames' bytes to come back first.
    let c: gen::Credit = next_of(&mut link, gen::Credit::TYPE)
        .await
        .decode()
        .unwrap();
    assert!(c.bytes >= 15 << 20);
    link.lane(good)
        .unwrap()
        .tx
        .send_raw(Chunk::TYPE, 0, 6, chunk(job, 0, held, &data[held..]))
        .await
        .unwrap();
    link.control
        .send(&FileRoot {
            job_id: job,
            file_id: 0,
            root: *blake3::hash(&data).as_bytes(),
        })
        .await
        .unwrap();
    let (_, done) = until_done(&mut link).await;
    assert_eq!(done.status, 0, "{:?}", done.message);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_open_does_not_stall_the_session_and_pipelined_pages_wait_for_it() {
    // Ruling 1: JobOpen's work runs off the reader thread. Opening takes longer than the
    // liveness window on both sides; pings keep flowing, an RPC answers meanwhile, and the
    // pages sent before the ack are applied after it, in order.
    let d = dir("wire-slowopen");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 100, 600, 2000, 0);
    srv.set_open_delay_ms(1500);
    let s = connect(&srv.addr(), ids.0.clone(), ids.1.clone(), "rust", fast())
        .await
        .unwrap();
    let job = [0x54u8; 16];
    let m = Manifest {
        entries: vec![file("a", 3), file("b", 4)],
    };
    let root = d.join("dest");
    let mut link = s.job(job);
    let t0 = Instant::now();
    link.control.send(&open_msg(job, &root)).await.unwrap();
    send_manifest(&link, job, &m).await;
    let r = s.rpc(gen::METHOD_NODE_INFO, &[]).await.unwrap();
    assert_eq!(r.status, 0);
    assert!(
        t0.elapsed() < Duration::from_millis(1200),
        "the RPC waited for the open"
    );
    let ack: JobOpenAck = next_control(&mut link).await.decode().unwrap();
    assert_eq!(ack.status, 0);
    assert!(t0.elapsed() >= Duration::from_millis(1400));
    let map: JobMap = next_control(&mut link).await.decode().unwrap();
    assert_eq!((map.status, map.last), (0, 1));
    assert!(!s.is_closed());
    let lane = link.opener().unwrap().open().await.unwrap();
    let tx = link.lane(lane).unwrap().tx;
    tx.send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, b"aaa"))
        .await
        .unwrap();
    tx.send_raw(Bundle::TYPE, 0, 2, bundle(job, 1, b"bbbb"))
        .await
        .unwrap();
    let (_, done) = until_done(&mut link).await;
    assert_eq!(done.status, 0, "{:?}", done.message);
    assert_eq!(std::fs::read(root.join("b")).unwrap(), b"bbbb");
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_before_the_map_are_held_and_applied_in_order_after_it() {
    // Ruling 10/14: lane frames that arrive before the job's map is out wait, and go to the
    // workers in arrival order. One worker and two writes of the same group make the order
    // visible: the second (right) bytes must win, or the root check fails.
    let d = dir("wire-held");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data_with(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0, 1);
    srv.set_map_delay_ms(1000);
    let s = session(&srv, &ids).await;
    let job = [0x55u8; 16];
    let data = big_data((2 << 20) + 5);
    let m = Manifest {
        entries: vec![file("f", data.len() as u64)],
    };
    let root = d.join("dest");
    let mut link = s.job(job);
    assert_eq!(open(&mut link, job, &root).await.status, 0);
    send_manifest(&link, job, &m).await;
    let lane = link.opener().unwrap().open().await.unwrap();
    let tx = link.lane(lane).unwrap().tx;
    let wrong = vec![0xeeu8; 1 << 20];
    tx.send_raw(Chunk::TYPE, 0, 1, chunk(job, 0, 0, &wrong))
        .await
        .unwrap();
    tx.send_raw(Chunk::TYPE, 0, 2, chunk(job, 0, 0, &data[..1 << 20]))
        .await
        .unwrap();
    tx.send_raw(Chunk::TYPE, 0, 3, chunk(job, 0, 1 << 20, &data[1 << 20..]))
        .await
        .unwrap();
    link.control
        .send(&FileRoot {
            job_id: job,
            file_id: 0,
            root: *blake3::hash(&data).as_bytes(),
        })
        .await
        .unwrap();
    // While the map is held back, all three frames wait in the job and none was applied.
    let t = Instant::now();
    loop {
        let (applied, held) = srv.job_counts(job);
        assert_eq!(applied, 0, "a frame was applied before the map");
        if held == 3 {
            break;
        }
        assert!(t.elapsed() < Duration::from_millis(800), "held {held}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Received (memory) comes before the map; nothing reaches the disk before it.
    let mut got_map = false;
    let mut received = 0;
    loop {
        let f = next_control(&mut link).await;
        match f.ty {
            Received::TYPE => received += 1,
            JobMap::TYPE => {
                assert_eq!(f.decode::<JobMap>().unwrap().status, 0);
                got_map = true;
            }
            gen::Durable::TYPE => assert!(got_map, "a Durable before the map"),
            gen::FileRetry::TYPE => panic!("held frames were applied out of order"),
            JobDone::TYPE => {
                let done: JobDone = f.decode().unwrap();
                assert_eq!(done.status, 0, "{:?}", done.message);
                break;
            }
            _ => {}
        }
    }
    assert!(got_map);
    assert_eq!(received, 3);
    assert_eq!(std::fs::read(root.join("f")).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread")]
async fn chunks_for_small_files_and_records_for_large_ones_are_refused() {
    // Ruling 5 (SPEC §12.2): the cutoff (256 KiB) decides how a file travels, so neither
    // a stray .ava-part for a small file nor a whole-file write of a large one can happen.
    let d = dir("wire-cutoff");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;
    for (n, (job, large)) in [([0x56u8; 16], false), ([0x57u8; 16], true)]
        .into_iter()
        .enumerate()
    {
        let size: usize = if large { 300 << 10 } else { 10 };
        let data = big_data(size);
        let m = Manifest {
            entries: vec![file("f", size as u64)],
        };
        let root = d.join(format!("dest{n}"));
        let mut link = s.job(job);
        assert_eq!(open(&mut link, job, &root).await.status, 0);
        send_manifest(&link, job, &m).await;
        let map: JobMap = next_control(&mut link).await.decode().unwrap();
        assert_eq!(map.status, 0);
        let lane = link.opener().unwrap().open().await.unwrap();
        let tx = link.lane(lane).unwrap().tx;
        if large {
            tx.send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, &data))
                .await
                .unwrap();
        } else {
            tx.send_raw(Chunk::TYPE, 0, 1, chunk(job, 0, 0, &data))
                .await
                .unwrap();
        }
        let (_, done) = until_done(&mut link).await;
        assert_eq!(done.status, gen::ERR_PROTOCOL, "large={large}");
        assert!(!root.with_extension("ava-part").join("f.ava-part").exists());
        assert!(!s.is_closed());
        link.opener().unwrap().close(lane);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_job_to_a_destination_in_use_is_refused_busy() {
    let d = dir("wire-sameroot");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;
    let root = d.join("dest");
    let (a, b) = ([0x58u8; 16], [0x59u8; 16]);
    let mut la = s.job(a);
    assert_eq!(open(&mut la, a, &root).await.status, 0);
    let mut lb = s.job(b);
    let ack = open(&mut lb, b, &root).await;
    assert_eq!(ack.status, gen::ERR_BUSY);
    // Another destination is fine, and so is the same job opening again.
    let mut lc = s.job([0x5au8; 16]);
    assert_eq!(
        open(&mut lc, [0x5au8; 16], &d.join("other")).await.status,
        0
    );
    assert_eq!(open(&mut la, a, &root).await.status, 0);
    // Once the first job ends, the destination is free.
    let m = Manifest {
        entries: vec![file("x", 1)],
    };
    send_manifest(&la, a, &m).await;
    let _map = next_control(&mut la).await;
    let lane = la.opener().unwrap().open().await.unwrap();
    la.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle(a, 0, b"x"))
        .await
        .unwrap();
    let (_, done) = until_done(&mut la).await;
    assert_eq!(done.status, 0);
    assert_eq!(open(&mut lb, b, &root).await.status, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_session_parks_its_job_and_a_new_session_takes_it_over() {
    let d = dir("wire-park");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let job = [0x5bu8; 16];
    let root = d.join("dest");
    let m = Manifest {
        entries: vec![file("x", 2)],
    };
    {
        let s = session(&srv, &ids).await;
        let mut link = s.job(job);
        assert_eq!(open(&mut link, job, &root).await.status, 0);
        assert_eq!(srv.job_attached(job), 1);
        s.close().await;
    }
    let t = Instant::now();
    while srv.job_attached(job) != 0 {
        assert!(t.elapsed() < Duration::from_secs(5), "never parked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let s = session(&srv, &ids).await;
    let mut link = s.job(job);
    // Unknown manifest: Resume is told to send it again; JobOpen re-attaches.
    link.control
        .send(&Resume {
            job_id: job,
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    // (the re-attach also re-sends the grant as a Credit, §11.5: skip it)
    let map: JobMap = next_of(&mut link, JobMap::TYPE).await.decode().unwrap();
    assert_eq!(map.status, gen::ERR_UNKNOWN_JOB);
    assert_eq!(open(&mut link, job, &root).await.status, 0);
    assert_eq!(srv.job_attached(job), 1);
    send_manifest(&link, job, &m).await;
    let _map = next_control(&mut link).await;
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, b"ok"))
        .await
        .unwrap();
    let (_, done) = until_done(&mut link).await;
    assert_eq!(done.status, 0);
    assert_eq!(std::fs::read(root.join("x")).unwrap(), b"ok");
}

/// SPEC.md §11.5, "credit after Resume": a Resume of a parked job restarts the sender's
/// window — the receiver sends the current grant as a Credit — and the job then completes.
#[tokio::test(flavor = "multi_thread")]
async fn a_resume_after_a_dropped_session_sends_credit_and_the_job_completes() {
    let d = dir("wire-resume-credit");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let job = [0x5du8; 16];
    let root = d.join("dest");
    let m = Manifest {
        entries: vec![file("x", 2)],
    };
    let grant;
    {
        let s = session(&srv, &ids).await;
        let mut link = s.job(job);
        grant = open(&mut link, job, &root).await.credit;
        assert!(grant > 0);
        send_manifest(&link, job, &m).await;
        let _map = next_of(&mut link, JobMap::TYPE).await;
        s.close().await;
    }
    let t = Instant::now();
    while srv.job_attached(job) != 0 {
        assert!(t.elapsed() < Duration::from_secs(5), "never parked");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let s = session(&srv, &ids).await;
    let mut link = s.job(job);
    link.control
        .send(&Resume {
            job_id: job,
            manifest_hash: m.hash(),
        })
        .await
        .unwrap();
    let credit: Credit = next_of(&mut link, Credit::TYPE).await.decode().unwrap();
    assert_eq!(credit.bytes, grant, "the grant is re-sent as Credit");
    let map: JobMap = next_of(&mut link, JobMap::TYPE).await.decode().unwrap();
    assert_eq!(map.status, 0);
    let lane = link.opener().unwrap().open().await.unwrap();
    link.lane(lane)
        .unwrap()
        .tx
        .send_raw(Bundle::TYPE, 0, 1, bundle(job, 0, b"ok"))
        .await
        .unwrap();
    let (_, done) = until_done(&mut link).await;
    assert_eq!(done.status, 0);
    assert_eq!(std::fs::read(root.join("x")).unwrap(), b"ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resume_for_a_job_nobody_has_is_told_to_open() {
    let d = dir("wire-unknown");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;
    let job = [0x5cu8; 16];
    let mut link = s.job(job);
    link.control
        .send(&Resume {
            job_id: job,
            manifest_hash: [0; 32],
        })
        .await
        .unwrap();
    let map: JobMap = next_control(&mut link).await.decode().unwrap();
    assert_eq!((map.status, map.last), (gen::ERR_UNKNOWN_JOB, 1));
}

#[test]
fn the_reaper_takes_parked_jobs_only_and_never_one_just_attached() {
    // Rulings 3 and 4, in C: a job created unattached is stamped parked and reaped after the
    // (shortened) park age; one attached at creation survives it; find_attach under the
    // table lock leaves a parked job nothing the reaper can take.
    let d = dir("wire-reap");
    assert_eq!(ava1_ctest::c_reap_rules(&d.join("jobs")), 0);
}

#[test]
fn ended_uploads_leave_the_table_while_their_session_runs_on() {
    // #475, in C: a session runs job after job, so the uploads that ended on it make room for
    // the next JobOpen (the one that ended longest ago and nobody holds goes first) and leave
    // 10 s after their end; running or still-settling jobs never do, nor a copy.
    let d = dir("wire-ended");
    assert_eq!(ava1_ctest::c_ended_transfers(&d.join("jobs")), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_restarts_on_the_same_port() {
    let d = dir("wire-restart");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let mut srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let port = srv.port;
    srv.restart_data();
    assert_eq!(srv.port, port);
    let s = session(&srv, &ids).await;
    assert_eq!(s.rpc(gen::METHOD_NODE_INFO, &[]).await.unwrap().status, 0);
}

#[test]
fn n3_retire_never_frees_a_job_that_left_the_table() {
    // A job unlisted by someone else (a cancel) with two references left is not the
    // table's and the caller's: retire must refuse it, not free it under the other holder.
    assert_eq!(ava1_ctest::c_retire_unlisted(), 0);
}
