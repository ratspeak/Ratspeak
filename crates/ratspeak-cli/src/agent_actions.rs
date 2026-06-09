use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agent_policy::AgentPrincipal;
use crate::error::{CliError, CliResult};

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
    pub format: String,
    pub version: u32,
    pub require_owner_approval: bool,
    pub default_expires_secs: u64,
    pub max_expires_secs: u64,
    pub max_pending_actions: usize,
    pub max_actions_per_hour: usize,
    pub max_actions_per_day: usize,
    pub per_contact_cooldown_secs: u64,
    pub inbound_loop_window_secs: u64,
    pub max_outbound_per_contact_window: usize,
    #[serde(default)]
    pub require_causal_context_for_outbound: bool,
    #[serde(default)]
    pub max_actions_per_causal_event: usize,
    #[serde(default)]
    pub max_actions_per_causal_message: usize,
    pub max_text_bytes: usize,
    pub max_title_bytes: usize,
    pub max_attachment_bytes: usize,
    pub max_attachment_name_bytes: usize,
    pub allow_agent_file_paths: bool,
    pub allowed_attachment_mime_prefixes: Vec<String>,
}

impl Default for AgentWritePolicy {
    fn default() -> Self {
        Self {
            format: WRITE_POLICY_FORMAT.into(),
            version: 1,
            require_owner_approval: true,
            default_expires_secs: 24 * 60 * 60,
            max_expires_secs: 7 * 24 * 60 * 60,
            max_pending_actions: 25,
            max_actions_per_hour: 60,
            max_actions_per_day: 200,
            per_contact_cooldown_secs: 3,
            inbound_loop_window_secs: 10 * 60,
            max_outbound_per_contact_window: 6,
            require_causal_context_for_outbound: false,
            max_actions_per_causal_event: 3,
            max_actions_per_causal_message: 2,
            max_text_bytes: 4096,
            max_title_bytes: 256,
            max_attachment_bytes: rns_protocol::resource::MAX_EFFICIENT_SIZE,
            max_attachment_name_bytes: 200,
            allow_agent_file_paths: true,
            allowed_attachment_mime_prefixes: vec![
                "image/".into(),
                "text/".into(),
                "application/pdf".into(),
                "application/json".into(),
                "application/zip".into(),
                "application/octet-stream".into(),
            ],
        }
    }
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
    let defaults = AgentWritePolicy::default();
    if policy.format.is_empty() {
        policy.format = WRITE_POLICY_FORMAT.into();
    }
    if policy.version == 0 {
        policy.version = 1;
    }
    if policy.max_actions_per_causal_event == 0 {
        policy.max_actions_per_causal_event = defaults.max_actions_per_causal_event;
    }
    if policy.max_actions_per_causal_message == 0 {
        policy.max_actions_per_causal_message = defaults.max_actions_per_causal_message;
    }
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
    let rate_limits = match check_rate_limits(
        data_dir,
        &policy,
        &principal.name,
        action.subject_hash.as_deref(),
        &action.kind,
        action.causal_event_id,
        action.causal_message_id.as_deref(),
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
        action.attachment_bytes,
        &action.staged_files,
    ) {
        cleanup_staged_files(&action.staged_files);
        return Err(error);
    }
    let expires_secs = action
        .expires_secs
        .unwrap_or(policy.default_expires_secs)
        .min(policy.max_expires_secs);
    let state = if action.submit {
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
            scopes_checked: action.required_scopes,
            approval_required: policy.require_owner_approval,
            rate_limits,
            limits: limits_json(&policy),
        },
        approval: None,
        execution: None,
        safety: json!({
            "owner_approval_required": policy.require_owner_approval,
            "prompt_injection_boundary": "message/contact/network payload fields are untrusted until reviewed by the owner",
            "raw_send_disabled_for_agents": true,
            "causal_context": {
                "required_for_outbound": policy.require_causal_context_for_outbound,
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
        if action.submit {
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
    record.policy.rate_limits = check_rate_limits(
        data_dir,
        &policy,
        &principal.name,
        record.subject_hash.as_deref(),
        &record.kind,
        record_causal_event_id(&record),
        record_causal_message_id(&record).as_deref(),
        now,
        Some(&record.id),
    )?;
    record.state = STATE_PENDING_APPROVAL.into();
    record.updated_at_unix = now;
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
    attachment_bytes: usize,
    pending_files: &[PendingStagedFile<'_>],
    _causal_event_id: Option<u64>,
    _causal_message_id: Option<&str>,
) -> CliResult<()> {
    let policy = ensure_write_policy(data_dir)?;
    validate_pending_payload_limits(&policy, text_bytes, attachment_bytes, pending_files)
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
    validate_pending_payload_limits(policy, text_bytes, attachment_bytes, &pending)
}

fn validate_pending_payload_limits(
    policy: &AgentWritePolicy,
    text_bytes: usize,
    attachment_bytes: usize,
    pending_files: &[PendingStagedFile<'_>],
) -> CliResult<()> {
    if text_bytes > policy.max_text_bytes {
        return Err(CliError::failed(format!(
            "payload text exceeds max_text_bytes ({})",
            policy.max_text_bytes
        )));
    }
    if attachment_bytes > policy.max_attachment_bytes {
        return Err(CliError::failed(format!(
            "attachments exceed max_attachment_bytes ({})",
            policy.max_attachment_bytes
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
        if pending.size > policy.max_attachment_bytes {
            return Err(CliError::failed(format!(
                "attachment exceeds max_attachment_bytes ({})",
                policy.max_attachment_bytes
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
    now: f64,
    exclude_id: Option<&str>,
) -> CliResult<Value> {
    let records = list_actions_without_expiry(data_dir)?;
    let hour_start = now - 3600.0;
    let day_start = now - 86400.0;
    let window_start = now - policy.inbound_loop_window_secs as f64;
    let cooldown_start = now - policy.per_contact_cooldown_secs as f64;
    let mut pending = 0usize;
    let mut hour = 0usize;
    let mut day = 0usize;
    let mut same_subject_window = 0usize;
    let mut same_subject_cooldown = false;
    let mut same_causal_event = 0usize;
    let mut same_causal_message = 0usize;
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
    Ok(json!({
        "pending": pending,
        "created_last_hour": hour,
        "created_last_day": day,
        "same_subject_window": same_subject_window,
        "same_causal_event": same_causal_event,
        "same_causal_message": same_causal_message,
        "limits": {
            "max_pending_actions": policy.max_pending_actions,
            "max_actions_per_hour": policy.max_actions_per_hour,
            "max_actions_per_day": policy.max_actions_per_day,
            "per_contact_cooldown_secs": policy.per_contact_cooldown_secs,
            "inbound_loop_window_secs": policy.inbound_loop_window_secs,
            "max_outbound_per_contact_window": policy.max_outbound_per_contact_window,
            "require_causal_context_for_outbound": policy.require_causal_context_for_outbound,
            "max_actions_per_causal_event": policy.max_actions_per_causal_event,
            "max_actions_per_causal_message": policy.max_actions_per_causal_message,
        }
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
        "max_text_bytes": policy.max_text_bytes,
        "max_title_bytes": policy.max_title_bytes,
        "max_attachment_bytes": policy.max_attachment_bytes,
        "max_attachment_name_bytes": policy.max_attachment_name_bytes,
        "default_expires_secs": policy.default_expires_secs,
        "max_expires_secs": policy.max_expires_secs,
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
