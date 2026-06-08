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

State commands emit JSON by default. Use `--pretty` for formatted JSON and
`--jsonl` for list-like record streams.

Supported milestone-1 commands:

```sh
ratspeakctl version
ratspeakctl system status
ratspeakctl system startup
ratspeakctl system setup-status
ratspeakctl system unread [--identity HASH]
ratspeakctl system db-stats
ratspeakctl profile show
ratspeakctl status
ratspeakctl agent create NAME [--identity new] [--scope SCOPE] [--allow-contact HASH]
ratspeakctl agent list
ratspeakctl agent show NAME
ratspeakctl identity get
ratspeakctl identity current
ratspeakctl identity list
ratspeakctl identity create [--nickname NAME] [--activate]
ratspeakctl identity activate HASH
ratspeakctl contacts list [--identity HASH]
ratspeakctl contacts blocked [--identity HASH]
ratspeakctl peers list [--identity HASH] [--recency-secs N]
ratspeakctl conversations list
ratspeakctl messages list <dest_hash> [--identity HASH] [--limit N]
ratspeakctl messages search <query> [--identity HASH] [--limit N]
ratspeakctl propagation status
ratspeakctl network status
ratspeakctl network alerts
ratspeakctl network announces
```

These commands may initialize or migrate the selected profile database, matching
normal Ratspeak startup behavior.

`identity create` prints a recovery phrase once in JSON. Treat it as private
key material. `identity activate` is an offline profile change; restart
`ratspeakd` or the Tauri app if that profile is already running.

`agent create` creates a separate agent profile under
`.ratspeak/agents/NAME` by default, creates a recoverable identity for that
profile, and writes `.ratspeak/agent.json` inside the agent profile. Requested
scopes and allowed contacts are recorded for the future daemon policy layer;
only local profile setup is active in this milestone.

`ratspeakd` holds a cooperative lock at `.ratspeak/profile.lock`. Owner-run
identity writes in `ratspeakctl` refuse to run while that lock exists. The
Tauri app does not yet participate in this lock, so do not mutate the same
profile from the CLI while the GUI is running.

## ratspeakd

`ratspeakd` starts the same Tauri-free runtime used by the app:

```sh
ratspeakd --data-dir /path/to/profile run
ratspeakd --data-dir /path/to/profile run --events-jsonl
```

`--events-jsonl` emits runtime events and notifications on stdout as JSONL.
Daemon lifecycle messages go to stderr.

## Current Guardrails

Milestone 1 does not support autonomous message sends, file sends, contact
writes, propagation control, identity export, local daemon API access, or MCP
access.

The intended agent path is:

1. Run one owner-controlled profile per agent identity.
2. Use read-only CLI/status commands first.
3. Add the local daemon API with scopes, audit, approvals, and durable event
   cursors before adding write-capable agent operations.
