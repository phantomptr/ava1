# The C library

`c/` is an AVA1 node: it accepts sessions, pairs new devices, serves RPCs and runs uploads,
downloads and on-device copies into and out of the device's filesystem. It is written for small
and embedded devices: C11, POSIX threads and sockets, no allocations on the per-byte path, its own
bounded thread and file-descriptor budgets.

[`examples/c/node.c`](../examples/c/node.c) is a complete node in about 130 lines; this page
explains its parts.

## Building

```sh
make lib            # build/libava1.a
make example-c      # build/ava1-node, the example
make interop        # the example node against the Rust example client
```

Or compile the sources into your own build: every `c/*.c` except `platform_ps5.c` (or with it
instead of `platform_posix.c`), `c/gen/ava1_gen.c`, `c/third_party/monocypher/monocypher.c`, and
BLAKE3's `blake3.c`, `blake3_dispatch.c`, `blake3_portable.c` (add its assembly or define
`BLAKE3_NO_*` for the kernels you leave out). Include paths: `c`, `c/gen`, and the two
`c/third_party` folders. Build `ava1_chacha_avx2.c` with `-mavx2` on x86-64; the AEAD checks CPUID
before it uses it. `-O2` matters: every byte of a transfer is sealed and hashed.

Optional build-time switches:

| Define | Effect |
|---|---|
| `AVA1_TIMING_FLAG="/path"` | a file whose presence turns on one stage-timing line per download job (as does the `AVA1_TIMING` environment variable) |
| `AVA1_LOG_SMALL_OFF_FLAG="/path"` | a file whose presence turns the small-file log off (SPEC §15.7) |
| `AVA1_AEAD_PORTABLE` | the portable ChaCha20-Poly1305 only |

## The platform layer

Four functions in [`ava1_platform.h`](../c/ava1_platform.h): secure random bytes, sleep, file
preallocation, and setting a file's modification time. `platform_posix.c` implements them for
Linux, macOS and the BSDs; `platform_ps5.c` is the PlayStation 5 one. A new platform needs only
these.

## Starting a node

1. **Identity.** `ava1_identity_load_or_create(path, &cfg.identity)` keeps the node's X25519 key
   in a file (created on first run).
2. **Trust.** `cfg.peers_path` is the file of devices the node trusts, one per line. It is written
   by pairing; you can also seed it.
3. **Pairing.** `cfg.pairing_window_s` opens pairing by itself for that long while nobody is
   trusted; `ava1_server_open_pairing(seconds)` opens it at any time. `cfg.on_pair_request`
   receives the code to show the user. The limits on guesses, notifications and unconfirmed
   sessions have safe defaults (the comments in [`ava1_server.h`](../c/ava1_server.h)).
4. **The data plane.** Fill an `ava1_data_cfg_t`: the jobs folder (journals live there), and
   `may_write` / `may_read` / `same_device` callbacks that decide which absolute paths clients may
   touch. `ava1_data_start(&dc)`, then `cfg.data = ava1_data_hooks()` and add
   `AVA1_CAP_DATA_PLANE` to `cfg.caps`.
5. **RPC.** `cfg.rpc` runs on a worker thread for every request. Call `ava1_data_rpc` first (it
   answers the protocol's own methods and returns -1 for anything else), then answer `node.info`
   and your application's methods, or return `AVA1_ERR_UNKNOWN_METHOD`.
6. `ava1_server_start(&cfg)`. Stop with `ava1_server_stop()` and `ava1_data_stop()`.

`ava1_events_set_path(path)` turns on a small human-readable event log (one line per job event,
rolled at 1 MiB) that is useful in bug reports.

## Long operations

An application's long-running work (deleting a tree, hashing, unpacking) can run as a job that
clients start with `job.run`, poll with `job.status` and stop with `job.cancel`:

```c
static int op_hash(void *arg, ava1_op_ctx_t *c, const uint8_t *args, size_t len) {
    /* ... ava1_op_set_total(c, files, bytes); ava1_op_add(c, 1, n); ...
     * check ava1_op_cancelled(c) between steps; ava1_op_set_result(c, body, body_len); */
    return AVA1_STATUS_OK;
}
ava1_op_register(MY_OP_HASH, op_hash, NULL);
ava1_op_register_ex(MY_OP_BACKUP, op_backup, NULL, AVA1_OP_ONCE); /* never re-run on a repeat */
```

Operation numbers are yours (put them in your extension schema, see
[extending.md](extending.md)).

## Trusting the client that launched the node

A node binary can carry a 64-byte trust slot that the client deploying it stamps with its own key
(and, optionally, a launch token). `ava1_trust_slot_key()` and `ava1_trust_slot_token()` read it,
so that client is trusted without pairing and can verify it reached the node it launched
(SPEC §5.1–5.2).
