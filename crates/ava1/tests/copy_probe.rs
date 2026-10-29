//! Per-frame copy probes for the data plane (run by hand: `cargo test --release -p ava1
//! --test copy_probe -- --ignored --nocapture`). They print per-frame costs and assert only
//! that the bytes are right: wall-clock numbers depend on the machine.
use std::time::Instant;

use ava1::conn::{FrameReader, FrameWriter};
use ava1::gen::{Bundle, BundleRecord, Chunk};
use ava1::wire::{FrameMessage, Message, Writer};

const ROUNDS: usize = 200;

fn best_of<F: FnMut()>(mut f: F) -> f64 {
    // The machine may be loaded: report the fastest of several batches, per call.
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        for _ in 0..ROUNDS {
            f();
        }
        best = best.min(t.elapsed().as_secs_f64() / ROUNDS as f64);
    }
    best * 1e6
}

/// A 4 MiB `Chunk` body: `to_bytes` on an empty writer against one presized for the body.
#[test]
#[ignore = "manual: prints the encode cost of a 4 MiB chunk"]
fn chunk_encode() {
    let data = vec![0x5au8; 4 << 20];
    let c = Chunk {
        job_id: [1; 16],
        file_id: 7,
        offset: 0,
        data,
    };
    let plain = best_of(|| {
        std::hint::black_box(c.to_bytes().unwrap());
    });
    let sized = best_of(|| {
        let mut w = Writer::with_capacity(c.data.len() + 64);
        c.encode_into(&mut w).unwrap();
        std::hint::black_box(w.buf);
    });
    let mut w = Writer::with_capacity(c.data.len() + 64);
    c.encode_into(&mut w).unwrap();
    assert_eq!(w.buf, c.to_bytes().unwrap());
    eprintln!("COPY chunk encode 4 MiB: to_bytes {plain:.0} us, presized {sized:.0} us");
}

/// A bundle of 64 16 KiB records: cloning the records into the message against moving them.
#[test]
#[ignore = "manual: prints the encode cost of a bundle"]
fn bundle_encode() {
    let recs: Vec<BundleRecord> = (0..64)
        .map(|i| BundleRecord {
            file_id: i,
            root: [i as u8; 32],
            data: vec![i as u8; 16 << 10],
        })
        .collect();
    let cloned = best_of(|| {
        let r = recs.clone(); // what the sender holds
        let b = Bundle {
            job_id: [1; 16],
            records: r.to_vec(),
        };
        std::hint::black_box(b.to_bytes().unwrap());
    });
    let moved = best_of(|| {
        let r = recs.clone();
        let len = r.iter().map(|r| r.data.len() + 64).sum::<usize>() + 64;
        let b = Bundle {
            job_id: [1; 16],
            records: r,
        };
        let mut w = Writer::with_capacity(len);
        b.encode_into(&mut w).unwrap();
        std::hint::black_box(w.buf);
    });
    let baseline = best_of(|| {
        std::hint::black_box(recs.clone());
    });
    eprintln!(
        "COPY bundle encode 64 x 16 KiB: clone+to_bytes {:.0} us, move+presized {:.0} us \
         (both net of the {baseline:.0} us the probe spends cloning its input)",
        cloned - baseline,
        moved - baseline
    );
}

/// One sealed 4 MiB frame written, then read back and opened.
#[test]
#[ignore = "manual: prints the seal and open cost of a 4 MiB frame"]
fn frame_round_trip() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let key = [0x33u8; 32];
    let body = vec![0xa5u8; 4 << 20];
    let wire = rt.block_on(async {
        let mut w = FrameWriter::new(Vec::new());
        w.set_key(key);
        w.send(Chunk::TYPE, 0, &body).await.unwrap();
        w.into_inner()
    });
    let send = best_of(|| {
        rt.block_on(async {
            let mut w = FrameWriter::new(Vec::with_capacity(wire.len()));
            w.set_key(key);
            w.send(Chunk::TYPE, 0, &body).await.unwrap();
            std::hint::black_box(w.into_inner());
        })
    });
    let recv = best_of(|| {
        rt.block_on(async {
            let mut r = FrameReader::new(&wire[..]);
            r.set_key(key);
            r.set_max_body(u32::MAX);
            let f = r.recv().await.unwrap();
            assert_eq!(f.body.len(), body.len());
            std::hint::black_box(f);
        })
    });
    let plain_wire = rt.block_on(async {
        let mut w = FrameWriter::new(Vec::new());
        w.send(Chunk::TYPE, 0, &body).await.unwrap();
        w.into_inner()
    });
    // Unsealed, the read is all copy: what the body's zero-fill costs shows here.
    let recv_plain = best_of(|| {
        rt.block_on(async {
            let mut r = FrameReader::new(&plain_wire[..]);
            r.set_max_body(u32::MAX);
            let f = r.recv().await.unwrap();
            assert_eq!(f.body.len(), body.len());
            std::hint::black_box(f);
        })
    });
    eprintln!(
        "COPY frame 4 MiB: sealed send {send:.0} us, sealed recv {recv:.0} us, \
         unsealed recv {recv_plain:.0} us"
    );
}
