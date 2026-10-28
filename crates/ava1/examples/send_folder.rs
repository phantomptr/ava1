//! Sends a folder to an AVA1 node and back, both ends in this process.
//!
//! ```text
//! cargo run -p ava1 --example send_folder -- <folder>
//! ```
//!
//! The node is the crate's own server with a `FolderHost` (it stores what it receives under
//! a share folder); a real deployment runs the node on the other device (the C library, or this
//! server) and only the client part below on this one. The two identities are introduced to
//! each other directly; on a real network the client pairs once with the code the node shows.
use std::sync::{Arc, Mutex};

use ava1::gen::{self, NodeInfo};
use ava1::host::FolderHost;
use ava1::keys::Identity;
use ava1::manifest;
use ava1::peers::PeerStore;
use ava1::send::{send_job, SendOptions};
use ava1::server::{self, ServerCtx};
use ava1::session::{connect, RpcReply, Timing};
use ava1::source::LocalSource;
use ava1::wire::Message;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let folder = std::env::args()
        .nth(1)
        .ok_or("usage: send_folder <folder>")?;
    let work = std::env::temp_dir().join(format!("ava1-example-{}", std::process::id()));

    // Two devices, each with its own long-term key, each trusting the other.
    let node_id = Identity::generate()?;
    let client_id = Arc::new(Identity::generate()?);
    let mut node_peers = PeerStore::in_memory();
    node_peers.add(client_id.public(), "example client")?;
    let mut client_peers = PeerStore::in_memory();
    client_peers.add(node_id.public(), "example node")?;

    // The node: answers node.info, and keeps uploads under <work>/share.
    let rpc: server::RpcHandler = Box::new(|method, _body| {
        if method != gen::METHOD_NODE_INFO {
            return RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: Vec::new(),
            };
        }
        let info = NodeInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            platform: std::env::consts::OS.into(),
            name: "example node".into(),
            firmware: None,
        };
        RpcReply {
            status: gen::STATUS_OK,
            body: info.to_bytes().unwrap_or_default(),
        }
    });
    let host = Arc::new(FolderHost {
        root: work.join("share"),
        jobs_dir: work.join("jobs"),
    });
    let ctx = Arc::new(ServerCtx::new(node_id, "example node", node_peers, rpc).with_jobs(host));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(server::serve(listener, ctx));

    // The client: connect, ask who it is, then send the folder as one job.
    let session = connect(
        &addr.to_string(),
        client_id,
        Arc::new(Mutex::new(client_peers)),
        "example client",
        Timing::default(),
    )
    .await?;
    let info = session.node_info().await?;
    println!(
        "connected to {} ({} {})",
        info.name, info.platform, info.version
    );

    let source = LocalSource::new(folder.clone().into());
    let files = Arc::new(manifest::walk(&source, &|_: &str| false)?);
    let job_id: [u8; 16] = ava1::keys::random_bytes()?;
    let mut link = session.job(job_id);
    let started = std::time::Instant::now();
    let result = send_job(
        &mut link,
        files,
        Arc::new(source),
        SendOptions::upload("received"),
    )
    .await?;
    println!(
        "status {} in {:.2?}: {} is now at {}",
        result.status,
        started.elapsed(),
        folder,
        work.join("share/received").display()
    );
    session.close().await;
    Ok(())
}
