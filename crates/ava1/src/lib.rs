//! AVA1 — Adaptive Verified Assembly, version 1. Normative spec: `spec/SPEC.md`.
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
