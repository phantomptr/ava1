# AVA1 client contract

What a client, a node and the application that embeds them agree on beyond the wire format in
`SPEC.md`: how a node is replaced and stopped, what a client may show from a job's `Status`, and
how to read and switch the receive path of the C node. An application's own surface (its user
interface, HTTP routes, status words, job records) is the application's, written in its profile
(SPEC.md §7.3 names an example).

## 1. Node lifecycle

Three starting situations, and who handles each:

1. **A client finds no AVA1 node.** Nothing answers on the port, or the peer is not an AVA1 node
   (the handshake fails, or ends `ERR_UNSUPPORTED_VERSION`). To a client this is a node that is
   down; how it starts one (if it can) is the application's.
2. **A new node starts while an older instance runs on the same device.** The application must end
   the older one before the new one serves (C: `ava1_server_start` waits up to 5 s for an earlier
   server's connections to end, then binds). The signal is the application's; a flag file the old
   instance polls works. Two rules carry over to any such signal: the new instance's token is
   random (from the system's CSPRNG, never a time-based id, since a node's clock may be moved while
   it runs), and the old instance acts only on a token other than its own that was not already
   there when its poll started. The flag is removed at startup and after a successful takeover, so
   a leftover after a crash or reboot does nothing.
3. **A client replaces a running node it can reach.** It asks it to stop over the paired session
   (an application method), or uses the signal in 2.

**The stop sequence** (every cooperative exit reaches it): a stop method's reply is written first,
and the stop starts on another thread a moment later (300 ms in the reference application), so the
caller sees its answer. Then, in this order: stop accepting (`ava1_server_stop`); wait for the
connections to end (`ava1_server_conns`, bounded, 2 s); wait for in-flight RPC workers
(`ava1_server_rpc_inflight`, same bound; the workers are detached, so this count is the only
handle on them); wait for any application call that must not be cut (a platform API a cut call
could wedge); stop the data layer (`ava1_data_stop`: jobs stopped, threads joined, journals closed
so durable jobs resume). The application bounds all of it with a watchdog (8 s in the reference
application). An exit that skips the sequence (a crash) is still safe for data: the journals resume
every durable job (SPEC.md §14), and a crash leaves nothing a later start cannot recover.

## 2. What a client shows

A client reads these from `Status` (SPEC.md §16.9) and the sender's own state; each is optional,
and a client that does not have one shows nothing for it.

| item | from | meaning |
|------|------|---------|
| skipping | the Rust sender (§17.2) | a solid-archive resume: the decoder is discarding data the receiver already has. Progress is decoded vs to skip. |
| bottleneck | the sender's `BN_*` (§16) | one of network (`BN_NETWORK`), source (`BN_SOURCE`), receiver drive (`BN_DISK`), receiver workers (`BN_WORKERS`), receiver memory (`BN_CREDIT`: the credit window is the limit), none (`BN_NONE`). A finished job keeps the last one. |
| settling | `JobDone` ext `settling`, `Status` ext `unswept` (§15.7) | files are still being made durable in place on the receiver after the job finished. Show it ("finishing on the receiver") while `unswept` > 0. |

A node that answers but refuses an unpaired client (`ERR_NOT_PAIRED`, `ERR_PAIRING_CLOSED`) is a live
node that needs pairing, not a node that is down: a client that restarts nodes it thinks are down
must not restart one in that state.

**Pairing flow** (SPEC.md §5 step 5). A client holds the handshake whose code is on the node's
display until the user confirms or dismisses it, so asking twice shows one code. The states a client
distinguishes: accepted (a live paired session), code (a handshake waits for the user to type the
code the node shows; the client never has its own copy), closed (the node's pairing window is shut),
wrong node (a different key than the one pinned for this address answers), wrong code (try again: a
new handshake, a new code). "Forget" removes the key pinned for an address. Whether the node at a
new address is the one pinned for an old address is decided by the handshake's proof of the pinned
key, never by name. Pairing against the C node is tested in `crates/ava1-ctest/tests/server.rs`.

**Addresses.** A client owns the port (SPEC.md §1); callers give it a host.

### 4.2 Receive/apply path changes (perf-apply; host and loopback tests only)

Host and loopback tests only; every hardware run must be repeated after these changes. What changed
and what each is expected to move:

| change | where | expected effect |
|--------|-------|-----------------|
| preallocation outside `j->mu` (§2.1) | `lfile_open` | removes the multi-second stall of every worker and the feeder at each new large file on a slow drive; a slow USB drive's upload should lose its preallocation freeze for the other lanes. Preallocation and ENOSPC-first are unchanged. |
| commits on workers (§3.3) | `ava1_apply_commit_ready` | a corpus with many large files (four fsyncs each) stops blocking every batch; the commits overlap with the chunk work. |
| striped directory fsyncs (§3.3, §4) | `ava1_sync_dirset` in prepare and every batch; Rust `LocalSink::sync` | prepare's serial ~3 ms x thousands of parents and the per-batch serial directory fsyncs shrink by up to the worker count; this is the expected top item for a many-small-files upload. |
| per-chunk fsync on a slow drive (§6) | `sync_batch`, `write_chunk` | should bring a slow USB drive's large-file upload up to the drive's sequential rate; the line `slow drive: ... fsync per chunk from now on` shows it fired. |
| per-job stats | `ava1_apply_summary` | one line at every job's end: share of wall time in scan / data fsync / dirs / journal (job thread) and commit / preallocate (summed over workers); plus `preallocate took N ms for M MiB` and, above 1 s per GiB, `preallocation on this drive is slow`. |

The periodic per-batch line (`per batch ms: scan ... data ... dirs ...`) is opt-in like the sender's
stage timers: `AVA1_TIMING=1` in the environment, or the timing flag file when the application's
build names one (§4.2.1). The end-of-job line above is always printed. To read where a slow upload
spends its time: turn the flag on, run it, and read the node's standard error (or wherever the
application sends it).

Deferred (recorded here, not done):

- **Directory-fsync deferral** (the second option of the perf-apply review) was not taken: striping
  keeps the invariant "no journal record names a file whose directory is unsynced" (SPEC 15.4) and
  needs no `lstat` of every done file on resume. Durable-by-log (§4.3) supersedes both.
- **Rust receiver**: `LocalSink` (downloads) still does one `fsync` per file serially and opens each
  file under its state mutex; only its directory fsyncs are parallel now. It never preallocates, so
  item 1 has no Rust counterpart. A packed-sink design belongs with durable-by-log (§4.3).
- **Compaction while commits are in flight** is skipped for that batch (it retries after the next one);
  on a job that always has a large file committing, the journal grows past `AVA1_JNL_COMPACT_AT` until a
  quiet batch. Not measured; if it shows, compact between a commit's journal record and the next one's
  start instead.
- The slow-drive switch is one-way for the life of the job and never reverts; a drive that recovers
  (a USB hub contention that ends) keeps paying one small flush per chunk.

### 4.2.1 Debug flag files

The C node reads two optional flag files. Their paths are set by the application at build time; a
build that names none has only the environment switch.

| file | build define | effect | read |
|------|--------------|--------|------|
| timing | `-DAVA1_TIMING_FLAG='"/path"'` (environment: `AVA1_TIMING`) | the periodic per-batch stage line and the sender's stage timers | on every job |
| log-small off | `-DAVA1_LOG_SMALL_OFF_FLAG='"/path"'` (or set `ava1_log_small_flag_path`) | durable-by-log off for jobs opened while it exists (§4.3); recovery still runs | at every JobOpen |

Capture with the timing flag on: the end-of-job line (`dirs` near 0 is the durable-by-log signature),
`preallocate took N ms`, any `slow drive: ... fsync per chunk from now on`, and the `recovered N logged files, M lost (resent)`
line after a crash test. If a device incident follows a run, repeat it with the log-small-off flag present: if the incident
disappears the log path is implicated, and no rebuild was needed to find out.

### 4.3 Durable-by-log small files (host and loopback tests only)

The receiver (the C node and the Rust receiver) appends each small file to a pack log (`<job dir>/pack.<n>`), fsyncs the
log once per batch, journals the batch with the pack range, and makes the files durable in place later (the sweep).
A batch costs two fsyncs (log, journal) instead of N + D + 1 (SPEC 15.7). Host and loopback tests only; every
tiny-file and large-corpus measurement must be repeated on real drives.

| what | expected effect |
|------|-----------------|
| tiny-file upload | no longer bounded by the drive's per-file fsync rate (`disk.calibrate` shows it; a few hundred files/s on some internal drives); bounded by file creation. A corpus of hundreds of thousands of files should leave the per-file fsync and the per-batch directory fsyncs of §4.2 behind. |
| slow USB drive | a run bound by per-file fsync becomes sequential appends. |
| JobDone | arrives when the log is durable; files settle for a few seconds behind it (a merge or single file). A new-folder upload settles before its rename, so its tail waits (about the last 3 s of files). |

How to read a run: the node's end-of-job line (`finished in N ms ... data fsync ... dirs ...`) should show
`dirs` near 0 during the transfer; `recovered N logged files, M lost (resent)` appears at node start or JobOpen
after a crash. **Runtime off-switch:** create the log-small-off flag file (§4.2.1; any content) and every upload job
*opened from then on* takes the per-file fsync path, with no rebuild and no restart; delete the file to turn the log back
on. A job decides once, at open, and never switches mid-job. The node prints
`[ava1] job XXXXXXXX: durable-by-log OFF (debug flag)` to standard error at the start of such a job, so a run's
log says which path produced its numbers. Recovery ignores the switch: a crashed logged job's pack files are still
replayed at node start or JobOpen. Knobs (`ava1_data_cfg_t`): `log_small` (default on; `AVA1_LOG_SMALL_OFF` selects the
per-file path, kept for one release), `pack_segment` (64 MiB), `unswept_max` (256 MiB), `sweep_age_ms` (3000).

Rust receiver (`LocalSink`, downloads): on by default except on macOS, where it measured slower on loopback (2,270 vs
2,870 files/s for 2,000 tiny files; a plain fsync never reaches the drive there). `AVA1_LOG_SMALL=1/0`
forces it. The Rust receiver settles every logged file before it ends a job (it has no thread to settle behind JobDone),
so its unswept cap is soft (one credit window past `unswept_max`).

Failure paths: a failed sweep is retried with a backoff and, after five failures, reported in `Status`
(`code`/`current`); the Rust sender fails the upload on it, or reports a warning when it stops waiting (30 s, or a
cancel). Housekeeping recovers parked, reaped and crashed job directories that hold a log; the C node's job GC never
touches one. The Rust job GC (`journal::gc`) does not special-case pack files: a Rust download settles everything
before it ends, so a directory it left behind promised nothing to anyone.

Sweep content check (done): the sweep's first pass compares the file's BLAKE3 with its log record's before it
fsyncs and releases the record, so a file with the right size and wrong bytes (zero-filled blocks after a power cut) is re-made
from the log instead of being made durable as is. The reasoning held: the file was just written and is in the page cache, the
record is a sequential read, files are bounded by `PACK_REC_MAX`, and the check never fails a sound file (an unreadable record
skips it). Writing the test found a real bug the check exposed: the sync batch sorted its file ids but not their record
locations, so the sweep queue and the per-segment journal ranges paired ids with other files' records, and a live re-make
failed with `EIO`. Both arrays are now sorted together.

Deferred:

- **Pack preallocation**: segments are not preallocated (the log is fsynced every batch). If a drive shows the sparse
  collapse seen with unpreallocated part files, add `posix_fallocate` of the segment at roll time.
- **Syscalls per file**: the C node still does open, write, fchmod, utimensat by path, close for a logged file; the design's
  `open(mode)` + `futimens` saves two syscalls and was not done.
- **One pack writer at a time**: the append (offset + pwrite) is under one lock so a batch's range is a run of whole
  records; a 256 KiB record is ~100 us. If a profile shows it, shard the log per worker and journal one range per shard.
- **The sweep on a worker**: a sweep's directory syncs are serial (a worker must not wait on other workers' stripes). If
  `dirs` shows in the end-of-job line, give the sweep its own helper thread.
- **Rust macOS default**: revisit once a host where fsync is expensive (a Windows or Linux host) has numbers.
