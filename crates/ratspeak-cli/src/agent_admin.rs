//! Shared owner-side agent administration.
//!
//! This module keeps the policy/approval/credential behavior single-sourced
//! for both `ratspeakctl` and the desktop Settings UI.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent_actions::{self, Actor};
use crate::agent_policy::{
    AGENT_MANIFEST_FORMAT, AgentAuth, AgentCommandHints, AgentEnforcement, AgentGrant,
    AgentManifest, agent_manifest_path, agent_root_from_owner_data_dir, agent_token_path,
    create_agent_credential, normalize_agent_scopes, push_unique, read_agent_manifest,
    sorted_unique, token_hash, write_agent_credential, write_agent_manifest,
};
use crate::error::{CliError, CliResult};
use crate::profile::{self, Profile};

#[derive(Debug, Clone, Default)]
pub struct AgentCreateOptions {
    pub name: String,
    pub identity_mode: String,
    pub explicit_profile_dir: Option<PathBuf>,
    pub requested_scopes: Vec<String>,
    pub presets: Vec<String>,
    pub allowed_contacts: Vec<String>,
    pub allowed_conversations: Vec<String>,
    pub unknown_contacts: String,
    pub nickname: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentGrantUpdate {
    pub name: String,
    pub scopes: Vec<String>,
    pub presets: Vec<String>,
    pub contacts: Vec<String>,
    pub conversations: Vec<String>,
    pub unknown_contacts: Option<String>,
    pub replace_scopes: bool,
    pub replace_contacts: bool,
    pub replace_conversations: bool,
    pub activate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPolicyPatch {
    #[serde(default)]
    pub policy: Option<agent_actions::AgentWritePolicy>,
    #[serde(default)]
    pub set: Vec<PolicySet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicySet {
    pub key: String,
    pub value: Value,
}

pub fn create_agent(profile: &Profile, mut opts: AgentCreateOptions) -> CliResult<Value> {
    validate_agent_name(&opts.name)?;

    if opts.identity_mode.is_empty() {
        opts.identity_mode = "new".into();
    }
    if opts.identity_mode != "new" {
        return Err(CliError::usage(
            "agent create currently supports only --identity new",
        ));
    }
    if opts.unknown_contacts.is_empty() {
        opts.unknown_contacts = "deny".into();
    }
    if !matches!(opts.unknown_contacts.as_str(), "deny" | "allow") {
        return Err(CliError::usage(
            "--unknown-contacts must be either deny or allow",
        ));
    }
    let allowed_contacts = normalize_contact_grants(opts.allowed_contacts)?;
    let allowed_conversations = normalize_conversation_grants(opts.allowed_conversations)?;

    for preset in &opts.presets {
        opts.requested_scopes.extend(agent_preset_scopes(preset)?);
    }

    let agent_root = opts
        .explicit_profile_dir
        .clone()
        .unwrap_or_else(|| profile.config.data_dir.join("agents").join(&opts.name));
    let agent_manifest_path = agent_manifest_path(&agent_root);
    if agent_manifest_path.exists() {
        return Err(CliError::failed(format!(
            "agent already exists: {}",
            agent_manifest_path.display()
        )));
    }

    let _owner_lock = ratspeak_runtime::profile_lock::try_acquire_profile_lock(
        &profile.config.data_dir,
        "ratspeak owner agent create",
    )
    .map_err(|e| CliError::failed(format!("failed to acquire owner profile lock: {e}")))?;

    let agent_profile = profile::open_profile(agent_root.clone())?;
    let _agent_lock = ratspeak_runtime::profile_lock::try_acquire_profile_lock(
        &agent_profile.config.data_dir,
        "ratspeak owner agent create",
    )
    .map_err(|e| CliError::failed(format!("failed to acquire agent profile lock: {e}")))?;

    let nickname = opts.nickname.as_deref().unwrap_or(&opts.name);
    let created = ratspeak_runtime::identity_service::create_recoverable_identity(
        &agent_profile.config.data_dir,
        &agent_profile.db,
        Some(nickname),
        true,
    )
    .map_err(|e| CliError::failed(format!("failed to create agent identity: {e}")))?;

    let (effective_scopes, pending_scopes, normalized_requested) =
        normalize_agent_scopes(opts.requested_scopes)?;
    let now = unix_now_secs();
    let credential = create_agent_credential(&opts.name, &created.hash, now);
    let token_path = agent_token_path(&agent_root);
    let write_policy = agent_actions::ensure_write_policy(&agent_profile.config.data_dir)?;
    let sorted_contacts = sorted_unique(allowed_contacts);

    let manifest = AgentManifest {
        format: AGENT_MANIFEST_FORMAT.into(),
        version: 1,
        name: opts.name.clone(),
        created_at_unix: now,
        profile_root: agent_root.clone(),
        profile_data_dir: agent_profile.config.data_dir.clone(),
        identity_hash: created.hash.clone(),
        lxmf_hash: created.lxmf_hash.clone(),
        display_name: created.display_name.clone(),
        requested_scopes: normalized_requested,
        effective_scopes: effective_scopes.clone(),
        pending_scopes: pending_scopes.clone(),
        allowed_contacts: sorted_contacts.clone(),
        allowed_conversations: allowed_conversations.clone(),
        unknown_contacts: opts.unknown_contacts.clone(),
        grant: AgentGrant {
            status: "active".into(),
            revision: 1,
            scopes: effective_scopes,
            pending_scopes,
            allowed_contacts: sorted_contacts,
            allowed_conversations,
            unknown_contacts: opts.unknown_contacts,
            updated_at_unix: now,
            revoked_at_unix: None,
            revoke_reason: None,
        },
        auth: AgentAuth {
            token_hash: token_hash(&credential.token),
            token_file: token_path.clone(),
            rotated_at_unix: now,
        },
        enforcement: AgentEnforcement {
            local_daemon_api: true,
            contact_allowlist: true,
            write_actions: true,
            owner_approval: true,
            audit_log: true,
            rate_limits: true,
            note: "ratspeakd enforces daemon API auth, read/write scopes, contact/conversation allowlists, owner approval, audit logging, and profile-local rate limits".into(),
        },
        commands: command_hints_for_agent_root(&agent_root),
    };
    write_agent_manifest(&agent_manifest_path, &manifest)?;
    write_agent_credential(&token_path, &credential)?;
    append_agent_admin_audit(
        &agent_root,
        "grant.created",
        json!({
            "agent": manifest.name,
            "grant_revision": manifest.grant.revision,
            "scopes": manifest.grant.scopes,
            "allowed_contacts": manifest.grant.allowed_contacts,
            "allowed_conversations": manifest.grant.allowed_conversations,
            "unknown_contacts": manifest.grant.unknown_contacts,
        }),
    );

    let created_payload = serde_json::to_value(&created)?;
    Ok(agent_created_payload(
        profile,
        manifest,
        credential,
        write_policy,
        created_payload,
    ))
}

pub fn list_agent_manifests(profile: &Profile) -> CliResult<Vec<Value>> {
    let agents_dir = profile.config.data_dir.join("agents");
    let mut records = Vec::new();
    if agents_dir.is_dir() {
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = agent_manifest_path(&entry.path());
            if let Some(manifest) = read_agent_manifest(&path)? {
                records.push(serde_json::to_value(manifest)?);
            }
        }
    }
    records.sort_by(|a, b| {
        a.get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .cmp(b.get("name").and_then(Value::as_str).unwrap_or(""))
    });
    Ok(records)
}

pub fn list_agent_summaries(profile: &Profile) -> CliResult<Value> {
    let mut agents = Vec::new();
    for manifest_value in list_agent_manifests(profile)? {
        let manifest: AgentManifest = serde_json::from_value(manifest_value)?;
        agents.push(agent_summary(profile, &manifest)?);
    }
    Ok(json!({
        "agents": agents,
        "presets": agent_presets_payload(),
        "policy_defaults": agent_actions::AgentWritePolicy::default(),
        "onboarding": onboarding_contract_payload(profile),
    }))
}

pub fn show_agent_manifest(profile: &Profile, name: &str) -> CliResult<AgentManifest> {
    validate_agent_name(name)?;
    let path = agent_manifest_path(&agent_root_from_owner_data_dir(
        &profile.config.data_dir,
        name,
    ));
    read_agent_manifest(&path)?.ok_or_else(|| CliError::failed(format!("agent not found: {name}")))
}

pub fn show_agent_bundle(profile: &Profile, name: &str) -> CliResult<Value> {
    let manifest = show_agent_manifest(profile, name)?;
    let summary = agent_summary(profile, &manifest)?;
    let policy = show_agent_policy(profile, name)?;
    let audit = list_agent_audit(profile, Some(name), 25)?;
    let approvals = list_agent_approvals(
        profile,
        Some(name),
        Some(agent_actions::STATE_PENDING_APPROVAL),
    )?;
    Ok(json!({
        "agent": manifest,
        "summary": summary,
        "policy": policy["policy"].clone(),
        "policy_file": policy["policy_file"].clone(),
        "approvals": approvals["actions"].clone(),
        "audit": audit["audit"].clone(),
        "connection": connection_bundle(profile, name)?,
    }))
}

pub fn update_agent_grant(profile: &Profile, update: AgentGrantUpdate) -> CliResult<Value> {
    validate_agent_name(&update.name)?;
    let agent_root = agent_root_from_owner_data_dir(&profile.config.data_dir, &update.name);
    let path = agent_manifest_path(&agent_root);
    let mut manifest = read_agent_manifest(&path)?
        .ok_or_else(|| CliError::failed(format!("agent not found: {}", update.name)))?;

    let now = unix_now_secs();
    let mut grant = manifest.effective_grant();
    let mut changed = false;

    if !update.scopes.is_empty() || !update.presets.is_empty() {
        let mut expanded_scopes = update.scopes;
        for preset in update.presets {
            expanded_scopes.extend(agent_preset_scopes(&preset)?);
        }
        let (effective, pending, requested) = normalize_agent_scopes(expanded_scopes)?;
        if update.replace_scopes {
            grant.scopes = effective.clone();
            grant.pending_scopes = pending.clone();
            manifest.requested_scopes = requested;
        } else {
            for scope in effective {
                push_unique(&mut grant.scopes, scope);
            }
            for scope in pending {
                push_unique(&mut grant.pending_scopes, scope);
            }
            for scope in requested {
                push_unique(&mut manifest.requested_scopes, scope);
            }
        }
        changed = true;
    }

    if !update.contacts.is_empty() || update.replace_contacts {
        let normalized_contacts = normalize_contact_grants(update.contacts)?;
        if update.replace_contacts {
            grant.allowed_contacts.clear();
        }
        for contact in normalized_contacts {
            push_unique(&mut grant.allowed_contacts, contact);
        }
        grant.allowed_contacts = sorted_unique(grant.allowed_contacts);
        changed = true;
    }

    if !update.conversations.is_empty() || update.replace_conversations {
        let normalized = normalize_conversation_grants(update.conversations)?;
        if update.replace_conversations {
            grant.allowed_conversations.clear();
        }
        for conversation in normalized {
            push_unique(&mut grant.allowed_conversations, conversation);
        }
        grant.allowed_conversations = sorted_unique(grant.allowed_conversations);
        changed = true;
    }

    if let Some(value) = update.unknown_contacts {
        if !matches!(value.as_str(), "deny" | "allow") {
            return Err(CliError::usage(
                "--unknown-contacts must be either deny or allow",
            ));
        }
        grant.unknown_contacts = value;
        changed = true;
    }

    if update.activate {
        grant.status = "active".into();
        grant.revoked_at_unix = None;
        grant.revoke_reason = None;
        changed = true;
    }

    if changed {
        grant.revision += 1;
        grant.updated_at_unix = now;
        manifest.grant = grant.clone();
        manifest.effective_scopes = grant.scopes.clone();
        manifest.pending_scopes = grant.pending_scopes.clone();
        manifest.allowed_contacts = grant.allowed_contacts.clone();
        manifest.allowed_conversations = grant.allowed_conversations.clone();
        manifest.unknown_contacts = grant.unknown_contacts.clone();
        write_agent_manifest(&path, &manifest)?;
        append_agent_admin_audit(
            &agent_root,
            "grant.updated",
            json!({
                "agent": manifest.name,
                "grant_revision": grant.revision,
                "scopes": grant.scopes,
                "pending_scopes": grant.pending_scopes,
                "allowed_contacts": grant.allowed_contacts,
                "allowed_conversations": grant.allowed_conversations,
                "unknown_contacts": grant.unknown_contacts,
            }),
        );
    }

    Ok(json!({
        "agent": manifest.name,
        "grant": grant,
        "changed": changed,
        "runtime_note": "restart ratspeakd for this agent profile after changing grants",
    }))
}

pub fn revoke_agent(profile: &Profile, name: &str, reason: Option<String>) -> CliResult<Value> {
    validate_agent_name(name)?;
    let agent_root = agent_root_from_owner_data_dir(&profile.config.data_dir, name);
    let path = agent_manifest_path(&agent_root);
    let mut manifest = read_agent_manifest(&path)?
        .ok_or_else(|| CliError::failed(format!("agent not found: {name}")))?;
    let now = unix_now_secs();
    let mut grant = manifest.effective_grant();
    grant.status = "revoked".into();
    grant.revision += 1;
    grant.updated_at_unix = now;
    grant.revoked_at_unix = Some(now);
    grant.revoke_reason = reason;
    manifest.grant = grant.clone();
    write_agent_manifest(&path, &manifest)?;
    append_agent_admin_audit(
        &agent_root,
        "grant.revoked",
        json!({
            "agent": manifest.name,
            "grant_revision": grant.revision,
            "reason": grant.revoke_reason,
        }),
    );

    Ok(json!({
        "agent": manifest.name,
        "grant": grant,
        "runtime_note": "restart ratspeakd for this agent profile after revoking grants",
    }))
}

pub fn rotate_agent_token(profile: &Profile, name: &str) -> CliResult<Value> {
    validate_agent_name(name)?;
    let agent_root = agent_root_from_owner_data_dir(&profile.config.data_dir, name);
    let path = agent_manifest_path(&agent_root);
    let mut manifest = read_agent_manifest(&path)?
        .ok_or_else(|| CliError::failed(format!("agent not found: {name}")))?;
    let previous_manifest = manifest.clone();
    let now = unix_now_secs();
    let credential = create_agent_credential(name, &manifest.identity_hash, now);
    let token_path = agent_token_path(&agent_root);
    manifest.auth = AgentAuth {
        token_hash: token_hash(&credential.token),
        token_file: token_path.clone(),
        rotated_at_unix: now,
    };
    let mut grant = manifest.effective_grant();
    grant.revision += 1;
    grant.updated_at_unix = now;
    manifest.grant = grant.clone();
    write_agent_manifest(&path, &manifest)?;
    if let Err(error) = write_agent_credential(&token_path, &credential) {
        let _ = write_agent_manifest(&path, &previous_manifest);
        return Err(error);
    }
    append_agent_admin_audit(
        &agent_root,
        "token.rotated",
        json!({
            "agent": manifest.name,
            "grant_revision": grant.revision,
            "token_file": token_path,
            "token_hash": token_hash(&credential.token),
        }),
    );

    Ok(json!({
        "agent": manifest.name,
        "credential": {
            "token_file": token_path,
            "token_hash": token_hash(&credential.token),
            "rotated_at_unix": now,
        },
        "grant": grant,
        "runtime_note": "restart ratspeakd for this agent profile after rotating credentials",
    }))
}

pub fn show_agent_policy(profile: &Profile, name: &str) -> CliResult<Value> {
    let (manifest, agent_profile) = open_agent_profile(profile, name)?;
    let policy = agent_actions::ensure_write_policy(&agent_profile.config.data_dir)?;
    Ok(json!({
        "agent": manifest.name,
        "policy": policy,
        "policy_file": agent_actions::write_policy_path(&agent_profile.config.data_dir),
    }))
}

pub fn validate_agent_policy(profile: &Profile, name: &str) -> CliResult<Value> {
    let (manifest, agent_profile) = open_agent_profile(profile, name)?;
    let policy = agent_actions::read_write_policy(&agent_profile.config.data_dir)?;
    agent_actions::validate_write_policy(&policy)?;
    Ok(json!({
        "agent": manifest.name,
        "ok": true,
        "policy_revision": policy.policy_revision,
        "policy_file": agent_actions::write_policy_path(&agent_profile.config.data_dir),
    }))
}

pub fn set_agent_policy(
    profile: &Profile,
    name: &str,
    patch: AgentPolicyPatch,
) -> CliResult<Value> {
    let (manifest, agent_profile) = open_agent_profile(profile, name)?;
    let current = agent_actions::ensure_write_policy(&agent_profile.config.data_dir)?;
    let mut policy = patch.policy.unwrap_or_else(|| current.clone());
    let before = serde_json::to_value(&current)?;

    for set in patch.set {
        apply_policy_value(&mut policy, &set.key, set.value)?;
    }

    let changed = serde_json::to_value(&policy)? != before;
    if changed {
        policy.policy_revision += 1;
        agent_actions::write_write_policy(&agent_profile.config.data_dir, &policy)?;
        append_agent_admin_audit(
            &manifest.profile_root,
            "policy.updated",
            json!({
                "agent": manifest.name,
                "policy_revision": policy.policy_revision,
                "policy_file": agent_actions::write_policy_path(&agent_profile.config.data_dir),
            }),
        );
    }

    Ok(json!({
        "agent": manifest.name,
        "changed": changed,
        "policy": policy,
        "policy_file": agent_actions::write_policy_path(&agent_profile.config.data_dir),
        "runtime_note": "policy changes are read by ratspeakd on the next action create/submit/execute",
    }))
}

pub fn list_agent_approvals(
    profile: &Profile,
    agent: Option<&str>,
    state: Option<&str>,
) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let records = agent_actions::list_actions(&target.config.data_dir, None, state)?
        .into_iter()
        .map(|record| agent_actions::public_action(record, false))
        .collect::<Vec<_>>();
    Ok(json!({ "actions": records }))
}

pub fn show_agent_approval(profile: &Profile, agent: Option<&str>, id: &str) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let record = agent_actions::read_action(&target.config.data_dir, id)?;
    Ok(agent_actions::public_action(record, true))
}

pub fn inspect_agent_staged_file(
    profile: &Profile,
    agent: Option<&str>,
    id: &str,
    file_id: Option<&str>,
    preview_bytes: usize,
) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    agent_actions::inspect_staged_file(&target.config.data_dir, id, file_id, preview_bytes)
}

pub fn approve_agent_action(
    profile: &Profile,
    agent: Option<&str>,
    id: &str,
    note: Option<String>,
    execute: bool,
) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let mut payload = agent_actions::public_action(
        agent_actions::approve_action(&target.config.data_dir, id, note)?,
        true,
    );
    append_owner_action_event(&target.config.data_dir, "agent.action.approved", id);
    if execute {
        payload = crate::daemon_api::request(
            &target.config.data_dir,
            "actions.execute",
            json!({ "id": id }),
        )?
        .ok_or_else(|| {
            CliError::failed(
                "actions.execute requires ratspeakd running for the selected agent profile",
            )
        })?;
    }
    Ok(payload)
}

pub fn reject_agent_action(
    profile: &Profile,
    agent: Option<&str>,
    id: &str,
    note: Option<String>,
) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let record = agent_actions::reject_action(&target.config.data_dir, id, note)?;
    append_owner_action_event(&target.config.data_dir, "agent.action.rejected", id);
    Ok(agent_actions::public_action(record, true))
}

pub fn cancel_agent_action(
    profile: &Profile,
    agent: Option<&str>,
    id: &str,
    note: Option<String>,
) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let record = agent_actions::cancel_action(&target.config.data_dir, id, Actor::owner(), note)?;
    append_owner_action_event(&target.config.data_dir, "agent.action.cancelled", id);
    Ok(agent_actions::public_action(record, true))
}

pub fn execute_agent_action(profile: &Profile, agent: Option<&str>, id: &str) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    crate::daemon_api::request(
        &target.config.data_dir,
        "actions.execute",
        json!({ "id": id }),
    )?
    .ok_or_else(|| {
        CliError::failed(
            "actions.execute requires ratspeakd running for the selected agent profile",
        )
    })
}

pub fn expire_agent_actions(profile: &Profile, agent: Option<&str>) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let expired = agent_actions::expire_due_actions(&target.config.data_dir)?;
    Ok(json!({ "expired": expired }))
}

pub fn list_agent_audit(profile: &Profile, agent: Option<&str>, limit: usize) -> CliResult<Value> {
    let target = approval_target_profile(profile, agent)?;
    let records = agent_actions::list_audit(&target.config.data_dir, limit)?
        .into_iter()
        .map(|record| serde_json::to_value(record).unwrap_or(Value::Null))
        .collect::<Vec<_>>();
    Ok(json!({ "audit": records }))
}

pub fn connection_bundle(profile: &Profile, name: &str) -> CliResult<Value> {
    let manifest = show_agent_manifest(profile, name)?;
    let owner_root_arg = profile.data_root.display().to_string();
    let agent_root_arg = manifest.profile_root.display().to_string();
    Ok(json!({
        "format": "ratspeak.agent-connection.v1",
        "agent": manifest.name,
        "profile_root": manifest.profile_root,
        "profile_data_dir": manifest.profile_data_dir,
        "identity_hash": manifest.identity_hash,
        "lxmf_hash": manifest.lxmf_hash,
        "token_file": manifest.auth.token_file,
        "token_hash": manifest.auth.token_hash,
        "credential": {
            "redacted": true,
            "reason": "local agent processes read the private token file directly; the desktop settings UI exposes token_file and token_hash only"
        },
        "daemon": {
            "start": ["ratspeakd", "--data-dir", agent_root_arg.clone()],
            "endpoint_file": manifest.profile_data_dir.join("ratspeakd-api.json"),
        },
        "cli_contract": {
            "events": ["ratspeakctl", "--data-dir", agent_root_arg.clone(), "--jsonl", "events", "stream"],
            "read_conversation": ["ratspeakctl", "--data-dir", agent_root_arg.clone(), "conversations", "read", "<conversation-id>", "--json"],
            "draft": ["ratspeakctl", "--data-dir", agent_root_arg.clone(), "messages", "draft", "<conversation-id>", "--text", "<text>", "--client-action-id", "<id>"],
            "send": ["ratspeakctl", "--data-dir", agent_root_arg.clone(), "messages", "send", "<action-id>"],
            "owner_approvals": ["ratspeakctl", "--data-dir", owner_root_arg, "approvals", "list", "--agent", name],
        },
        "prompt_injection_boundary": {
            "remote_text_is_untrusted": true,
            "agents_should_treat_message_content_as_data": true,
        }
    }))
}

pub fn redact_agent_private_material(mut payload: Value) -> Value {
    if let Some(identity) = payload.get_mut("identity").and_then(Value::as_object_mut) {
        if identity.remove("mnemonic").is_some() {
            identity.insert("mnemonic_redacted".into(), Value::Bool(true));
            identity.insert(
                "mnemonic_redaction_reason".into(),
                Value::String(
                    "agent recovery material is stored in the agent profile and is not exposed to the desktop settings UI"
                        .into(),
                ),
            );
        }
    }
    payload
}

pub fn agent_presets_payload() -> Value {
    json!({
        "inbox-reader": {
            "label": "Inbox reader",
            "description": "Read identity, contacts, messages, and event stream.",
            "scopes": agent_preset_scopes("inbox-reader").unwrap_or_default(),
        },
        "reply-assistant": {
            "label": "Reply assistant",
            "description": "Read messages/events and draft/send text replies behind policy.",
            "scopes": agent_preset_scopes("reply-assistant").unwrap_or_default(),
        },
        "media-assistant": {
            "label": "Media assistant",
            "description": "Reply assistant plus attachment and image drafts.",
            "scopes": agent_preset_scopes("media-assistant").unwrap_or_default(),
        },
        "network-helper": {
            "label": "Network helper",
            "description": "Network status, announces, and path requests behind policy.",
            "scopes": agent_preset_scopes("network-helper").unwrap_or_default(),
        },
        "openclaw-basic": {
            "label": "OpenClaw basic",
            "description": "OpenClaw-ready reply assistant contract.",
            "scopes": agent_preset_scopes("openclaw-basic").unwrap_or_default(),
        },
    })
}

pub fn validate_agent_name(name: &str) -> CliResult<()> {
    if name.is_empty() || name.len() > 64 {
        return Err(CliError::usage(
            "agent name must be between 1 and 64 characters",
        ));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CliError::usage(
            "agent name may contain only ASCII letters, numbers, '.', '-', and '_'",
        ));
    }
    if name == "." || name == ".." {
        return Err(CliError::usage("agent name cannot be '.' or '..'"));
    }
    Ok(())
}

pub fn agent_preset_scopes(preset: &str) -> CliResult<Vec<String>> {
    let scopes = match preset {
        "inbox-reader" => vec![
            "read:status",
            "read:identity",
            "read:contacts",
            "read:messages",
            "read:events",
        ],
        "reply-assistant" | "openclaw-basic" => vec![
            "read:status",
            "read:identity",
            "read:contacts",
            "read:messages",
            "read:events",
            "read:actions",
            "read:audit",
            "write:drafts",
            "write:messages",
        ],
        "media-assistant" => vec![
            "read:status",
            "read:identity",
            "read:contacts",
            "read:messages",
            "read:events",
            "read:actions",
            "read:audit",
            "write:drafts",
            "write:messages",
            "write:attachments",
            "write:images",
        ],
        "network-helper" => vec![
            "read:status",
            "read:network",
            "read:events",
            "read:actions",
            "write:announces",
            "write:paths",
        ],
        other => {
            return Err(CliError::usage(format!(
                "unsupported agent preset: {other}; expected inbox-reader, reply-assistant, media-assistant, network-helper, or openclaw-basic"
            )));
        }
    };
    Ok(scopes.into_iter().map(str::to_string).collect())
}

pub fn normalize_conversation_grants(values: Vec<String>) -> CliResult<Vec<String>> {
    let mut normalized = Vec::new();
    for value in values {
        let Some(dest_hash) = crate::agent_policy::dest_hash_from_conversation_id(&value) else {
            return Err(CliError::usage(format!(
                "invalid --allow-conversation id or hash: {value}"
            )));
        };
        push_unique(
            &mut normalized,
            crate::agent_policy::conversation_id_for_dest(&dest_hash),
        );
    }
    Ok(sorted_unique(normalized))
}

pub fn normalize_contact_grants(values: Vec<String>) -> CliResult<Vec<String>> {
    let mut normalized = Vec::new();
    for value in values {
        let contact = value.trim().to_ascii_lowercase();
        validate_contact_hash(&contact)?;
        push_unique(&mut normalized, contact);
    }
    Ok(sorted_unique(normalized))
}

pub fn apply_policy_value(
    policy: &mut agent_actions::AgentWritePolicy,
    key: &str,
    value: Value,
) -> CliResult<()> {
    let key = key.trim().replace('-', "_");
    match key.as_str() {
        "require_owner_approval" => policy.require_owner_approval = value_as_bool(&key, &value)?,
        "auto_approval_enabled" | "auto_approval" => {
            policy.auto_approval_enabled = value_as_bool(&key, &value)?;
        }
        "auto_approval_requires_causal_context" => {
            policy.auto_approval_requires_causal_context = value_as_bool(&key, &value)?;
        }
        "auto_approval_requires_verified_causal_context" => {
            policy.auto_approval_requires_verified_causal_context = value_as_bool(&key, &value)?;
        }
        "auto_approval_allow_attachments" => {
            policy.auto_approval_allow_attachments = value_as_bool(&key, &value)?;
        }
        "auto_approval_max_attachment_bytes" | "auto_max_attachment_bytes" => {
            policy.auto_approval_max_attachment_bytes = value_as_usize(&key, &value)?;
        }
        "auto_approval_max_messages_per_contact_hour" | "auto_max_messages_per_contact_hour" => {
            policy.auto_approval_max_messages_per_contact_hour = value_as_usize(&key, &value)?;
        }
        "auto_approval_max_messages_per_contact_day" | "auto_max_messages_per_contact_day" => {
            policy.auto_approval_max_messages_per_contact_day = value_as_usize(&key, &value)?;
        }
        "deny_execute_on_policy_revision_change" => {
            policy.deny_execute_on_policy_revision_change = value_as_bool(&key, &value)?;
        }
        "deny_execute_on_grant_revision_change" => {
            policy.deny_execute_on_grant_revision_change = value_as_bool(&key, &value)?;
        }
        "require_causal_context_for_outbound" | "require_causal_context" => {
            policy.require_causal_context_for_outbound = value_as_bool(&key, &value)?;
        }
        "require_verified_causal_context" => {
            policy.require_verified_causal_context = value_as_bool(&key, &value)?;
        }
        "allow_agent_file_paths" => policy.allow_agent_file_paths = value_as_bool(&key, &value)?,
        "allow_message_attachments" | "allow_attachments" => {
            policy.allow_message_attachments = value_as_bool(&key, &value)?;
        }
        "allow_message_images" | "allow_images" => {
            policy.allow_message_images = value_as_bool(&key, &value)?;
        }
        "allow_message_reactions" | "allow_reactions" => {
            policy.allow_message_reactions = value_as_bool(&key, &value)?;
        }
        "allow_contact_mutations" => {
            policy.allow_contact_mutations = value_as_bool(&key, &value)?;
        }
        "allow_conversation_mutations" => {
            policy.allow_conversation_mutations = value_as_bool(&key, &value)?;
        }
        "allow_conversation_delete" => {
            policy.allow_conversation_delete = value_as_bool(&key, &value)?;
        }
        "allow_identity_announce" => {
            policy.allow_identity_announce = value_as_bool(&key, &value)?;
        }
        "allow_path_request" => policy.allow_path_request = value_as_bool(&key, &value)?,
        "require_owner_approval_for_attachments" => {
            policy.require_owner_approval_for_attachments = value_as_bool(&key, &value)?;
        }
        "require_owner_approval_for_network" => {
            policy.require_owner_approval_for_network = value_as_bool(&key, &value)?;
        }
        "require_owner_approval_for_contact_mutations" => {
            policy.require_owner_approval_for_contact_mutations = value_as_bool(&key, &value)?;
        }
        "require_owner_approval_for_conversation_mutations" => {
            policy.require_owner_approval_for_conversation_mutations = value_as_bool(&key, &value)?;
        }
        "allow_forced_propagated_delivery" => {
            policy.allow_forced_propagated_delivery = value_as_bool(&key, &value)?;
        }
        "allow_static_propagation_nodes_only" | "static_propagation_nodes_only" => {
            policy.allow_static_propagation_nodes_only = value_as_bool(&key, &value)?;
        }
        "allow_unknown_path_requests" => {
            policy.allow_unknown_path_requests = value_as_bool(&key, &value)?;
        }
        "reply_requires_existing_message" => {
            policy.reply_requires_existing_message = value_as_bool(&key, &value)?;
        }
        "reply_to_must_match_causal_message" => {
            policy.reply_to_must_match_causal_message = value_as_bool(&key, &value)?;
        }
        "causal_subject_must_match" => {
            policy.causal_subject_must_match = value_as_bool(&key, &value)?;
        }
        "causal_event_must_be_inbound" => {
            policy.causal_event_must_be_inbound = value_as_bool(&key, &value)?;
        }
        "max_text_chars" => policy.max_text_chars = value_as_usize(&key, &value)?,
        "max_text_bytes" => policy.max_text_bytes = value_as_usize(&key, &value)?,
        "max_title_chars" => policy.max_title_chars = value_as_usize(&key, &value)?,
        "max_title_bytes" => policy.max_title_bytes = value_as_usize(&key, &value)?,
        "max_attachment_bytes" => policy.max_attachment_bytes = value_as_usize(&key, &value)?,
        "max_file_bytes" => policy.max_file_bytes = value_as_usize(&key, &value)?,
        "max_image_bytes" => policy.max_image_bytes = value_as_usize(&key, &value)?,
        "max_attachments_per_action" => {
            policy.max_attachments_per_action = value_as_usize(&key, &value)?;
        }
        "max_attachment_name_bytes" => {
            policy.max_attachment_name_bytes = value_as_usize(&key, &value)?;
        }
        "default_expires_secs" => policy.default_expires_secs = value_as_u64(&key, &value)?,
        "max_expires_secs" => policy.max_expires_secs = value_as_u64(&key, &value)?,
        "max_actions_per_hour" => policy.max_actions_per_hour = value_as_usize(&key, &value)?,
        "max_actions_per_day" => policy.max_actions_per_day = value_as_usize(&key, &value)?,
        "max_pending_actions" => policy.max_pending_actions = value_as_usize(&key, &value)?,
        "per_contact_cooldown_secs" => {
            policy.per_contact_cooldown_secs = value_as_u64(&key, &value)?;
        }
        "inbound_loop_window_secs" => {
            policy.inbound_loop_window_secs = value_as_u64(&key, &value)?;
        }
        "max_outbound_per_contact_window" => {
            policy.max_outbound_per_contact_window = value_as_usize(&key, &value)?;
        }
        "max_causal_age_secs" => policy.max_causal_age_secs = value_as_u64(&key, &value)?,
        "max_actions_per_causal_event" => {
            policy.max_actions_per_causal_event = value_as_usize(&key, &value)?;
        }
        "max_actions_per_causal_message" => {
            policy.max_actions_per_causal_message = value_as_usize(&key, &value)?;
        }
        "max_messages_per_contact_hour" => {
            policy.max_messages_per_contact_hour = value_as_usize(&key, &value)?;
        }
        "max_messages_per_contact_day" => {
            policy.max_messages_per_contact_day = value_as_usize(&key, &value)?;
        }
        "max_reactions_per_hour" => {
            policy.max_reactions_per_hour = value_as_usize(&key, &value)?;
        }
        "max_reactions_per_day" => policy.max_reactions_per_day = value_as_usize(&key, &value)?,
        "max_reactions_per_message" => {
            policy.max_reactions_per_message = value_as_usize(&key, &value)?;
        }
        "max_contact_mutations_per_hour" => {
            policy.max_contact_mutations_per_hour = value_as_usize(&key, &value)?;
        }
        "max_contact_mutations_per_day" => {
            policy.max_contact_mutations_per_day = value_as_usize(&key, &value)?;
        }
        "max_conversation_mutations_per_hour" => {
            policy.max_conversation_mutations_per_hour = value_as_usize(&key, &value)?;
        }
        "max_conversation_mutations_per_day" => {
            policy.max_conversation_mutations_per_day = value_as_usize(&key, &value)?;
        }
        "max_network_actions_per_hour" => {
            policy.max_network_actions_per_hour = value_as_usize(&key, &value)?;
        }
        "max_network_actions_per_day" => {
            policy.max_network_actions_per_day = value_as_usize(&key, &value)?;
        }
        "max_announces_per_hour" => policy.max_announces_per_hour = value_as_usize(&key, &value)?,
        "max_announces_per_day" => policy.max_announces_per_day = value_as_usize(&key, &value)?,
        "min_announce_interval_secs" => {
            policy.min_announce_interval_secs = value_as_u64(&key, &value)?;
        }
        "max_path_requests_per_hour" => {
            policy.max_path_requests_per_hour = value_as_usize(&key, &value)?;
        }
        "max_path_requests_per_day" => {
            policy.max_path_requests_per_day = value_as_usize(&key, &value)?;
        }
        "min_path_request_interval_secs" => {
            policy.min_path_request_interval_secs = value_as_u64(&key, &value)?;
        }
        "auto_approval_max_text_chars" | "auto_max_text_chars" => {
            policy.auto_approval_max_text_chars = value_as_usize(&key, &value)?;
        }
        "auto_approval_max_text_bytes" | "auto_max_text_bytes" => {
            policy.auto_approval_max_text_bytes = value_as_usize(&key, &value)?;
        }
        "auto_approval_unknown_contacts" => {
            policy.auto_approval_unknown_contacts = value_as_string(&key, &value)?;
        }
        "auto_approval_max_actions_per_hour" | "auto_max_actions_per_hour" => {
            policy.auto_approval_max_actions_per_hour = value_as_usize(&key, &value)?;
        }
        "auto_approval_max_actions_per_day" | "auto_max_actions_per_day" => {
            policy.auto_approval_max_actions_per_day = value_as_usize(&key, &value)?;
        }
        "allowed_delivery_methods" => {
            policy.allowed_delivery_methods = value_as_string_list(&key, &value)?;
        }
        "auto_approval_allowed_delivery_methods" => {
            policy.auto_approval_allowed_delivery_methods = value_as_string_list(&key, &value)?;
        }
        "allowed_attachment_mime_prefixes" => {
            policy.allowed_attachment_mime_prefixes = value_as_string_list(&key, &value)?;
        }
        "denied_attachment_mime_prefixes" => {
            policy.denied_attachment_mime_prefixes = value_as_string_list(&key, &value)?;
        }
        "denied_text_substrings" => {
            policy.denied_text_substrings = value_as_string_list(&key, &value)?;
        }
        "reject_control_chars" => policy.reject_control_chars = value_as_bool(&key, &value)?,
        "blocked_action_kinds" => {
            policy.blocked_action_kinds = value_as_string_list(&key, &value)?;
        }
        "auto_approval_allowed_action_kinds" => {
            policy.auto_approval_allowed_action_kinds = value_as_string_list(&key, &value)?;
        }
        "auto_approval_allowed_contacts" => {
            policy.auto_approval_allowed_contacts = value_as_string_list(&key, &value)?;
        }
        "auto_approval_allowed_conversations" => {
            policy.auto_approval_allowed_conversations = value_as_string_list(&key, &value)?;
        }
        "allowed_path_request_hashes" => {
            policy.allowed_path_request_hashes = value_as_string_list(&key, &value)?;
        }
        "allowed_propagation_node_hashes" => {
            policy.allowed_propagation_node_hashes = value_as_string_list(&key, &value)?;
        }
        "allowed_source_roots" => {
            policy.allowed_source_roots = value_as_string_list(&key, &value)?
                .into_iter()
                .map(PathBuf::from)
                .collect();
        }
        other => {
            return Err(CliError::usage(format!(
                "unsupported agent policy key: {other}"
            )));
        }
    }
    Ok(())
}

fn agent_created_payload(
    profile: &Profile,
    manifest: AgentManifest,
    credential: crate::agent_policy::AgentCredential,
    write_policy: agent_actions::AgentWritePolicy,
    created: Value,
) -> Value {
    let owner_root_arg = profile.data_root.display().to_string();
    let agent_root_arg = manifest.profile_root.display().to_string();
    json!({
        "agent": manifest,
        "credential": {
            "format": credential.format,
            "token_file": credential_token_file(&owner_root_arg, &agent_root_arg),
            "token_hash": token_hash(&credential.token),
            "created_at_unix": credential.created_at_unix,
        },
        "identity": created,
        "next": {
            "start_daemon": format!("ratspeakd --data-dir {agent_root_arg}"),
            "inspect": format!("ratspeakctl --data-dir {agent_root_arg} status --pretty"),
            "events": format!("ratspeakctl --data-dir {agent_root_arg} --jsonl events stream"),
            "steps": [
                {
                    "actor": "owner",
                    "purpose": "review pending agent actions",
                    "argv": ["ratspeakctl", "--data-dir", owner_root_arg.clone(), "approvals", "list", "--agent", credential.agent_name.clone()]
                },
                {
                    "actor": "owner",
                    "purpose": "approve an action after review",
                    "argv": ["ratspeakctl", "--data-dir", owner_root_arg, "approvals", "approve", "--agent", credential.agent_name.clone(), "<action-id>"]
                },
                {
                    "actor": "agent-runtime",
                    "purpose": "run the Ratspeak daemon for the agent identity",
                    "argv": ["ratspeakd", "--data-dir", agent_root_arg.clone(), "run"]
                },
                {
                    "actor": "agent",
                    "purpose": "stream grant-filtered events as JSONL",
                    "argv": ["ratspeakctl", "--data-dir", agent_root_arg, "--jsonl", "events", "stream"]
                }
            ],
            "write_policy": write_policy,
            "note": "read scopes, daemon API auth, allowlists, durable events, write proposals, owner approvals, audit, and rate limits are active"
        }
    })
}

fn credential_token_file(_owner_root_arg: &str, agent_root_arg: &str) -> PathBuf {
    PathBuf::from(agent_root_arg)
        .join(".ratspeak")
        .join("agent.token")
}

fn agent_summary(profile: &Profile, manifest: &AgentManifest) -> CliResult<Value> {
    let (_, agent_profile) = open_agent_profile(profile, &manifest.name)?;
    let pending = agent_actions::list_actions(
        &agent_profile.config.data_dir,
        None,
        Some(agent_actions::STATE_PENDING_APPROVAL),
    )?
    .len();
    let approved = agent_actions::list_actions(
        &agent_profile.config.data_dir,
        None,
        Some(agent_actions::STATE_APPROVED),
    )?
    .len();
    let drafts = agent_actions::list_actions(
        &agent_profile.config.data_dir,
        None,
        Some(agent_actions::STATE_DRAFT),
    )?
    .len();
    let policy = agent_actions::ensure_write_policy(&agent_profile.config.data_dir)?;
    Ok(json!({
        "name": manifest.name,
        "display_name": manifest.display_name,
        "identity_hash": manifest.identity_hash,
        "lxmf_hash": manifest.lxmf_hash,
        "status": manifest.effective_grant().status,
        "grant_revision": manifest.effective_grant().revision,
        "profile_root": manifest.profile_root,
        "token_file": manifest.auth.token_file,
        "token_hash": manifest.auth.token_hash,
        "policy_revision": policy.policy_revision,
        "auto_approval_enabled": policy.auto_approval_enabled,
        "require_owner_approval": policy.require_owner_approval,
        "counts": {
            "pending_approval": pending,
            "approved": approved,
            "draft": drafts,
        },
    }))
}

fn onboarding_contract_payload(profile: &Profile) -> Value {
    json!({
        "owner_profile_root": profile.data_root,
        "recommended_flow": [
            "Create or select an agent from Settings > Agents.",
            "Limit the grant to the contacts/conversations the agent should see.",
            "Keep manual approval on until the guardrails match your risk tolerance.",
            "Give the connection bundle to the local agent process or adapter.",
            "Review approvals and audit entries from the same Settings panel."
        ],
        "agent_contract": {
            "events": "ratspeakctl --data-dir <agent-profile> --jsonl events stream",
            "read": "ratspeakctl --data-dir <agent-profile> conversations read <conversation-id> --json",
            "write": "ratspeakctl --data-dir <agent-profile> messages draft|send ...",
            "approval": "owner approves, rejects, cancels, expires, or executes action records",
        }
    })
}

fn command_hints_for_agent_root(agent_root: &Path) -> AgentCommandHints {
    let root = agent_root.display().to_string();
    AgentCommandHints {
        start_daemon: vec![
            "ratspeakd".into(),
            "--data-dir".into(),
            root.clone(),
            "--events-jsonl".into(),
        ],
        status: vec![
            "ratspeakctl".into(),
            "--data-dir".into(),
            root.clone(),
            "status".into(),
        ],
        events_preview: vec![
            "ratspeakd".into(),
            "--data-dir".into(),
            root.clone(),
            "--events-jsonl".into(),
        ],
        events_stream: vec![
            "ratspeakctl".into(),
            "--data-dir".into(),
            root,
            "--jsonl".into(),
            "events".into(),
            "stream".into(),
        ],
    }
}

fn open_agent_profile(profile: &Profile, name: &str) -> CliResult<(AgentManifest, Profile)> {
    let manifest = show_agent_manifest(profile, name)?;
    let agent_profile = profile::open_profile(manifest.profile_root.clone())?;
    Ok((manifest, agent_profile))
}

fn approval_target_profile(profile: &Profile, agent: Option<&str>) -> CliResult<Profile> {
    if let Some(agent_name) = agent {
        validate_agent_name(agent_name)?;
        let (_, agent_profile) = open_agent_profile(profile, agent_name)?;
        Ok(agent_profile)
    } else {
        Ok(profile.clone())
    }
}

fn validate_contact_hash(contact: &str) -> CliResult<()> {
    if !ratspeak_runtime::helpers::validate_hex(contact, 16, 64) {
        return Err(CliError::usage(format!(
            "invalid --allow-contact hash: {contact}"
        )));
    }
    Ok(())
}

fn value_as_bool(name: &str, value: &Value) -> CliResult<bool> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    match value_as_string(name, value)?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => Err(CliError::usage(format!("{name} must be true or false"))),
    }
}

fn value_as_usize(name: &str, value: &Value) -> CliResult<usize> {
    if let Some(value) = value.as_u64() {
        return Ok(value as usize);
    }
    value_as_string(name, value)?
        .trim()
        .parse::<usize>()
        .map_err(|_| CliError::usage(format!("{name} must be an unsigned integer")))
}

fn value_as_u64(name: &str, value: &Value) -> CliResult<u64> {
    if let Some(value) = value.as_u64() {
        return Ok(value);
    }
    value_as_string(name, value)?
        .trim()
        .parse::<u64>()
        .map_err(|_| CliError::usage(format!("{name} must be an unsigned integer")))
}

fn value_as_string(name: &str, value: &Value) -> CliResult<String> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| (!value.is_null()).then(|| value.to_string()))
        .ok_or_else(|| CliError::usage(format!("{name} requires a value")))
}

fn value_as_string_list(name: &str, value: &Value) -> CliResult<Vec<String>> {
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .map(|value| value_as_string(name, value))
            .collect();
    }
    Ok(vec![value_as_string(name, value)?])
}

fn append_owner_action_event(data_dir: &Path, event: &str, action_id: &str) {
    let _ = crate::event_store::EventStore::append_daemon_event(
        data_dir,
        event,
        json!({
            "action_id": action_id,
            "actor": "owner",
        }),
    );
}

pub fn append_agent_admin_audit(agent_root: &Path, event: &str, details: Value) {
    let data_dir = agent_root.join(".ratspeak");
    let _ = agent_actions::append_audit(
        &data_dir,
        Actor::owner(),
        event,
        "ok",
        None,
        details,
        vec!["token".into()],
    );
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
