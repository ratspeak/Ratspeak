//! Loads the `ratspeak.agent-adapter.v1` config and resolves the API key.
//! The key is never stored by Ratspeak — only the env var name or a file path.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AdapterConfig {
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub secret_env: Option<String>,
    #[serde(default)]
    pub secret_file: Option<PathBuf>,
}

impl AdapterConfig {
    pub fn load(agent_root: &Path) -> Result<Self, String> {
        let path = agent_root.join(".ratspeak").join("agent-adapter.json");
        let bytes = std::fs::read(&path)
            .map_err(|e| format!("read {}: {e} (run `ratspeakctl agent adapter set`)", path.display()))?;
        let config: AdapterConfig =
            serde_json::from_slice(&bytes).map_err(|e| format!("parse adapter config: {e}"))?;
        // Refuse an unknown schema major rather than guessing (see the runner
        // contract doc).
        if !config.format.is_empty() && !config.format.starts_with("ratspeak.agent-adapter.") {
            return Err(format!("unsupported adapter format: {}", config.format));
        }
        Ok(config)
    }

    /// Resolve the provider API key from `secret_env` (preferred) or `secret_file`.
    pub fn resolve_key(&self) -> Result<String, String> {
        if let Some(env) = self.secret_env.as_deref() {
            if let Ok(value) = std::env::var(env) {
                if !value.trim().is_empty() {
                    return Ok(value);
                }
            }
        }
        if let Some(file) = &self.secret_file {
            let value = std::fs::read_to_string(file)
                .map_err(|e| format!("read secret_file {}: {e}", file.display()))?;
            if !value.trim().is_empty() {
                return Ok(value.trim().to_string());
            }
        }
        Err(format!(
            "no provider API key found; set env var {}",
            self.secret_env.as_deref().unwrap_or("<unset>")
        ))
    }

    pub fn base_url(&self) -> &str {
        self.base_url
            .as_deref()
            .unwrap_or("https://api.venice.ai/api/v1")
    }

    pub fn model(&self) -> &str {
        self.model.as_deref().unwrap_or("zai-org-glm-5")
    }
}
