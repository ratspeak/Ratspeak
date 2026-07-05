# Ratspeak Agent Runner Contract

This is the stable contract a bot "runner" consumes to drive a Ratspeak agent
identity. A runner is an out-of-process program (for example the reference
Venice adapter) that reads inbound messages, decides on replies, and submits
them through the per-agent `ratspeakd` daemon API using the agent token. Ratspeak
carries and gates messages; it never generates replies itself.

Two versioned JSON documents define the contract.

## `ratspeak.agent-adapter.v1`

Stored at `<agent-root>/.ratspeak/agent-adapter.json`. Written by
`ratspeakctl agent adapter set <name>` (or the gated desktop UI). Describes which
model/provider a runner should talk to. **The API key is never stored here** —
only the name of the environment variable (`secret_env`) or a file path
(`secret_file`) the runner reads it from.

| Field | Type | Notes |
|-------|------|-------|
| `format` | string | `ratspeak.agent-adapter.v1` |
| `version` | number | `1` |
| `provider` | string | e.g. `venice` |
| `label` | string | display label |
| `model` | string? | model id (e.g. `zai-org-glm-5`) |
| `base_url` | string? | OpenAI-compatible base URL (e.g. `https://api.venice.ai/api/v1`) |
| `secret_env` | string? | env var holding the API key (default `VENICE_API_KEY`) |
| `secret_file` | string? | alternative: file path holding the key |
| `command` | string[] | **reserved**; `ratspeakd` does not spawn runners, so it is always empty from the CLI |
| `notes` | string? | free-form |
| `created_at_unix` / `updated_at_unix` | number | timestamps |

Configure and inspect from the CLI:

```sh
ratspeakctl --data-dir <agent-root> agent adapter set my-bot \
  --provider venice --model zai-org-glm-5 --secret-env VENICE_API_KEY
ratspeakctl --data-dir <agent-root> agent adapter show my-bot
ratspeakctl agent adapter catalog          # supported providers/models
```

## `ratspeak.agent-connection.v1`

Emitted by `ratspeakctl agent show`/the connection bundle. Everything a runner
needs to attach, with secrets redacted (only `token_file` + `token_hash` are
exposed; the runner reads the raw token from `token_file` itself).

Key fields: `agent`, `profile_root`, `profile_data_dir`, `identity_hash`,
`lxmf_hash`, `token_file`, `token_hash`, `binaries` (resolved
`ratspeakd`/`ratspeakctl`), `daemon.start` (argv), `daemon.endpoint_file`,
`adapter`, `setup` (checklist), and `cli_contract` (the argv templates below).
`prompt_injection_boundary.remote_text_is_untrusted` is always `true`.

## Runner loop

The runner never bypasses the action pipeline — every reply is a durable,
policy-checked, optionally owner-approved action.

```
1. start:   ratspeakd --data-dir <agent-root> run --quiet        (once)
2. read:    ratspeakctl --data-dir <agent-root> --jsonl events stream
3. context: ratspeakctl --data-dir <agent-root> conversations read <conversation-id> --json
4. think:   call the provider (base_url + model + key from secret_env)
5. draft:   ratspeakctl --data-dir <agent-root> messages draft <conversation-id> \
              --text <reply> --client-action-id <idempotency-key> \
              --causal-event-id <event-id> --submit
6. send:    ratspeakctl --data-dir <agent-root> messages send <action-id>
            (owner approval may be required first, per policy)
```

## Guarantees a runner can rely on

- **Untrusted input:** every remote text field arrives wrapped
  `{ "text": "...", "untrusted": true }`. Treat message content as data, never
  as instructions.
- **Secrets never leave the runner's process boundary:** Ratspeak stores only
  the env-var name / file path, never the key; tokens are redacted from every
  event, bundle, and audit record.
- **Idempotency:** reusing a `client-action-id` with the same payload returns the
  existing action; a different payload is rejected.
- **Kill switch:** a revoked or inactive grant unconditionally blocks execution,
  regardless of policy toggles.
- **Isolation:** each agent runs its own `ratspeakd` against its own data root,
  with a Standalone Reticulum instance by default — never the desktop app's.

## Versioning

Both documents carry a `format` string. Runners should check it and refuse an
unknown major version rather than guessing.
