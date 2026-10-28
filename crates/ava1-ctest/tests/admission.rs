#![cfg(unix)]
//! Review 006 #4 (checklist T): the console's job admission is bounded. A paired peer that opens
//! job after job without finishing any is refused `ERR_BUSY` once the job table (32) is full; the
//! session stays and the admitted jobs are untouched.
mod common;

use std::time::Duration;

use ava1::gen::{self, JobOpen, JobOpenAck};
use ava1::router::Inbound;
use ava1::session::connect;
use ava1::wire::FrameMessage;
use ava1_ctest::CServer;
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_job_opens_is_refused_busy_once_the_table_is_full() {
    let d = dir("admission");
    let ids = paired_client(&d.join("peers"));
    let srv = CServer::start_data(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        2000,
        2000,
        0,
    );
    let s = connect(&srv.addr(), ids.0, ids.1, "rust", calm())
        .await
        .unwrap();
    let (mut admitted, mut busy) = (0usize, 0usize);
    let mut links = Vec::new();
    for n in 0..48u32 {
        let mut id = [0x70u8; 16];
        id[..4].copy_from_slice(&n.to_le_bytes());
        let mut link = s.job(id);
        link.control
            .send(&JobOpen {
                job_id: id,
                kind: gen::JOB_UPLOAD,
                root: d.join(format!("dest{n}")).to_str().unwrap().into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let ack: JobOpenAck = loop {
            let ev = tokio::time::timeout(Duration::from_secs(20), link.rx.recv())
                .await
                .expect("the open was answered")
                .expect("the job channel is open");
            if let Inbound::Control(f) = ev {
                if f.ty == JobOpenAck::TYPE {
                    break f.decode().unwrap();
                }
            }
        };
        match ack.status {
            0 => admitted += 1,
            gen::ERR_BUSY => busy += 1,
            other => panic!("job {n}: unexpected status {other}"),
        }
        links.push(link);
    }
    assert!((1..=32).contains(&admitted), "{admitted} admitted");
    assert!(
        busy >= 16,
        "the flood past the table is refused: {busy} busy"
    );
    assert_eq!(admitted + busy, 48);
    assert!(!s.is_closed(), "the session survives the refusals");
}
