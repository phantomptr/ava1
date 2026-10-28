#![cfg(unix)]
//! The Rust sender on the wire (Task 15): a real `send_job` run against the C receiver
//! (Task 14's data layer), plain and through a shaped link.
mod common;

use std::sync::{Arc, Mutex};

use ava1::manifest::Manifest;
use ava1::send::{send_job, SendOptions};
use ava1::session::{connect, Session};
use ava1::source::LocalSource;
use ava1_chaos::{ChaosConfig, ChaosProxy};
use ava1_ctest::CServer;
use common::*;

type Ids = (
    Arc<ava1::keys::Identity>,
    Arc<Mutex<ava1::peers::PeerStore>>,
);

/// (the manifest, the tiny files as (name, bytes), the large file's bytes)
type Tree = (Manifest, Vec<(String, Vec<u8>)>, Vec<u8>);

/// The C server reads its peers file once, at start: pair before starting it.
async fn session(srv: &CServer, ids: &Ids) -> Session {
    connect(&srv.addr(), ids.0.clone(), ids.1.clone(), "rust", calm())
        .await
        .unwrap()
}

/// Many tiny files plus one multi-group file, walked into a manifest.
fn tree(dir: &std::path::Path) -> Tree {
    let mut expect: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..150 {
        let data: Vec<u8> = (0..(i % 2000 + 1))
            .map(|j| ((i * 31 + j * 7) % 251) as u8)
            .collect();
        let name = format!("f{i:03}.bin");
        std::fs::write(dir.join(&name), &data).unwrap();
        expect.push((name, data));
    }
    let big: Vec<u8> = (0..((2 << 20) + 3))
        .map(|j| (j * 7 + j / 4093) as u8)
        .collect();
    std::fs::write(dir.join("big.bin"), &big).unwrap();
    let src = LocalSource::new(dir.to_path_buf());
    (ava1::manifest::walk(&src, &|_| false).unwrap(), expect, big)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rust_sender_uploads_many_tiny_files_and_one_large_file() {
    let d = dir("send-rs");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 2000, 2000, 0);
    let s = session(&srv, &ids).await;

    let src_dir = d.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let (m, expect, big) = tree(&src_dir);
    let m = Arc::new(m);
    let root = d.join("dest");
    let mut link = s.job([0x61u8; 16]);
    let report = send_job(
        &mut link,
        m.clone(),
        Arc::new(LocalSource::new(src_dir)),
        SendOptions::upload(root.to_str().unwrap()),
    )
    .await
    .expect("the upload completed");
    assert_eq!(report.status, 0, "{:?}", report.message);
    assert_eq!(report.files, m.files());
    assert_eq!(report.bytes, m.bytes());
    assert_eq!(report.resent, 0, "nothing was resent on a clean link");

    for (name, want) in &expect {
        let got = std::fs::read(root.join(name)).unwrap();
        assert_eq!(&got, want, "{name}");
    }
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), big);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rust_sender_completes_through_a_shaped_link() {
    // The link itself is the limit (4 MiB/s through the chaos proxy): the per-lane
    // in-flight cap must size itself by the lane's real rate, or frames pile up behind
    // the dead-air limit and the job stalls (correction 3's scenario).
    let d = dir("send-shaped");
    let peers = d.join("peers");
    let ids = paired_client(&peers);
    let srv = CServer::start_data(SECRET, &peers, &d.join("jobs"), 200, 4000, 4000, 0);
    let proxy = ChaosProxy::start_on(
        "127.0.0.1:0",
        srv.addr().parse().unwrap(),
        ChaosConfig {
            bytes_per_sec: Some(4 << 20),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let s = connect(
        &proxy.addr.to_string(),
        ids.0.clone(),
        ids.1.clone(),
        "rust",
        calm(),
    )
    .await
    .unwrap();

    let src_dir = d.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let big: Vec<u8> = (0..(12 << 20)).map(|j| (j * 7 + j / 4093) as u8).collect();
    std::fs::write(src_dir.join("big.bin"), &big).unwrap();
    let src = LocalSource::new(src_dir.clone());
    let m = Arc::new(ava1::manifest::walk(&src, &|_| false).unwrap());
    let root = d.join("dest");
    let mut link = s.job([0x62u8; 16]);
    let report = send_job(
        &mut link,
        m.clone(),
        Arc::new(src),
        SendOptions::upload(root.to_str().unwrap()),
    )
    .await
    .expect("the upload completed");
    assert_eq!(report.status, 0, "{:?}", report.message);
    assert_eq!(report.bytes, m.bytes());
    assert_eq!(std::fs::read(root.join("big.bin")).unwrap(), big);
}
