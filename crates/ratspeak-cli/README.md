# ratspeak-cli

`ratspeak-cli` provides the first headless Ratspeak entry points:

- `ratspeakctl` for scriptable, read-only profile inspection.
- `ratspeakd` for running the Ratspeak runtime without the Tauri UI.

This crate depends on `ratspeak-runtime`, `ratspeak-db`, and `ratspeak-core`.
It intentionally does not depend on `ratspeak-tauri`.

## Profile Selection

Use `--data-dir PATH` or `RATSPEAK_DATA_DIR` to point the CLI at a Ratspeak
profile root. The database lives below that root at `.ratspeak/ratspeak.db`.

If no path is supplied, the CLI uses the desktop app identifier data directory
for the current platform:

- macOS: `~/Library/Application Support/org.ratspeak.desktop`
- Linux/BSD: `$XDG_DATA_HOME/org.ratspeak.desktop`, or
  `~/.local/share/org.ratspeak.desktop`
- Windows: `%APPDATA%\org.ratspeak.desktop`

## ratspeakctl

State commands emit JSON by default. Use `--pretty` for formatted JSON.

Supported milestone-1 commands:

```sh
ratspeakctl version
ratspeakctl profile show
ratspeakctl status
ratspeakctl identity get
ratspeakctl identity list
ratspeakctl contacts list [--identity HASH]
ratspeakctl contacts blocked [--identity HASH]
ratspeakctl conversations list
ratspeakctl messages list <dest_hash> [--identity HASH] [--limit N]
ratspeakctl messages search <query> [--identity HASH] [--limit N]
```

These commands may initialize or migrate the selected profile database, matching
normal Ratspeak startup behavior.

## ratspeakd

`ratspeakd` starts the same Tauri-free runtime used by the app:

```sh
ratspeakd --data-dir /path/to/profile run
ratspeakd --data-dir /path/to/profile run --events-jsonl
```

`--events-jsonl` emits runtime events and notifications on stdout as JSONL.
Daemon lifecycle messages go to stderr.

## Current Guardrails

Milestone 1 is read-only for `ratspeakctl`. It does not support autonomous
message sends, file sends, contact writes, propagation control, identity
export, local daemon API access, or MCP access.

The intended agent path is:

1. Run one owner-controlled profile per agent identity.
2. Use read-only CLI/status commands first.
3. Add the local daemon API with scopes, audit, approvals, and durable event
   cursors before adding write-capable agent operations.
