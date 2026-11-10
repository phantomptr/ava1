#![cfg(unix)]
//! JF_READBACK (SPEC.md §13.5): the receiver reads each large file back from its drive before the
//! commit and checks it against the CVs of what it received. Without it, bytes that changed on the
//! drive after they were received (a misplaced or short write, a filesystem fault, another process)
//! commit unnoticed, because every other check hashes the bytes as they arrived.
use ava1::gen::{self, ENTRY_FILE};
use ava1::manifest::{Entry, Manifest};
use ava1::verify::GROUP;
use ava1_ctest::*;

extern "C" {
    fn ava1_test_corrupt_at_commit(n: i32, at: u64);
    fn ava1_test_corrupt_left() -> i32;
    fn ava1_test_set_pwrite_max(n: usize);
    fn ava1_test_recv_set_flags(flags: u32);
    fn ava1_test_job_readback() -> i32;
}

fn tmp(tag: &str) -> TempDir {
    TempDir::new(format!("ava1-readback-{tag}-{}", std::process::id()))
}

fn file(path: &str, size: u64) -> Entry {
    Entry {
        kind: ENTRY_FILE,
        mode: 0o644,
        size,
        mtime: 1_600_000_000,
        path: path.into(),
        root: None,
    }
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(29).wrapping_add(seed) ^ (i >> 12) as u8)
        .collect()
}

fn send_large(job: &CApplyJob, id: u32, d: &[u8]) {
    let g = GROUP as usize;
    for o in (0..d.len()).step_by(g).rev() {
        job.chunk(id, o as u64, &d[o..(o + g).min(d.len())]);
    }
    job.root(id, *blake3::hash(d).as_bytes());
}

/// `send_large` for a file bigger than the job's buffer budget: a chunk the budget cannot take yet
/// is offered again once the workers have written some.
fn send_large_paced(job: &CApplyJob, id: u32, d: &[u8]) {
    let g = GROUP as usize;
    for o in (0..d.len()).step_by(g) {
        let t0 = std::time::Instant::now();
        while job.try_chunk(id, o as u64, &d[o..(o + g).min(d.len())]) != 0 {
            assert!(t0.elapsed().as_secs() < 30, "the budget never freed");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    job.root(id, *blake3::hash(d).as_bytes());
}

fn wait_count(job: &CApplyJob, needle: &str, n: usize) {
    let t0 = std::time::Instant::now();
    while job.events().matches(needle).count() < n {
        assert!(
            t0.elapsed().as_secs() < 10,
            "waiting for {n} x {needle}: {}",
            job.events()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Clears the test knobs whatever the test did.
struct Knobs;
impl Drop for Knobs {
    fn drop(&mut self) {
        unsafe {
            ava1_test_corrupt_at_commit(0, 0);
            ava1_test_set_pwrite_max(0);
        }
    }
}

/// The gap the read-back closes: a byte that changes on the drive after it was received and
/// hashed is committed, and the job reports success.
#[test]
fn without_readback_a_byte_changed_on_the_drive_commits_unnoticed() {
    let _k = Knobs;
    let t = tmp("off");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(3 * GROUP as usize + 99, 1);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, 0, &m, 0);
    unsafe { ava1_test_corrupt_at_commit(1, GROUP + 7) };
    send_large(&job, 0, &d);
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    let got = std::fs::read(root.join("big.bin")).unwrap();
    assert_eq!(got.len(), d.len());
    assert_ne!(got, d, "the changed byte is in the committed file");
    assert_eq!(got[GROUP as usize + 7], d[GROUP as usize + 7] ^ 0x5a);
}

#[test]
fn readback_finds_a_byte_changed_on_the_drive_and_the_file_is_sent_again() {
    let _k = Knobs;
    let t = tmp("once");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(5 * GROUP as usize + 3, 2);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_corrupt_at_commit(1, 4 * GROUP + 1) };
    send_large(&job, 0, &d);
    // FileRetry{file 0, RETRY_VERIFY}: the sender's cue to send the whole file again
    job.wait_event("retry 0 1", 10_000);
    assert_eq!(unsafe { ava1_test_corrupt_left() }, 0);
    assert!(!root.join("big.bin").exists(), "nothing was committed");
    send_large(&job, 0, &d);
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
}

#[test]
fn a_file_that_never_reads_back_right_fails_the_job_with_err_verify() {
    let _k = Knobs;
    let t = tmp("always");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(2 * GROUP as usize + 10, 3);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_corrupt_at_commit(1000, 5) };
    // each mismatch asks for the file again (FileRetry), until AVA1_READBACK_TRIES (3) are spent
    send_large(&job, 0, &d);
    wait_count(&job, "retry 0 1", 1);
    send_large(&job, 0, &d);
    wait_count(&job, "retry 0 1", 2);
    send_large(&job, 0, &d);
    assert_eq!(job.wait(10_000), gen::ERR_VERIFY as i32, "{}", job.events());
    assert_eq!(
        job.events().matches("retry 0 1").count(),
        2,
        "{}",
        job.events()
    );
    assert!(!root.join("big.bin").exists());
}

/// Short writes: every pwrite takes at most 4097 bytes; the apply loops until all of a chunk is on
/// the drive, and the read-back agrees.
#[test]
fn short_writes_land_every_byte_and_read_back_right() {
    let _k = Knobs;
    let t = tmp("short");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(6 * GROUP as usize + 12_345, 4);
    let small = data(300 * 1024, 5); // a large file of one group: hashed from the drive by the commit
    let m = Manifest {
        entries: vec![
            file("a.bin", d.len() as u64),
            file("b.bin", small.len() as u64),
        ],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_set_pwrite_max(4097) };
    send_large(&job, 0, &d);
    send_large(&job, 1, &small);
    assert_eq!(job.wait(15_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), d);
    assert_eq!(std::fs::read(root.join("b.bin")).unwrap(), small);
}

/// JF_READBACK is a request for one attempt, not part of the job: a resume that turns it on or off
/// resumes the same job (its durable bytes kept), and the latest JobOpen decides.
#[test]
fn a_resume_may_turn_readback_on_or_off() {
    let _k = Knobs;
    let t = tmp("toggle");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = data(4 * GROUP as usize, 6);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    let r = CRecv::open(&t.join("jobs"), &t.join("dest"), 0, gen::POLICY_REPLACE, 0);
    assert_eq!(unsafe { ava1_test_job_readback() }, 0);
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    r.chunk(0, 0, &d[..2 * GROUP as usize]);
    r.wait_event("durable", 5000);
    unsafe { ava1_test_recv_set_flags(gen::JF_READBACK) };
    let r = r.restart(0); // the payload restarts; the next JobOpen asks for the read-back
    assert_eq!(unsafe { ava1_test_job_readback() }, 1);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert!(ev.contains("partial=1"), "the durable half was kept: {ev}");
    unsafe { ava1_test_corrupt_at_commit(1, 3 * GROUP) };
    r.chunk(0, 2 * GROUP, &d[2 * GROUP as usize..]);
    r.root(0, *blake3::hash(&d).as_bytes());
    r.wait_event("retry 0 1", 10_000); // the read-back is on for the resumed job
    r.chunk(0, 0, &d[..2 * GROUP as usize]);
    r.chunk(0, 2 * GROUP, &d[2 * GROUP as usize..]);
    r.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
    unsafe { ava1_test_recv_set_flags(0) };
}

/// A file big enough to be read back in stripes on several threads: a byte changed deep in the last
/// stripe is found, and the file is sent again. Ignored by default (hashing 1 GiB in a debug build is
/// slow): `cargo test --release -p ava1-ctest --test readback -- --ignored`.
#[test]
#[ignore = "hashes 1 GiB; run with --release -- --ignored"]
fn a_big_file_is_read_back_in_stripes_and_a_late_change_is_found() {
    let _k = Knobs;
    let t = tmp("stripes");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let size = (1usize << 30) + 4097; // 4 stripes of 256 MiB groups, a short tail
    let d = data(size, 7);
    let m = Manifest {
        entries: vec![file("big.bin", size as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_corrupt_at_commit(1, size as u64 - 3) };
    send_large_paced(&job, 0, &d);
    job.wait_event("retry 0 1", 60_000);
    send_large_paced(&job, 0, &d);
    assert_eq!(job.wait(60_000), 0, "{}", job.events());
    assert!(std::fs::read(root.join("big.bin")).unwrap() == d);
}

mod common;

/// End to end, Rust sender to C receiver, with the read-back on: a file past 4 GiB whose bytes at
/// 2^31 and 2^32 (where an offset cut to 32 bits would wrap) are marked, a payload restart half way
/// (the resume keeps the durable groups), and several lanes writing at their own offsets. The
/// result is compared byte for byte with the source. It writes 4 GiB, so it is ignored by default:
/// `cargo test --release -p ava1-ctest --test readback -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "writes 4 GiB; run with --release -- --ignored"]
async fn a_file_past_4_gib_resumes_after_a_restart_and_reads_back_whole() {
    use std::io::Read;
    use std::os::unix::fs::FileExt;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Duration;

    use ava1::send::Progress;
    use common::*;

    const MIB: u64 = 1 << 20;
    let d = dir("e2e-4g");
    let src = d.join("big.bin");
    let size = (4u64 << 30) + 3 * MIB + 17;
    {
        let f = std::fs::File::create(&src).unwrap();
        f.set_len(size).unwrap(); // sparse: reading it costs no disk
        let marks = [
            0u64,
            MIB - 3,
            (1 << 31) - 5,
            1 << 31,
            (1 << 32) - 3,
            (1 << 32) + MIB + 11,
            size - 8,
        ];
        for (k, at) in marks.into_iter().enumerate() {
            f.write_all_at(format!("mark{k:04}").as_bytes(), at)
                .unwrap();
        }
        // and a stretch of varied bytes across the 4 GiB line
        let v: Vec<u8> = (0..3 * MIB as usize).map(|i| (i * 7 + 1) as u8).collect();
        f.write_all_at(&v, (1 << 32) - MIB).unwrap();
    }
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let mut srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let addr = srv.addr();
    let progress = Arc::new(Progress::default());
    let (pg, pg2) = (progress.clone(), progress.clone());
    let dest = d.join("out.bin");
    let (src2, dest_s) = (src.clone(), dest.to_str().unwrap().to_string());
    let up = tokio::spawn(async move {
        upload(&addr, me, mine, &src2, &dest_s, [9; 16], move |o| {
            o.progress = pg.clone();
            o.flags |= gen::JF_READBACK;
        })
        .await
    });
    let t0 = std::time::Instant::now();
    while pg2.bytes_durable.load(Ordering::Relaxed) < size / 2 {
        assert!(
            t0.elapsed() < Duration::from_secs(600),
            "half never became durable"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    srv.restart_data(); // the payload dies and comes back: memory gone, journal and part file kept
    let (r, sessions) = up.await.unwrap();
    assert_eq!(r.status, 0, "{:?}", r.message);
    assert!(sessions >= 2, "the restart was a resume");
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), size);
    let (mut a, mut b) = (
        std::fs::File::open(&src).unwrap(),
        std::fs::File::open(&dest).unwrap(),
    );
    let (mut x, mut y) = (vec![0u8; 8 << 20], vec![0u8; 8 << 20]);
    let mut off = 0u64;
    while off < size {
        let n = ((size - off) as usize).min(x.len());
        a.read_exact(&mut x[..n]).unwrap();
        b.read_exact(&mut y[..n]).unwrap();
        assert!(x[..n] == y[..n], "the copy differs in the 8 MiB at {off}");
        off += n as u64;
    }
    drop(srv);
}
