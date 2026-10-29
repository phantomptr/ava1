#![allow(dead_code)]
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::gen;
use ava1::keys::Identity;
use ava1::peers::PeerStore;
use ava1::server::{self, Limits, RpcHandler, ServerCtx};
use ava1::session::{RpcReply, Timing};
use ava1::wire::Message;
use tokio::net::TcpListener;

pub fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_millis(100),
        dead_after: Duration::from_millis(500),
        handshake: Duration::from_millis(500),
        ..Timing::default()
    }
}

/// For tests of what a transfer does, not of how fast a dead peer is noticed: fast pings, but a
/// dead-after and handshake an instrumented (coverage) build on a loaded CI runner cannot miss.
/// fast()'s 500 ms ended healthy uploads there with "the session ended" (data_rust, 8.7.0's CI).
#[allow(dead_code)]
pub fn transfer() -> Timing {
    Timing {
        dead_after: Duration::from_secs(5),
        handshake: Duration::from_secs(5),
        ..fast()
    }
}

pub fn node_info_rpc(name: &'static str) -> RpcHandler {
    Box::new(move |method, _| {
        if method == gen::METHOD_NODE_INFO {
            let info = gen::NodeInfo {
                version: "test".into(),
                platform: "rust".into(),
                name: name.into(),
                firmware: None,
            };
            RpcReply {
                status: gen::STATUS_OK,
                body: info.to_bytes().unwrap(),
            }
        } else {
            RpcReply {
                status: gen::ERR_UNKNOWN_METHOD,
                body: Vec::new(),
            }
        }
    })
}

pub async fn start(ctx: ServerCtx) -> (SocketAddr, Arc<ServerCtx>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let ctx = Arc::new(ctx);
    tokio::spawn(server::serve(l, ctx.clone()));
    (addr, ctx)
}

/// A server and a client identity that already know each other.
pub async fn paired() -> (
    SocketAddr,
    Arc<ServerCtx>,
    Arc<Identity>,
    Arc<Mutex<PeerStore>>,
) {
    paired_with(fast()).await
}

pub type RawReader = ava1::conn::FrameReader<tokio::net::tcp::OwnedReadHalf>;
pub type RawWriter = ava1::conn::FrameWriter<tokio::net::tcp::OwnedWriteHalf>;

/// A hand-driven client: connects (with a small receive buffer, so a peer's replies back
/// up quickly when nobody reads them) and completes the handshake, nothing more — no
/// heartbeats, no reading unless the test reads.
pub async fn raw_session(addr: SocketAddr, me: &Identity) -> (RawReader, RawWriter) {
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.set_recv_buffer_size(4096).unwrap();
    let stream = sock.connect(addr).await.unwrap();
    let (rh, wh) = stream.into_split();
    let (mut r, mut w) = (
        ava1::conn::FrameReader::new(rh),
        ava1::conn::FrameWriter::new(wh),
    );
    let est = ava1::handshake::client(&mut r, &mut w, me, "raw", |_| true, 0)
        .await
        .unwrap();
    assert!(est.pairing.is_none(), "raw sessions are for paired devices");
    (r, w)
}

/// Waits (up to `limit`) for `cond`; returns how long it took, or None.
pub async fn wait_for(limit: Duration, mut cond: impl FnMut() -> bool) -> Option<Duration> {
    let t = std::time::Instant::now();
    while !cond() {
        if t.elapsed() > limit {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Some(t.elapsed())
}

/// Limits that let one address (every test is 127.0.0.1) reach the global caps.
pub fn roomy() -> Limits {
    Limits {
        conns_per_ip: 10_000,
        ..Limits::default()
    }
}

/// `paired()` with other timing.
pub async fn paired_with(
    timing: Timing,
) -> (
    SocketAddr,
    Arc<ServerCtx>,
    Arc<Identity>,
    Arc<Mutex<PeerStore>>,
) {
    paired_opts(timing, Limits::default()).await
}

/// `paired()` with the server configured by `f` (a job host, extra hooks).
pub async fn paired_ctx(
    f: impl FnOnce(ServerCtx) -> ServerCtx,
) -> (
    SocketAddr,
    Arc<ServerCtx>,
    Arc<Identity>,
    Arc<Mutex<PeerStore>>,
) {
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut s_peers = PeerStore::in_memory();
    s_peers.add(c_id.public(), "client").unwrap();
    let mut c_peers = PeerStore::in_memory();
    c_peers.add(s_id.public(), "server").unwrap();
    let ctx = f(ServerCtx::new(
        s_id,
        "Rust test server",
        s_peers,
        node_info_rpc("Rust test server"),
    )
    .with_timing(fast()));
    let (addr, ctx) = start(ctx).await;
    (addr, ctx, c_id, Arc::new(Mutex::new(c_peers)))
}

pub async fn paired_opts(
    timing: Timing,
    limits: Limits,
) -> (
    SocketAddr,
    Arc<ServerCtx>,
    Arc<Identity>,
    Arc<Mutex<PeerStore>>,
) {
    let (s_id, c_id) = (
        Identity::generate().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    );
    let mut s_peers = PeerStore::in_memory();
    s_peers.add(c_id.public(), "client").unwrap();
    let mut c_peers = PeerStore::in_memory();
    c_peers.add(s_id.public(), "server").unwrap();
    let ctx = ServerCtx::new(
        s_id,
        "Rust test server",
        s_peers,
        node_info_rpc("Rust test server"),
    )
    .with_timing(timing)
    .with_limits(limits);
    let (addr, ctx) = start(ctx).await;
    (addr, ctx, c_id, Arc::new(Mutex::new(c_peers)))
}

pub fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ava1-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The code the console shows for the session of `key` (its screen: the one place the code
/// exists). Waits for the session to be registered after its Welcome.
pub async fn console_code(ctx: &ServerCtx, key: &[u8; 32]) -> u32 {
    let mut code = None;
    wait_for(Duration::from_secs(5), || {
        code = ctx.code_for_test(key);
        code.is_some()
    })
    .await
    .expect("the console shows a code");
    code.unwrap()
}
