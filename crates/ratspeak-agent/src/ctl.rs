//! Drives Ratspeak through the documented `ratspeakctl` contract. Every reply
//! goes through the action pipeline (`messages draft --submit`) so the daemon's
//! policy, allowlist, approval, and rate limits still gate it — the runner never
//! bypasses guardrails. A future dedicated binary can replace this subprocess
//! layer with a direct daemon-API client without changing the loop.

use std::process::{Child, Command, Stdio};

use serde_json::Value;

pub struct Ctl {
    bin: String,
    data_dir: String,
}

impl Ctl {
    pub fn new(bin: String, data_dir: String) -> Self {
        Self { bin, data_dir }
    }

    fn base(&self) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(["--data-dir", &self.data_dir]);
        cmd
    }

    /// Spawn the durable JSONL event stream; the caller reads records from stdout.
    pub fn stream_events(&self) -> Result<Child, String> {
        self.base()
            .args(["--jsonl", "events", "stream"])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("spawn events stream: {e}"))
    }

    /// Draft a reply and submit it for policy check / owner approval.
    pub fn draft_and_submit(
        &self,
        conversation_id: &str,
        text: &str,
        client_action_id: &str,
        causal_event_id: Option<u64>,
    ) -> Result<Value, String> {
        let mut cmd = self.base();
        cmd.args([
            "messages",
            "draft",
            conversation_id,
            "--text",
            text,
            "--client-action-id",
            client_action_id,
            "--submit",
        ]);
        if let Some(event_id) = causal_event_id {
            cmd.args(["--causal-event-id", &event_id.to_string()]);
        }
        let out = cmd.output().map_err(|e| format!("draft: {e}"))?;
        let value: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            serde_json::json!({ "stdout": String::from_utf8_lossy(&out.stdout) })
        });
        if !out.status.success() {
            return Err(format!("draft failed: {value}"));
        }
        Ok(value)
    }
}
