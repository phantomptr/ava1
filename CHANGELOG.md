# Changelog

All notable changes to the AVA1 crates and C library. The wire format is version 1 throughout;
see [README](README.md#versions).

## 1.0.0

The first release as a standalone library. The protocol has carried every transfer in
[ps5upload](https://github.com/phantomptr/ps5upload) since its 6.0 release; this release moves it
into its own repository, unchanged on the wire.

- The core schema (`schema/ava1.toml`) holds the protocol only. Applications describe their own
  methods in an extension schema, and `ava1-gen --ext <name>` generates their Rust and C codecs
  on top of the core ones. Method numbers 1–3 and 16–31 are the protocol's; 4–15 and 32 and up
  are the application's.
- C: `job.run` operations are registered by the application (`ava1_op_register`,
  `ava1_op_register_ex` with `AVA1_OP_ONCE` for operations a repeat must not run twice). File
  paths the library used to assume are set by the application: `ava1_events_set_path`, and the
  `AVA1_TIMING_FLAG` / `AVA1_LOG_SMALL_OFF_FLAG` build defines.
- Rust: diagnostic lines go through `ava1::diag::set_sink` (standard error by default).
- Environment variables are `AVA1_TIMING`, `AVA1_LOG_SMALL`, `AVA1_LANES`, `AVA1_CHUNK` and
  `AVA1_LANES_FIRST`.
- Examples: `send_folder` (both ends in one process), `send_to_node` (a client that pairs), and a
  complete C node (`examples/c/node.c`), tested against each other in CI.
- Licensed MIT OR Apache-2.0.
