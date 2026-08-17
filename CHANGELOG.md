# Changelog

This changelog records user-visible Ratspeak changes and release-engineering
changes that materially affect how an artifact is reproduced.

## [Unreleased]

### Changed

- Adopted upstream `opus-rs` 0.1.29 through rsLXST with heap-backed codec
  state, raised the unified first-party MSRV to Rust 1.87, retained Android
  ARMv7 voice support, and made Android i686 explicitly unsupported.
- Prepared the `1.0.26o` source candidate with one reviewed, exact component
  graph for CI and every platform release workflow.
- Added platform-scoped source BOM generation with both Cargo lockfile hashes,
  exact component commits, component compatibility versions, declared release
  toolchains, and runner-observed tool versions.
- Pinned the Node runtime used to qualify release source and emit BOM evidence.
- Made the Android and iOS build numbers monotonic and independent of the
  user-visible `1.0.26` marketing version.
- Declared every first-party Ratspeak package non-publishable and recorded
  compatible versions alongside all sibling path dependencies.

[Unreleased]: https://github.com/ratspeak/Ratspeak/compare/v1.0.26n...HEAD
