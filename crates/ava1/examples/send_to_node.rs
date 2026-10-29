//! Sends a folder to an AVA1 node on the network (the C example node, or any other).
//!
//! ```text
//! cargo run -p ava1 --example send_to_node -- <host:port> <folder> <destination> [state dir]
//! ```
//!
//! The first time, the node does not know this device: it shows a pairing code, and this
//! program asks for it on standard input. After that the two remember each other (this side in
//! `<state dir>/peers`, default `./ava1-client`) and no code is needed.
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ava1::keys::Identity;
use ava1::manifest;
use ava1::peers::PeerStore;
use ava1::send::{send_job, SendOptions};
use ava1::session::{connect, Timing};
use ava1::source::LocalSource;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [addr, folder, dest, ..] = args.as_slice() else {
        return Err("usage: send_to_node <host:port> <folder> <destination> [state dir]".into());
    };
    let state = PathBuf::from(args.get(3).map(String::as_str).unwrap_or("ava1-client"));
    std::fs::create_dir_all(&state)?;
    let me = Arc::new(Identity::load_or_create(&state.join("identity"))?);
    let peers = Arc::new(Mutex::new(PeerStore::load(&state.join("peers"))?));

    let mut session = connect(addr, me, peers, "send_to_node example", Timing::default()).await?;
    if session.pairing_pending() {
        eprintln!("the node shows a pairing code; type it here:");
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let code: u32 = line.trim().parse().map_err(|_| "the code is a number")?;
        session.confirm_pairing(code).await?;
        eprintln!("paired");
    }
    let info = session.node_info().await?;
    eprintln!(
        "connected to {} ({} {})",
        info.name, info.platform, info.version
    );

    let source = LocalSource::new(folder.into());
    let files = Arc::new(manifest::walk(&source, &|_: &str| false)?);
    let mut job = session.job(ava1::keys::random_bytes()?);
    let started = std::time::Instant::now();
    let done = send_job(&mut job, files, Arc::new(source), SendOptions::upload(dest)).await?;
    println!("status {} in {:.2?}", done.status, started.elapsed());
    session.close().await;
    if done.status == ava1::gen::STATUS_OK {
        Ok(())
    } else {
        Err(format!("the node ended the job with status {}", done.status).into())
    }
}
