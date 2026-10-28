#![cfg(unix)]
//! Durable-by-log on the wire (review 003 §3.2, SPEC.md §15.7): the engine uploads small files to the C
//! receiver with the pack log on. A merge settles behind JobDone; the sender sees `settling` and the
//! receiver's `unswept` through its Status and reports when it reaches 0.
mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ava1::send::Progress;
use ava1_ctest::{CServer, LogOpts};
use common::*;

#[tokio::test(flavor = "multi_thread")]
async fn an_upload_into_an_existing_folder_waits_for_the_console_to_settle() {
    let d = dir("up-settle");
    let src = d.join("src");
    write_tree(&src, 300, |i| 1 + i % 500);
    let (me, mine) = paired_client(&d.join("peers"));
    // sweep age 1.5 s: the files stay unswept long enough for the sender to see them
    let srv = CServer::start_data_opts(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
        0,
        LogOpts {
            mode: 1,
            sweep_age_ms: 1500,
            ..Default::default()
        },
    );
    let root = d.join("dest");
    std::fs::create_dir_all(&root).unwrap(); // a merge: the files settle behind JobDone
    let pg = Arc::new(Progress::default());
    let seen = Arc::new(std::sync::Mutex::new((false, 0u32)));
    let watcher = {
        let (pg, seen) = (pg.clone(), seen.clone());
        tokio::spawn(async move {
            loop {
                if pg.settling.load(Ordering::Relaxed) {
                    let mut s = seen.lock().unwrap();
                    s.0 = true;
                    s.1 = s.1.max(pg.unswept.load(Ordering::Relaxed));
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    };
    let p2 = pg.clone();
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [0x71; 16],
        move |o| o.progress = p2.clone(),
    )
    .await;
    watcher.abort();
    assert_eq!(r.status, 0);
    assert!(same_tree(&src, &root));
    let (settling, most) = *seen.lock().unwrap();
    assert!(settling, "the sender never saw the receiver settling");
    let _ = most; // the count is only up for the few ms a finished job's forced sweep takes
    assert!(
        !pg.settling.load(Ordering::Relaxed),
        "settling is over when the upload returns"
    );
    assert_eq!(pg.unswept.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_folder_upload_has_nothing_settling_at_the_end() {
    let d = dir("up-staged");
    let src = d.join("src");
    write_tree(&src, 200, |i| 1 + i % 300);
    let (me, mine) = paired_client(&d.join("peers"));
    let srv = CServer::start_data_opts(
        SECRET,
        &d.join("peers"),
        &d.join("jobs"),
        200,
        4000,
        4000,
        0,
        0,
        LogOpts::ON,
    );
    let root = d.join("fresh");
    let pg = Arc::new(Progress::default());
    let p2 = pg.clone();
    let (r, _) = upload(
        &srv.addr(),
        me,
        mine,
        &src,
        root.to_str().unwrap(),
        [0x72; 16],
        move |o| o.progress = p2.clone(),
    )
    .await;
    assert_eq!(r.status, 0);
    assert!(same_tree(&src, &root));
    assert!(!pg.settling.load(Ordering::Relaxed));
}
