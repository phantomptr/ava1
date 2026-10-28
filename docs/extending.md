# Extending AVA1 with your own methods

A session carries RPCs besides transfers: `RpcRequest{method, body}` answered by
`RpcResponse{status, body}`. The protocol defines a few methods itself; everything else is the
application's.

| Numbers | Owner |
|---|---|
| 1–3 | the protocol: `node.info`, `pairing.open`, `crypto.bench` |
| 16–31 | the protocol: the data plane (`job.copy`, `job.status`, `job.cancel`, `disk.calibrate`), `job.run`, `job.list`, node-to-node copies |
| 4–15, 32–65534 | the application |

A node that serves application methods advertises `CAP_MGMT`; an unknown method answers
`ERR_UNKNOWN_METHOD`, which is how a client tells an older node from a newer one. Requests are
capped at 56 KiB and replies at 256 KiB; at most 8 requests are in flight per session
(SPEC §7.4).

## An extension schema

Describe your methods and bodies in a TOML file with the same rules as
[`schema/ava1.toml`](../schema/ava1.toml):

```toml
[[const]]
name = "METHOD_DEVICE_STATUS"
ty = "u16"
value = 4

[[const]]
name = "JOB_OP_HASH"     # a job.run operation
ty = "u8"
value = 1

[[struct]]
name = "DeviceStatus"
fields = [
  { name = "uptime_s", ty = "u64" },
  { name = "label", ty = "str" },
]
ext = [
  { tag = 1, name = "battery", ty = "u8" },   # optional, tagged: older readers skip it
]
```

An extension may add constants and structs, not frame types (those are the protocol's), and may
not reuse a name the core schema has. For bodies you do not want to type, the core's `MgmtText`
carries text (JSON, say) and pages long replies with `more`.

Generate the codecs:

```sh
cargo install ava1-gen
ava1-gen --ext app --schema app.toml --rust src/app_gen.rs --c-dir src/gen \
         --source app.toml --regenerate "run make gen"
ava1-gen --ext app ... --check     # in CI: fails when the generated files are stale
```

- Rust: `src/app_gen.rs` uses `ava1::wire`, so it is a module of your crate (`mod app_gen;`).
  Its conformance helpers are in `app_gen::conformance`.
- C: `ava1_app_gen.h` includes the core `ava1_gen.h`. Constants keep the `AVA1_` prefix
  (`AVA1_METHOD_DEVICE_STATUS`) and types the `ava1_` prefix (`ava1_device_status_t`), so
  application code reads like protocol code.

## Serving and calling

C node:

```c
if (method == AVA1_METHOD_DEVICE_STATUS) {
    ava1_device_status_t st = { .uptime_s = uptime(), .label = (const uint8_t *)"lab", .label_len = 3 };
    ava1_w_t w;
    ava1_w_init(&w, out, cap);
    if (ava1_device_status_encode(&st, &w) != 0) return AVA1_ERR_INTERNAL;
    *out_len = w.len;
    return AVA1_STATUS_OK;
}
```

Rust client:

```rust,ignore
let reply = session.rpc(app_gen::METHOD_DEVICE_STATUS, &[]).await?;
let status = app_gen::DeviceStatus::decode(&reply.body)?;
```

The Rust node (`ava1::server::ServerCtx`) takes an `RpcHandler` closure for the same purpose.

## Versioning your methods

Method numbers and fields are a wire contract. Add new methods and new tagged `ext` fields
freely; never renumber a method, change a field's type, or reuse a retired number. A client
that needs a newer method checks for `ERR_UNKNOWN_METHOD` and degrades.
