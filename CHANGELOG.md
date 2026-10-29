# Changelog

All notable changes to the AVA1 crates and C library. The wire format is version 1 throughout;
see [README](README.md#versions).

## 1.0.2

The known limits of 1.0.1, fixed. Nothing changes on the wire or on disk, and the public API only
grows.

Fixes, Rust:

- A job directory is also locked across processes (an advisory lock on `<job dir>/run.lock`),
  so two programs sharing one jobs directory never run the same job at once; 1.0.1 locked it
  within one process only.
- A frame that overran its credit is answered on the lane it came on: a lane joined since on
  the same id is no longer told `ERR_CREDIT` or closed for it.
- `Progress::phase`, `skip_done_bytes` and `skip_total_bytes` now report a sequential source's
  resume skip (a 7z folder or solid RAR decoded and discarded up to what the receiver lacks):
  `phase` is the new `PHASE_SKIP` from the start of the pass until every discarded byte is
  counted, then `PHASE_SEND`. The total is the held groups of partly durable files plus what
  the new defaulted `SeqSource::bytes_to_skip` returns; a source that keeps the default
  (`None`) leaves its own discards out of both counters.

Documentation:

- Comments and docs no longer cite private planning documents; the crate's front page notes
  that a crate on Rust 2024 writes `ava1::r#gen`.

## 1.0.1

Fixes, speed-ups, documentation and release work. Nothing changes on the wire or in the journal
and pack-log formats: either side of this release works with the other side of 1.0.0.

Fixes, Rust:

- A folder download retried right as the first attempt finished could fail with "appeared during
  the download" and leave the files in `.ava-part`. One receiver now runs per job directory at a
  time, and a resume keeps the staging decision its journal recorded.
- The receiver bounds a manifest as it arrives (consecutive ids, at most 4,000,000 entries, as
  the C node already did), gives up on a manifest that never comes, and honours `JobCancel`
  while waiting for it.
- A duplicate small-file record that arrived while the first copy was still being written or
  synced could truncate and rewrite a file the journal already called durable; it is skipped.
- A lane that overruns its credit is closed after `ERR_CREDIT`, not only told.
- The sender keeps draining acknowledgements and credit while a lane opens or settles, instead
  of stalling the whole job for up to 20 s; a burst of `FileRetry` no longer starts a reader
  set per retry.
- Pack-log segments found at recovery are removed once swept, and appending after recovery no
  longer fails on a read-only segment; a pack log never overwrites a segment it did not
  recover.
- `Ava1Error::Refused` no longer claims the other device refused when the refusal was local; a
  Pong that echoes no Ping of ours no longer sets the RTT; a loaded identity secret is wiped.

Fixes, C:

- A session whose Welcome could not be sent kept its slot until restart: a few connection resets
  could block pairing or every client.
- A manifest replaced mid-job could race the lane feeder (use-after-free); a late `JobMap` could
  race the download reader.
- A worker could deadlock committing a file it was opening; a reset could truncate another
  file's part file; directories made for a file's path, and a new job's directory, were not
  synced; a short journal write could hide later records; a failed sync after compaction left
  appends on the old journal; deep staged trees were never collected.

Faster:

- C: CRC-32C is table-driven (about 9x); small files through the pack log run about 4x faster.
- Rust: a resumed job's setup no longer blocks the runtime (about 5x faster on a 512 MiB resume);
  bundles encode without copying their records; frames are read without zero-filling first.

Documentation and release:

- Rust: every public item is documented, the crate's front page has an overview and a
  quickstart, and `missing_docs` is enforced. The schema takes an optional `doc` for each entry,
  which `ava1-gen` writes into the generated Rust and C. Generated Rust now opens with a module
  doc: `ava1-gen --check` reports an extension's files as stale until they are regenerated once.
- The crates declare their minimum Rust version (1.89), and CI builds with it.
- Releases publish to crates.io through trusted publishing (see CONTRIBUTING.md).
- `manifest::PathError` has a new variant, `TooMany`.

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
