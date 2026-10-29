#![cfg(unix)]
//! Upload throughput probes (run by hand: `cargo test --release -p ava1-ctest --test
//! perf_upload -- --ignored --nocapture`). They print numbers; they assert only that the
//! transfer is correct, because wall-clock rates depend on the machine.
mod common;

use std::sync::Arc;
use std::time::Instant;

use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::*;

const MIB: u64 = 1 << 20;

async fn run(tag: &str, mib: u64, wire_mib: Option<u64>, persist: bool) -> f64 {
    let d = dir(tag);
    let f = d.join("big.bin");
    std::fs::write(
        &f,
        (0..(mib * MIB) as usize)
            .map(|i| (i.wrapping_mul(7) ^ (i >> 13)) as u8)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let proxy = match wire_mib {
        Some(w) => Some(Arc::new(
            ChaosProxy::start(
                srv.addr().parse().unwrap(),
                ChaosConfig {
                    bytes_per_sec: Some(w * MIB),
                    ..Default::default()
                },
            )
            .await
            .unwrap(),
        )),
        None => None,
    };
    let addr = proxy
        .as_ref()
        .map(|p| p.addr.to_string())
        .unwrap_or_else(|| srv.addr());
    let dest = d.join("out/big.bin");
    let persist_dir = d.join("persist");
    let t = Instant::now();
    let (r, _) = upload(&addr, me, mine, &f, dest.to_str().unwrap(), [9; 16], |o| {
        if persist {
            o.persist = Some(persist_dir.clone());
        }
    })
    .await;
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(r.status, 0);
    let rate = (mib * MIB) as f64 / secs / 1e6;
    eprintln!(
        "PERF {tag}: {mib} MiB in {secs:.2}s = {rate:.1} MB/s (persist={persist}, wire={wire_mib:?}, lanes={})",
        r.max_lanes
    );
    let _ = std::fs::remove_dir_all(&d);
    rate
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn large_file_rates() {
    for persist in [false, true, false, true] {
        run("perf-open", 1024, None, persist).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn large_file_rates_on_a_gigabit_wire() {
    for persist in [false, true, false, true] {
        run("perf-wire", 1024, Some(105), persist).await;
    }
}

/// Small files (under the 256 KiB cutoff) through the durable-by-log path, which checksums every
/// byte into the pack log: run with AVA1_TEST_LOG_SMALL=1.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn small_file_rates() {
    for size in [1024usize, 64 << 10, 200 << 10] {
        let n = ((256usize << 20) / size).min(8000);
        let d = dir(&format!("perf-small-{size}"));
        let src = d.join("src");
        write_tree(&src, n, |i| size - (i % 7));
        let peers = d.join("peers");
        let (me, mine) = paired_client(&peers);
        let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
        let t = Instant::now();
        let (r, _) = upload(
            &srv.addr(),
            me,
            mine,
            &src,
            d.join("dst").to_str().unwrap(),
            [8; 16],
            |_| {},
        )
        .await;
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(r.status, 0);
        eprintln!(
            "PERF small {size} B x {n}: {secs:.2}s = {:.0} files/s, {:.1} MB/s",
            n as f64 / secs,
            (n * size) as f64 / secs / 1e6
        );
        drop(srv);
        let _ = std::fs::remove_dir_all(&d);
    }
}
