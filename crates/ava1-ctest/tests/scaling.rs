#![cfg(unix)]
//! The upload rate must not depend on how many files the job holds: per-batch work in
//! the receiver is proportional to the batch, not to the manifest.
mod common;

use std::time::Instant;

use ava1_ctest::CServer;
use common::*;

/// Uploads `n` tiny files and returns files/s (wall clock from the first connect).
async fn rate(n: usize) -> f64 {
    let d = dir(&format!("scale-{n}"));
    let src = d.join("src");
    write_tree_small(&src, n);
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, fsync_us());
    let root = d.join("dest");
    let t = Instant::now();
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [3; 16],
        |_| {},
    )
    .await;
    let el = t.elapsed().as_secs_f64();
    assert_eq!(r.status, 0);
    let _ = std::fs::remove_dir_all(&d);
    let fps = n as f64 / el;
    eprintln!("SCALE n={n} secs={el:.2} files/s={fps:.0}");
    fps
}

fn write_tree_small(dir: &std::path::Path, n: usize) {
    // 1 KiB files in many directories of ~100 entries.
    let buf = vec![7u8; 1024];
    for i in 0..n {
        let p = dir.join(format!("d{:04}/f{i:06}", i / 100));
        if i % 100 == 0 {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        }
        std::fs::write(&p, &buf).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tiny_file_rate_is_flat_in_the_file_count() {
    // Warm up (page cache, thread pools), then compare small against large.
    let _ = rate(2_000).await;
    let small = rate(2_000).await.min(rate(2_000).await);
    let big = rate(50_000).await;
    assert!(
        big >= 0.7 * small,
        "50k files ran at {big:.0} files/s, under 70% of the 2k rate ({small:.0} files/s)"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual: prints the files/s curve"]
async fn curve() {
    let ns: Vec<usize> = std::env::var("SCALE_NS")
        .unwrap_or("2000,20000,100000".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    for n in ns {
        rate(n).await;
    }
}

/// Per-file fsync cost the receiver pretends the drive has (SCALE_FSYNC_US, default 0).
fn fsync_us() -> u32 {
    std::env::var("SCALE_FSYNC_US")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual: uploads the tree in SCALE_SRC"]
async fn tree() {
    let src = std::path::PathBuf::from(std::env::var("SCALE_SRC").unwrap());
    let d = dir("scale-tree");
    let peers = d.join("peers");
    let (me, mine) = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, fsync_us());
    let root = d.join("dest");
    let t = Instant::now();
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [4; 16],
        |_| {},
    )
    .await;
    eprintln!(
        "SCALE tree secs={:.2} status={}",
        t.elapsed().as_secs_f64(),
        r.status
    );
    let _ = std::fs::remove_dir_all(&d);
}
