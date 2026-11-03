#![cfg(unix)]
//! Resumed-upload throughput against the C receiver (run by hand:
//! `cargo test --release -p ava1-ctest --test resume_perf -- --ignored --nocapture`).
//! One large file: a fresh upload, then the same upload with the payload killed at ~40%
//! and resumed. Prints the rate over time and the fresh vs resumed rate.
//! `AVA1_BENCH_MIB` (file size, default 2048), `AVA1_BENCH_WIRE` (MiB/s cap per direction,
//! default none), `AVA1_BENCH_LINK` (MiB/s of one link every connection shares, default
//! none) and `AVA1_BENCH_DELAY_MS` (added per chunk each way, default 0).
mod common;

use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ava1::send::Progress;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::*;

const MIB: u64 = 1 << 20;

fn env_u64(k: &str) -> Option<u64> {
    std::env::var(k).ok().and_then(|v| v.trim().parse().ok())
}

fn write_file(p: &std::path::Path, mib: u64) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(p).unwrap());
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut buf = vec![0u8; MIB as usize];
    for _ in 0..mib {
        for c in buf.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            c.copy_from_slice(&x.to_le_bytes()[..c.len()]);
        }
        f.write_all(&buf).unwrap();
    }
    f.flush().unwrap();
}

/// Samples `bytes_durable` once a second until `done`, printing MB/s per second.
fn sampler(tag: String, pg: Arc<Progress>, done: Arc<std::sync::atomic::AtomicBool>) {
    std::thread::spawn(move || {
        let mut last = pg.bytes_durable.load(Ordering::Relaxed);
        let mut line = String::new();
        let mut n = 0;
        while !done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(1));
            let now = pg.bytes_durable.load(Ordering::Relaxed);
            line += &format!(
                " {:.0}/{}{}",
                now.saturating_sub(last) as f64 / 1e6,
                pg.lanes.load(Ordering::Relaxed),
                ["?", "N", "S", "D", "W", "C"]
                    .get(pg.bottleneck.load(Ordering::Relaxed) as usize)
                    .unwrap_or(&"?")
            );
            last = now;
            n += 1;
            if n % 20 == 0 {
                eprintln!("RATE {tag}:{line}");
                line.clear();
            }
        }
        if !line.is_empty() {
            eprintln!("RATE {tag}:{line}");
        }
    });
}

async fn run(tag: &str, mib: u64, kill_at: Option<f64>) -> (f64, Option<f64>) {
    let d = dir(tag);
    let f = d.join("big.bin");
    write_file(&f, mib);
    let size = mib * MIB;
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let mut srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let wire = env_u64("AVA1_BENCH_WIRE");
    let delay = env_u64("AVA1_BENCH_DELAY_MS").unwrap_or(0);
    let link = env_u64("AVA1_BENCH_LINK");
    let proxy = if wire.is_some() || link.is_some() || delay > 0 {
        Some(
            ChaosProxy::start(
                srv.addr().parse().unwrap(),
                ChaosConfig {
                    bytes_per_sec: wire.map(|w| w * MIB),
                    link_bytes_per_sec: link.map(|w| w * MIB),
                    delay: Duration::from_millis(delay),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        )
    } else {
        None
    };
    let addr = proxy
        .as_ref()
        .map(|p| p.addr.to_string())
        .unwrap_or_else(|| srv.addr());
    let dest = d.join("out.bin");
    let dest_s = dest.to_str().unwrap().to_string();
    let persist = d.join("persist");
    let pg = Arc::new(Progress::default());
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    sampler(tag.to_string(), pg.clone(), done.clone());
    let pg2 = pg.clone();
    let t0 = Instant::now();
    let up = tokio::spawn(async move {
        upload(&addr, me, mine, &f, &dest_s, [7; 16], move |o| {
            o.progress = pg2.clone();
            o.persist = Some(persist.clone());
        })
        .await
    });
    let mut resumed = None;
    if let Some(frac) = kill_at {
        while (pg.bytes_durable.load(Ordering::Relaxed) as f64) < frac * size as f64 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let first = t0.elapsed().as_secs_f64();
        let at = pg.bytes_durable.load(Ordering::Relaxed);
        srv.restart_data();
        resumed = Some((Instant::now(), at, first));
    }
    let (r, sessions) = up.await.unwrap();
    let total = t0.elapsed().as_secs_f64();
    done.store(true, Ordering::Relaxed);
    assert_eq!(r.status, 0);
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), size);
    let fresh_rate;
    let mut resumed_rate = None;
    match resumed {
        Some((t, at, first)) => {
            fresh_rate = at as f64 / first / 1e6;
            let secs = t.elapsed().as_secs_f64();
            let rr = (size - at) as f64 / secs / 1e6;
            resumed_rate = Some(rr);
            eprintln!(
                "PERF {tag}: {mib} MiB; before the kill {:.0} MiB in {first:.2}s = {fresh_rate:.1} MB/s; \
                 resumed {:.0} MiB in {secs:.2}s = {rr:.1} MB/s ({sessions} sessions, lanes {})",
                at as f64 / MIB as f64,
                (size - at) as f64 / MIB as f64,
                r.max_lanes
            );
        }
        None => {
            fresh_rate = size as f64 / total / 1e6;
            eprintln!(
                "PERF {tag}: {mib} MiB fresh in {total:.2}s = {fresh_rate:.1} MB/s (lanes {})",
                r.max_lanes
            );
        }
    }
    drop(proxy);
    drop(srv);
    let _ = std::fs::remove_dir_all(&d);
    (fresh_rate, resumed_rate)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn resumed_large_file_rate() {
    let mib = env_u64("AVA1_BENCH_MIB").unwrap_or(2048);
    let only = std::env::var("AVA1_BENCH_ONLY").unwrap_or_default();
    let (fresh, _) = if only == "resume" {
        (0.0, None)
    } else {
        run("rperf-fresh", mib, None).await
    };
    if only == "fresh" {
        return;
    }
    let (before, resumed) = run("rperf-resume", mib, Some(0.4)).await;
    eprintln!(
        "PERF summary: fresh {fresh:.1} MB/s, resumed job before kill {before:.1} MB/s, after {:.1} MB/s",
        resumed.unwrap()
    );
}
