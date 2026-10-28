//! The pool must not let live frame memory pass the admit budget (review perf-lanes): with
//! 2 MiB frames (not a size class) the peak bytes handed out during a real upload stay at or
//! below the budget, and every buffer is released at the end.
#![cfg(unix)]
mod common;

use ava1_ctest::CServer;
use common::*;
use std::os::raw::c_int;

extern "C" {
    fn ava1_frame_pool_outstanding() -> usize;
    fn ava1_frame_pool_outstanding_bytes() -> u64;
    fn ava1_frame_pool_peak_bytes() -> u64;
    fn ava1_frame_pool_reset_peak();
    fn ava1_frame_pool_idle(cls: c_int) -> usize;
}

const MIB: u64 = 1 << 20;
/// The data layer's default admit budget (`ava1_data_start`).
const BUDGET: u64 = 96 * MIB;

async fn upload_with_chunk(tag: &str, chunk_mib: &str, mib: u64) {
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
    // The feeder takes held frames in slow batches, so frames pile up to the credit window.
    srv.knob("feed_delay_ms", 150);
    std::env::set_var("AVA1_CHUNK", chunk_mib);
    let (base, base_bytes) = unsafe {
        ava1_frame_pool_reset_peak();
        (
            ava1_frame_pool_outstanding(),
            ava1_frame_pool_outstanding_bytes(),
        )
    };
    let dest = d.join("out/big.bin");
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &f,
        dest.to_str().unwrap(),
        [9; 16],
        |_| {},
    )
    .await;
    std::env::remove_var("AVA1_CHUNK");
    assert_eq!(r.status, 0);
    assert_eq!(std::fs::read(&f).unwrap(), std::fs::read(&dest).unwrap());
    drop(srv);
    unsafe {
        let peak = ava1_frame_pool_peak_bytes();
        eprintln!("{tag}: peak {} MiB of {} MiB", peak / MIB, BUDGET / MIB);
        assert!(peak > 2 * MIB, "the lanes did hold frames ({peak})");
        assert!(
            peak <= BUDGET,
            "{chunk_mib} MiB frames: peak {peak} passed the budget {BUDGET}"
        );
        assert_eq!(
            ava1_frame_pool_outstanding(),
            base,
            "a buffer leaked or was double counted"
        );
        assert_eq!(ava1_frame_pool_outstanding_bytes(), base_bytes);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// One test: the pool counters and the chunk env var are process-global.
#[tokio::test(flavor = "multi_thread")]
async fn live_frame_memory_stays_within_the_admit_budget() {
    upload_with_chunk("fb-2mib", "2", 192).await;
    // A class-sized chunk is pooled; the same bound holds, and idle buffers are kept.
    upload_with_chunk("fb-4mib", "4", 192).await;
    assert!(
        unsafe { ava1_frame_pool_idle(1) } > 0,
        "4 MiB chunks were pooled for reuse"
    );
}
