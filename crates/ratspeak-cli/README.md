# ratspeak-cli

`ratspeak-cli` provides the first headless Ratspeak entry points:

- `ratspeakctl` for scriptable profile inspection and approval-gated agent
  actions.
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
ratspeakctl daemon wait-ready [--timeout-secs N]
ratspeakctl daemon methods
ratspeakctl profile show
ratspeakctl status
ratspeakctl agent onboard NAME [--preset PRESET] [--allow-contact HASH]
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
ratspeakctl contacts add <dest-hash> [--display-name NAME]
ratspeakctl contacts remove <dest-hash>
ratspeakctl contacts block <dest-hash> [--display-name NAME]
ratspeakctl contacts unblock <dest-hash>
ratspeakctl peers list [--identity HASH] [--recency-secs N]
ratspeakctl conversations list
ratspeakctl conversations read <conversation-id> [--identity HASH] [--limit N]
ratspeakctl conversations mark-read <conversation-id>
ratspeakctl conversations hide <conversation-id>
ratspeakctl conversations unhide <conversation-id>
ratspeakctl conversations delete <conversation-id>
ratspeakctl messages list <conversation-id> [--identity HASH] [--limit N]
ratspeakctl messages search <query> [--identity HASH] [--limit N]
ratspeakctl messages draft <conversation-id> --text TEXT [--submit]
ratspeakctl messages send <action-id>
ratspeakctl messages reply <conversation-id> --reply-to MSG --text TEXT [--submit]
ratspeakctl messages send-file <conversation-id> --file PATH [--mime MIME]
ratspeakctl messages send-image <conversation-id> --file PATH [--mime MIME]
ratspeakctl messages react <conversation-id> --message-id MSG --emoji EMOJI
ratspeakctl messages actions list|show|cancel
ratspeakctl approvals list|show|inspect-file|approve|reject|cancel|execute --agent NAME
ratspeakctl audit list [--agent NAME] [--limit N]
ratspeakctl events stream [--agent NAME] [--cursor N] [--limit N] [--once]
ratspeakctl propagation status
ratspeakctl network status
ratspeakctl network alerts
ratspeakctl network announces
ratspeakctl network announce
ratspeakctl network path request <hash>
```

These commands may initialize or migrate the selected profile database, matching
normal Ratspeak startup behavior.

`identity create` prints a recovery phrase once in JSON. Treat it as private
key material. `identity activate` is an offline profile change; restart
`ratspeakd` or the Tauri app if that profile is already running.

`agent onboard` is the preferred beta entry point for non-developer setup. It
defaults to the `reply-assistant` preset and returns machine-readable
`next.steps[].argv` arrays that can be handed to an agent supervisor. Presets
are `inbox-reader`, `reply-assistant`, `media-assistant`, `network-helper`, and
`openclaw-basic`.

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

Read scopes are `status:read`, `identity:read`, `contacts:read`,
`messages:read`, `events:read`, `network:read`, `actions:read`, and
`audit:read`. Write scopes are effective for approval-gated actions:
`drafts:write`, `messages:write`, `attachments:write`, `images:write`,
`reactions:write`, `announces:write`, `paths:write`, `contacts:write`,
`conversations:write`, and `network:write`. Aliases like `read:messages` and
`write:drafts` are accepted. `messages:write` does not imply files/images,
reactions, announces, contacts, or network actions.

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
- `ratspeakctl messages draft`
- `ratspeakctl messages send`
- `ratspeakctl messages reply`
- `ratspeakctl messages send-file`
- `ratspeakctl messages send-image`
- `ratspeakctl messages react`
- `ratspeakctl messages actions`
- `ratspeakctl approvals execute`
- `ratspeakctl contacts add/remove/block/unblock`
- `ratspeakctl conversations mark-read/hide/unhide/delete`
- `ratspeakctl network announce`
- `ratspeakctl network path request`
- `ratspeakctl events stream`
- `ratspeakctl propagation status`
- `ratspeakctl network status`

Agent-scoped conversation IDs are stable strings of the form
`lxmf:<destination-hash>`. Agent message and conversation payloads wrap
message text, titles, previews, and display names in explicit
`{"text": "...", "untrusted": true}` objects so agent tooling does not confuse
remote message content with trusted instructions.

## Agent Write Actions

Agent write commands do not send directly. They create durable action records in
the selected profile under `.ratspeak/agent-actions/actions/`. Attachments and
images are copied into `.ratspeak/agent-actions/staged-files/`, and agent-facing
JSON redacts the private staged path while preserving filename, MIME type, byte
count, and SHA-256 digest.

The standard flow is:

```sh
ratspeakctl --data-dir AGENT_PROFILE messages draft lxmf:<contact> --text "hello"
ratspeakctl --data-dir AGENT_PROFILE messages send <action-id>
ratspeakctl --data-dir OWNER_PROFILE approvals approve --agent NAME <action-id>
ratspeakctl --data-dir AGENT_PROFILE messages send <action-id>
```

Bots should include `--client-action-id` on write commands. It is a durable
idempotency key: retrying with the same ID and identical payload returns the
original action, while reusing the ID with different payload is rejected. Bots
should also include `--causal-event-id` or `--causal-message-id` when responding
to event-stream input so the write policy can prevent feedback loops.

The first `messages send` moves the draft to `pending_approval`. After owner
approval, running `messages send <action-id>` again executes the already
approved action through `ratspeakd`. Owners may also run
`approvals approve --execute --agent NAME <action-id>` or
`approvals execute --agent NAME <action-id>`.

Approval states are `draft`, `pending_approval`, `approved`, `rejected`,
`cancelled`, `expired`, `executing`, `sent`, `applied`, and `failed`.
`sent` is used for LXMF message/file/image actions. `applied` is used for local
actions such as reactions, contact writes, conversation state changes, manual
announces, and path requests.

The profile-local write policy lives at
`.ratspeak/agent-actions/agent-write-policy.json`. Defaults are intentionally
conservative:

- owner approval required: `true`
- default approval expiry: 24 hours
- max pending actions: 25
- max actions per hour/day: 60 / 200
- per-contact cooldown: 3 seconds
- loop window: 10 minutes, max 6 outbound actions per contact
- max outbound actions per causal event/message: 3 / 2
- max text bytes: 4096
- max attachment bytes: Reticulum efficient resource limit
- allowed MIME prefixes: images, text, PDF, JSON, ZIP, and octet-stream

These settings are user-configurable per agent profile. Raising attachment
limits, disabling approval, or allowing broader MIME types should be treated as
security-sensitive owner configuration.

## Audit Log

Agent grants, write action creation/submission, owner approvals/rejections,
cancellations, expirations, execution attempts, delivery/apply results, daemon
auth failures, policy denials, event reads, grant changes, and token rotations
are recorded in `.ratspeak/agent-actions/audit.jsonl`. Use:

```sh
ratspeakctl --data-dir OWNER_PROFILE audit list --agent NAME
ratspeakctl --data-dir AGENT_PROFILE audit list
```

Audit records include actor type, actor, event, outcome, action ID, subject
hash, and structured details. Tokens, raw attachment bytes, base64 payloads, and
private staged paths are not logged.

Use `approvals inspect-file --agent NAME ACTION_ID` before approving
unexpected file or image actions. It returns the local staged path, size, MIME
type, SHA-256 digest, and a text preview for text-like attachments.

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

The local daemon API supports authenticated, grant-filtered reads, durable event
replay, and approval-gated write actions. It does not support identity export,
remote API access, direct MCP access, raw autonomous sends, blackhole
escalation, propagation configuration changes, or arbitrary daemon filesystem
access.

Run one owner-controlled profile per agent identity. The owner profile updates
grants and approvals; the agent profile runs `ratspeakd` and executes only
approved actions that still pass the active grant, allowlist, expiration, and
rate policy checks.

For the beta Windows transport, Ratspeak uses loopback TCP or the profile-local
filesystem request/response transport with mandatory daemon token auth. A
named-pipe transport with OS ACLs remains the preferred future Windows shape.

See `docs/agent-onboarding-runbook.md` for the full agent/bot onboarding flow.
