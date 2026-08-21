# Rust API

Ratspeak is an application, not a reusable Rust SDK. Its five Rust library
packages remain application-internal and `publish = false`. Their public items
coordinate workspace crates, Tauri integration, tests, and platform shells;
they are not a third-party SemVer promise.

Internal boundaries still matter. Ratspeak uses the recommended component
paths for LXMF messages (`lxmf_core::message_api`), LXST telephony service
construction (`TelephonyService::registered`), and LRGP protocol values
(`lrgp::protocol`) while leaving component implementation APIs under their
owners' stability policies.

- `ratspeak-core` contains shared domain and configuration contracts;
- `ratspeak-db` owns application persistence;
- `ratspeak-runtime` owns Reticulum, LXMF, voice, and game orchestration;
- `ratspeak-tauri` binds runtime behavior to Tauri commands and events; and
- the standalone `ratspeak` shell exposes the application entry point.

## Compatibility checks

The `api/` directory contains the evidence used by CI:

- `stability.json` records package tiers, source commits, snapshot hashes, and
  the current review decision; and
- `snapshots/` records the explicit all-feature Apple ARM64 Rust API plus the
  manifest, feature, dependency, target, and MSRV contract.

These checks prevent accidental cross-crate breakage and unexpected surface
growth. They do not replace review of Tauri command names and payloads,
database migrations, persisted files, JNI symbols, web events, platform
lifecycle behavior, or protocol wire behavior. Those remain separate product
contracts.

The snapshot also omits auto-derived, auto-trait, and blanket implementations,
so it is not by itself a complete SemVer verdict. Run the checks with:

```sh
python3 tools/check-api-baseline.py
python3 tools/check-api-manifest.py
python3 tools/check-api-compatibility.py
```

Snapshot updates require a clean source commit and an explicit review recorded
in `api/stability.json`. Additions, removals, deprecations, platform impact, and
version consequences must be reviewed before accepting new evidence. Removals
are accepted only for packages explicitly classified as application-internal
and governed by a reviewed snapshot; reusable/public packages remain closed to
removals.
