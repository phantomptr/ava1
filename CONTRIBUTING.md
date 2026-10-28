# Contributing

Thanks for helping. Issues and pull requests are welcome at
<https://github.com/phantomptr/ava1>.

## Before you open a pull request

```sh
make check
```

runs what CI runs on every pull request: formatting, clippy with warnings denied, the generated
codecs being current, the Rust tests, the C AEAD test, the C↔Rust interop tests, the C library
with gcc and clang under `-Wall -Wextra -Werror`, and the example node against the example client.
CI also runs the interop tests under ASan + UBSan and fuzzes the decoders.

## Ground rules

- **The wire format is a contract.** Version 1 peers already run in the field. Do not renumber a
  frame type, method, error or field, or change a field's type. Add new fields as tagged `ext`
  fields and new frames as ignorable; new capabilities get a new `CAP_*` bit.
- **Two implementations, one behaviour.** A protocol change lands in the spec, the schema, the
  Rust crate and the C library together, with a test in `crates/ava1-ctest` that runs one side
  against the other, and vectors in `vectors/` when an encoding is involved.
- **Edit the schema, not the codecs.** `crates/ava1/src/gen.rs` and `c/gen/` are generated: change
  `schema/ava1.toml` and run `make gen`.
- **C:** C11, no wall-clock time for timing (`CLOCK_MONOTONIC`), every allocation and thread
  bounded, no new dependency without a strong reason. Code in `c/third_party` is vendored and
  unmodified.

By contributing you agree that your contribution is dual licensed under MIT OR Apache-2.0, as in
the [README](README.md#license).
