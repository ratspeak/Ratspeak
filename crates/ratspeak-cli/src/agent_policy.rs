use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CliError, CliResult};

pub const AGENT_MANIFEST_FORMAT: &str = "ratspeak.agent.v1";
pub const AGENT_TOKEN_FORMAT: &str = "ratspeak.agent-token.v1";
pub const AGENT_MANIFEST_FILE: &str = "agent.json";
pub const AGENT_TOKEN_FILE: &str = "agent.token";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentManifest {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub created_at_unix: f64,
    pub profile_root: PathBuf,
    pub profile_data_dir: PathBuf,
    pub identity_hash: String,
    pub lxmf_hash: String,
    pub display_name: String,
    pub requested_scopes: Vec<String>,
    pub effective_scopes: Vec<String>,
    pub pending_scopes: Vec<String>,
    pub allowed_contacts: Vec<String>,
    #[serde(default)]
    pub allowed_conversations: Vec<String>,
    pub unknown_contacts: String,
    #[serde(default)]
    pub grant: AgentGrant,
    #[serde(default)]
    pub auth: AgentAuth,
    pub enforcement: AgentEnforcement,
    pub commands: AgentCommandHints,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentGrant {
    pub status: String,
    pub revision: u64,
    pub scopes: Vec<String>,
    pub pending_scopes: Vec<String>,
    pub allowed_contacts: Vec<String>,
    pub allowed_conversations: Vec<String>,
    pub unknown_contacts: String,
    pub updated_at_unix: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoked_at_unix: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revoke_reason: Option<String>,
}

impl Default for AgentGrant {
    fn default() -> Self {
        Self {
            status: "active".into(),
            revision: 1,
            scopes: Vec::new(),
            pending_scopes: Vec::new(),
            allowed_contacts: Vec::new(),
            allowed_conversations: Vec::new(),
            unknown_contacts: "deny".into(),
            updated_at_unix: 0.0,
            revoked_at_unix: None,
            revoke_reason: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentAuth {
    #[serde(default)]
    pub token_hash: String,
    #[serde(default)]
    pub token_file: PathBuf,
    #[serde(default)]
    pub rotated_at_unix: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEnforcement {
    pub local_daemon_api: bool,
    pub contact_allowlist: bool,
    pub write_actions: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCommandHints {
    pub start_daemon: Vec<String>,
    pub status: Vec<String>,
    pub events_preview: Vec<String>,
    #[serde(default)]
    pub events_stream: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCredential {
    pub format: String,
    pub version: u32,
    pub agent_name: String,
    pub identity_hash: String,
    pub token: String,
    pub created_at_unix: f64,
}

#[derive(Debug, Clone)]
pub struct AgentPrincipal {
    pub name: String,
    pub identity_hash: String,
    pub scopes: Vec<String>,
    pub pending_scopes: Vec<String>,
    pub allowed_contacts: Vec<String>,
    pub allowed_conversations: Vec<String>,
    pub unknown_contacts: String,
    pub revision: u64,
}

#[derive(Debug, Clone)]
pub enum AccessMode {
    Owner,
    Agent(AgentPrincipal),
}

impl AccessMode {
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent(_))
    }

    pub fn principal(&self) -> Option<&AgentPrincipal> {
        match self {
            Self::Agent(principal) => Some(principal),
            Self::Owner => None,
        }
    }
}

impl AgentManifest {
    pub fn effective_grant(&self) -> AgentGrant {
        let mut grant = self.grant.clone();
        if grant.scopes.is_empty() && !self.effective_scopes.is_empty() {
            grant.scopes = self.effective_scopes.clone();
        }
        if grant.pending_scopes.is_empty() && !self.pending_scopes.is_empty() {
            grant.pending_scopes = self.pending_scopes.clone();
        }
        if grant.allowed_contacts.is_empty() && !self.allowed_contacts.is_empty() {
            grant.allowed_contacts = self.allowed_contacts.clone();
        }
        if grant.allowed_conversations.is_empty() && !self.allowed_conversations.is_empty() {
            grant.allowed_conversations = self.allowed_conversations.clone();
        }
        if grant.unknown_contacts.is_empty() {
            grant.unknown_contacts = if self.unknown_contacts.is_empty() {
                "deny".into()
            } else {
                self.unknown_contacts.clone()
            };
        }
        if grant.status.is_empty() {
            grant.status = "active".into();
        }
        if grant.revision == 0 {
            grant.revision = 1;
        }
        grant
    }

    pub fn principal(&self) -> AgentPrincipal {
        let grant = self.effective_grant();
        AgentPrincipal {
            name: self.name.clone(),
            identity_hash: self.identity_hash.clone(),
            scopes: grant.scopes,
            pending_scopes: grant.pending_scopes,
            allowed_contacts: grant.allowed_contacts,
            allowed_conversations: grant.allowed_conversations,
            unknown_contacts: grant.unknown_contacts,
            revision: grant.revision,
        }
    }
}

pub fn agent_manifest_path(agent_root: &Path) -> PathBuf {
    agent_root.join(".ratspeak").join(AGENT_MANIFEST_FILE)
}

pub fn agent_token_path(agent_root: &Path) -> PathBuf {
    agent_root.join(".ratspeak").join(AGENT_TOKEN_FILE)
}

pub fn agent_manifest_path_from_data_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(AGENT_MANIFEST_FILE)
}

pub fn agent_token_path_from_data_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(AGENT_TOKEN_FILE)
}

pub fn agent_root_from_owner_data_dir(owner_data_dir: &Path, name: &str) -> PathBuf {
    owner_data_dir.join("agents").join(name)
}

pub fn read_agent_manifest(path: &Path) -> CliResult<Option<AgentManifest>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let manifest = serde_json::from_slice(&bytes)?;
    Ok(Some(manifest))
}

pub fn read_agent_manifest_from_data_dir(data_dir: &Path) -> CliResult<Option<AgentManifest>> {
    read_agent_manifest(&agent_manifest_path_from_data_dir(data_dir))
}

pub fn write_agent_manifest(path: &Path, manifest: &AgentManifest) -> CliResult<()> {
    write_json_file(path, manifest, false)
}

pub fn write_agent_credential(path: &Path, credential: &AgentCredential) -> CliResult<()> {
    write_json_file(path, credential, true)
}

pub fn read_agent_credential_from_data_dir(data_dir: &Path) -> CliResult<Option<AgentCredential>> {
    let path = agent_token_path_from_data_dir(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

pub fn create_agent_credential(agent_name: &str, identity_hash: &str, now: f64) -> AgentCredential {
    AgentCredential {
        format: AGENT_TOKEN_FORMAT.into(),
        version: 1,
        agent_name: agent_name.into(),
        identity_hash: identity_hash.into(),
        token: generate_token(),
        created_at_unix: now,
    }
}

pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

pub fn token_matches(token: &str, expected_hash: &str) -> bool {
    constant_time_eq(token_hash(token).as_bytes(), expected_hash.as_bytes())
}

pub fn normalize_agent_scopes(
    scopes: Vec<String>,
) -> CliResult<(Vec<String>, Vec<String>, Vec<String>)> {
    let scopes = if scopes.is_empty() {
        vec!["status:read".into(), "identity:read".into()]
    } else {
        scopes
    };
    let mut requested = Vec::new();
    let mut effective = Vec::new();
    let mut pending = Vec::new();
    for scope in scopes {
        let normalized = normalize_scope_alias(&scope)
            .ok_or_else(|| CliError::usage(format!("unsupported agent scope: {scope}")))?;
        push_unique(&mut requested, normalized.clone());
        if agent_scope_is_effective_now(&normalized) {
            push_unique(&mut effective, normalized);
        } else {
            push_unique(&mut pending, normalized);
        }
    }
    Ok((effective, pending, requested))
}

pub fn normalize_scope_alias(scope: &str) -> Option<String> {
    match scope.trim() {
        "read:status" | "status:read" => Some("status:read".into()),
        "read:identity" | "identity:read" => Some("identity:read".into()),
        "read:contacts" | "contacts:read" => Some("contacts:read".into()),
        "read:messages" | "messages:read" => Some("messages:read".into()),
        "read:events" | "events:read" => Some("events:read".into()),
        "read:network" | "network:read" => Some("network:read".into()),
        "write:drafts" | "drafts:write" => Some("drafts:write".into()),
        "write:messages" | "messages:write" => Some("messages:write".into()),
        _ => None,
    }
}

pub fn agent_scope_is_effective_now(scope: &str) -> bool {
    matches!(
        scope,
        "status:read"
            | "identity:read"
            | "contacts:read"
            | "messages:read"
            | "events:read"
            | "network:read"
    )
}

pub fn conversation_id_for_dest(dest_hash: &str) -> String {
    format!("lxmf:{dest_hash}")
}

pub fn dest_hash_from_conversation_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let dest = trimmed.strip_prefix("lxmf:").unwrap_or(trimmed);
    ratspeak_runtime::helpers::validate_hex(dest, 16, 64).then(|| dest.to_string())
}

pub fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

pub fn sorted_unique(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn write_json_file<T: Serialize>(path: &Path, value: &T, private: bool) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    if private {
        restrict_file_permissions(&tmp)?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}
