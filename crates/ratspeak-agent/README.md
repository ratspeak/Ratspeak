# ratspeak-agent (reference runner)

A minimal, out-of-process bot runner for a Ratspeak agent identity. It streams
inbound messages from a per-agent `ratspeakd`, asks an OpenAI-compatible provider
(Venice by default) for a reply, and submits it through the action pipeline so
the daemon's policy, allowlist, approval, and rate limits still gate every reply.

It talks to Ratspeak only via `ratspeakctl` against the agent's own data root —
never the desktop app's profile or identity. This is the reference the dedicated
`ratspeak-agent` binary will grow from; the subprocess `ctl` layer is the seam a
direct daemon-API client will later replace.

## Use

```sh
# 1. Create an agent and point it at a provider (owner profile):
ratspeakctl --data-dir <owner> agent onboard my-bot --allow-contact <hash>
ratspeakctl --data-dir <owner>/agents/my-bot agent adapter set my-bot \
  --provider venice --model zai-org-glm-5 --secret-env VENICE_API_KEY

# 2. Start the agent daemon:
ratspeakd --data-dir <owner>/agents/my-bot run --quiet

# 3. Run the reference runner (key in the configured env var):
export VENICE_API_KEY=...
ratspeak-agent --data-dir <owner>/agents/my-bot
#   --dry-run       print replies instead of submitting
#   --system TEXT   override the system prompt
#   --max-tokens N  cap reply length
```

See `../ratspeak-cli/docs/agent-runner-contract.md` for the full contract.
