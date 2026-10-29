# ava1-gen

<p align="center"><img src="https://raw.githubusercontent.com/phantomptr/ava1/main/branding/png/lockup-horizontal-black.png" alt="AVA1" width="300" /></p>

Generates the message codecs of the [AVA1](https://github.com/phantomptr/ava1) protocol, in Rust
and C, from a TOML schema: the protocol's own (`schema/ava1.toml`, with no arguments inside the
repository), or an application's extension with its own methods and messages:

```sh
cargo install ava1-gen
ava1-gen --ext app --schema app.toml --rust src/app_gen.rs --c-dir src/gen [--check]
```

See [docs/extending.md](https://github.com/phantomptr/ava1/blob/main/docs/extending.md).

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
