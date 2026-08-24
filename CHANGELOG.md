# Changelog

This changelog records user-visible Ratspeak changes and release-engineering
changes that materially affect how an artifact is reproduced.

## [Unreleased]

## [1.0.30] - 2026-08-24

### Fixed

- Restored mobile message actions on text, links, and media while keeping
  deliberate native text selection available from an elevated message, and
  improved the corresponding desktop mouse and keyboard interactions.
- Made previously paired Bluetooth RNodes advertise automatically after a
  power cycle and reconnect without requiring pairing mode again.
- Made Android RNode recovery show accurate waiting, connecting, initializing,
  and connected states without replacing an active retry, and fixed the
  pairing sheet jumping when the system PIN prompt closes.

## [1.0.29] - 2026-08-21

### Fixed

- Qualified and fixed Bluetooth RNode support on iOS and Android for Heltec
  V3/V4 and LilyGO T114/T-Echo, including fresh pairing, adapter toggles,
  walking out of range, automatic reconnect, and queued-message recovery.
- Made RNode readiness reflect the completed hardware handshake immediately,
  prevented traffic loss across reconnect generations, and used each device's
  advertised four-character identifier in its default interface name.
- Made manual and interface-online announces coalesce into one prompt queue
  operation, report actual interface acceptance, and remain responsive during
  background maintenance.
- Moved LXMF identity, ratchet, and router persistence off the protocol lock
  and made pruning incremental, durable, and safe under concurrent activity.
- Aligned LXMF first-hop establishment timing with Reticulum so slower radio
  links are not failed prematurely.
- Clarified pending-message cancellation as stopping local retries without
  promising that a copy already handed to the network can be recalled.
- Enabled native selection and copying of sent and received message text, and
  applied the same desktop typing-assistance policy to Direct Messages and
  Channels.

### Changed

- Added an exact dependency manifest and coordinated annotated sibling tags
  for reproducible v1.0.29 builds without date-based revision guessing.

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

[Unreleased]: https://github.com/ratspeak/Ratspeak/compare/v1.0.30...HEAD
[1.0.30]: https://github.com/ratspeak/Ratspeak/compare/v1.0.29...v1.0.30
[1.0.29]: https://github.com/ratspeak/Ratspeak/compare/v1.0.28...v1.0.29
[1.0.28]: https://github.com/ratspeak/Ratspeak/compare/v1.0.27...v1.0.28
[1.0.27]: https://github.com/ratspeak/Ratspeak/compare/v1.0.26o...v1.0.27
