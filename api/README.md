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

Process startup must call
`ratspeak_runtime::lxmf::initialize_storage_namespace(data_root)` before
constructing configuration, opening the database pool, or opening identity and
network storage. `ratspeak_tauri::init_core` performs this step automatically.
Standalone runtime callers and test fixtures must do it explicitly. Live
`LxmfManager` rebuilds validate completed namespace markers; they never repair
or republish active roots. An incomplete namespace is recovered at the next
initial startup, before those file users exist.

The same call takes the profile's owner lock, `.ratspeak-host.lock` in the data
root, and the process keeps it until exit. While another process holds it, the
call fails with `ResourceBusy` carrying `owner_lock::ProfileInUse` before
reconciling anything. On a filesystem that cannot lock, such as some network and
FUSE mounts, the call succeeds without that exclusion, and
`owner_lock::unenforced(data_root)` reports it. Next, before reconciling, and
after refusing a linked `.ratspeak`, it reads the database's schema version
over a short-lived read-only connection (`ratspeak_db::refuse_newer_database`).
A database written by a newer build fails with `NewerSchema`, and one whose
version cannot be read fails too. The check never writes to the database. With
no WAL or journal file beside it, it opens the file as immutable wherever an
SQLite URI can name the path, which leaves no side files; otherwise it reads
through the side files already there.
`ratspeak_db::init_schema` applies the same check, and creates a schema and
its version row in one transaction.
`init_core` reports any failure as a `StartupFailure`: the `StartupError` the
window shows, plus the failing step and I/O error kind.

Factory reset stops the identity/storage workers before calling
`clear_identity_storage_namespace`. Reset preserves the established database
and identities roots, records its full child wipe in the outside namespace
marker, and fences new imports while cleanup is pending. Initial startup can
finish a recorded reset; successful completion retires that pending state
before deliberately restored identities are accepted.

- `ratspeak-core` contains shared domain and configuration contracts;
- `ratspeak-db` owns application persistence;
- `ratspeak-runtime` owns Reticulum, LXMF, voice, and game orchestration;
- `ratspeak-tauri` binds runtime behavior to Tauri commands and events; and
- the standalone `ratspeak` shell exposes the application entry point.

## Compatibility checks

The `api/` directory contains the evidence used by CI:

- `stability.json` records package tiers, compatibility baselines, and snapshot
  hashes; and
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

After reviewing an API change, run
`python3 tools/check-api-baseline.py --update` and commit the source and snapshot
changes together. CI compares snapshots with the current source; compatibility
checks retain the historical baseline.

Removals are accepted only for packages explicitly classified as application-internal
and governed by a reviewed snapshot; reusable/public packages remain closed to
removals.
