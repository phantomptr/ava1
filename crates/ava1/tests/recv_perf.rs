//! Rust receiver throughput probes (run by hand: `cargo test --release -p ava1 --test recv_perf
//! -- --ignored --nocapture --test-threads=1`). A Rust sender uploads into a `FolderHost` over
//! loopback, so every byte goes through the Rust receiver's large-file path (chunk write, group
//! CVs, outboard, sync batch, commit). They print numbers; they assert only that the transfer is
//! correct, because wall-clock rates depend on the machine.
mod common;

use std::sync::Arc;
use std::time::Instant;

use ava1::host::FolderHost;
use ava1::manifest;
use ava1::send::{send_job, SendOptions};
use ava1::session::connect;
use ava1::source::LocalSource;

const MIB: u64 = 1 << 20;

/// Uploads `files` files of `mib` MiB each and returns MiB/s.
async fn upload(tag: &str, files: usize, mib: u64) -> f64 {
    let d = common::temp_dir(tag);
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for f in 0..files {
        std::fs::write(
            src.join(format!("big{f}.bin")),
            (0..(mib * MIB) as usize)
                .map(|i| (i.wrapping_mul(7 + f) ^ (i >> 13)) as u8)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let (addr, _ctx, id, peers) =
        common::paired_ctx(|c| c.with_jobs(host).with_timing(common::transfer())).await;
    let s = connect(&addr.to_string(), id, peers, "client", common::transfer())
        .await
        .unwrap();
    let source = LocalSource::new(src.clone());
    let m = Arc::new(manifest::walk(&source, &|_: &str| false).unwrap());
    let mut link = s.job([0x7a; 16]);
    let t = Instant::now();
    let r = send_job(&mut link, m, Arc::new(source), SendOptions::upload("in"))
        .await
        .unwrap();
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(r.status, 0);
    for f in 0..files {
        let name = format!("big{f}.bin");
        assert!(
            std::fs::read(src.join(&name)).unwrap()
                == std::fs::read(d.join("share/in").join(&name)).unwrap(),
            "{name} differs"
        );
    }
    let rate = (files as u64 * mib) as f64 / secs;
    eprintln!("RECV {tag}: {files} x {mib} MiB in {secs:.2}s = {rate:.0} MiB/s");
    let _ = std::fs::remove_dir_all(&d);
    rate
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn one_large_file_into_the_rust_receiver() {
    for k in 0..3 {
        upload(&format!("rp-one-{k}"), 1, 512).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn several_large_files_into_the_rust_receiver() {
    for k in 0..3 {
        upload(&format!("rp-many-{k}"), 8, 64).await;
    }
}
