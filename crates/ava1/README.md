# ava1

<p align="center"><img src="https://raw.githubusercontent.com/phantomptr/ava1/main/branding/png/lockup-horizontal-black.png" alt="AVA1" width="360" /></p>

[![ci](https://github.com/phantomptr/ava1/actions/workflows/ci.yml/badge.svg)](https://github.com/phantomptr/ava1/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ava1)](https://crates.io/crates/ava1)
[![docs.rs](https://img.shields.io/docsrs/ava1)](https://docs.rs/ava1)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

The Rust implementation of [AVA1](https://github.com/phantomptr/ava1) (Adaptive Verified
Assembly, version 1), an encrypted, resumable, multi-lane protocol for moving large files and
folders between two devices over TCP. It has a client (connect, pair, upload, download, RPC) and
a node (`server::ServerCtx`, with `host::FolderHost` to serve a folder), and is tested on every
change against the C node in the same repository.

AVA1 was designed for transfers to a PlayStation 5: in
[ps5upload](https://github.com/phantomptr/ps5upload) the C node runs on the console and this
crate is the client on a PC, phone or server. Nothing in it is specific to the PS5, and it works
between any two devices.

- **Fast.** Up to 8 parallel lanes; a governor sets lanes and chunk size from whatever is slowest
  at the moment (network, source disk, receiving disk).
- **Resumable.** The receiver journals what is durably written; a dropped connection or a
  restarted device picks up where it stopped.
- **Verified.** Every file is checked with BLAKE3 in 1 MiB groups before it counts as written.
- **Encrypted and paired.** Noise XX, ChaCha20-Poly1305 on every frame, and a one-time pairing
  code, after which the devices recognise each other's keys.
- **Extensible.** Sessions carry RPCs; applications add their own methods with
  [`ava1-gen`](https://crates.io/crates/ava1-gen).

```toml
[dependencies]
ava1 = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust,ignore
let mut session = ava1::session::connect(addr, identity, peers, "my laptop", Timing::default()).await?;
if session.pairing_pending() {
    session.confirm_pairing(code_shown_by_the_node).await?;
}
let source = LocalSource::new(folder.into());
let files = Arc::new(ava1::manifest::walk(&source, &|_: &str| false)?);
let mut job = session.job(ava1::keys::random_bytes()?);
let done = send_job(&mut job, files, Arc::new(source), SendOptions::upload("backups/today")).await?;
```

Complete programs: [`send_folder`](https://github.com/phantomptr/ava1/blob/main/crates/ava1/examples/send_folder.rs)
(a node and a client in one process) and
[`send_to_node`](https://github.com/phantomptr/ava1/blob/main/crates/ava1/examples/send_to_node.rs)
(a client that pairs over the network):

```sh
cargo run -p ava1 --example send_folder -- path/to/folder
```

See the [API documentation](https://docs.rs/ava1), the
[specification](https://github.com/phantomptr/ava1/blob/main/spec/SPEC.md), and the
[repository README](https://github.com/phantomptr/ava1#readme).

## Support

AVA1 is made by [PhantomPtr](https://x.com/phantomptr). Questions are welcome on
[Discord](https://discord.gg/fzK3xddtrM), and if AVA1 is useful to you, you can
[buy me a coffee on Ko-fi](https://ko-fi.com/B0B81S0WUA).

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/phantomptr/ava1/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/phantomptr/ava1/blob/main/LICENSE-MIT) at your option.
