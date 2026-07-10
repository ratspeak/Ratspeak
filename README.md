<div align="center">

<img src="src-tauri/icons/128x128.png" width="88" height="88" alt="Ratspeak logo">

# Ratspeak

Ratspeak is a native desktop and mobile client for E2EE conversations over
Reticulum, a new type of mesh networking. Ratspeak gives you messaging, file/image sharing, voice calls (experimental), LoRa capability, WiFi, BLE, TCP, offline messaging, turn-based games, and more.

[Docs](https://ratspeak.org/docs.html) |
[Build from source](https://ratspeak.org/docs.html#getting-started/building-from-source) |
[rsReticulum](https://github.com/ratspeak/rsReticulum) |
[rsLXMF](https://github.com/ratspeak/rsLXMF) |
[rsLXST](https://github.com/ratspeak/rsLXST)


[![License: AGPL-3.0-or-later](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)
[![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Status](https://img.shields.io/badge/status-alpha-yellow.svg)](#feature-status)

<img src="docs/readme/ratspeak-showcase.png" alt="Ratspeak running on desktop and mobile" width="100%">

###### *Note: Ratspeak is currently in ALPHA. If you are looking for a more stable<br> experience, I recommend waiting until v1.1.0 is released.*

</div>

## What It Is

Ratspeak is for private messaging when the normal internet is unavailable,
untrusted, or not the path you want to depend on. When your cell tower is down, when natural disaster hits, or when you just want an alternative. When you know the current system is broken.

It runs on
[Reticulum](https://github.com/ratspeak/rsReticulum) and [LXMF](https://github.com/ratspeak/rsLXMF), so conversations can happen
over regular internet, LoRa radios, WiFi, Bluetooth, there is no limit - if it can move data it can be a part of the mesh.

There is no Ratspeak account server, no central database, no hub where everything routes through by default. Your Reticulum identity is generated on
your device and becomes your address on the mesh, no personal information needed.

## Current State

Ratspeak is in experimental/alpha status. That means there are bugs, there are quirks, and things are not perfect. We stand by a strict contribute, don't complain policy. If something isn't working up to your standards, or at all, contribute by opening an issue and providing valuable feedback required to fix the issue. Code does not have emotion, so there's no reason a bug report should either.

Supported app targets are macOS, Windows, Linux, Android, and iOS. Public
desktop and Android packages will be linked from
[ratspeak.org/download.html](https://ratspeak.org/download.html) as they are
released. iOS does not have a public download yet; and macOS is unsigned, with Window's .MSIX needing signing for BLE Peering support. These will come once LLC formation is complete and I have the patience to deal with Apple and signing-certificates.

## What You Get

- Account-free messaging over Reticulum.
- Full offline messaging support.
- Local Network, TCP, RNode/LoRa support, Bluetooth Peering, and more.
- Contacts, discovered peers, path requests, interface status, propagation
  status, and transport health in the app.
- Experimental peer-to-peer voice calls over [LXST](https://github.com/ratspeak/rsLXST)
  (contacts-only, 0-hop, native microphone/speaker).
- Chess and Tic-Tac-Toe.
- I'm tired boss, this whole README is going to get a revamp.

## Install

Use the download page when public builds are available:
[ratspeak.org/download.html](https://ratspeak.org/download.html).

For setup help, see:

- [Install and Platform Setup](https://ratspeak.org/docs.html#getting-started/install-and-platform-setup)
- [Your First Session](https://ratspeak.org/docs.html#getting-started/your-first-session)
- [Troubleshooting](https://ratspeak.org/docs.html#reference/troubleshooting)

## Build From Source

The full build guide is here:
[Building from Source](https://ratspeak.org/docs.html#getting-started/building-from-source).
It covers desktop prerequisites, Android APKs, iOS signing, and the required
sibling checkout layout.

After installing the desktop prerequisites, the shortest local path is:

```bash
mkdir ratspeak-src
cd ratspeak-src
git clone https://github.com/ratspeak/rsReticulum
git clone https://github.com/ratspeak/rsLXMF
git clone https://github.com/ratspeak/lrgp-rs
git clone https://github.com/ratspeak/rsLXST   # experimental voice; skip with --no-default-features
git clone https://github.com/ratspeak/Ratspeak

cd Ratspeak
bash dashboard/build-css.sh
cd src-tauri
cargo tauri dev
```

For a release bundle, run `cargo tauri build` from `Ratspeak/src-tauri`.
Desktop bundles land under `Ratspeak/src-tauri/target/release/bundle/`.

To build without the experimental voice stack and skip the rsLXST sibling,
pass `--no-default-features` to `cargo tauri dev` or `cargo tauri build`.

## Voice (experimental)

Voice calls run on [LXST](https://github.com/ratspeak/rsLXST) over Reticulum
links — no servers, no relays. The stack is new and intentionally narrow:

- Microphone and speaker access goes through the OS: `RECORD_AUDIO` and
  `MODIFY_AUDIO_SETTINGS` on Android, `NSMicrophoneUsageDescription` on
  macOS/iOS, the default audio device on Linux/Windows.
- Incoming calls are restricted to contacts on a cached 0-hop path. Calls
  from non-contacts are dropped before any audio path opens.
- Persistently rejected callers are blackholed (rate-limit reason) so they
  cannot keep ringing the device.
- The `lxst-voice` Cargo feature is on by default. Disable it with
  `cargo tauri dev --no-default-features` if you want to build Ratspeak
  without the voice stack.

Voice is experimental — expect rough edges. Codec quality, call setup,
ringtones, and platform audio routing are all subject to change.

## Platform Notes

- iOS does not support general USB serial. Local Network, multicast, notifications, and
  background behavior depend on Apple permissions as well, and currently don't have support at this time.
- Windows Bluetooth Peer advertiser support needs the future signed MSIX lane.
- Linux Bluetooth Peer depends on BlueZ GATT server and LE advertising support.
- Voice calls require microphone permission per platform; the prompt is
  triggered the first time you place or answer a call.

## License

GNU Affero General Public License v3.0 or later. See [`LICENSE`](LICENSE).
