# ava1

The Rust implementation of [AVA1](https://github.com/phantomptr/ava1), an encrypted, resumable,
multi-lane file-transfer protocol: a client (connect, pair, upload, download, RPC) and a node
(`server::ServerCtx`, with `host::FolderHost` to serve a folder). Tested against the C
implementation in the same repository.

```sh
cargo run -p ava1 --example send_folder -- path/to/folder
```

See the [repository README](https://github.com/phantomptr/ava1#readme) and the
[specification](https://github.com/phantomptr/ava1/blob/main/spec/SPEC.md).

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
