//! A sequential source's resume skip as `Progress` reports it (SPEC.md §17.2), end to end:
//! a Rust sender with a solid in-memory source against a Rust folder host.
mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::gen;
use ava1::host::FolderHost;
use ava1::manifest::{Entry, Manifest};
use ava1::send::{send_job, Progress, SendOptions, PHASE_SEND, PHASE_SKIP};
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1::session::connect;
use ava1::source::LocalSource;

/// One solid block: every pass decodes from the first entry, dropping what is not wanted.
struct Solid {
    entries: Vec<(String, Vec<u8>)>,
    /// Fail the pass after this many delivered entries, once the receiver reported them
    /// durable (the first attempt of a job that is then resumed).
    cut_after: Option<(usize, Arc<Progress>)>,
    /// Time each dropped entry takes to decode.
    skip_delay: Duration,
    discarded: AtomicU64,
}

impl SeqSource for Solid {
    fn pass(
        &self,
        restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
    ) -> std::io::Result<()> {
        let mut delivered = 0;
        for (p, d) in self.entries.iter().skip(restart.0 as usize) {
            if let Some((n, pg)) = &self.cut_after {
                if delivered == *n {
                    let t = Instant::now();
                    while pg.files_durable.load(Ordering::Relaxed) < *n as u64 {
                        assert!(t.elapsed() < Duration::from_secs(20), "never durable");
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    return Err(std::io::Error::other("cut"));
                }
            }
            if want(p, d.len() as u64) == Keep::Skip {
                std::thread::sleep(self.skip_delay);
                self.discarded.fetch_add(d.len() as u64, Ordering::Relaxed);
                continue;
            }
            sink.begin(p)?;
            for c in d.chunks(65536) {
                sink.data(c)?;
            }
            sink.end()?;
            delivered += 1;
        }
        Ok(())
    }
    fn restart_for(&self, _id: u32) -> Restart {
        Restart::START
    }
    fn bytes_skipped(&self) -> u64 {
        self.discarded.load(Ordering::Relaxed)
    }
    fn bytes_to_skip(
        &self,
        restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
    ) -> Option<u64> {
        Some(
            self.entries
                .iter()
                .skip(restart.0 as usize)
                .filter(|(p, d)| want(p, d.len() as u64) == Keep::Skip)
                .map(|(_, d)| d.len() as u64)
                .sum(),
        )
    }
}

fn entries() -> Vec<(String, Vec<u8>)> {
    let pat = |n: usize, k: u8| -> Vec<u8> { (0..n).map(|i| (i as u8) ^ k).collect() };
    // Sorted, so decode order is manifest order.
    vec![
        ("a".to_string(), pat(3000, 1)),
        ("b".to_string(), pat(4000, 2)),
        ("c".to_string(), pat(5000, 3)),
        ("d".to_string(), pat(6000, 4)),
    ]
}

fn manifest(entries: &[(String, Vec<u8>)]) -> Arc<Manifest> {
    Arc::new(Manifest {
        entries: entries
            .iter()
            .map(|(p, b)| Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: b.len() as u64,
                mtime: 0,
                path: p.clone(),
                root: None,
            })
            .collect(),
    })
}

/// Samples `phase` and `skip_done_bytes` until `stop`: the distinct values, in order.
fn watch(pg: Arc<Progress>, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<(Vec<u8>, Vec<u64>)> {
    std::thread::spawn(move || {
        let (mut phases, mut dones) = (Vec::new(), Vec::new());
        loop {
            let last = stop.load(Ordering::Relaxed);
            let ph = pg.phase.load(Ordering::Relaxed);
            if phases.last() != Some(&ph) {
                phases.push(ph);
            }
            let d = pg.skip_done_bytes.load(Ordering::Relaxed);
            if dones.last() != Some(&d) {
                dones.push(d);
            }
            if last {
                return (phases, dones);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })
}

/// Uploads `entries()` as job `job`; with `resume`, a first attempt lands `a` and `b` and
/// fails, and the job is sent again. Returns the last attempt's progress and what the
/// watcher saw of it.
async fn upload(tag: &str, job: u8, resume: bool) -> (Arc<Progress>, Vec<u8>, Vec<u64>) {
    let d = common::temp_dir(tag);
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) =
        common::paired_ctx(|c| c.with_jobs(host).with_timing(common::transfer())).await;
    let entries = entries();
    let m = manifest(&entries);
    let empty = common::temp_dir(&format!("{tag}-src"));
    if resume {
        let s = connect(
            &addr.to_string(),
            id.clone(),
            peers.clone(),
            "client",
            common::transfer(),
        )
        .await
        .unwrap();
        let mut o = SendOptions::upload("in");
        o.seq = Some(Arc::new(Solid {
            entries: entries.clone(),
            cut_after: Some((2, o.progress.clone())),
            skip_delay: Duration::ZERO,
            discarded: AtomicU64::new(0),
        }));
        let mut link = s.job([job; 16]);
        let r = send_job(
            &mut link,
            m.clone(),
            Arc::new(LocalSource::new(empty.clone())),
            o,
        )
        .await;
        assert!(r.is_err(), "the first attempt was cut");
    }
    let s = connect(&addr.to_string(), id, peers, "client", common::transfer())
        .await
        .unwrap();
    let mut o = SendOptions::upload("in");
    let pg = o.progress.clone();
    o.seq = Some(Arc::new(Solid {
        entries: entries.clone(),
        cut_after: None,
        skip_delay: Duration::from_millis(150),
        discarded: AtomicU64::new(0),
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = watch(pg.clone(), stop.clone());
    let mut link = s.job([job; 16]);
    let r = tokio::time::timeout(
        Duration::from_secs(30),
        send_job(&mut link, m, Arc::new(LocalSource::new(empty)), o),
    )
    .await
    .expect("the job ended")
    .unwrap();
    stop.store(true, Ordering::Relaxed);
    let (phases, dones) = watcher.join().unwrap();
    assert_eq!(r.status, 0);
    for (p, b) in &entries {
        assert_eq!(
            &std::fs::read(d.join("share/in").join(p)).unwrap(),
            b,
            "{p}"
        );
    }
    (pg, phases, dones)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_solid_job_reports_its_skip() {
    let (pg, phases, dones) = upload("seq-skip-resume", 0x51, true).await;
    // a and b are durable: the resume decodes and drops their 7000 bytes first.
    assert_eq!(pg.skipped_files.load(Ordering::Relaxed), 2);
    assert_eq!(pg.skip_total_bytes.load(Ordering::Relaxed), 7000);
    assert_eq!(pg.skip_done_bytes.load(Ordering::Relaxed), 7000);
    assert_eq!(pg.phase.load(Ordering::Relaxed), PHASE_SEND);
    assert_eq!(phases, [PHASE_SEND, PHASE_SKIP, PHASE_SEND], "{dones:?}");
    assert!(dones.windows(2).all(|w| w[0] <= w[1]), "{dones:?}");
    assert_eq!(dones.last(), Some(&7000));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_solid_job_reports_no_skip() {
    let (pg, phases, dones) = upload("seq-skip-fresh", 0x52, false).await;
    assert_eq!(pg.skip_total_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(pg.skip_done_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(phases, [PHASE_SEND]);
    assert_eq!(dones, [0]);
}
