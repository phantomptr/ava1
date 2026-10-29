//! T28 regression, kept in its own binary: sixteen concurrent sessions starve the 500 ms
//! liveness timers of any sibling test in the same process.
//!
//! A large file's root rides the control connection and its chunks the lanes, so the root
//! can arrive after the batch that made the file's last range durable. The receiver used
//! to skip such a file forever: the job never finished and the closing handshake never
//! happened (about 1 run in 4 under concurrency). The ordered path sends every file
//! through that route, so many ordered downloads at once, each bounded, loop the close path.
mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen;
use ava1::host::FolderHost;
use ava1::manifest::Manifest;
use ava1::recv::{download_job, RecvOptions, Sink};
use ava1::session::{connect, Timing};

/// Keeps the bytes in memory: the receiver's commit-time check reads them back.
#[derive(Default)]
struct MemSink(Mutex<HashMap<u32, Vec<u8>>>);

impl Sink for MemSink {
    fn prepare(&self, _m: &Manifest) -> std::io::Result<()> {
        Ok(())
    }
    fn write_at(&self, id: u32, off: u64, d: &[u8]) -> std::io::Result<()> {
        let mut m = self.0.lock().unwrap();
        let b = m.entry(id).or_default();
        let end = off as usize + d.len();
        if b.len() < end {
            b.resize(end, 0);
        }
        b[off as usize..end].copy_from_slice(d);
        Ok(())
    }
    fn write_whole(&self, id: u32, d: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().insert(id, d.to_vec());
        Ok(())
    }
    fn sync(&self, _ids: &[u32]) -> std::io::Result<()> {
        Ok(())
    }
    fn read_at(&self, id: u32, off: u64, b: &mut [u8]) -> std::io::Result<usize> {
        let m = self.0.lock().unwrap();
        let Some(f) = m.get(&id) else { return Ok(0) };
        let start = off as usize;
        if start >= f.len() {
            return Ok(0);
        }
        let n = b.len().min(f.len() - start);
        b[..n].copy_from_slice(&f[start..start + n]);
        Ok(n)
    }
    fn commit(&self, _id: u32) -> std::io::Result<()> {
        Ok(())
    }
    fn finish(&self) -> std::io::Result<()> {
        Ok(())
    }
}

fn tree(d: &std::path::Path) {
    for i in 0..500 {
        let p = d.join(format!("a{}/f{i}", i % 9));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let n = if i % 100 == 0 { (3 << 20) + i } else { i * 7 };
        std::fs::write(p, (0..n).map(|k| (k + i) as u8).collect::<Vec<_>>()).unwrap();
    }
}

async fn ordered_download(tag: &str, job: u8) {
    let d = common::temp_dir(tag);
    tree(&d.join("share/out"));
    let host = Arc::new(FolderHost {
        root: d.join("share"),
        jobs_dir: d.join("hjobs"),
    });
    let t = Timing::default();
    let (addr, _ctx, id, peers) = common::paired_ctx(|c| c.with_jobs(host).with_timing(t)).await;
    let s = connect(&addr.to_string(), id, peers, "client", t)
        .await
        .unwrap();
    let mut link = s.job([job; 16]);
    download_job(
        &mut link,
        "out",
        gen::JF_ORDERED,
        Arc::new(MemSink::default()),
        RecvOptions {
            credit: 64 << 20,
            flags: gen::JF_ORDERED,
            jobs_dir: d.join("jobs"),
            ordered: true,
            progress: Arc::default(),
            cancel: Arc::default(),
            progress_deadline: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_ordered_downloads_all_finish() {
    for round in 0..3u8 {
        let mut all = Vec::new();
        for n in 0..16u8 {
            all.push(tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(90),
                    ordered_download(&format!("cr-{round}-{n}"), 10 + n),
                )
                .await
                .expect("an ordered download never finished");
            }));
        }
        for t in all {
            t.await.unwrap();
        }
    }
}
