use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agent_policy::AgentPrincipal;
use crate::error::{CliError, CliResult};
use crate::event_store;

pub const ACTION_FORMAT: &str = "ratspeak.agent-action.v1";
pub const AUDIT_FORMAT: &str = "ratspeak.agent-audit.v1";
pub const WRITE_POLICY_FORMAT: &str = "ratspeak.agent-write-policy.v1";
pub const ACTIONS_DIR_NAME: &str = "agent-actions";
pub const ACTION_RECORDS_DIR_NAME: &str = "actions";
pub const STAGED_FILES_DIR_NAME: &str = "staged-files";
pub const AUDIT_LOG_FILE: &str = "audit.jsonl";
pub const WRITE_POLICY_FILE: &str = "agent-write-policy.json";

pub const STATE_DRAFT: &str = "draft";
pub const STATE_PENDING_APPROVAL: &str = "pending_approval";
pub const STATE_APPROVED: &str = "approved";
pub const STATE_REJECTED: &str = "rejected";
pub const STATE_CANCELLED: &str = "cancelled";
pub const STATE_EXPIRED: &str = "expired";
pub const STATE_EXECUTING: &str = "executing";
pub const STATE_SENT: &str = "sent";
pub const STATE_APPLIED: &str = "applied";
pub const STATE_FAILED: &str = "failed";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWritePolicy {
    #[serde(default = "default_write_policy_format")]
    pub format: String,
    #[serde(default = "default_policy_version")]
    pub version: u32,
    #[serde(default = "default_policy_revision")]
    pub policy_revision: u64,
    #[serde(default = "default_true")]
    pub require_owner_approval: bool,
    #[serde(default)]
    pub auto_approval_enabled: bool,
    #[serde(default = "default_auto_approval_allowed_action_kinds")]
    pub auto_approval_allowed_action_kinds: Vec<String>,
    #[serde(default)]
    pub auto_approval_allowed_contacts: Vec<String>,
    #[serde(default)]
    pub auto_approval_allowed_conversations: Vec<String>,
    #[serde(default = "default_unknown_contacts")]
    pub auto_approval_unknown_contacts: String,
    #[serde(default = "default_auto_approval_allowed_delivery_methods")]
    pub auto_approval_allowed_delivery_methods: Vec<String>,
    #[serde(default = "default_true")]
    pub auto_approval_requires_causal_context: bool,
    #[serde(default = "default_true")]
    pub auto_approval_requires_verified_causal_context: bool,
    #[serde(default)]
    pub auto_approval_allow_attachments: bool,
    #[serde(default = "default_auto_text_bytes")]
    pub auto_approval_max_text_bytes: usize,
    #[serde(default = "default_auto_text_chars")]
    pub auto_approval_max_text_chars: usize,
    #[serde(default)]
    pub auto_approval_max_attachment_bytes: usize,
    #[serde(default = "default_auto_actions_per_hour")]
    pub auto_approval_max_actions_per_hour: usize,
    #[serde(default = "default_auto_actions_per_day")]
    pub auto_approval_max_actions_per_day: usize,
    #[serde(default = "default_auto_messages_per_contact_hour")]
    pub auto_approval_max_messages_per_contact_hour: usize,
    #[serde(default = "default_auto_messages_per_contact_day")]
    pub auto_approval_max_messages_per_contact_day: usize,
    #[serde(default = "default_true")]
    pub deny_execute_on_policy_revision_change: bool,
    #[serde(default = "default_true")]
    pub deny_execute_on_grant_revision_change: bool,
    #[serde(default)]
    pub blocked_action_kinds: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_message_attachments: bool,
    #[serde(default = "default_true")]
    pub allow_message_images: bool,
    #[serde(default = "default_true")]
    pub allow_message_reactions: bool,
    #[serde(default = "default_true")]
    pub allow_contact_mutations: bool,
    #[serde(default = "default_true")]
    pub allow_conversation_mutations: bool,
    #[serde(default = "default_true")]
    pub allow_conversation_delete: bool,
    #[serde(default = "default_true")]
    pub allow_identity_announce: bool,
    #[serde(default = "default_true")]
    pub allow_path_request: bool,
    #[serde(default = "default_true")]
    pub require_owner_approval_for_attachments: bool,
    #[serde(default = "default_true")]
    pub require_owner_approval_for_network: bool,
    #[serde(default = "default_true")]
    pub require_owner_approval_for_contact_mutations: bool,
    #[serde(default = "default_true")]
    pub require_owner_approval_for_conversation_mutations: bool,
    #[serde(default = "default_default_expires_secs")]
    pub default_expires_secs: u64,
    #[serde(default = "default_max_expires_secs")]
    pub max_expires_secs: u64,
    #[serde(default = "default_max_pending_actions")]
    pub max_pending_actions: usize,
    #[serde(default = "default_max_actions_per_hour")]
    pub max_actions_per_hour: usize,
    #[serde(default = "default_max_actions_per_day")]
    pub max_actions_per_day: usize,
    #[serde(default = "default_per_contact_cooldown_secs")]
    pub per_contact_cooldown_secs: u64,
    #[serde(default = "default_inbound_loop_window_secs")]
    pub inbound_loop_window_secs: u64,
    #[serde(default = "default_max_outbound_per_contact_window")]
    pub max_outbound_per_contact_window: usize,
    #[serde(default)]
    pub require_causal_context_for_outbound: bool,
    #[serde(default)]
    pub require_verified_causal_context: bool,
    #[serde(default = "default_max_causal_age_secs")]
    pub max_causal_age_secs: u64,
    #[serde(default = "default_true")]
    pub causal_subject_must_match: bool,
    #[serde(default = "default_true")]
    pub causal_event_must_be_inbound: bool,
    #[serde(default = "default_max_actions_per_causal_event")]
    pub max_actions_per_causal_event: usize,
    #[serde(default = "default_max_actions_per_causal_message")]
    pub max_actions_per_causal_message: usize,
    #[serde(default = "default_max_text_bytes")]
    pub max_text_bytes: usize,
    #[serde(default = "default_max_text_chars")]
    pub max_text_chars: usize,
    #[serde(default = "default_max_title_bytes")]
    pub max_title_bytes: usize,
    #[serde(default = "default_max_title_chars")]
    pub max_title_chars: usize,
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: usize,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: usize,
    #[serde(default = "default_max_image_bytes")]
    pub max_image_bytes: usize,
    #[serde(default = "default_max_attachments_per_action")]
    pub max_attachments_per_action: usize,
    #[serde(default = "default_max_attachment_name_bytes")]
    pub max_attachment_name_bytes: usize,
    #[serde(default = "default_true")]
    pub allow_agent_file_paths: bool,
    #[serde(default)]
    pub allowed_source_roots: Vec<PathBuf>,
    #[serde(default = "default_allowed_delivery_methods")]
    pub allowed_delivery_methods: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_forced_propagated_delivery: bool,
    #[serde(default)]
    pub denied_text_substrings: Vec<String>,
    #[serde(default = "default_true")]
    pub reject_control_chars: bool,
    #[serde(default = "default_allowed_attachment_mime_prefixes")]
    pub allowed_attachment_mime_prefixes: Vec<String>,
    #[serde(default)]
    pub denied_attachment_mime_prefixes: Vec<String>,
    #[serde(default = "default_max_messages_per_contact_hour")]
    pub max_messages_per_contact_hour: usize,
    #[serde(default = "default_max_messages_per_contact_day")]
    pub max_messages_per_contact_day: usize,
    #[serde(default = "default_max_reactions_per_hour")]
    pub max_reactions_per_hour: usize,
    #[serde(default = "default_max_reactions_per_day")]
    pub max_reactions_per_day: usize,
    #[serde(default = "default_max_reactions_per_message")]
    pub max_reactions_per_message: usize,
    #[serde(default = "default_max_contact_mutations_per_hour")]
    pub max_contact_mutations_per_hour: usize,
    #[serde(default = "default_max_contact_mutations_per_day")]
    pub max_contact_mutations_per_day: usize,
    #[serde(default = "default_max_conversation_mutations_per_hour")]
    pub max_conversation_mutations_per_hour: usize,
    #[serde(default = "default_max_conversation_mutations_per_day")]
    pub max_conversation_mutations_per_day: usize,
    #[serde(default = "default_max_network_actions_per_hour")]
    pub max_network_actions_per_hour: usize,
    #[serde(default = "default_max_network_actions_per_day")]
    pub max_network_actions_per_day: usize,
    #[serde(default = "default_max_announces_per_hour")]
    pub max_announces_per_hour: usize,
    #[serde(default = "default_max_announces_per_day")]
    pub max_announces_per_day: usize,
    #[serde(default = "default_min_announce_interval_secs")]
    pub min_announce_interval_secs: u64,
    #[serde(default = "default_max_path_requests_per_hour")]
    pub max_path_requests_per_hour: usize,
    #[serde(default = "default_max_path_requests_per_day")]
    pub max_path_requests_per_day: usize,
    #[serde(default = "default_min_path_request_interval_secs")]
    pub min_path_request_interval_secs: u64,
    #[serde(default)]
    pub allow_unknown_path_requests: bool,
    #[serde(default)]
    pub allowed_path_request_hashes: Vec<String>,
    #[serde(default)]
    pub allowed_propagation_node_hashes: Vec<String>,
    #[serde(default)]
    pub allow_static_propagation_nodes_only: bool,
    #[serde(default)]
    pub reply_requires_existing_message: bool,
    #[serde(default = "default_true")]
    pub reply_to_must_match_causal_message: bool,
}

impl Default for AgentWritePolicy {
    fn default() -> Self {
        Self {
            format: WRITE_POLICY_FORMAT.into(),
            version: 1,
            policy_revision: 1,
            require_owner_approval: true,
            auto_approval_enabled: false,
            auto_approval_allowed_action_kinds: default_auto_approval_allowed_action_kinds(),
            auto_approval_allowed_contacts: Vec::new(),
            auto_approval_allowed_conversations: Vec::new(),
            auto_approval_unknown_contacts: default_unknown_contacts(),
            auto_approval_allowed_delivery_methods: default_auto_approval_allowed_delivery_methods(
            ),
            auto_approval_requires_causal_context: true,
            auto_approval_requires_verified_causal_context: true,
            auto_approval_allow_attachments: false,
            auto_approval_max_text_bytes: default_auto_text_bytes(),
            auto_approval_max_text_chars: default_auto_text_chars(),
            auto_approval_max_attachment_bytes: 0,
            auto_approval_max_actions_per_hour: default_auto_actions_per_hour(),
            auto_approval_max_actions_per_day: default_auto_actions_per_day(),
            auto_approval_max_messages_per_contact_hour: default_auto_messages_per_contact_hour(),
            auto_approval_max_messages_per_contact_day: default_auto_messages_per_contact_day(),
            deny_execute_on_policy_revision_change: true,
            deny_execute_on_grant_revision_change: true,
            blocked_action_kinds: Vec::new(),
            allow_message_attachments: true,
            allow_message_images: true,
            allow_message_reactions: true,
            allow_contact_mutations: true,
            allow_conversation_mutations: true,
            allow_conversation_delete: true,
            allow_identity_announce: true,
            allow_path_request: true,
            require_owner_approval_for_attachments: true,
            require_owner_approval_for_network: true,
            require_owner_approval_for_contact_mutations: true,
            require_owner_approval_for_conversation_mutations: true,
            default_expires_secs: 24 * 60 * 60,
            max_expires_secs: 7 * 24 * 60 * 60,
            max_pending_actions: 25,
            max_actions_per_hour: 60,
            max_actions_per_day: 200,
            per_contact_cooldown_secs: 3,
            inbound_loop_window_secs: 10 * 60,
            max_outbound_per_contact_window: 6,
            require_causal_context_for_outbound: false,
            require_verified_causal_context: false,
            max_causal_age_secs: default_max_causal_age_secs(),
            causal_subject_must_match: true,
            causal_event_must_be_inbound: true,
            max_actions_per_causal_event: 3,
            max_actions_per_causal_message: 2,
            max_text_bytes: 4096,
            max_text_chars: 4096,
            max_title_bytes: 256,
            max_title_chars: 256,
            max_attachment_bytes: rns_protocol::resource::MAX_EFFICIENT_SIZE,
            max_file_bytes: rns_protocol::resource::MAX_EFFICIENT_SIZE,
            max_image_bytes: rns_protocol::resource::MAX_EFFICIENT_SIZE,
            max_attachments_per_action: 1,
            max_attachment_name_bytes: 200,
            allow_agent_file_paths: true,
            allowed_source_roots: Vec::new(),
            allowed_delivery_methods: default_allowed_delivery_methods(),
            allow_forced_propagated_delivery: true,
            denied_text_substrings: Vec::new(),
            reject_control_chars: true,
            allowed_attachment_mime_prefixes: default_allowed_attachment_mime_prefixes(),
            denied_attachment_mime_prefixes: Vec::new(),
            max_messages_per_contact_hour: default_max_messages_per_contact_hour(),
            max_messages_per_contact_day: default_max_messages_per_contact_day(),
            max_reactions_per_hour: default_max_reactions_per_hour(),
            max_reactions_per_day: default_max_reactions_per_day(),
            max_reactions_per_message: default_max_reactions_per_message(),
            max_contact_mutations_per_hour: default_max_contact_mutations_per_hour(),
            max_contact_mutations_per_day: default_max_contact_mutations_per_day(),
            max_conversation_mutations_per_hour: default_max_conversation_mutations_per_hour(),
            max_conversation_mutations_per_day: default_max_conversation_mutations_per_day(),
            max_network_actions_per_hour: default_max_network_actions_per_hour(),
            max_network_actions_per_day: default_max_network_actions_per_day(),
            max_announces_per_hour: default_max_announces_per_hour(),
            max_announces_per_day: default_max_announces_per_day(),
            min_announce_interval_secs: default_min_announce_interval_secs(),
            max_path_requests_per_hour: default_max_path_requests_per_hour(),
            max_path_requests_per_day: default_max_path_requests_per_day(),
            min_path_request_interval_secs: default_min_path_request_interval_secs(),
            allow_unknown_path_requests: false,
            allowed_path_request_hashes: Vec::new(),
            allowed_propagation_node_hashes: Vec::new(),
            allow_static_propagation_nodes_only: false,
            reply_requires_existing_message: false,
            reply_to_must_match_causal_message: true,
        }
    }
}

fn default_write_policy_format() -> String {
    WRITE_POLICY_FORMAT.into()
}

fn default_policy_version() -> u32 {
    1
}

fn default_policy_revision() -> u64 {
    1
}

fn default_true() -> bool {
    true
}

fn default_unknown_contacts() -> String {
    "deny".into()
}

fn default_default_expires_secs() -> u64 {
    24 * 60 * 60
}

fn default_max_expires_secs() -> u64 {
    7 * 24 * 60 * 60
}

fn default_max_pending_actions() -> usize {
    25
}

fn default_max_actions_per_hour() -> usize {
    60
}

fn default_max_actions_per_day() -> usize {
    200
}

fn default_per_contact_cooldown_secs() -> u64 {
    3
}

fn default_inbound_loop_window_secs() -> u64 {
    10 * 60
}

fn default_max_outbound_per_contact_window() -> usize {
    6
}

fn default_max_actions_per_causal_event() -> usize {
    3
}

fn default_max_actions_per_causal_message() -> usize {
    2
}

fn default_max_causal_age_secs() -> u64 {
    24 * 60 * 60
}

fn default_max_text_bytes() -> usize {
    4096
}

fn default_max_text_chars() -> usize {
    4096
}

fn default_max_title_bytes() -> usize {
    256
}

fn default_max_title_chars() -> usize {
    256
}

fn default_max_attachment_bytes() -> usize {
    rns_protocol::resource::MAX_EFFICIENT_SIZE
}

fn default_max_file_bytes() -> usize {
    rns_protocol::resource::MAX_EFFICIENT_SIZE
}

fn default_max_image_bytes() -> usize {
    rns_protocol::resource::MAX_EFFICIENT_SIZE
}

fn default_max_attachments_per_action() -> usize {
    1
}

fn default_max_attachment_name_bytes() -> usize {
    200
}

fn default_allowed_attachment_mime_prefixes() -> Vec<String> {
    vec![
        "image/".into(),
        "text/".into(),
        "application/pdf".into(),
        "application/json".into(),
        "application/zip".into(),
        "application/octet-stream".into(),
    ]
}

fn default_allowed_delivery_methods() -> Vec<String> {
    vec![
        "auto".into(),
        "direct".into(),
        "opportunistic".into(),
        "propagated".into(),
    ]
}

fn default_auto_approval_allowed_delivery_methods() -> Vec<String> {
    vec!["auto".into()]
}

fn default_auto_approval_allowed_action_kinds() -> Vec<String> {
    vec!["message.reply".into(), "message.send".into()]
}

fn default_auto_text_bytes() -> usize {
    1500
}

fn default_auto_text_chars() -> usize {
    1500
}

fn default_auto_actions_per_hour() -> usize {
    20
}

fn default_auto_actions_per_day() -> usize {
    100
}

fn default_auto_messages_per_contact_hour() -> usize {
    10
}

fn default_auto_messages_per_contact_day() -> usize {
    40
}

fn default_max_messages_per_contact_hour() -> usize {
    60
}

fn default_max_messages_per_contact_day() -> usize {
    200
}

fn default_max_reactions_per_hour() -> usize {
    120
}

fn default_max_reactions_per_day() -> usize {
    400
}

fn default_max_reactions_per_message() -> usize {
    3
}

fn default_max_contact_mutations_per_hour() -> usize {
    20
}

fn default_max_contact_mutations_per_day() -> usize {
    50
}

fn default_max_conversation_mutations_per_hour() -> usize {
    60
}

fn default_max_conversation_mutations_per_day() -> usize {
    200
}

fn default_max_network_actions_per_hour() -> usize {
    10
}

fn default_max_network_actions_per_day() -> usize {
    30
}

fn default_max_announces_per_hour() -> usize {
    2
}

fn default_max_announces_per_day() -> usize {
    12
}

fn default_min_announce_interval_secs() -> u64 {
    15 * 60
}

fn default_max_path_requests_per_hour() -> usize {
    20
}

fn default_max_path_requests_per_day() -> usize {
    100
}

fn default_min_path_request_interval_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentActionRecord {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub agent: String,
    pub identity_hash: String,
    pub kind: String,
    pub state: String,
    pub created_at_unix: f64,
    pub updated_at_unix: f64,
    pub expires_at_unix: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_hash: Option<String>,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub staged_files: Vec<StagedFile>,
    pub policy: ActionPolicySnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionRecord>,
    #[serde(default)]
    pub safety: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFile {
    pub id: String,
    pub kind: String,
    pub file_name: String,
    pub mime: String,
    pub size: usize,
    pub sha256: String,
    pub stored_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPolicySnapshot {
    pub grant_revision: u64,
    #[serde(default = "default_policy_revision")]
    pub policy_revision: u64,
    pub scopes_checked: Vec<String>,
    pub approval_required: bool,
    pub rate_limits: Value,
    pub limits: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub state: String,
    pub actor: String,
    pub decided_at_unix: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub state: String,
    pub attempted_at_unix: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at_unix: Option<f64>,
    #[serde(default)]
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub created_at_unix: f64,
    pub actor_type: String,
    pub actor: String,
    pub event: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_hash: Option<String>,
    #[serde(default)]
    pub details: Value,
    #[serde(default)]
    pub redactions: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct NewAction {
    pub kind: String,
    pub conversation_id: Option<String>,
    pub subject_hash: Option<String>,
    pub payload: Value,
    pub staged_files: Vec<StagedFile>,
    pub required_scopes: Vec<String>,
    pub client_action_id: Option<String>,
    pub client_action_fingerprint: String,
    pub causal_event_id: Option<u64>,
    pub causal_message_id: Option<String>,
    pub text_bytes: usize,
    pub text_chars: usize,
    pub attachment_bytes: usize,
    pub expires_secs: Option<u64>,
    pub submit: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct PendingStagedFile<'a> {
    pub file_name: &'a str,
    pub mime: &'a str,
    pub kind: &'a str,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct Actor {
    pub actor_type: String,
    pub actor: String,
}

impl Actor {
    pub fn agent(name: impl Into<String>) -> Self {
        Self {
            actor_type: "agent".into(),
            actor: name.into(),
        }
    }

    pub fn owner() -> Self {
        Self {
            actor_type: "owner".into(),
            actor: "owner".into(),
        }
    }

    pub fn daemon() -> Self {
        Self {
            actor_type: "daemon".into(),
            actor: "ratspeakd".into(),
        }
    }
}

pub fn actions_root(data_dir: &Path) -> PathBuf {
    data_dir.join(ACTIONS_DIR_NAME)
}

pub fn action_records_dir(data_dir: &Path) -> PathBuf {
    actions_root(data_dir).join(ACTION_RECORDS_DIR_NAME)
}

pub fn staged_files_dir(data_dir: &Path) -> PathBuf {
    actions_root(data_dir).join(STAGED_FILES_DIR_NAME)
}

pub fn audit_log_path(data_dir: &Path) -> PathBuf {
    actions_root(data_dir).join(AUDIT_LOG_FILE)
}

pub fn write_policy_path(data_dir: &Path) -> PathBuf {
    actions_root(data_dir).join(WRITE_POLICY_FILE)
}

pub fn read_write_policy(data_dir: &Path) -> CliResult<AgentWritePolicy> {
    let path = write_policy_path(data_dir);
    if !path.exists() {
        return Ok(AgentWritePolicy::default());
    }
    let bytes = fs::read(path)?;
    let mut policy: AgentWritePolicy = serde_json::from_slice(&bytes)?;
    normalize_write_policy(&mut policy);
    validate_write_policy(&policy)?;
    Ok(policy)
}

pub fn ensure_write_policy(data_dir: &Path) -> CliResult<AgentWritePolicy> {
    let policy = read_write_policy(data_dir)?;
    let path = write_policy_path(data_dir);
    if !path.exists() {
        write_json_private(&path, &policy)?;
    }
    Ok(policy)
}

pub fn write_write_policy(data_dir: &Path, policy: &AgentWritePolicy) -> CliResult<()> {
    let mut policy = policy.clone();
    normalize_write_policy(&mut policy);
    validate_write_policy(&policy)?;
    write_json_private(&write_policy_path(data_dir), &policy)
}

pub fn validate_write_policy(policy: &AgentWritePolicy) -> CliResult<()> {
    if policy.format != WRITE_POLICY_FORMAT {
        return Err(CliError::usage(format!(
            "unsupported write policy format: {}",
            policy.format
        )));
    }
    if !matches!(
        policy.auto_approval_unknown_contacts.as_str(),
        "deny" | "allow"
    ) {
        return Err(CliError::usage(
            "auto_approval_unknown_contacts must be either deny or allow",
        ));
    }
    for kind in policy
        .blocked_action_kinds
        .iter()
        .chain(policy.auto_approval_allowed_action_kinds.iter())
    {
        validate_action_kind(kind)?;
    }
    for method in policy
        .allowed_delivery_methods
        .iter()
        .chain(policy.auto_approval_allowed_delivery_methods.iter())
    {
        validate_delivery_method(method)?;
    }
    for contact in policy
        .auto_approval_allowed_contacts
        .iter()
        .chain(policy.allowed_path_request_hashes.iter())
        .chain(policy.allowed_propagation_node_hashes.iter())
    {
        if !ratspeak_runtime::helpers::validate_hex(contact, 32, 32) {
            return Err(CliError::usage(format!(
                "policy hash values must be 32 hex characters: {contact}"
            )));
        }
    }
    for conversation in &policy.auto_approval_allowed_conversations {
        let Some(hash) = crate::agent_policy::dest_hash_from_conversation_id(conversation) else {
            return Err(CliError::usage(format!(
                "invalid auto approval conversation id: {conversation}"
            )));
        };
        if !ratspeak_runtime::helpers::validate_hex(&hash, 32, 32) {
            return Err(CliError::usage(format!(
                "auto approval conversation hash must be 32 hex characters: {conversation}"
            )));
        }
    }
    if policy.max_expires_secs < policy.default_expires_secs {
        return Err(CliError::usage(
            "max_expires_secs must be greater than or equal to default_expires_secs",
        ));
    }
    Ok(())
}

fn normalize_write_policy(policy: &mut AgentWritePolicy) {
    if policy.format.is_empty() {
        policy.format = WRITE_POLICY_FORMAT.into();
    }
    if policy.version == 0 {
        policy.version = 1;
    }
    if policy.policy_revision == 0 {
        policy.policy_revision = 1;
    }
    policy.allowed_delivery_methods = normalize_string_list(&policy.allowed_delivery_methods);
    policy.auto_approval_allowed_delivery_methods =
        normalize_string_list(&policy.auto_approval_allowed_delivery_methods);
    policy.blocked_action_kinds = normalize_string_list(&policy.blocked_action_kinds);
    policy.auto_approval_allowed_action_kinds =
        normalize_string_list(&policy.auto_approval_allowed_action_kinds);
    policy.allowed_attachment_mime_prefixes =
        normalize_string_list(&policy.allowed_attachment_mime_prefixes);
    policy.denied_attachment_mime_prefixes =
        normalize_string_list(&policy.denied_attachment_mime_prefixes);
    policy.denied_text_substrings = policy
        .denied_text_substrings
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect();
    policy.auto_approval_allowed_contacts =
        normalize_hash_list(&policy.auto_approval_allowed_contacts);
    policy.allowed_path_request_hashes = normalize_hash_list(&policy.allowed_path_request_hashes);
    policy.allowed_propagation_node_hashes =
        normalize_hash_list(&policy.allowed_propagation_node_hashes);
    policy.auto_approval_allowed_conversations = policy
        .auto_approval_allowed_conversations
        .iter()
        .filter_map(|value| crate::agent_policy::dest_hash_from_conversation_id(value))
        .map(|hash| crate::agent_policy::conversation_id_for_dest(&hash))
        .collect();
    policy.auto_approval_allowed_conversations.sort();
    policy.auto_approval_allowed_conversations.dedup();
}

fn normalize_string_list(values: &[String]) -> Vec<String> {
    let mut out = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn normalize_hash_list(values: &[String]) -> Vec<String> {
    let mut out = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

pub fn create_action(
    data_dir: &Path,
    principal: &AgentPrincipal,
    action: NewAction,
) -> CliResult<AgentActionRecord> {
    if let Some(client_action_id) = action.client_action_id.as_deref()
        && let Some(existing) = find_idempotent_action(data_dir, &principal.name, client_action_id)?
    {
        let result = ensure_idempotent_action_matches(
            &existing,
            &action.kind,
            action.subject_hash.as_deref(),
            action.conversation_id.as_deref(),
            &action.client_action_fingerprint,
        );
        cleanup_staged_files(&action.staged_files);
        result?;
        return Ok(existing);
    }
    let policy = ensure_write_policy(data_dir)?;
    let now = unix_now_secs();
    let action_policy = evaluate_action_policy(data_dir, &policy, principal, &action, now)?;
    let rate_limits = match check_rate_limits(
        data_dir,
        &policy,
        &principal.name,
        action.subject_hash.as_deref(),
        &action.kind,
        action.causal_event_id,
        action.causal_message_id.as_deref(),
        payload_message_id(&action.payload),
        now,
        None,
    ) {
        Ok(rate_limits) => rate_limits,
        Err(error) => {
            cleanup_staged_files(&action.staged_files);
            return Err(error);
        }
    };
    if let Err(error) = validate_payload_limits(
        &policy,
        action.text_bytes,
        action.text_chars,
        action.attachment_bytes,
        &action.staged_files,
    ) {
        cleanup_staged_files(&action.staged_files);
        return Err(error);
    }
    if let Err(error) = validate_action_guardrails(data_dir, &policy, principal, &action, now) {
        cleanup_staged_files(&action.staged_files);
        return Err(error);
    }
    let expires_secs = action
        .expires_secs
        .unwrap_or(policy.default_expires_secs)
        .min(policy.max_expires_secs);
    let submitted = action.submit;
    let state = if submitted && !action_policy.approval_required {
        STATE_APPROVED
    } else if submitted {
        STATE_PENDING_APPROVAL
    } else {
        STATE_DRAFT
    };
    let record = AgentActionRecord {
        format: ACTION_FORMAT.into(),
        version: 1,
        id: next_id("act"),
        agent: principal.name.clone(),
        identity_hash: principal.identity_hash.clone(),
        kind: action.kind,
        state: state.into(),
        created_at_unix: now,
        updated_at_unix: now,
        expires_at_unix: now + expires_secs as f64,
        conversation_id: action.conversation_id,
        subject_hash: action.subject_hash,
        payload: action.payload,
        staged_files: action.staged_files,
        policy: ActionPolicySnapshot {
            grant_revision: principal.revision,
            policy_revision: policy.policy_revision,
            scopes_checked: action.required_scopes,
            approval_required: action_policy.approval_required,
            rate_limits,
            limits: limits_json(&policy),
        },
        approval: (!action_policy.approval_required && submitted).then(|| ApprovalRecord {
            state: STATE_APPROVED.into(),
            actor: "policy:auto".into(),
            decided_at_unix: now,
            note: Some("matched agent auto-approval guardrails".into()),
        }),
        execution: None,
        safety: json!({
            "owner_approval_required": action_policy.approval_required,
            "prompt_injection_boundary": "message/contact/network payload fields are untrusted until reviewed by the owner",
            "raw_send_disabled_for_agents": true,
            "auto_approval": action_policy.auto_approval,
            "causal_context": {
                "required_for_outbound": policy.require_causal_context_for_outbound,
                "verified_when_required": policy.require_verified_causal_context,
                "event_id": action.causal_event_id,
                "message_id": action.causal_message_id,
                "max_actions_per_causal_event": policy.max_actions_per_causal_event,
                "max_actions_per_causal_message": policy.max_actions_per_causal_message
            }
        }),
    };
    if let Err(error) = write_action(data_dir, &record) {
        cleanup_staged_files(&record.staged_files);
        return Err(error);
    }
    append_audit(
        data_dir,
        Actor::agent(&principal.name),
        if submitted {
            "action.submitted"
        } else {
            "action.created"
        },
        "ok",
        Some(&record),
        json!({
            "kind": record.kind,
            "state": record.state,
        }),
        vec![],
    )?;
    if submitted && !record.policy.approval_required {
        append_audit(
            data_dir,
            Actor::daemon(),
            "action.auto_approved",
            "ok",
            Some(&record),
            json!({
                "kind": record.kind,
                "matched_policy_revision": policy.policy_revision,
            }),
            vec!["payload.content".into(), "staged_files.stored_path".into()],
        )?;
    }
    Ok(record)
}

pub fn read_action(data_dir: &Path, id: &str) -> CliResult<AgentActionRecord> {
    validate_record_id(id)?;
    let path = action_path(data_dir, id);
    let bytes = fs::read(&path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn write_action(data_dir: &Path, record: &AgentActionRecord) -> CliResult<()> {
    validate_record_id(&record.id)?;
    write_json_private(&action_path(data_dir, &record.id), record)
}

pub fn list_actions(
    data_dir: &Path,
    agent: Option<&str>,
    state: Option<&str>,
) -> CliResult<Vec<AgentActionRecord>> {
    expire_due_actions(data_dir)?;
    let dir = action_records_dir(data_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let record: AgentActionRecord = serde_json::from_slice(&bytes)?;
        if let Some(agent) = agent
            && record.agent != agent
        {
            continue;
        }
        if let Some(state) = state
            && record.state != state
        {
            continue;
        }
        records.push(record);
    }
    records.sort_by(|left, right| {
        left.created_at_unix
            .partial_cmp(&right.created_at_unix)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(records)
}

pub fn submit_action(
    data_dir: &Path,
    id: &str,
    principal: &AgentPrincipal,
) -> CliResult<AgentActionRecord> {
    let mut record = read_action(data_dir, id)?;
    if record.agent != principal.name {
        return Err(CliError::failed(
            "agent cannot submit another agent's action",
        ));
    }
    if record.state != STATE_DRAFT {
        return Ok(record);
    }
    let policy = ensure_write_policy(data_dir)?;
    let now = unix_now_secs();
    if now >= record.expires_at_unix {
        record.state = STATE_EXPIRED.into();
        record.updated_at_unix = now;
        write_action(data_dir, &record)?;
        return Err(CliError::failed("action has expired"));
    }
    recheck_record_policy(data_dir, principal, &record, now)?;
    let action_policy = evaluate_record_action_policy(data_dir, &policy, principal, &record, now)?;
    record.policy.rate_limits = check_rate_limits(
        data_dir,
        &policy,
        &principal.name,
        record.subject_hash.as_deref(),
        &record.kind,
        record_causal_event_id(&record),
        record_causal_message_id(&record).as_deref(),
        payload_message_id(&record.payload),
        now,
        Some(&record.id),
    )?;
    record.policy.approval_required = action_policy.approval_required;
    record.policy.policy_revision = policy.policy_revision;
    record.state = if action_policy.approval_required {
        STATE_PENDING_APPROVAL.into()
    } else {
        STATE_APPROVED.into()
    };
    record.updated_at_unix = now;
    if !action_policy.approval_required {
        record.approval = Some(ApprovalRecord {
            state: STATE_APPROVED.into(),
            actor: "policy:auto".into(),
            decided_at_unix: now,
            note: Some("matched agent auto-approval guardrails".into()),
        });
        if let Some(obj) = record.safety.as_object_mut() {
            obj.insert("owner_approval_required".into(), json!(false));
            obj.insert("auto_approval".into(), action_policy.auto_approval.clone());
        }
    }
    write_action(data_dir, &record)?;
    append_audit(
        data_dir,
        Actor::agent(&principal.name),
        "action.submitted",
        "ok",
        Some(&record),
        json!({ "kind": record.kind }),
        vec![],
    )?;
    if !record.policy.approval_required {
        append_audit(
            data_dir,
            Actor::daemon(),
            "action.auto_approved",
            "ok",
            Some(&record),
            json!({
                "kind": record.kind,
                "matched_policy_revision": policy.policy_revision,
            }),
            vec!["payload.content".into(), "staged_files.stored_path".into()],
        )?;
    }
    Ok(record)
}

pub fn approve_action(
    data_dir: &Path,
    id: &str,
    note: Option<String>,
) -> CliResult<AgentActionRecord> {
    set_approval_state(data_dir, id, STATE_APPROVED, note, "action.approved", "ok")
}

pub fn reject_action(
    data_dir: &Path,
    id: &str,
    note: Option<String>,
) -> CliResult<AgentActionRecord> {
    set_approval_state(data_dir, id, STATE_REJECTED, note, "action.rejected", "ok")
}

pub fn cancel_action(
    data_dir: &Path,
    id: &str,
    actor: Actor,
    note: Option<String>,
) -> CliResult<AgentActionRecord> {
    let mut record = read_action(data_dir, id)?;
    if matches!(
        record.state.as_str(),
        STATE_SENT | STATE_APPLIED | STATE_REJECTED | STATE_CANCELLED
    ) {
        return Ok(record);
    }
    let now = unix_now_secs();
    record.state = STATE_CANCELLED.into();
    record.updated_at_unix = now;
    record.approval = Some(ApprovalRecord {
        state: STATE_CANCELLED.into(),
        actor: actor.actor.clone(),
        decided_at_unix: now,
        note,
    });
    write_action(data_dir, &record)?;
    append_audit(
        data_dir,
        actor,
        "action.cancelled",
        "ok",
        Some(&record),
        json!({ "kind": record.kind }),
        vec![],
    )?;
    Ok(record)
}

pub fn mark_executing(data_dir: &Path, id: &str) -> CliResult<AgentActionRecord> {
    let mut record = read_action(data_dir, id)?;
    let now = unix_now_secs();
    if record.state != STATE_APPROVED {
        return Err(CliError::failed(format!(
            "action is not approved: {}",
            record.state
        )));
    }
    if now >= record.expires_at_unix {
        record.state = STATE_EXPIRED.into();
        record.updated_at_unix = now;
        write_action(data_dir, &record)?;
        return Err(CliError::failed("action has expired"));
    }
    record.state = STATE_EXECUTING.into();
    record.updated_at_unix = now;
    record.execution = Some(ExecutionRecord {
        state: STATE_EXECUTING.into(),
        attempted_at_unix: now,
        completed_at_unix: None,
        result: Value::Null,
        error: None,
    });
    write_action(data_dir, &record)?;
    append_audit(
        data_dir,
        Actor::daemon(),
        "action.execution_started",
        "ok",
        Some(&record),
        json!({ "kind": record.kind }),
        vec![],
    )?;
    Ok(record)
}

pub fn mark_execution_complete(
    data_dir: &Path,
    id: &str,
    state: &str,
    result: Value,
    error: Option<String>,
) -> CliResult<AgentActionRecord> {
    let mut record = read_action(data_dir, id)?;
    let now = unix_now_secs();
    record.state = state.into();
    record.updated_at_unix = now;
    record.execution = Some(ExecutionRecord {
        state: state.into(),
        attempted_at_unix: record
            .execution
            .as_ref()
            .map(|execution| execution.attempted_at_unix)
            .unwrap_or(now),
        completed_at_unix: Some(now),
        result: result.clone(),
        error: error.clone(),
    });
    write_action(data_dir, &record)?;
    append_audit(
        data_dir,
        Actor::daemon(),
        "action.execution_finished",
        if error.is_some() { "error" } else { "ok" },
        Some(&record),
        json!({
            "kind": record.kind,
            "state": state,
            "result": result,
            "error": error,
        }),
        vec!["payload.content".into(), "staged_files.stored_path".into()],
    )?;
    Ok(record)
}

pub fn expire_due_actions(data_dir: &Path) -> CliResult<usize> {
    let dir = action_records_dir(data_dir);
    if !dir.is_dir() {
        return Ok(0);
    }
    let now = unix_now_secs();
    let mut expired = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let mut record: AgentActionRecord = serde_json::from_slice(&bytes)?;
        if now < record.expires_at_unix
            || matches!(
                record.state.as_str(),
                STATE_SENT
                    | STATE_APPLIED
                    | STATE_FAILED
                    | STATE_REJECTED
                    | STATE_CANCELLED
                    | STATE_EXPIRED
            )
        {
            continue;
        }
        record.state = STATE_EXPIRED.into();
        record.updated_at_unix = now;
        write_action(data_dir, &record)?;
        append_audit(
            data_dir,
            Actor::daemon(),
            "action.expired",
            "ok",
            Some(&record),
            json!({ "kind": record.kind }),
            vec![],
        )?;
        expired += 1;
    }
    Ok(expired)
}

pub fn append_audit(
    data_dir: &Path,
    actor: Actor,
    event: &str,
    outcome: &str,
    action: Option<&AgentActionRecord>,
    details: Value,
    redactions: Vec<String>,
) -> CliResult<AuditRecord> {
    let record = AuditRecord {
        format: AUDIT_FORMAT.into(),
        version: 1,
        id: next_id("aud"),
        created_at_unix: unix_now_secs(),
        actor_type: actor.actor_type,
        actor: actor.actor,
        event: event.into(),
        outcome: outcome.into(),
        action_id: action.map(|action| action.id.clone()),
        subject_hash: action.and_then(|action| action.subject_hash.clone()),
        details,
        redactions,
    };
    let path = audit_log_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        restrict_dir_permissions(parent)?;
    }
    let existed = path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    if !existed {
        restrict_file_permissions(&path)?;
    }
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    Ok(record)
}

pub fn list_audit(data_dir: &Path, limit: usize) -> CliResult<Vec<AuditRecord>> {
    let path = audit_log_path(data_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    let mut records = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        records.push(serde_json::from_str::<AuditRecord>(line)?);
    }
    if records.len() > limit {
        records = records.split_off(records.len() - limit);
    }
    Ok(records)
}

pub fn stage_file(
    data_dir: &Path,
    file_name: &str,
    mime: &str,
    kind: &str,
    bytes: &[u8],
) -> CliResult<StagedFile> {
    let id = next_id("file");
    let safe_name = sanitize_file_name(file_name, mime, kind);
    let sha256 = sha256_hex(bytes);
    let dir = staged_files_dir(data_dir).join(&id);
    fs::create_dir_all(&dir)?;
    restrict_dir_permissions(&dir)?;
    let stored_path = dir.join(&safe_name);
    fs::write(&stored_path, bytes)?;
    restrict_file_permissions(&stored_path)?;
    Ok(StagedFile {
        id,
        kind: kind.into(),
        file_name: safe_name,
        mime: mime.into(),
        size: bytes.len(),
        sha256,
        stored_path,
    })
}

pub fn inspect_staged_file(
    data_dir: &Path,
    action_id: &str,
    file_id: Option<&str>,
    max_preview_bytes: usize,
) -> CliResult<Value> {
    let record = read_action(data_dir, action_id)?;
    let staged = if let Some(file_id) = file_id {
        record
            .staged_files
            .iter()
            .find(|file| file.id == file_id)
            .ok_or_else(|| CliError::failed(format!("staged file not found: {file_id}")))?
    } else {
        record
            .staged_files
            .first()
            .ok_or_else(|| CliError::failed("action has no staged files"))?
    };
    let bytes = fs::read(&staged.stored_path)?;
    let preview_limit = max_preview_bytes.min(bytes.len());
    let preview = if staged.mime.starts_with("text/")
        || staged.mime == "application/json"
        || staged.mime.ends_with("+json")
    {
        Some(String::from_utf8_lossy(&bytes[..preview_limit]).to_string())
    } else {
        None
    };
    Ok(json!({
        "action_id": record.id,
        "agent": record.agent,
        "kind": record.kind,
        "state": record.state,
        "file": {
            "id": staged.id,
            "kind": staged.kind,
            "file_name": staged.file_name,
            "mime": staged.mime,
            "size": staged.size,
            "sha256": staged.sha256,
            "stored_path": staged.stored_path,
            "preview_text": preview,
            "preview_truncated": preview_limit < bytes.len(),
        },
        "owner_review": {
            "safe_to_approve_without_content_review": false,
            "note": "staged file bytes are local owner data; inspect before approving unexpected attachments"
        }
    }))
}

pub fn preflight_new_action(
    data_dir: &Path,
    _principal: &AgentPrincipal,
    _kind: &str,
    _subject_hash: Option<&str>,
    text_bytes: usize,
    text_chars: usize,
    attachment_bytes: usize,
    pending_files: &[PendingStagedFile<'_>],
    _causal_event_id: Option<u64>,
    _causal_message_id: Option<&str>,
) -> CliResult<()> {
    let policy = ensure_write_policy(data_dir)?;
    validate_pending_payload_limits(
        &policy,
        text_bytes,
        text_chars,
        attachment_bytes,
        pending_files,
    )
}

pub fn find_idempotent_action(
    data_dir: &Path,
    agent: &str,
    client_action_id: &str,
) -> CliResult<Option<AgentActionRecord>> {
    validate_client_action_id(client_action_id)?;
    let mut records = list_actions_without_expiry(data_dir)?;
    records.sort_by(|left, right| {
        left.created_at_unix
            .partial_cmp(&right.created_at_unix)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(records.into_iter().find(|record| {
        record.agent == agent
            && record
                .payload
                .get("client_action_id")
                .and_then(Value::as_str)
                == Some(client_action_id)
    }))
}

pub fn ensure_idempotent_action_matches(
    record: &AgentActionRecord,
    kind: &str,
    subject_hash: Option<&str>,
    conversation_id: Option<&str>,
    fingerprint: &str,
) -> CliResult<()> {
    if record.kind != kind
        || record.subject_hash.as_deref() != subject_hash
        || record.conversation_id.as_deref() != conversation_id
    {
        return Err(CliError::failed(
            "client_action_id already belongs to a different action target",
        ));
    }
    if let Some(existing_fingerprint) = record
        .payload
        .get("client_action_fingerprint")
        .and_then(Value::as_str)
        && existing_fingerprint != fingerprint
    {
        return Err(CliError::failed(
            "client_action_id replay payload does not match original action",
        ));
    }
    Ok(())
}

pub fn validate_client_action_id(value: &str) -> CliResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
    {
        return Err(CliError::usage(
            "client_action_id must be 1-128 ASCII letters, numbers, '.', '-', '_' or ':'",
        ));
    }
    Ok(())
}

pub fn fingerprint_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    sha256_hex(&bytes)
}

pub fn public_action(mut record: AgentActionRecord, include_payload: bool) -> Value {
    for staged in &mut record.staged_files {
        staged.stored_path = PathBuf::from("<redacted>");
    }
    let mut value = serde_json::to_value(record).unwrap_or_else(|_| Value::Null);
    if !include_payload && let Some(obj) = value.as_object_mut() {
        obj.insert(
            "payload".into(),
            json!({
                "redacted": true,
                "reason": "payload content is shown only through action show/owner review"
            }),
        );
    }
    value
}

pub fn recheck_action_for_execute(
    data_dir: &Path,
    principal: &AgentPrincipal,
    record: &AgentActionRecord,
) -> CliResult<Value> {
    let now = unix_now_secs();
    recheck_record_policy(data_dir, principal, record, now)?;
    let policy = ensure_write_policy(data_dir)?;
    check_rate_limits(
        data_dir,
        &policy,
        &principal.name,
        record.subject_hash.as_deref(),
        &record.kind,
        record_causal_event_id(record),
        record_causal_message_id(record).as_deref(),
        payload_message_id(&record.payload),
        now,
        Some(&record.id),
    )
}

#[derive(Debug, Clone)]
struct ActionPolicyDecision {
    approval_required: bool,
    auto_approval: Value,
}

fn evaluate_action_policy(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    principal: &AgentPrincipal,
    action: &NewAction,
    now: f64,
) -> CliResult<ActionPolicyDecision> {
    let auto_approval = auto_approval_decision_for_action(
        data_dir,
        policy,
        &principal.name,
        action.subject_hash.as_deref(),
        action.conversation_id.as_deref(),
        &action.kind,
        &action.payload,
        &action.staged_files,
        action.text_bytes,
        action.text_chars,
        action.attachment_bytes,
        action.causal_event_id,
        action.causal_message_id.as_deref(),
        now,
    )?;
    let high_risk_reason = owner_approval_reason(policy, &action.kind);
    let auto_allowed = auto_approval
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = if high_risk_reason.is_some() {
        true
    } else if auto_allowed {
        false
    } else {
        policy.require_owner_approval
    };
    Ok(ActionPolicyDecision {
        approval_required,
        auto_approval,
    })
}

fn evaluate_record_action_policy(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    principal: &AgentPrincipal,
    record: &AgentActionRecord,
    now: f64,
) -> CliResult<ActionPolicyDecision> {
    let (text_bytes, text_chars) = record_text_counts(record);
    let attachment_bytes = record_attachment_bytes(record);
    let auto_approval = auto_approval_decision_for_action(
        data_dir,
        policy,
        &principal.name,
        record.subject_hash.as_deref(),
        record.conversation_id.as_deref(),
        &record.kind,
        &record.payload,
        &record.staged_files,
        text_bytes,
        text_chars,
        attachment_bytes,
        record_causal_event_id(record),
        record_causal_message_id(record).as_deref(),
        now,
    )?;
    let high_risk_reason = owner_approval_reason(policy, &record.kind);
    let auto_allowed = auto_approval
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = if high_risk_reason.is_some() {
        true
    } else if auto_allowed {
        false
    } else {
        policy.require_owner_approval
    };
    Ok(ActionPolicyDecision {
        approval_required,
        auto_approval,
    })
}

fn recheck_record_policy(
    data_dir: &Path,
    principal: &AgentPrincipal,
    record: &AgentActionRecord,
    now: f64,
) -> CliResult<()> {
    let policy = ensure_write_policy(data_dir)?;
    if policy.deny_execute_on_grant_revision_change
        && record.policy.grant_revision != 0
        && record.policy.grant_revision != principal.revision
    {
        return Err(CliError::failed(format!(
            "agent grant changed since action was created (action={}, current={})",
            record.policy.grant_revision, principal.revision
        )));
    }
    if policy.deny_execute_on_policy_revision_change
        && record.policy.policy_revision != 0
        && record.policy.policy_revision != policy.policy_revision
    {
        return Err(CliError::failed(format!(
            "agent write policy changed since action was created (action={}, current={})",
            record.policy.policy_revision, policy.policy_revision
        )));
    }
    let (text_bytes, text_chars) = record_text_counts(record);
    validate_payload_limits(
        &policy,
        text_bytes,
        text_chars,
        record_attachment_bytes(record),
        &record.staged_files,
    )?;
    validate_record_guardrails(data_dir, &policy, principal, record, now)
}

fn auto_approval_decision_for_action(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    agent: &str,
    subject_hash: Option<&str>,
    conversation_id: Option<&str>,
    kind: &str,
    payload: &Value,
    staged_files: &[StagedFile],
    text_bytes: usize,
    text_chars: usize,
    attachment_bytes: usize,
    causal_event_id: Option<u64>,
    causal_message_id: Option<&str>,
    now: f64,
) -> CliResult<Value> {
    let mut reasons = Vec::new();
    if !policy.auto_approval_enabled {
        reasons.push("auto_approval_disabled".to_string());
    }
    if !policy
        .auto_approval_allowed_action_kinds
        .iter()
        .any(|candidate| candidate == kind)
    {
        reasons.push("kind_not_auto_approved".to_string());
    }
    if owner_approval_reason(policy, kind).is_some() {
        reasons.push("high_risk_action_requires_owner_approval".to_string());
    }
    if !auto_approval_subject_allowed(policy, subject_hash, conversation_id) {
        reasons.push("subject_not_auto_approved".to_string());
    }
    let delivery_method = delivery_method_from_payload(payload);
    if !policy
        .auto_approval_allowed_delivery_methods
        .iter()
        .any(|candidate| candidate == &delivery_method)
    {
        reasons.push("delivery_method_not_auto_approved".to_string());
    }
    if !policy.auto_approval_allow_attachments && !staged_files.is_empty() {
        reasons.push("attachments_not_auto_approved".to_string());
    }
    if attachment_bytes > policy.auto_approval_max_attachment_bytes {
        reasons.push("attachment_bytes_exceed_auto_limit".to_string());
    }
    if text_bytes > policy.auto_approval_max_text_bytes {
        reasons.push("text_bytes_exceed_auto_limit".to_string());
    }
    if text_chars > policy.auto_approval_max_text_chars {
        reasons.push("text_chars_exceed_auto_limit".to_string());
    }
    if policy.auto_approval_requires_causal_context
        && is_outbound_action(kind)
        && causal_event_id.is_none()
        && causal_message_id.is_none()
    {
        reasons.push("causal_context_required".to_string());
    }
    if policy.auto_approval_requires_verified_causal_context
        && is_outbound_action(kind)
        && validate_causal_context(
            data_dir,
            policy,
            subject_hash,
            causal_event_id,
            causal_message_id,
            now,
        )
        .is_err()
    {
        reasons.push("verified_causal_context_required".to_string());
    }
    let auto_limits = check_auto_approval_limits(data_dir, policy, agent, subject_hash, kind, now)?;
    if !auto_limits
        .get("allowed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        reasons.push("auto_approval_rate_limit".to_string());
    }
    Ok(json!({
        "enabled": policy.auto_approval_enabled,
        "allowed": reasons.is_empty(),
        "reasons": reasons,
        "delivery_method": delivery_method,
        "limits": auto_limits,
    }))
}

fn auto_approval_subject_allowed(
    policy: &AgentWritePolicy,
    subject_hash: Option<&str>,
    conversation_id: Option<&str>,
) -> bool {
    let Some(subject_hash) = subject_hash else {
        return true;
    };
    if policy
        .auto_approval_allowed_contacts
        .iter()
        .any(|candidate| candidate == subject_hash)
    {
        return true;
    }
    let conversation = conversation_id
        .map(str::to_string)
        .unwrap_or_else(|| crate::agent_policy::conversation_id_for_dest(subject_hash));
    if policy
        .auto_approval_allowed_conversations
        .iter()
        .any(|candidate| candidate == subject_hash || candidate == &conversation)
    {
        return true;
    }
    policy.auto_approval_unknown_contacts == "allow"
}

fn check_auto_approval_limits(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    agent: &str,
    subject_hash: Option<&str>,
    kind: &str,
    now: f64,
) -> CliResult<Value> {
    let records = list_actions_without_expiry(data_dir)?;
    let hour_start = now - 3600.0;
    let day_start = now - 86400.0;
    let mut auto_hour = 0usize;
    let mut auto_day = 0usize;
    let mut auto_subject_hour = 0usize;
    let mut auto_subject_day = 0usize;
    for record in records.iter().filter(|record| record.agent == agent) {
        if !record_auto_approved(record) {
            continue;
        }
        if record.created_at_unix >= hour_start {
            auto_hour += 1;
        }
        if record.created_at_unix >= day_start {
            auto_day += 1;
        }
        if is_message_action(&record.kind)
            && is_message_action(kind)
            && subject_hash.is_some()
            && record.subject_hash.as_deref() == subject_hash
        {
            if record.created_at_unix >= hour_start {
                auto_subject_hour += 1;
            }
            if record.created_at_unix >= day_start {
                auto_subject_day += 1;
            }
        }
    }
    let allowed = auto_hour < policy.auto_approval_max_actions_per_hour
        && auto_day < policy.auto_approval_max_actions_per_day
        && auto_subject_hour < policy.auto_approval_max_messages_per_contact_hour
        && auto_subject_day < policy.auto_approval_max_messages_per_contact_day;
    Ok(json!({
        "allowed": allowed,
        "auto_created_last_hour": auto_hour,
        "auto_created_last_day": auto_day,
        "auto_messages_to_subject_last_hour": auto_subject_hour,
        "auto_messages_to_subject_last_day": auto_subject_day,
        "max_auto_actions_per_hour": policy.auto_approval_max_actions_per_hour,
        "max_auto_actions_per_day": policy.auto_approval_max_actions_per_day,
        "max_auto_messages_per_contact_hour": policy.auto_approval_max_messages_per_contact_hour,
        "max_auto_messages_per_contact_day": policy.auto_approval_max_messages_per_contact_day,
    }))
}

fn record_auto_approved(record: &AgentActionRecord) -> bool {
    !record.policy.approval_required
        || record
            .approval
            .as_ref()
            .is_some_and(|approval| approval.actor == "policy:auto")
}

fn validate_action_guardrails(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    principal: &AgentPrincipal,
    action: &NewAction,
    now: f64,
) -> CliResult<()> {
    validate_action_shape(
        data_dir,
        policy,
        principal,
        &action.kind,
        action.subject_hash.as_deref(),
        &action.payload,
        &action.staged_files,
        action.causal_event_id,
        action.causal_message_id.as_deref(),
        now,
    )
}

fn validate_record_guardrails(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    principal: &AgentPrincipal,
    record: &AgentActionRecord,
    now: f64,
) -> CliResult<()> {
    validate_action_shape(
        data_dir,
        policy,
        principal,
        &record.kind,
        record.subject_hash.as_deref(),
        &record.payload,
        &record.staged_files,
        record_causal_event_id(record),
        record_causal_message_id(record).as_deref(),
        now,
    )
}

fn validate_action_shape(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    principal: &AgentPrincipal,
    kind: &str,
    subject_hash: Option<&str>,
    payload: &Value,
    staged_files: &[StagedFile],
    causal_event_id: Option<u64>,
    causal_message_id: Option<&str>,
    now: f64,
) -> CliResult<()> {
    validate_action_kind(kind)?;
    if policy
        .blocked_action_kinds
        .iter()
        .any(|candidate| candidate == kind)
    {
        return Err(CliError::failed(format!(
            "action kind is blocked by policy: {kind}"
        )));
    }
    match kind {
        "message.attachment" if !policy.allow_message_attachments => {
            return Err(CliError::failed(
                "message attachments are blocked by agent policy",
            ));
        }
        "message.image" if !policy.allow_message_images => {
            return Err(CliError::failed(
                "message images are blocked by agent policy",
            ));
        }
        "message.reaction" if !policy.allow_message_reactions => {
            return Err(CliError::failed(
                "message reactions are blocked by agent policy",
            ));
        }
        "contact.add" | "contact.remove" | "contact.block" | "contact.unblock"
            if !policy.allow_contact_mutations =>
        {
            return Err(CliError::failed(
                "contact mutations are blocked by agent policy",
            ));
        }
        "conversation.mark_read"
        | "conversation.hide"
        | "conversation.unhide"
        | "conversation.delete"
            if !policy.allow_conversation_mutations =>
        {
            return Err(CliError::failed(
                "conversation mutations are blocked by agent policy",
            ));
        }
        "conversation.delete" if !policy.allow_conversation_delete => {
            return Err(CliError::failed(
                "conversation delete is blocked by agent policy",
            ));
        }
        "identity.announce" if !policy.allow_identity_announce => {
            return Err(CliError::failed(
                "identity announce is blocked by agent policy",
            ));
        }
        "network.path_request" if !policy.allow_path_request => {
            return Err(CliError::failed(
                "path requests are blocked by agent policy",
            ));
        }
        _ => {}
    }
    let delivery_method = delivery_method_from_payload(payload);
    if !policy
        .allowed_delivery_methods
        .iter()
        .any(|candidate| candidate == &delivery_method)
    {
        return Err(CliError::failed(format!(
            "delivery method is blocked by policy: {delivery_method}"
        )));
    }
    if delivery_method == "propagated" && !policy.allow_forced_propagated_delivery {
        return Err(CliError::failed(
            "forced propagated delivery is blocked by agent policy",
        ));
    }
    if policy.reject_control_chars {
        for value in payload_text_values(payload) {
            if value
                .chars()
                .any(|ch| ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t')
            {
                return Err(CliError::failed(
                    "payload text contains blocked control characters",
                ));
            }
        }
    }
    for value in payload_text_values(payload) {
        let lower = value.to_ascii_lowercase();
        if let Some(blocked) = policy
            .denied_text_substrings
            .iter()
            .find(|needle| lower.contains(&needle.to_ascii_lowercase()))
        {
            return Err(CliError::failed(format!(
                "payload text contains denied substring: {blocked}"
            )));
        }
    }
    if kind == "network.path_request" {
        let subject = subject_hash.ok_or_else(|| CliError::failed("path request has no hash"))?;
        if !ratspeak_runtime::helpers::validate_hex(subject, 32, 32) {
            return Err(CliError::failed(
                "path request hash must be exactly 32 hex characters",
            ));
        }
        if !policy.allowed_path_request_hashes.is_empty()
            && !policy
                .allowed_path_request_hashes
                .iter()
                .any(|candidate| candidate == subject)
        {
            return Err(CliError::failed(
                "path request hash is not in the policy allowlist",
            ));
        }
        if !policy.allow_unknown_path_requests
            && policy.allowed_path_request_hashes.is_empty()
            && !principal_explicitly_allows_subject(principal, subject)
        {
            return Err(CliError::failed(
                "unknown path requests are blocked by agent policy",
            ));
        }
    }
    if policy.require_causal_context_for_outbound
        && is_outbound_action(kind)
        && causal_event_id.is_none()
        && causal_message_id.is_none()
    {
        return Err(CliError::failed(
            "loop prevention: outbound action requires causal event or message metadata",
        ));
    }
    if policy.require_verified_causal_context && causal_event_id.is_some() {
        validate_causal_context(
            data_dir,
            policy,
            subject_hash,
            causal_event_id,
            causal_message_id,
            now,
        )?;
    }
    if kind == "message.reply"
        && policy.reply_to_must_match_causal_message
        && let Some(causal_message_id) = causal_message_id
    {
        let reply_to_id = payload
            .get("reply_to_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !reply_to_id.is_empty() && reply_to_id != causal_message_id {
            return Err(CliError::failed(
                "reply_to_id must match causal_message_id by policy",
            ));
        }
    }
    if !staged_files.is_empty() && !policy.allow_agent_file_paths {
        return Err(CliError::failed(
            "agent file/image staging is blocked by agent policy",
        ));
    }
    Ok(())
}

fn principal_explicitly_allows_subject(principal: &AgentPrincipal, subject: &str) -> bool {
    if principal
        .allowed_contacts
        .iter()
        .any(|candidate| candidate == subject)
    {
        return true;
    }
    let conversation_id = crate::agent_policy::conversation_id_for_dest(subject);
    principal
        .allowed_conversations
        .iter()
        .any(|candidate| candidate == subject || candidate == &conversation_id)
}

fn validate_causal_context(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    subject_hash: Option<&str>,
    causal_event_id: Option<u64>,
    causal_message_id: Option<&str>,
    now: f64,
) -> CliResult<()> {
    let event_id = causal_event_id
        .ok_or_else(|| CliError::failed("verified causal context requires a causal_event_id"))?;
    let event = event_store::find_event_by_id(data_dir, event_id)?
        .ok_or_else(|| CliError::failed(format!("causal event not found: {event_id}")))?;
    if now - event.created_at_unix > policy.max_causal_age_secs as f64 {
        return Err(CliError::failed(format!(
            "causal event is older than max_causal_age_secs ({})",
            policy.max_causal_age_secs
        )));
    }
    if policy.causal_subject_must_match
        && subject_hash.is_some()
        && event.subject_hash.as_deref() != subject_hash
    {
        return Err(CliError::failed(
            "causal event subject does not match action subject",
        ));
    }
    if policy.causal_event_must_be_inbound {
        let direction = event
            .payload
            .get("direction")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event.event.as_deref() != Some("lxmf_message")
            || matches!(direction, "outbound" | "sent")
        {
            return Err(CliError::failed(
                "causal event must be an inbound message by policy",
            ));
        }
    }
    if let Some(causal_message_id) = causal_message_id {
        let event_message = event
            .message_id
            .as_deref()
            .or_else(|| event.payload.get("id").and_then(Value::as_str));
        if event_message.is_some() && event_message != Some(causal_message_id) {
            return Err(CliError::failed(
                "causal message id does not match causal event",
            ));
        }
    }
    Ok(())
}

fn owner_approval_reason(policy: &AgentWritePolicy, kind: &str) -> Option<&'static str> {
    match kind {
        "message.attachment" | "message.image" if policy.require_owner_approval_for_attachments => {
            Some("attachments_require_owner_approval")
        }
        "identity.announce" | "network.path_request"
            if policy.require_owner_approval_for_network =>
        {
            Some("network_actions_require_owner_approval")
        }
        "contact.add" | "contact.remove" | "contact.block" | "contact.unblock"
            if policy.require_owner_approval_for_contact_mutations =>
        {
            Some("contact_mutations_require_owner_approval")
        }
        "conversation.mark_read"
        | "conversation.hide"
        | "conversation.unhide"
        | "conversation.delete"
            if policy.require_owner_approval_for_conversation_mutations =>
        {
            Some("conversation_mutations_require_owner_approval")
        }
        _ => None,
    }
}

fn set_approval_state(
    data_dir: &Path,
    id: &str,
    state: &str,
    note: Option<String>,
    event: &str,
    outcome: &str,
) -> CliResult<AgentActionRecord> {
    let mut record = read_action(data_dir, id)?;
    let now = unix_now_secs();
    if now >= record.expires_at_unix {
        record.state = STATE_EXPIRED.into();
        record.updated_at_unix = now;
        write_action(data_dir, &record)?;
        return Err(CliError::failed("action has expired"));
    }
    if !matches!(
        record.state.as_str(),
        STATE_PENDING_APPROVAL | STATE_APPROVED | STATE_REJECTED
    ) {
        return Err(CliError::failed(format!(
            "action is not awaiting approval: {}",
            record.state
        )));
    }
    record.state = state.into();
    record.updated_at_unix = now;
    record.approval = Some(ApprovalRecord {
        state: state.into(),
        actor: "owner".into(),
        decided_at_unix: now,
        note,
    });
    write_action(data_dir, &record)?;
    append_audit(
        data_dir,
        Actor::owner(),
        event,
        outcome,
        Some(&record),
        json!({ "kind": record.kind, "state": state }),
        vec!["payload.content".into(), "staged_files.stored_path".into()],
    )?;
    Ok(record)
}

fn validate_payload_limits(
    policy: &AgentWritePolicy,
    text_bytes: usize,
    text_chars: usize,
    attachment_bytes: usize,
    staged_files: &[StagedFile],
) -> CliResult<()> {
    let pending = staged_files
        .iter()
        .map(|staged| PendingStagedFile {
            file_name: &staged.file_name,
            mime: &staged.mime,
            kind: &staged.kind,
            size: staged.size,
        })
        .collect::<Vec<_>>();
    validate_pending_payload_limits(policy, text_bytes, text_chars, attachment_bytes, &pending)
}

fn validate_pending_payload_limits(
    policy: &AgentWritePolicy,
    text_bytes: usize,
    text_chars: usize,
    attachment_bytes: usize,
    pending_files: &[PendingStagedFile<'_>],
) -> CliResult<()> {
    if text_bytes > policy.max_text_bytes {
        return Err(CliError::failed(format!(
            "payload text exceeds max_text_bytes ({})",
            policy.max_text_bytes
        )));
    }
    if text_chars > policy.max_text_chars {
        return Err(CliError::failed(format!(
            "payload text exceeds max_text_chars ({})",
            policy.max_text_chars
        )));
    }
    if attachment_bytes > policy.max_attachment_bytes {
        return Err(CliError::failed(format!(
            "attachments exceed max_attachment_bytes ({})",
            policy.max_attachment_bytes
        )));
    }
    if pending_files.len() > policy.max_attachments_per_action {
        return Err(CliError::failed(format!(
            "attachments exceed max_attachments_per_action ({})",
            policy.max_attachments_per_action
        )));
    }
    for pending in pending_files {
        let safe_name = sanitize_file_name(pending.file_name, pending.mime, pending.kind);
        if safe_name.len() > policy.max_attachment_name_bytes {
            return Err(CliError::failed(format!(
                "attachment filename exceeds max_attachment_name_bytes ({})",
                policy.max_attachment_name_bytes
            )));
        }
        let allowed = policy
            .allowed_attachment_mime_prefixes
            .iter()
            .any(|prefix| pending.mime.starts_with(prefix));
        if !allowed {
            return Err(CliError::failed(format!(
                "attachment MIME type is not allowed: {}",
                pending.mime
            )));
        }
        let denied = policy
            .denied_attachment_mime_prefixes
            .iter()
            .any(|prefix| pending.mime.starts_with(prefix));
        if denied {
            return Err(CliError::failed(format!(
                "attachment MIME type is denied: {}",
                pending.mime
            )));
        }
        if pending.size > policy.max_attachment_bytes {
            return Err(CliError::failed(format!(
                "attachment exceeds max_attachment_bytes ({})",
                policy.max_attachment_bytes
            )));
        }
        if pending.kind == "image" && pending.size > policy.max_image_bytes {
            return Err(CliError::failed(format!(
                "image exceeds max_image_bytes ({})",
                policy.max_image_bytes
            )));
        }
        if pending.kind != "image" && pending.size > policy.max_file_bytes {
            return Err(CliError::failed(format!(
                "file exceeds max_file_bytes ({})",
                policy.max_file_bytes
            )));
        }
    }
    Ok(())
}

fn cleanup_staged_files(staged_files: &[StagedFile]) {
    for staged in staged_files {
        if let Some(dir) = staged.stored_path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn check_rate_limits(
    data_dir: &Path,
    policy: &AgentWritePolicy,
    agent: &str,
    subject_hash: Option<&str>,
    kind: &str,
    causal_event_id: Option<u64>,
    causal_message_id: Option<&str>,
    reaction_message_id: Option<&str>,
    now: f64,
    exclude_id: Option<&str>,
) -> CliResult<Value> {
    let records = list_actions_without_expiry(data_dir)?;
    let hour_start = now - 3600.0;
    let day_start = now - 86400.0;
    let window_start = now - policy.inbound_loop_window_secs as f64;
    let cooldown_start = now - policy.per_contact_cooldown_secs as f64;
    let announce_interval_start = now - policy.min_announce_interval_secs as f64;
    let path_interval_start = now - policy.min_path_request_interval_secs as f64;
    let mut pending = 0usize;
    let mut hour = 0usize;
    let mut day = 0usize;
    let mut same_subject_window = 0usize;
    let mut same_subject_message_hour = 0usize;
    let mut same_subject_message_day = 0usize;
    let mut same_subject_cooldown = false;
    let mut same_causal_event = 0usize;
    let mut same_causal_message = 0usize;
    let mut reactions_hour = 0usize;
    let mut reactions_day = 0usize;
    let mut same_message_reactions = 0usize;
    let mut contact_mutations_hour = 0usize;
    let mut contact_mutations_day = 0usize;
    let mut conversation_mutations_hour = 0usize;
    let mut conversation_mutations_day = 0usize;
    let mut network_actions_hour = 0usize;
    let mut network_actions_day = 0usize;
    let mut announces_hour = 0usize;
    let mut announces_day = 0usize;
    let mut announce_interval_active = false;
    let mut path_requests_hour = 0usize;
    let mut path_requests_day = 0usize;
    let mut path_interval_active = false;
    let requested_reaction_message = reaction_message_id
        .or(causal_message_id)
        .map(str::to_string);
    for record in records.iter().filter(|record| record.agent == agent) {
        if exclude_id == Some(record.id.as_str()) {
            continue;
        }
        if matches!(
            record.state.as_str(),
            STATE_DRAFT | STATE_PENDING_APPROVAL | STATE_APPROVED
        ) {
            pending += 1;
        }
        if record.created_at_unix >= hour_start {
            hour += 1;
        }
        if record.created_at_unix >= day_start {
            day += 1;
        }
        if let Some(subject_hash) = subject_hash
            && record.subject_hash.as_deref() == Some(subject_hash)
            && is_outbound_action(&record.kind)
        {
            if record.created_at_unix >= window_start {
                same_subject_window += 1;
            }
            if record.created_at_unix >= cooldown_start
                && matches!(
                    record.state.as_str(),
                    STATE_DRAFT
                        | STATE_PENDING_APPROVAL
                        | STATE_APPROVED
                        | STATE_EXECUTING
                        | STATE_SENT
                        | STATE_APPLIED
                )
            {
                same_subject_cooldown = true;
            }
            if is_message_action(&record.kind) {
                if record.created_at_unix >= hour_start {
                    same_subject_message_hour += 1;
                }
                if record.created_at_unix >= day_start {
                    same_subject_message_day += 1;
                }
            }
        }
        if is_outbound_action(&record.kind) {
            if let Some(expected_causal_event_id) = causal_event_id
                && record_causal_event_id(&record) == Some(expected_causal_event_id)
            {
                same_causal_event += 1;
            }
            if let Some(expected_causal_message_id) = causal_message_id
                && record_causal_message_id(&record).as_deref() == Some(expected_causal_message_id)
            {
                same_causal_message += 1;
            }
        }
        if is_reaction_action(&record.kind) {
            if record.created_at_unix >= hour_start {
                reactions_hour += 1;
            }
            if record.created_at_unix >= day_start {
                reactions_day += 1;
            }
            if let Some(expected_message) = requested_reaction_message.as_deref()
                && record.payload.get("message_id").and_then(Value::as_str)
                    == Some(expected_message)
            {
                same_message_reactions += 1;
            }
        }
        if is_contact_mutation(&record.kind) {
            if record.created_at_unix >= hour_start {
                contact_mutations_hour += 1;
            }
            if record.created_at_unix >= day_start {
                contact_mutations_day += 1;
            }
        }
        if is_conversation_mutation(&record.kind) {
            if record.created_at_unix >= hour_start {
                conversation_mutations_hour += 1;
            }
            if record.created_at_unix >= day_start {
                conversation_mutations_day += 1;
            }
        }
        if is_network_action(&record.kind) {
            if record.created_at_unix >= hour_start {
                network_actions_hour += 1;
            }
            if record.created_at_unix >= day_start {
                network_actions_day += 1;
            }
        }
        if record.kind == "identity.announce" {
            if record.created_at_unix >= hour_start {
                announces_hour += 1;
            }
            if record.created_at_unix >= day_start {
                announces_day += 1;
            }
            if record.created_at_unix >= announce_interval_start {
                announce_interval_active = true;
            }
        }
        if record.kind == "network.path_request" {
            if record.created_at_unix >= hour_start {
                path_requests_hour += 1;
            }
            if record.created_at_unix >= day_start {
                path_requests_day += 1;
            }
            if record.created_at_unix >= path_interval_start {
                path_interval_active = true;
            }
        }
    }
    if policy.require_causal_context_for_outbound
        && is_outbound_action(kind)
        && causal_event_id.is_none()
        && causal_message_id.is_none()
    {
        return Err(CliError::failed(
            "loop prevention: outbound action requires causal event or message metadata",
        ));
    }
    if pending >= policy.max_pending_actions {
        return Err(CliError::failed(format!(
            "rate limit: max_pending_actions reached ({})",
            policy.max_pending_actions
        )));
    }
    if hour >= policy.max_actions_per_hour {
        return Err(CliError::failed(format!(
            "rate limit: max_actions_per_hour reached ({})",
            policy.max_actions_per_hour
        )));
    }
    if day >= policy.max_actions_per_day {
        return Err(CliError::failed(format!(
            "rate limit: max_actions_per_day reached ({})",
            policy.max_actions_per_day
        )));
    }
    if same_subject_window >= policy.max_outbound_per_contact_window {
        return Err(CliError::failed(format!(
            "loop prevention: max_outbound_per_contact_window reached ({})",
            policy.max_outbound_per_contact_window
        )));
    }
    if same_subject_cooldown && is_outbound_action(kind) {
        return Err(CliError::failed(format!(
            "rate limit: per-contact cooldown active ({}s)",
            policy.per_contact_cooldown_secs
        )));
    }
    if is_message_action(kind) && same_subject_message_hour >= policy.max_messages_per_contact_hour
    {
        return Err(CliError::failed(format!(
            "rate limit: max_messages_per_contact_hour reached ({})",
            policy.max_messages_per_contact_hour
        )));
    }
    if is_message_action(kind) && same_subject_message_day >= policy.max_messages_per_contact_day {
        return Err(CliError::failed(format!(
            "rate limit: max_messages_per_contact_day reached ({})",
            policy.max_messages_per_contact_day
        )));
    }
    if is_reaction_action(kind) && reactions_hour >= policy.max_reactions_per_hour {
        return Err(CliError::failed(format!(
            "rate limit: max_reactions_per_hour reached ({})",
            policy.max_reactions_per_hour
        )));
    }
    if is_reaction_action(kind) && reactions_day >= policy.max_reactions_per_day {
        return Err(CliError::failed(format!(
            "rate limit: max_reactions_per_day reached ({})",
            policy.max_reactions_per_day
        )));
    }
    if is_reaction_action(kind) && same_message_reactions >= policy.max_reactions_per_message {
        return Err(CliError::failed(format!(
            "rate limit: max_reactions_per_message reached ({})",
            policy.max_reactions_per_message
        )));
    }
    if is_contact_mutation(kind) && contact_mutations_hour >= policy.max_contact_mutations_per_hour
    {
        return Err(CliError::failed(format!(
            "rate limit: max_contact_mutations_per_hour reached ({})",
            policy.max_contact_mutations_per_hour
        )));
    }
    if is_contact_mutation(kind) && contact_mutations_day >= policy.max_contact_mutations_per_day {
        return Err(CliError::failed(format!(
            "rate limit: max_contact_mutations_per_day reached ({})",
            policy.max_contact_mutations_per_day
        )));
    }
    if is_conversation_mutation(kind)
        && conversation_mutations_hour >= policy.max_conversation_mutations_per_hour
    {
        return Err(CliError::failed(format!(
            "rate limit: max_conversation_mutations_per_hour reached ({})",
            policy.max_conversation_mutations_per_hour
        )));
    }
    if is_conversation_mutation(kind)
        && conversation_mutations_day >= policy.max_conversation_mutations_per_day
    {
        return Err(CliError::failed(format!(
            "rate limit: max_conversation_mutations_per_day reached ({})",
            policy.max_conversation_mutations_per_day
        )));
    }
    if is_network_action(kind) && network_actions_hour >= policy.max_network_actions_per_hour {
        return Err(CliError::failed(format!(
            "rate limit: max_network_actions_per_hour reached ({})",
            policy.max_network_actions_per_hour
        )));
    }
    if is_network_action(kind) && network_actions_day >= policy.max_network_actions_per_day {
        return Err(CliError::failed(format!(
            "rate limit: max_network_actions_per_day reached ({})",
            policy.max_network_actions_per_day
        )));
    }
    if kind == "identity.announce" && announces_hour >= policy.max_announces_per_hour {
        return Err(CliError::failed(format!(
            "rate limit: max_announces_per_hour reached ({})",
            policy.max_announces_per_hour
        )));
    }
    if kind == "identity.announce" && announces_day >= policy.max_announces_per_day {
        return Err(CliError::failed(format!(
            "rate limit: max_announces_per_day reached ({})",
            policy.max_announces_per_day
        )));
    }
    if kind == "identity.announce" && announce_interval_active {
        return Err(CliError::failed(format!(
            "rate limit: min_announce_interval_secs active ({}s)",
            policy.min_announce_interval_secs
        )));
    }
    if kind == "network.path_request" && path_requests_hour >= policy.max_path_requests_per_hour {
        return Err(CliError::failed(format!(
            "rate limit: max_path_requests_per_hour reached ({})",
            policy.max_path_requests_per_hour
        )));
    }
    if kind == "network.path_request" && path_requests_day >= policy.max_path_requests_per_day {
        return Err(CliError::failed(format!(
            "rate limit: max_path_requests_per_day reached ({})",
            policy.max_path_requests_per_day
        )));
    }
    if kind == "network.path_request" && path_interval_active {
        return Err(CliError::failed(format!(
            "rate limit: min_path_request_interval_secs active ({}s)",
            policy.min_path_request_interval_secs
        )));
    }
    if let Some(causal_event_id) = causal_event_id
        && same_causal_event >= policy.max_actions_per_causal_event
    {
        return Err(CliError::failed(format!(
            "loop prevention: causal event {causal_event_id} already has {} outbound actions",
            policy.max_actions_per_causal_event
        )));
    }
    if let Some(causal_message_id) = causal_message_id
        && same_causal_message >= policy.max_actions_per_causal_message
    {
        return Err(CliError::failed(format!(
            "loop prevention: causal message {causal_message_id} already has {} outbound actions",
            policy.max_actions_per_causal_message
        )));
    }
    let limits = json!({
        "max_pending_actions": policy.max_pending_actions,
        "max_actions_per_hour": policy.max_actions_per_hour,
        "max_actions_per_day": policy.max_actions_per_day,
        "max_messages_per_contact_hour": policy.max_messages_per_contact_hour,
        "max_messages_per_contact_day": policy.max_messages_per_contact_day,
        "max_reactions_per_hour": policy.max_reactions_per_hour,
        "max_reactions_per_day": policy.max_reactions_per_day,
        "max_reactions_per_message": policy.max_reactions_per_message,
        "max_contact_mutations_per_hour": policy.max_contact_mutations_per_hour,
        "max_contact_mutations_per_day": policy.max_contact_mutations_per_day,
        "max_conversation_mutations_per_hour": policy.max_conversation_mutations_per_hour,
        "max_conversation_mutations_per_day": policy.max_conversation_mutations_per_day,
        "max_network_actions_per_hour": policy.max_network_actions_per_hour,
        "max_network_actions_per_day": policy.max_network_actions_per_day,
        "max_announces_per_hour": policy.max_announces_per_hour,
        "max_announces_per_day": policy.max_announces_per_day,
        "min_announce_interval_secs": policy.min_announce_interval_secs,
        "max_path_requests_per_hour": policy.max_path_requests_per_hour,
        "max_path_requests_per_day": policy.max_path_requests_per_day,
        "min_path_request_interval_secs": policy.min_path_request_interval_secs,
        "per_contact_cooldown_secs": policy.per_contact_cooldown_secs,
        "inbound_loop_window_secs": policy.inbound_loop_window_secs,
        "max_outbound_per_contact_window": policy.max_outbound_per_contact_window,
        "require_causal_context_for_outbound": policy.require_causal_context_for_outbound,
        "max_actions_per_causal_event": policy.max_actions_per_causal_event,
        "max_actions_per_causal_message": policy.max_actions_per_causal_message,
    });
    Ok(json!({
        "pending": pending,
        "created_last_hour": hour,
        "created_last_day": day,
        "same_subject_window": same_subject_window,
        "same_subject_messages_last_hour": same_subject_message_hour,
        "same_subject_messages_last_day": same_subject_message_day,
        "same_causal_event": same_causal_event,
        "same_causal_message": same_causal_message,
        "reactions_last_hour": reactions_hour,
        "reactions_last_day": reactions_day,
        "same_message_reactions": same_message_reactions,
        "contact_mutations_last_hour": contact_mutations_hour,
        "contact_mutations_last_day": contact_mutations_day,
        "conversation_mutations_last_hour": conversation_mutations_hour,
        "conversation_mutations_last_day": conversation_mutations_day,
        "network_actions_last_hour": network_actions_hour,
        "network_actions_last_day": network_actions_day,
        "announces_last_hour": announces_hour,
        "announces_last_day": announces_day,
        "path_requests_last_hour": path_requests_hour,
        "path_requests_last_day": path_requests_day,
        "limits": limits
    }))
}

fn list_actions_without_expiry(data_dir: &Path) -> CliResult<Vec<AgentActionRecord>> {
    let dir = action_records_dir(data_dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        records.push(serde_json::from_slice(&bytes)?);
    }
    Ok(records)
}

fn limits_json(policy: &AgentWritePolicy) -> Value {
    json!({
        "policy_revision": policy.policy_revision,
        "max_text_bytes": policy.max_text_bytes,
        "max_text_chars": policy.max_text_chars,
        "max_title_bytes": policy.max_title_bytes,
        "max_title_chars": policy.max_title_chars,
        "max_attachment_bytes": policy.max_attachment_bytes,
        "max_file_bytes": policy.max_file_bytes,
        "max_image_bytes": policy.max_image_bytes,
        "max_attachments_per_action": policy.max_attachments_per_action,
        "max_attachment_name_bytes": policy.max_attachment_name_bytes,
        "allowed_delivery_methods": policy.allowed_delivery_methods,
        "allow_forced_propagated_delivery": policy.allow_forced_propagated_delivery,
        "allow_agent_file_paths": policy.allow_agent_file_paths,
        "allowed_source_roots": policy.allowed_source_roots,
        "allowed_attachment_mime_prefixes": policy.allowed_attachment_mime_prefixes,
        "denied_attachment_mime_prefixes": policy.denied_attachment_mime_prefixes,
        "default_expires_secs": policy.default_expires_secs,
        "max_expires_secs": policy.max_expires_secs,
        "auto_approval_enabled": policy.auto_approval_enabled,
        "auto_approval_allowed_action_kinds": policy.auto_approval_allowed_action_kinds,
        "auto_approval_allowed_delivery_methods": policy.auto_approval_allowed_delivery_methods,
    })
}

fn is_outbound_action(kind: &str) -> bool {
    matches!(
        kind,
        "message.send"
            | "message.reply"
            | "message.attachment"
            | "message.image"
            | "message.reaction"
            | "identity.announce"
            | "network.path_request"
    )
}

fn is_message_action(kind: &str) -> bool {
    matches!(
        kind,
        "message.send" | "message.reply" | "message.attachment" | "message.image"
    )
}

fn is_reaction_action(kind: &str) -> bool {
    kind == "message.reaction"
}

fn is_contact_mutation(kind: &str) -> bool {
    matches!(
        kind,
        "contact.add" | "contact.remove" | "contact.block" | "contact.unblock"
    )
}

fn is_conversation_mutation(kind: &str) -> bool {
    matches!(
        kind,
        "conversation.mark_read"
            | "conversation.hide"
            | "conversation.unhide"
            | "conversation.delete"
    )
}

fn is_network_action(kind: &str) -> bool {
    matches!(kind, "identity.announce" | "network.path_request")
}

fn validate_action_kind(kind: &str) -> CliResult<()> {
    if matches!(
        kind,
        "message.send"
            | "message.reply"
            | "message.attachment"
            | "message.image"
            | "message.reaction"
            | "identity.announce"
            | "network.path_request"
            | "contact.add"
            | "contact.remove"
            | "contact.block"
            | "contact.unblock"
            | "conversation.mark_read"
            | "conversation.hide"
            | "conversation.unhide"
            | "conversation.delete"
    ) {
        Ok(())
    } else {
        Err(CliError::usage(format!(
            "unsupported action kind in policy: {kind}"
        )))
    }
}

fn validate_delivery_method(method: &str) -> CliResult<()> {
    if matches!(method, "auto" | "direct" | "opportunistic" | "propagated") {
        Ok(())
    } else {
        Err(CliError::usage(format!(
            "unsupported delivery method in policy: {method}"
        )))
    }
}

fn delivery_method_from_payload(payload: &Value) -> String {
    payload
        .get("delivery_method")
        .and_then(Value::as_str)
        .unwrap_or("auto")
        .trim()
        .to_ascii_lowercase()
}

fn payload_text_values(payload: &Value) -> Vec<&str> {
    [
        "content",
        "title",
        "reply_to_preview",
        "reason",
        "display_name",
    ]
    .iter()
    .filter_map(|key| payload.get(*key).and_then(Value::as_str))
    .collect()
}

fn payload_message_id(payload: &Value) -> Option<&str> {
    payload.get("message_id").and_then(Value::as_str)
}

fn record_text_counts(record: &AgentActionRecord) -> (usize, usize) {
    let text = payload_text_values(&record.payload);
    let bytes = text.iter().map(|value| value.len()).sum();
    let chars = text.iter().map(|value| value.chars().count()).sum();
    (bytes, chars)
}

fn record_attachment_bytes(record: &AgentActionRecord) -> usize {
    record.staged_files.iter().map(|file| file.size).sum()
}

fn record_causal_event_id(record: &AgentActionRecord) -> Option<u64> {
    record
        .payload
        .get("causal")
        .and_then(|value| value.get("event_id"))
        .and_then(Value::as_u64)
}

fn record_causal_message_id(record: &AgentActionRecord) -> Option<String> {
    record
        .payload
        .get("causal")
        .and_then(|value| value.get("message_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn action_path(data_dir: &Path, id: &str) -> PathBuf {
    action_records_dir(data_dir).join(format!("{id}.json"))
}

fn validate_record_id(id: &str) -> CliResult<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CliError::usage("invalid action id"));
    }
    Ok(())
}

fn write_json_private<T: Serialize>(path: &Path, value: &T) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        restrict_dir_permissions(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    restrict_file_permissions(&tmp)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn restrict_dir_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn next_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mut random = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut random);
    format!("{prefix}_{nanos}_{}", hex::encode(random))
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

fn sanitize_file_name(name: &str, mime: &str, fallback_stem: &str) -> String {
    let mut clean = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '.' || *c == '-' || *c == '_' || *c == ' ')
        .take(200)
        .collect::<String>()
        .trim()
        .to_string();
    if clean.is_empty() {
        clean = fallback_stem.to_string();
    }
    let has_ext = clean
        .rsplit_once('.')
        .map(|(_, ext)| {
            !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .unwrap_or(false);
    if has_ext {
        return clean;
    }
    let ext = extension_for_mime(mime);
    if ext.is_empty() {
        clean
    } else {
        format!("{clean}.{ext}")
    }
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/bmp" => "bmp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "application/json" => "json",
        "application/zip" => "zip",
        _ => "",
    }
}
