# API stability

Ratspeak is an application, not a reusable Rust SDK. All five Rust library
packages are classified **application internal** and remain `publish = false`.
Their public items exist to coordinate workspace crates, Tauri integration,
tests, and platform shells; they are not a third-party SemVer promise.

Internal boundaries still matter. `api-stability.json` and
`api-baseline/*.txt` record every explicit public Rust item with pinned
`cargo-public-api` and rustdoc versions. CI rejects unreviewed drift so changes
cannot silently break another workspace layer or conceal a broadening API.

- `ratspeak-core` contains shared domain/configuration and emitter contracts.
- `ratspeak-db` exposes the application persistence layer.
- `ratspeak-runtime` exposes extensive application orchestration and currently
  has the largest internal surface.
- `ratspeak-tauri` binds runtime behavior to Tauri commands and events.
- the standalone `ratspeak` shell exposes only its application entry surface.

This Rust snapshot does not replace compatibility review for Tauri command
names and payloads, database migrations, persisted files, mobile JNI, web
events, or protocol behavior. Those remain separate product contracts.

No visibility, signature, IPC, persistence, runtime, or platform behavior is
changed at this checkpoint. Later package-boundary changes require a reviewed
snapshot diff and coordinated migration, but do not require public-library
SemVer unless a package is deliberately promoted in a future decision.

The canonical snapshot uses all features on `aarch64-apple-darwin` and omits
auto-derived, auto-trait, and blanket implementations. Android-specific native
surfaces remain protected by the Android target and minified JNI gates.

```sh
cargo install cargo-public-api --version 0.52.0 --locked
rustup toolchain install nightly-2026-08-01 --profile minimal
python3 tools/check-api-baseline.py
```

Use `--update` only after reviewing and recording the compatibility impact.
