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

## Releasing

1. Set `version` in the root `Cargo.toml`, add its section to `CHANGELOG.md`, and run `make check`
   and `make package`.
2. Tag the commit `vX.Y.Z` and push the tag. The release workflow checks that the tag matches
   `Cargo.toml` and creates a GitHub pre-release with the C sources archive and the changelog
   section as its notes.
3. Promote the pre-release to a full release on GitHub. That publishes `ava1-gen` and then `ava1`
   to crates.io; docs.rs builds the documentation from there.

crates.io credentials: each crate lists this repository, workflow `release.yml` and environment
`crates-io` as a trusted publisher (crates.io → the crate → Settings → Trusted Publishing), so
the workflow needs no stored token. A crate has to exist before it can have a trusted
publisher: for a crate's first release, add a `CARGO_REGISTRY_TOKEN` repository secret (a
crates.io API token with the publish-new and publish-update scopes), and delete it once trusted
publishing is set up.

By contributing you agree that your contribution is dual licensed under MIT OR Apache-2.0, as in
the [README](README.md#license).
