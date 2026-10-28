//! Launch tokens end to end (SPEC.md §5.2): a server stamped with this client's key and a
//! token it issued pairs with no code; anything else pairs the usual way.
mod common;

use std::sync::{Arc, Mutex};

use ava1::keys::Identity;
use ava1::launch::{LaunchTokens, TOKEN_TTL_S};
use ava1::peers::PeerStore;
use ava1::server::ServerCtx;
use ava1::session::connect;
use ava1::Ava1Error;
use common::*;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A server launched by `launcher` with `token` (None: a key-only stamp), whose pairing
/// window is open, like a freshly launched helper's.
async fn launched_server(
    launcher: [u8; 32],
    token: Option<[u8; 16]>,
) -> (std::net::SocketAddr, Arc<ServerCtx>, [u8; 32]) {
    let id = Identity::generate().unwrap();
    let key = id.public();
    let mut ctx = ServerCtx::new(
        id,
        "console",
        PeerStore::in_memory(),
        node_info_rpc("console"),
    )
    .with_timing(fast());
    ctx = match token {
        Some(t) => ctx.with_launch(launcher, t),
        None => ctx.with_launcher(launcher),
    };
    ctx.open_pairing(std::time::Duration::from_secs(60));
    let (addr, ctx) = start(ctx).await;
    (addr, ctx, key)
}

fn client_with(tokens: LaunchTokens) -> (Arc<Identity>, Arc<Mutex<PeerStore>>) {
    (
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(
            PeerStore::in_memory().with_launch_tokens(tokens),
        )),
    )
}

#[tokio::test]
async fn the_launched_helper_pairs_with_no_code_and_is_remembered() {
    let tokens = LaunchTokens::in_memory();
    let token = tokens.issue().unwrap();
    let (me, peers) = client_with(tokens);
    let (addr, ctx, server_key) = launched_server(me.public(), Some(token)).await;
    let s = connect(
        &addr.to_string(),
        me.clone(),
        peers.clone(),
        "laptop",
        fast(),
    )
    .await
    .unwrap();
    assert!(!s.pairing_pending(), "no prompt");
    assert!(
        peers.lock().unwrap().contains(&server_key),
        "stored silently"
    );
    assert_eq!(s.node_info().await.unwrap().name, "console");
    s.open_lane().await.unwrap();
    s.close().await;
    // The window it did not need is still open for anyone else; the server stored nothing.
    assert!(ctx.pairing_open());
    // Next time it is simply a known device: no proof needed.
    let (_, fresh) = client_with(LaunchTokens::in_memory());
    fresh.lock().unwrap().add(server_key, "console").unwrap();
    let s = connect(&addr.to_string(), me, fresh, "laptop", fast())
        .await
        .unwrap();
    assert!(!s.pairing_pending());
}

#[tokio::test]
async fn a_token_this_client_did_not_issue_means_pairing() {
    let tokens = LaunchTokens::in_memory();
    tokens.issue().unwrap();
    let (me, peers) = client_with(tokens);
    let (addr, _ctx, server_key) = launched_server(me.public(), Some([0x99; 16])).await;
    let s = connect(&addr.to_string(), me, peers.clone(), "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
    assert!(!peers.lock().unwrap().contains(&server_key));
    assert!(matches!(s.node_info().await, Err(Ava1Error::NotPaired)));
}

#[tokio::test]
async fn an_expired_token_means_pairing() {
    let tokens = LaunchTokens::in_memory();
    let token = [0x42; 16];
    tokens.record(token, now() - TOKEN_TTL_S - 60).unwrap();
    let (me, peers) = client_with(tokens);
    let (addr, _ctx, _) = launched_server(me.public(), Some(token)).await;
    let s = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
}

#[tokio::test]
async fn a_key_only_stamp_pairs_as_before() {
    let tokens = LaunchTokens::in_memory();
    tokens.issue().unwrap();
    let (me, peers) = client_with(tokens);
    let (addr, _ctx, _) = launched_server(me.public(), None).await;
    let mut s = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending(), "the client still compares a code once");
    // ... and the server needs no confirmation of its own: it trusts its launcher.
    s.confirm_trusted().unwrap();
    s.node_info().await.unwrap();
}

#[tokio::test]
async fn another_client_gets_no_proof_even_holding_the_token() {
    let tokens = LaunchTokens::in_memory();
    let token = tokens.issue().unwrap();
    let launcher = Identity::generate().unwrap().public();
    let (addr, _ctx, server_key) = launched_server(launcher, Some(token)).await;
    // Not the launcher's key: unknown to the server, so it pairs through the window.
    let (me, peers) = client_with(tokens);
    let s = connect(&addr.to_string(), me, peers.clone(), "phone", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
    assert!(!peers.lock().unwrap().contains(&server_key));
}

#[tokio::test]
async fn a_client_without_tokens_pairs_as_before() {
    let (me, peers) = (
        Arc::new(Identity::generate().unwrap()),
        Arc::new(Mutex::new(PeerStore::in_memory())),
    );
    let (addr, _ctx, _) = launched_server(me.public(), Some([1; 16])).await;
    let s = connect(&addr.to_string(), me, peers, "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending());
}

#[tokio::test]
async fn a_sniffed_token_cannot_impersonate_the_console_after_the_real_one_used_it() {
    let tokens = LaunchTokens::in_memory();
    let token = tokens.issue().unwrap();
    let (me, peers) = client_with(tokens);
    let (real, _c1, real_key) = launched_server(me.public(), Some(token)).await;
    let s = connect(
        &real.to_string(),
        me.clone(),
        peers.clone(),
        "laptop",
        fast(),
    )
    .await
    .unwrap();
    assert!(!s.pairing_pending(), "the real helper pairs silently");
    // A sniffer read the unauthenticated ELF, so it holds the token and the launcher's key.
    let (fake, _c2, fake_key) = launched_server(me.public(), Some(token)).await;
    assert_ne!(real_key, fake_key);
    let s = connect(&fake.to_string(), me, peers.clone(), "laptop", fast())
        .await
        .unwrap();
    assert!(s.pairing_pending(), "the replayed proof is not accepted");
    assert!(!peers.lock().unwrap().contains(&fake_key));
}
