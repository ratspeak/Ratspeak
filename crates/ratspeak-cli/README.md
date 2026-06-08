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

Supported commands:

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
ratspeakctl agent grant NAME [--scope SCOPE] [--allow-contact HASH] [--allow-conversation ID]
ratspeakctl agent revoke NAME [--reason TEXT]
ratspeakctl agent rotate-token NAME
ratspeakctl identity get
ratspeakctl identity current
ratspeakctl identity list
ratspeakctl identity create [--nickname NAME] [--activate]
ratspeakctl identity activate HASH
ratspeakctl contacts list [--identity HASH]
ratspeakctl contacts blocked [--identity HASH]
ratspeakctl peers list [--identity HASH] [--recency-secs N]
ratspeakctl conversations list
ratspeakctl conversations read <conversation-id> [--identity HASH] [--limit N]
ratspeakctl messages list <conversation-id> [--identity HASH] [--limit N]
ratspeakctl messages search <query> [--identity HASH] [--limit N]
ratspeakctl events stream [--agent NAME] [--cursor N] [--limit N] [--once]
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
profile, writes `.ratspeak/agent.json`, and writes a private
`.ratspeak/agent.token` credential. The manifest stores only the token hash.
When `ratspeakd` runs for that profile, local API calls must present the token
and are enforced against the active grant scopes plus contact/conversation
allowlists.

`agent grant`, `agent revoke`, and `agent rotate-token` update the manifest and
credential files from the owner profile. Restart `ratspeakd` for the agent
profile after changing grants or credentials.

`ratspeakd` holds a cooperative lock at `.ratspeak/profile.lock`. Owner-run
identity writes in `ratspeakctl` refuse to run while that lock exists. The
Tauri app does not yet participate in this lock, so do not mutate the same
profile from the CLI while the GUI is running.

When `ratspeakd` is running for the selected profile, these read commands are
served through the live daemon API instead of the offline database path:

- `ratspeakctl status`
- `ratspeakctl identity current`
- `ratspeakctl identity list`
- `ratspeakctl contacts list`
- `ratspeakctl contacts blocked`
- `ratspeakctl peers list`
- `ratspeakctl conversations list`
- `ratspeakctl conversations read`
- `ratspeakctl messages list`
- `ratspeakctl messages search`
- `ratspeakctl events stream`
- `ratspeakctl propagation status`
- `ratspeakctl network status`

Agent-scoped conversation IDs are stable strings of the form
`lxmf:<destination-hash>`. Agent message and conversation payloads wrap
message text, titles, previews, and display names in explicit
`{"text": "...", "untrusted": true}` objects so agent tooling does not confuse
remote message content with trusted instructions.

## ratspeakd

`ratspeakd` starts the same Tauri-free runtime used by the app:

```sh
ratspeakd --data-dir /path/to/profile run
ratspeakd --data-dir /path/to/profile run --events-jsonl
```

`--events-jsonl` mirrors durable runtime event envelopes and notifications on
stdout as JSONL. Daemon lifecycle messages go to stderr. `ratspeakctl events
stream` reads the same durable event log through the local daemon API using a
monotonic cursor for reconnect/replay.

`ratspeakd` also publishes a local daemon API endpoint manifest at
`.ratspeak/ratspeakd-api.json`. The primary Unix transport is a domain socket at
`.ratspeak/ratspeakd.sock` on macOS/Linux. If Unix sockets are unavailable or
the profile path is too long for `sockaddr_un`, the daemon falls back to
loopback TCP, then to a profile-local filesystem request/response transport.
All transports use one JSON request per call and one JSON response:

```json
{"id":"request-id","version":1,"method":"status.get","params":{}}
```

Successful responses set `ok: true` and return `result`. Failed responses set
`ok: false` and return `error.code` plus `error.message`. Current stable error
codes are `invalid_json`, `version_mismatch`, `method_not_found`,
`unauthorized`, `forbidden`, `grant_revoked`, `service_unavailable`,
`bad_params`, and `internal`.

## Current Guardrails

The local daemon API currently supports authenticated, grant-filtered reads and
durable event replay only. It does not support autonomous message sends,
drafts, file sends, contact writes, propagation control, identity export, MCP
access, or remote API access.

The intended agent path is:

1. Run one owner-controlled profile per agent identity.
2. Use authenticated read-only CLI/status/event commands through `ratspeakd`.
3. Add audit, approvals, and draft/send operations before enabling
   write-capable agent actions.
