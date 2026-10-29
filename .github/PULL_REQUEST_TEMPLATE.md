## What this changes
<!-- What the change does and why. -->

## Type of change
<!-- Mark the ones that apply with an [x]. -->

- [ ] Bug fix
- [ ] New feature
- [ ] Performance improvement
- [ ] Documentation
- [ ] Breaking change to the Rust API or the C API

## The wire format
<!-- Version 1 peers already run in the field (CONTRIBUTING.md). -->

- [ ] Nothing changes on the wire or on disk
- [ ] It changes the protocol, and the spec, the schema, the Rust crate and the C library change together, with a test in `crates/ava1-ctest` that runs one side against the other

## Checks

- [ ] `make check` passes
- [ ] New behaviour has a test, and a fixed bug has a test that failed before the fix
- [ ] Generated code comes from `make gen`, not from editing `crates/ava1/src/gen.rs` or `c/gen/`
- [ ] `CHANGELOG.md` has a line for anything a user would notice
