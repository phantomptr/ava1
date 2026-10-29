# AVA1

[![ci](https://github.com/phantomptr/ava1/actions/workflows/ci.yml/badge.svg)](https://github.com/phantomptr/ava1/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ava1.svg)](https://crates.io/crates/ava1)
[![docs.rs](https://img.shields.io/docsrs/ava1)](https://docs.rs/ava1)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

**AVA1 (Adaptive Verified Assembly, version 1)** is a protocol for moving large files and folders
between two devices over TCP, fast and safely. It was designed for sending files from a PC, phone or
server to a PlayStation 5, and works between any two devices:

- **Fast.** A transfer spreads over up to 8 parallel lanes. A governor adjusts lanes and chunk
  size to whatever is slowest at the moment (network, source disk, receiving disk) and reports it.
- **Resumable.** The receiver keeps a journal of what is durably written. A dropped connection
  or a restarted device picks up where it stopped instead of starting over.
- **Verified.** Every file is checked with BLAKE3, in 1 MiB groups, before it counts as written.
  Damaged data is fetched again.
- **Encrypted and paired.** A Noise XX handshake and ChaCha20-Poly1305 on every frame. Devices
  pair once with a short code the receiving device shows (CPace), and recognise each other's keys
  from then on.
- **Extensible.** Besides transfers, a session carries RPCs. An application defines its own
  methods and message types in a schema, and the generator writes the Rust and C codecs.

The repository holds the [specification](spec/SPEC.md), the [schema](schema/ava1.toml), the
[test vectors](vectors/), and two implementations that are tested against each other on every
change:

| | Where | What it has |
|---|---|---|
| Rust | [`crates/ava1`](crates/ava1) | Client and node (tokio): connect, pair, upload, download, RPC, a folder-backed node |
| C | [`c/`](c) | Node: the server, data plane, journal and apply, for small or embedded devices (POSIX, a four-function platform layer) |

AVA1 was built for [ps5upload](https://github.com/phantomptr/ps5upload), where the C node runs on
a PlayStation 5 and the Rust client on a PC, phone or server, and it carries every transfer
there. Nothing in this repository depends on that application.

## Rust

```toml
[dependencies]
ava1 = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

[`examples/send_folder.rs`](crates/ava1/examples/send_folder.rs) starts a node and sends a
folder to it, both in one process:

```sh
cargo run -p ava1 --example send_folder -- path/to/folder
```

The client part of it, against a node that already knows this device:

```rust,ignore
let session = ava1::session::connect(&addr, identity, peers, "my laptop", Timing::default()).await?;
let info = session.node_info().await?;

let source = LocalSource::new(folder.into());
let files = Arc::new(ava1::manifest::walk(&source, &|_: &str| false)?);
let mut job = session.job(ava1::keys::random_bytes()?);
let done = send_job(&mut job, files, Arc::new(source), SendOptions::upload("backups/today")).await?;
```

A device's identity is a key pair kept in a file (`Identity::load_or_create`), and the devices it
trusts are a peer store (`PeerStore::load`). A new device pairs while the node's pairing window
is open (`ServerCtx::open_pairing`, or `ava1_server_open_pairing` in C): the node shows a code,
and the client passes it to `Session::confirm_pairing`. Each side stores the other only if the
other proved it knew the code.

The library writes a few diagnostic lines (a transfer's bottleneck summary, kernel buffer sizes)
to standard error. Send them to your own log with `ava1::diag::set_sink`.

## C

```sh
make lib    # build/libava1.a, from c/ plus the bundled Monocypher and BLAKE3
```

The C library is a node: you give it an identity, a peers file, an RPC handler and a jobs
folder, and it serves sessions, pairing and transfers. It needs a C11 compiler and POSIX threads
and sockets. The platform layer is four functions ([`c/ava1_platform.h`](c/ava1_platform.h));
`c/platform_posix.c` covers Linux, macOS and the BSDs. See [docs/c-library.md](docs/c-library.md).

## Your own methods

Method numbers 1–3 and 16–31 are the protocol's. An application owns 4–15 and 32 and up, and
describes their bodies in its own schema:

```sh
cargo install ava1-gen
ava1-gen --ext app --schema my_app.toml --rust src/app_gen.rs --c-dir src/gen
```

That writes a Rust module and an `ava1_app_gen.{h,c}` pair that build on the core codecs. Long
operations run as jobs (`job.run`) with progress and cancel. See [docs/extending.md](docs/extending.md).

## Repository layout

```
spec/            the normative specification, the client contract, the nonce audit
schema/          ava1.toml: every message, struct and constant of the core protocol
vectors/         canonical encodings, handshake and key-derivation vectors
crates/ava1      the Rust implementation (client and node)
crates/ava1-gen  the codec generator (Rust and C) for the core and for application schemas
crates/ava1-ctest  the C library's tests, driven from Rust against the Rust implementation
crates/ava1-chaos  a TCP proxy that misbehaves on purpose, for the tests
c/               the C implementation; c/gen is generated, c/third_party is Monocypher and BLAKE3
docs/            using the C library, extending the protocol
```

## Development

```sh
make check      # format, clippy, generated code up to date, all tests, C with gcc and clang
make test       # just the tests (the interop tests run one at a time: the C server is a singleton)
make sanitize   # the interop tests under ASan + UBSan (Linux)
make fuzz       # C and Rust decoders (clang with libFuzzer, Rust nightly for cargo-fuzz)
```

See [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues privately: [SECURITY.md](SECURITY.md).

## Versions

The wire format is version 1. Its frames start with the bytes `A1`, and it grows without breaking
older peers through tagged extension fields, ignorable frame types and capability bits. A change
that old peers could not follow would be a new protocol with a new name. The crates follow
semantic versioning separately from that.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Bundled third-party code keeps its own licence: [Monocypher](c/third_party/monocypher/LICENCE.md)
(BSD-2-Clause or CC0-1.0) and [BLAKE3](c/third_party/blake3/README.md) (CC0-1.0 or Apache-2.0).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this repository by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
