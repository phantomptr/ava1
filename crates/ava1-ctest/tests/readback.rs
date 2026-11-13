#![cfg(unix)]
//! JF_READBACK (SPEC.md §13.5): the receiver reads each large file back from its drive and checks it
//! against the CVs of what it received, before the commit. Without it, bytes that changed on the drive
//! after they were received (a misplaced or short write, a filesystem fault, another process) commit
//! unnoticed, because every other check hashes the bytes as they arrived.
//!
//! The read-back runs while a file arrives, on each stretch once a sync batch has made it durable, and
//! the commit reads only what is left. So a byte that changes on the drive before its group is read back
//! is found (`ava1_test_rb_corrupt` flips it right before that read, live or at the commit), and one that
//! changes after its group was read back is not (see
//! `a_byte_changed_after_its_group_was_read_back_is_not_found`).
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
    fn ava1_test_rb_corrupt(n: i32, at: u64);
    fn ava1_test_rb_corrupt_left() -> i32;
    fn ava1_test_rb_hold(on: i32);
    fn ava1_test_rb_stats(out: *mut u64);
    fn ava1_test_chunk_delay(ms: u32);
    fn ava1_test_apply_summary(out: *mut u8, cap: usize) -> usize;
    fn ava1_test_hold_batch_synced(on: i32);
    fn ava1_test_batch_synced_held() -> u32;
    fn ava1_test_apply_bytes_received() -> u64;
    fn ava1_test_apply_hold(on: i32);
}

const G: usize = GROUP as usize;

/// [0] bytes read back while the file arrived, [1] after its last byte, [2] files read back,
/// [3] mismatches, [4] live reads seen by the hook, [5] commit reads seen, [6] live reads held now,
/// [7] live reads done, [8] of those while chunks waited for a worker, [9] ranges batches handed to the
/// live read-back, [10] its threads, [11] reads that first asked the OS to drop their pages from the
/// cache, [12] of those it did, [13] reads with O_DIRECT.
fn rb_stats() -> [u64; 14] {
    let mut o = [0u64; 14];
    unsafe { ava1_test_rb_stats(o.as_mut_ptr()) };
    o
}

fn wait_until(what: &str, ms: u64, f: impl Fn() -> bool) {
    let t0 = std::time::Instant::now();
    while !f() {
        assert!(
            t0.elapsed().as_millis() < ms as u128,
            "waiting for {what}: {:?}",
            rb_stats()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The bytes of groups `gs` of `d`.
fn bytes_of(d: &[u8], gs: &[usize]) -> u64 {
    gs.iter()
        .map(|g| ((g + 1) * G).min(d.len()) - g * G)
        .sum::<usize>() as u64
}

fn send_groups(job: &CApplyJob, id: u32, d: &[u8], gs: &[usize]) {
    for &g in gs {
        job.chunk(id, (g * G) as u64, &d[g * G..((g + 1) * G).min(d.len())]);
    }
}

fn summary() -> String {
    let mut b = vec![0u8; 1024];
    let n = unsafe { ava1_test_apply_summary(b.as_mut_ptr(), b.len()) };
    String::from_utf8_lossy(&b[..n]).into_owned()
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
            ava1_test_rb_corrupt(0, 0);
            ava1_test_rb_hold(0);
            ava1_test_chunk_delay(0);
            ava1_test_set_pwrite_max(0);
            ava1_test_hold_batch_synced(0);
            ava1_test_apply_hold(0);
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
    unsafe { ava1_test_rb_corrupt(1, 4 * GROUP + 1) };
    send_large(&job, 0, &d);
    // FileRetry{file 0, RETRY_VERIFY}: the sender's cue to send the whole file again
    job.wait_event("retry 0 1", 10_000);
    assert_eq!(unsafe { ava1_test_rb_corrupt_left() }, 0);
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
    unsafe { ava1_test_rb_corrupt(1000, 5) };
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
    unsafe { ava1_test_rb_corrupt(1, 3 * GROUP) };
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
    unsafe { ava1_test_rb_corrupt(1, size as u64 - 3) };
    send_large_paced(&job, 0, &d);
    job.wait_event("retry 0 1", 60_000);
    send_large_paced(&job, 0, &d);
    assert_eq!(job.wait(60_000), 0, "{}", job.events());
    assert!(std::fs::read(root.join("big.bin")).unwrap() == d);
}

/// Four lanes, each writing its own groups and every lane in its own order: the groups are read back
/// as batches make them durable, whatever their order, and the commit reads only what is left. Every
/// byte is read back exactly once.
#[test]
fn groups_are_read_back_while_the_file_arrives_in_any_order() {
    let _k = Knobs;
    let t = tmp("live");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(24 * G + 4321, 8); // 25 groups, the last one short
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    // lane k has the groups k, k + 4, ...; the lanes take turns, the odd ones going backwards
    let lanes: Vec<Vec<usize>> = (0..4)
        .map(|k| {
            let mut v: Vec<usize> = (k..25).step_by(4).collect();
            if k % 2 == 1 {
                v.reverse();
            }
            v
        })
        .collect();
    let mut order = Vec::new();
    for i in 0..7 {
        for lane in &lanes {
            if let Some(&g) = lane.get(i) {
                order.push(g);
            }
        }
    }
    assert_eq!(order.len(), 25);
    let mut sent = 0u64;
    for wave in [&order[..9], &order[9..18]] {
        send_groups(&job, 0, &d, wave);
        sent += bytes_of(&d, wave);
        // no root yet, so nothing commits: the live read-back alone reads what is durable
        wait_until("the live read-back", 10_000, || rb_stats()[0] == sent);
    }
    send_groups(&job, 0, &d, &order[18..]);
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
    let st = rb_stats();
    assert!(st[0] >= sent, "{st:?}");
    assert_eq!(st[0] + st[1], d.len() as u64, "every byte once: {st:?}");
    assert_eq!((st[2], st[3]), (1, 0), "{st:?}");
    let line = summary();
    assert!(line.contains("while they arrived"), "{line}");
}

/// The receive workers never idle: every chunk's write takes 20 ms, and chunks keep queueing behind them
/// for the whole transfer. The read-back has threads of its own, so it still reads most of the file while
/// it arrives (all but what the last batches make durable), one read in flight while chunks wait.
#[test]
fn most_of_a_file_is_read_back_while_every_worker_is_busy_with_chunks() {
    let _k = Knobs;
    let t = tmp("busy");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(200 * G + 9, 16);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_chunk_delay(20) };
    send_large_paced(&job, 0, &d); // offers each chunk as soon as there is credit for it
    assert_eq!(job.wait(30_000), 0, "{}", job.events());
    unsafe { ava1_test_chunk_delay(0) };
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
    let st = rb_stats();
    let size = d.len() as u64;
    assert_eq!(st[0] + st[1], size, "every byte once: {st:?}");
    assert!(
        st[8] > 0,
        "no read while chunks waited: the workers were never all busy: {st:?}"
    );
    assert!(
        st[0] * 2 >= size,
        "only {} of {size} bytes read back live: {st:?}",
        st[0]
    );
    assert!(st[9] > 0 && st[10] >= 1, "{st:?}");
    let line = summary();
    assert!(line.contains("with chunks waiting"), "{line}");
}

/// Every read-back read, live or at the commit, first asks the OS to drop its range from the page cache
/// (a read answered from memory proves nothing about the drive). That the next read then misses the cache
/// cannot be checked portably; that the call is made for every read can. Linux has the call and must
/// answer it; macOS has none (the job's "read-back may read cached pages" line says so).
#[test]
fn every_read_back_read_first_drops_its_range_from_the_cache() {
    let _k = Knobs;
    let t = tmp("evict");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(20 * G + 333, 17);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    send_groups(&job, 0, &d, &(0..12).collect::<Vec<_>>());
    wait_until("the live read-back", 10_000, || rb_stats()[0] == 12 * GROUP);
    send_groups(&job, 0, &d, &(12..21).collect::<Vec<_>>());
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    let st = rb_stats();
    let reads = st[4] + st[5];
    assert!(st[4] >= 2 && reads >= 3, "{st:?}");
    assert_eq!(st[11], reads, "one drop per read: {st:?}");
    if cfg!(target_os = "linux") {
        assert_eq!(st[12], st[11], "posix_fadvise answered every drop: {st:?}");
    }
    if cfg!(target_os = "macos") {
        assert_eq!((st[12], st[13]), (0, 0), "{st:?}");
    }
    assert!(st[13] <= reads, "{st:?}");
    let line = summary();
    assert!(
        line.contains("reads with the cache dropped first"),
        "{line}"
    );
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
}

/// A byte that changes on the drive after its batch made it durable and before the live read-back reads
/// its group is found then. The sender is not told while it still sends the file (an old sender does not
/// expect it); the commit acts on it as on its own read-back: the file is sent again.
#[test]
fn a_byte_changed_before_its_group_is_read_back_is_found_while_the_file_arrives() {
    let _k = Knobs;
    let t = tmp("live-bad");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(12 * G + 77, 9);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_rb_corrupt(1, 5 * GROUP + 9) };
    let first: Vec<usize> = (0..8).rev().collect();
    send_groups(&job, 0, &d, &first);
    wait_until("the changed byte read back", 10_000, || unsafe {
        ava1_test_rb_corrupt_left() == 0
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!job.events().contains("retry"), "{}", job.events());
    assert!(rb_stats()[4] >= 1);
    send_groups(&job, 0, &d, &(8..13).collect::<Vec<_>>());
    job.root(0, *blake3::hash(&d).as_bytes());
    job.wait_event("retry 0 1", 10_000);
    assert_eq!(rb_stats()[3], 1);
    assert!(!root.join("big.bin").exists(), "nothing was committed");
    send_large(&job, 0, &d);
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
}

/// The documented gap: the read-back proves what the drive held once each group was durable. A byte
/// that changes after its group was read back, while the rest of the file still arrives, is not read
/// again, and the file commits with it.
#[test]
fn a_byte_changed_after_its_group_was_read_back_is_not_found() {
    let _k = Knobs;
    let t = tmp("live-late");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(10 * G, 10);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    send_groups(&job, 0, &d, &(0..6).collect::<Vec<_>>());
    wait_until("the live read-back", 10_000, || rb_stats()[0] == 6 * GROUP);
    unsafe { ava1_test_corrupt_at_commit(1, 2 * GROUP + 3) }; // flipped as the commit begins
    send_groups(&job, 0, &d, &(6..10).collect::<Vec<_>>());
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(unsafe { ava1_test_corrupt_left() }, 0);
    let got = std::fs::read(root.join("big.bin")).unwrap();
    assert_eq!(got[2 * G + 3], d[2 * G + 3] ^ 0x5a);
    let st = rb_stats();
    assert_eq!(st[0] + st[1], d.len() as u64, "{st:?}");
}

/// A late duplicate over groups already read back: they are durable again only once its batch is done,
/// and are read back again then (the commit trusts nothing read before the last write).
#[test]
fn a_group_written_again_is_read_back_again() {
    let _k = Knobs;
    let t = tmp("live-dup");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(10 * G + 5, 11);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    send_groups(&job, 0, &d, &(0..6).collect::<Vec<_>>());
    wait_until("the live read-back", 10_000, || rb_stats()[0] == 6 * GROUP);
    send_groups(&job, 0, &d, &[2]);
    wait_until("the group read back again", 10_000, || {
        rb_stats()[0] == 7 * GROUP
    });
    send_groups(&job, 0, &d, &(6..11).collect::<Vec<_>>());
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
    let st = rb_stats();
    assert_eq!(st[0] + st[1], d.len() as u64 + GROUP, "{st:?}");
}

/// A duplicate written while the batch that first makes its group durable is still running: the batch
/// ends, the group is read back (the duplicate's bytes, not yet synced), and only the next batch makes
/// the duplicate durable. The group is read back again after that batch, here by the commit, which
/// finds the byte that changed on the drive in between.
#[test]
fn a_group_written_again_during_its_first_batch_is_read_back_again() {
    let _k = Knobs;
    let t = tmp("live-dup-batch");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(8 * G + 5, 21);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_hold_batch_synced(1) };
    send_groups(&job, 0, &d, &(0..4).collect::<Vec<_>>());
    wait_until("the first batch synced", 10_000, || unsafe {
        ava1_test_batch_synced_held() == 1 && ava1_test_apply_bytes_received() == 4 * GROUP
    });
    send_groups(&job, 0, &d, &[1]); // the duplicate: written and recorded while that batch waits
    wait_until("the duplicate written", 10_000, || unsafe {
        ava1_test_apply_bytes_received() == 5 * GROUP
    });
    unsafe {
        ava1_test_apply_hold(1); // no second batch yet
        ava1_test_hold_batch_synced(0);
    }
    wait_until("the live read-back", 10_000, || rb_stats()[0] == 4 * GROUP);
    unsafe { ava1_test_corrupt_at_commit(1, GROUP + 7) }; // flipped as the commit's read-back begins
    send_groups(&job, 0, &d, &(4..9).collect::<Vec<_>>());
    job.root(0, *blake3::hash(&d).as_bytes());
    unsafe { ava1_test_apply_hold(0) };
    job.wait_event("retry 0 1", 10_000);
    assert_eq!(rb_stats()[3], 1);
    assert!(!root.join("big.bin").exists(), "nothing was committed");
    send_large(&job, 0, &d);
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
}

/// A group found wrong while the file arrives and then written again (a duplicate the sender had in
/// flight) is judged by its new bytes: read back again, right this time, and the file commits.
#[test]
fn a_group_found_wrong_and_written_again_is_judged_by_its_new_bytes() {
    let _k = Knobs;
    let t = tmp("live-fixed");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(9 * G, 15);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_rb_corrupt(1, 3 * GROUP + 2) };
    send_groups(&job, 0, &d, &(0..5).collect::<Vec<_>>());
    wait_until("the changed byte read back", 10_000, || unsafe {
        ava1_test_rb_corrupt_left() == 0
    });
    let before = rb_stats()[0];
    send_groups(&job, 0, &d, &[3]);
    wait_until("the group read back again", 10_000, || {
        rb_stats()[0] >= before + GROUP
    });
    send_groups(&job, 0, &d, &(5..9).collect::<Vec<_>>());
    job.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(job.wait(10_000), 0, "{}", job.events());
    assert!(!job.events().contains("retry"), "{}", job.events());
    assert_eq!(rb_stats()[3], 0);
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), d);
}

/// Three mismatches, each found by the live read-back while the file arrives: the job fails with
/// ERR_VERIFY after the third, as when the commit finds them.
#[test]
fn mismatches_found_while_the_file_arrives_fail_the_job_after_three() {
    let _k = Knobs;
    let t = tmp("live-always");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(10 * G, 12);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_rb_corrupt(1000, 3) };
    for round in 1..=3 {
        send_groups(&job, 0, &d, &(0..5).collect::<Vec<_>>());
        wait_until("the live read-back", 10_000, || unsafe {
            ava1_test_rb_corrupt_left() == 1000 - round
        });
        send_groups(&job, 0, &d, &(5..10).collect::<Vec<_>>());
        job.root(0, *blake3::hash(&d).as_bytes());
        if round < 3 {
            wait_count(&job, "retry 0 1", round as usize);
        }
    }
    assert_eq!(job.wait(10_000), gen::ERR_VERIFY as i32, "{}", job.events());
    assert_eq!(job.events().matches("retry 0 1").count(), 2);
    assert_eq!(rb_stats()[3], 3);
    assert!(!root.join("big.bin").exists());
}

/// A stop while a live read-back is reading does not wait for it to finish, and the resumed job reads
/// back what this process has not (the groups made durable before the restart included).
#[test]
fn a_stop_during_a_live_read_back_is_prompt_and_the_resume_reads_back_the_rest() {
    let _k = Knobs;
    let t = tmp("live-stop");
    std::fs::create_dir_all(t.join("dest")).unwrap();
    let d = data(12 * G + 5, 13);
    let m = Manifest {
        entries: vec![file("big", d.len() as u64)],
    };
    let r = CRecv::open(
        &t.join("jobs"),
        &t.join("dest"),
        gen::JF_READBACK,
        gen::POLICY_REPLACE,
        0,
    );
    r.manifest(&m);
    r.wait_event("map status=0", 5000);
    unsafe { ava1_test_rb_hold(1) };
    send_groups(&r, 0, &d, &(0..6).collect::<Vec<_>>());
    wait_until("a live read-back held", 10_000, || rb_stats()[6] >= 1);
    let t0 = std::time::Instant::now();
    let r = r.restart(0); // the payload stops mid read-back and comes back
    assert!(
        t0.elapsed().as_secs() < 5,
        "the stop took {:?}",
        t0.elapsed()
    );
    unsafe { ava1_test_rb_hold(0) };
    assert_eq!(unsafe { ava1_test_job_readback() }, 1);
    r.manifest(&m);
    let ev = r.wait_event("map status=0", 5000);
    assert!(
        ev.contains("partial=1"),
        "the durable groups were kept: {ev}"
    );
    send_groups(&r, 0, &d, &(6..13).collect::<Vec<_>>());
    r.root(0, *blake3::hash(&d).as_bytes());
    assert_eq!(r.wait(10_000), 0, "{}", r.events());
    assert_eq!(std::fs::read(t.join("dest/big")).unwrap(), d);
    let st = rb_stats();
    assert_eq!(st[0] + st[1], d.len() as u64, "{st:?}");
    assert_eq!(st[3], 0);
}

/// Ending a job (a cancel) while a live read-back is reading does not wait for it either.
#[test]
fn ending_a_job_during_a_live_read_back_is_prompt() {
    let _k = Knobs;
    let t = tmp("live-end");
    let root = t.join("dest");
    std::fs::create_dir_all(&root).unwrap();
    let d = data(8 * G, 14);
    let m = Manifest {
        entries: vec![file("big.bin", d.len() as u64)],
    };
    let job = CApplyJob::begin(&t.join("jobs"), &root, gen::JF_READBACK, &m, 0);
    unsafe { ava1_test_rb_hold(1) };
    send_groups(&job, 0, &d, &(0..4).collect::<Vec<_>>());
    wait_until("a live read-back held", 10_000, || rb_stats()[6] >= 1);
    let t0 = std::time::Instant::now();
    let _ = job.end();
    assert!(
        t0.elapsed().as_secs() < 5,
        "the end took {:?}",
        t0.elapsed()
    );
    assert_eq!(rb_stats()[6], 0, "the held read-back let go");
    assert!(!root.join("big.bin").exists());
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
