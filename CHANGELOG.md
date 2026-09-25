# Changelog

This changelog records user-visible Ratspeak changes and release-engineering
changes that materially affect how an artifact is reproduced.

## [Unreleased]

## [1.0.33] - 2026-09-24

### Fixed and improved

- Restored large file and photo staging by accepting valid full attachment chunks without mistaking Base64 padding for excess data.
- Kept automatic photo preparation within its size budget while preserving explicit image-size choices.
- Fixed attachment replacement and cancellation, long or Unicode filenames, and simultaneous large-image previews.
- Improved Bluetooth Peer packet sizing, flow control, and delivery scheduling, and kept active transfers alive while data is moving.
- Made transfer progress reflect payload bytes and removed repetitive chunk and progress details from Activity.
- Kept mobile chats at the intended scroll position when images load and when the iPhone keyboard opens.
- Removed stale startup warnings after a radio interface is removed or paused.
- Improved first-contact delivery to handhelds, bounded path recovery, and compression selection as peer capabilities become known.
- Retained deferred path-response announces through transport pressure and announce timing limits so discovery can recover without a manual announce.

### Known issue

- Some attachment transfers to NomadNet can appear delivered without being stored by the receiver due to an upstream Python Reticulum completion-state defect.

## [1.0.32] - 2026-09-19

### Added

- Added Android system sharing for text, links and single photos, with recent-chat
  and contact selection, photo sizing, protected drafts, and an explicit Send action.
- Added Developer Mode network settings for choosing a Ratspeak-managed stack
  or an authenticated existing local Reticulum instance.

### Fixed

- Prevented telemetry-only LXMF updates from creating empty chat messages,
  unread counts or notifications; preserved title-only messages and made
  unavailable attachments explicit.
- Fixed Windows System color mode so it follows operating-system changes
  without remaining locked to the previous Light or Dark choice.
- Refined mobile composer alignment, Network Ownership controls, Bluetooth
  progress sheets, and dismissible radio startup warnings.
- Simplified share recipient selection and dismissed the keyboard before
  photo sizing.
- Corrected same-radio relaying, bounded discovery retries and slow-link timing.
  Failed-route recovery preserves freshly learned local routes and supports
  bounded authenticated recovery through an existing Python stack. Legacy
  Python owners reset by destination; this is not an atomic remote-route check.
- Separated local send-capacity waits from delivery-proof timeouts, allowing
  failed Links and stalled attachments to release following messages for retry
  while healthy slow transfers retain their protocol-owned windows.
- Kept cancellation and delayed completion tied to their original messages,
  and retained attachment memory reservations through inbound processing.
  Cancellation cannot recall bytes already accepted by a driver.
- Corrected Android USB radio readiness and late write-completion accounting
  during reconnects.
- Corrected propagation-node discovery metadata and preserved stored offline
  messages across node restarts, including compatible recovery of older
  unstamped storage.
- Improved voice-preview startup, duration and recovery handling, preserved paused
  seeking, and bounded playback cleanup before retries or recording.
- Retained the Android runtime across task removal and Activity recreation,
  while fencing stale native callbacks and preserving explicit-stop behavior.
- Prevented stale search, conversation and contact results from replacing newer
  navigation or identity state; improved tablet typing, pickers and keyboard access.
- Protected network credentials and ownership transitions, and prevented delayed
  access imports from overwriting manually edited settings.

### Build

- Enforced the pinned Android NDK, release JNI boundaries and 16 KiB native
  packaging alignment; excluded dashboard test scripts from embedded assets.
- Added exact dependency-set source reconstruction and packaging guidance for
  reproducible downstream builds.
- Aligned networking dependencies across standalone core and packaged builds,
  including corrected Windows socket ownership and Wine compatibility.

### Known limitations

- Known interoperability limitation: some attachment transfers to NomadNet can
  appear delivered without being stored by the receiver due to an upstream
  Python Reticulum completion-state defect.

## [1.0.31] - 2026-08-26

### Fixed

- Prevented app-managed shared Reticulum instances on Linux and Android from
  colliding with other local instances that use a different port, while
  preserving explicit operator configurations.
- Restored mobile keyboard avoidance across Direct Messages, Channels, and
  input sheets, and made first-run setup dismiss the keyboard after submission.
- Enabled iOS notification sounds when permitted by the user's device settings.
- Hardened voice messages across Android versions, including short or silent
  recordings, first-time microphone permission, review and discard ownership,
  correct re-recorded previews, and visible waveform playback progress.
- Aligned Channel receive-window handling with Reticulum 1.4.2 so far-future
  packets are rejected until a retransmission enters the valid window.

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

[Unreleased]: https://github.com/ratspeak/Ratspeak/compare/v1.0.32...HEAD
[1.0.32]: https://github.com/ratspeak/Ratspeak/compare/v1.0.31...v1.0.32
[1.0.31]: https://github.com/ratspeak/Ratspeak/compare/v1.0.30...v1.0.31
[1.0.30]: https://github.com/ratspeak/Ratspeak/compare/v1.0.29...v1.0.30
[1.0.29]: https://github.com/ratspeak/Ratspeak/compare/v1.0.28...v1.0.29
[1.0.28]: https://github.com/ratspeak/Ratspeak/compare/v1.0.27...v1.0.28
[1.0.27]: https://github.com/ratspeak/Ratspeak/compare/v1.0.26o...v1.0.27
