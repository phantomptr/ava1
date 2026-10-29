//! AVA1 (Adaptive Verified Assembly, version 1): an encrypted, resumable, multi-lane protocol
//! for moving large files and folders between two devices over TCP. This crate is its Rust
//! implementation, a client and a node; the [specification] is normative, and a C node in the
//! same [repository] is tested against this crate on every change.
//!
//! AVA1 was designed for transfers to a PlayStation 5: in [ps5upload] the C node runs on the
//! console and this crate is the client on a PC, phone or server. Nothing in the protocol is
//! specific to the PS5, and it works between any two devices.
//!
//! - **Fast.** A transfer spreads over up to 8 parallel lanes; the [`governor`] sets lanes and
//!   chunk size from whatever is slowest at the moment (network, source disk, receiving disk).
//! - **Resumable.** The receiver keeps a [`journal`] of what is durably written; a dropped
//!   connection or a restarted device picks up where it stopped.
//! - **Verified.** Every file is checked with BLAKE3 in 1 MiB groups ([`verify`]) before it
//!   counts as written.
//! - **Encrypted and paired.** A Noise XX [`handshake`], ChaCha20-Poly1305 on every frame, and a
//!   one-time pairing code ([`cpace`]) after which the devices recognise each other's keys.
//! - **Extensible.** A session also carries RPCs; an application adds its own methods and
//!   messages with the `ava1-gen` code generator.
//!
//! # Sending a folder
//!
//! A client connects to a node, pairs the first time with the code the node shows, and sends a
//! folder as one job:
//!
//! ```no_run
//! use std::sync::{Arc, Mutex};
//!
//! use ava1::keys::Identity;
//! use ava1::peers::PeerStore;
//! use ava1::send::{send_job, SendOptions};
//! use ava1::session::{connect, Timing};
//! use ava1::source::LocalSource;
//!
//! # async fn run(code_shown_by_the_node: u32) -> Result<(), Box<dyn std::error::Error>> {
//! let me = Arc::new(Identity::load_or_create("state/identity".as_ref())?);
//! let peers = Arc::new(Mutex::new(PeerStore::load("state/peers".as_ref())?));
//!
//! let mut session = connect("192.168.1.20:9120", me, peers, "my laptop", Timing::default())
//!     .await?;
//! if session.pairing_pending() {
//!     session.confirm_pairing(code_shown_by_the_node).await?;
//! }
//!
//! let source = LocalSource::new("photos".into());
//! let files = Arc::new(ava1::manifest::walk(&source, &|_: &str| false)?);
//! let mut job = session.job(ava1::keys::random_bytes()?);
//! let done = send_job(&mut job, files, Arc::new(source), SendOptions::upload("backups/photos"))
//!     .await?;
//! assert_eq!(done.status, ava1::gen::STATUS_OK);
//! session.close().await;
//! # Ok(())
//! # }
//! ```
//!
//! The [examples] run as they are: `send_folder` starts a node and sends a folder to it in one
//! process, and `send_to_node` is a complete client that pairs over the network.
//!
//! # Running a node
//!
//! A node is a [`server::ServerCtx`] (its identity, its paired devices, an RPC handler, and a
//! job host such as [`host::FolderHost`]) served on a listener with [`server::serve`]. New
//! devices pair while [`server::ServerCtx::open_pairing`] holds the pairing window open.
//!
//! # Where things are
//!
//! | | |
//! |---|---|
//! | Sessions and RPC | [`session`] (client), [`server`] (node), [`router`] |
//! | Transfers | [`send`], [`recv`], [`host`], [`source`], [`seq`], [`manifest`], [`governor`] |
//! | Durability | [`journal`], [`packlog`], [`ranges`], [`verify`] |
//! | Identity and trust | [`keys`], [`peers`], [`handshake`], [`cpace`], [`trust`], [`launch`] |
//! | Wire format | [`gen`] (generated messages), [`wire`], [`frame`], [`conn`], [`crc32c`] |
//! | Support | [`diag`] (where diagnostic lines go), [`fslock`], [`hex`], [`Ava1Error`] |
//!
//! The generated messages live in [`gen`], whose name is a reserved word in Rust 2024: a crate
//! on edition 2024 writes `ava1::r#gen` (`use ava1::r#gen::{self, NodeInfo};`).
//!
//! The library writes a few diagnostic lines (a transfer's bottleneck summary, kernel buffer
//! sizes) to standard error; [`diag::set_sink`] sends them elsewhere.
//!
//! The wire format is version 1 and grows without breaking older peers (tagged extension
//! fields, ignorable frame types, capability bits). The crate follows semantic versioning
//! separately from it.
//!
//! [specification]: https://github.com/phantomptr/ava1/blob/main/spec/SPEC.md
//! [repository]: https://github.com/phantomptr/ava1
//! [ps5upload]: https://github.com/phantomptr/ps5upload
//! [examples]: https://github.com/phantomptr/ava1/tree/main/crates/ava1/examples
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/phantomptr/ava1/main/branding/png/icon-256.png"
)]
#![doc(
    html_favicon_url = "https://raw.githubusercontent.com/phantomptr/ava1/main/branding/png/favicon-32.png"
)]
#![warn(missing_docs)]
pub mod conn;
pub mod cpace;
pub mod crc32c;
pub mod diag;
mod error;
pub mod frame;
pub mod fslock;
#[rustfmt::skip]
pub mod gen;
pub mod governor;
pub mod handshake;
pub mod hex;
pub mod host;
pub mod journal;
pub mod keys;
pub mod launch;
mod link;
pub mod manifest;
pub mod packlog;
pub mod peers;
pub mod ranges;
pub mod recv;
pub mod router;
pub mod send;
pub mod seq;
pub mod server;
pub mod session;
pub mod source;
pub mod trust;
pub mod verify;
pub mod wire;

pub use error::Ava1Error;
