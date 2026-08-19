# Changelog

This changelog records user-visible Ratspeak changes and release-engineering
changes that materially affect how an artifact is reproduced.

## [Unreleased]

## [1.0.28] - 2026-08-18

### Fixed

- Fixed RNode startup compatibility by treating optional EEPROM information as
  advisory instead of rejecting otherwise usable hardware, including the T114.
- Restored reliable BLE transmission after an RNode connects.
- Fixed Announce sometimes staying queued even while an external RNode was
  connected and ready to transmit.
- Fixed Linux AppImage startup crashes caused by incompatible system
  AppIndicator and GLib libraries.
- Simplified interface status messages so connection and hardware warnings are
  shown as plain yellow text instead of pill-shaped badges.

## [1.0.27] - 2026-08-17

### Changed

- Classified every Rust package as application internal and added pinned,
  CI-enforced API snapshots to protect coordinated workspace changes without
  presenting Ratspeak as a public Rust SDK.
- Preserved and now inspects every name-based Android Rust/Kotlin JNI boundary
  in minified release artifacts, restoring BLE RNode, USB permission, platform
  replay, native file save, and call/voice-message audio entry points that R8
  could rename or remove beginning with `1.0.26d`.
- Adopted upstream `opus-rs` 0.1.29 through rsLXST with heap-backed codec
  state, raised the unified first-party MSRV to Rust 1.87, retained Android
  ARMv7 voice support, and made Android i686 explicitly unsupported.
- Qualified the `1.0.27` source with one reviewed, exact component graph for
  CI and every platform release workflow: rsReticulum/rsLXMF 1.2, rsLXST 0.2,
  and lrgp 0.4.1.
- Added platform-scoped source BOM generation with both Cargo lockfile hashes,
  exact component commits, component compatibility versions, declared release
  toolchains, and runner-observed tool versions.
- Pinned the Node runtime used to qualify release source and emit BOM evidence.
- Made the Android and iOS build numbers monotonic and independent of the
  user-visible marketing version.
- Declared every first-party Ratspeak package non-publishable and recorded
  compatible versions alongside all sibling path dependencies.

[Unreleased]: https://github.com/ratspeak/Ratspeak/compare/v1.0.28...HEAD
[1.0.28]: https://github.com/ratspeak/Ratspeak/compare/v1.0.27...v1.0.28
[1.0.27]: https://github.com/ratspeak/Ratspeak/compare/v1.0.26o...v1.0.27
