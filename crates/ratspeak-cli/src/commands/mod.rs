use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::agent_actions::{self, Actor};
use crate::agent_policy::{
    AGENT_MANIFEST_FORMAT, AgentAuth, AgentCommandHints, AgentEnforcement, AgentGrant,
    AgentManifest, agent_manifest_path, agent_root_from_owner_data_dir, agent_token_path,
    create_agent_credential, normalize_agent_scopes, push_unique, read_agent_manifest,
    sorted_unique, token_hash, write_agent_credential, write_agent_manifest,
};
use crate::error::{CliError, CliResult};
use crate::output::{OutputFormat, print_json, print_jsonl};
use crate::profile::{self, Profile};

const PEER_RECENCY_SECS: f64 = 7.0 * 86400.0;

#[derive(Debug, Default)]
struct GlobalOptions {
    data_root: Option<PathBuf>,
    output: OutputFormat,
}

pub async fn run_ctl(args: Vec<String>) -> CliResult<()> {
    let (global, args) = parse_global(args)?;
    if args.is_empty() || is_help(&args) {
        print_ctl_help();
        return Ok(());
    }
    if args[0] == "version" || matches!(args.get(0..2), Some(pair) if pair == ["system", "version"])
    {
        return print_json(&version_payload(), global.output);
    }

    let data_root = profile::resolve_data_root(global.data_root);
    let profile = profile::open_profile(data_root)?;

    match args[0].as_str() {
        "system" => run_system(&profile, &args[1..], global.output),
        "daemon" => run_daemon_tools(&profile, &args[1..], global.output),
        "profile" => run_profile(&profile, &args[1..], global.output),
        "status" => run_status(&profile, &args[1..], global.output),
        "agent" | "agents" => run_agent(&profile, &args[1..], global.output),
        "identity" => run_identity(&profile, &args[1..], global.output),
        "contacts" => run_contacts(&profile, &args[1..], global.output),
        "peers" | "peer" => run_peers(&profile, &args[1..], global.output),
        "conversations" => run_conversations(&profile, &args[1..], global.output).await,
        "messages" => run_messages(&profile, &args[1..], global.output),
        "approvals" | "approval" => run_approvals(&profile, &args[1..], global.output),
        "audit" => run_audit(&profile, &args[1..], global.output),
        "propagation" => run_propagation(&profile, &args[1..], global.output),
        "network" => run_network(&profile, &args[1..], global.output),
        "events" => run_events(&profile, &args[1..], global.output),
        other => Err(CliError::usage(format!(
            "unknown ratspeakctl command: {other}"
        ))),
    }
}

pub async fn run_daemon(args: Vec<String>) -> CliResult<()> {
    let (global, args) = parse_global(args)?;
    if is_help(&args) {
        print_daemon_help();
        return Ok(());
    }

    let mut emit_jsonl = false;
    let mut quiet = false;
    for arg in &args {
        match arg.as_str() {
            "run" => {}
            "--events-jsonl" => emit_jsonl = true,
            "--quiet" => quiet = true,
            other => {
                return Err(CliError::usage(format!(
                    "unknown ratspeakd option: {other}"
                )));
            }
        }
    }

    init_tracing();
    let data_root = profile::resolve_data_root(global.data_root);
    let lock_data_dir = data_root.join(".ratspeak");
    let profile_lock =
        ratspeak_runtime::profile_lock::try_acquire_profile_lock(&lock_data_dir, "ratspeakd")
            .map_err(|e| CliError::failed(format!("failed to acquire profile lock: {e}")))?;
    let state = crate::runtime_host::init_headless_runtime(data_root.clone(), emit_jsonl).await?;
    let api_server = crate::daemon_api::start_server(state.clone()).await?;
    if !quiet {
        eprintln!(
            "ratspeakd running; data_root={}; lock={}; api_endpoint={}; endpoint_file={}; events_jsonl={}",
            data_root.display(),
            profile_lock.path().display(),
            api_server.endpoint_label(),
            api_server.endpoint_path().display(),
            emit_jsonl
        );
    }

    tokio::signal::ctrl_c()
        .await
        .map_err(|e| CliError::failed(format!("failed to wait for shutdown signal: {e}")))?;
    if !quiet {
        eprintln!("ratspeakd shutting down");
    }
    ratspeak_runtime::shutdown_rns_lxmf(&state).await;
    Ok(())
}

fn run_profile(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "show" => print_json(&profile::profile_summary(profile), output),
        other => Err(CliError::usage(format!("unknown profile command: {other}"))),
    }
}

fn run_daemon_tools(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("status") {
        "status" => run_status(profile, &args[1..], output),
        "wait-ready" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let timeout_secs = take_u64_option(&mut rest, "--timeout-secs", 30)?;
            ensure_no_extra_args(&rest, "daemon wait-ready")?;
            let deadline = Instant::now() + Duration::from_secs(timeout_secs);
            loop {
                if let Some(payload) = daemon_read(profile, "status.get", json!({}))? {
                    return print_json(
                        &json!({
                            "ok": true,
                            "ready": true,
                            "status": payload,
                        }),
                        output,
                    );
                }
                if Instant::now() >= deadline {
                    return Err(CliError::failed(
                        "daemon did not become ready before timeout",
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        "methods" | "contract" => {
            ensure_no_extra_args(&args[1..], "daemon methods")?;
            print_json(&daemon_contract_payload(), output)
        }
        other => Err(CliError::usage(format!("unknown daemon command: {other}"))),
    }
}

fn run_system(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("status") {
        "status" => run_status(profile, &args[1..], output),
        "startup" => {
            ensure_no_extra_args(&args[1..], "system startup")?;
            print_json(
                &json!({
                    "stage": "offline",
                    "hw_locked": null,
                    "hw_locked_kind": null,
                }),
                output,
            )
        }
        "setup-status" => {
            ensure_no_extra_args(&args[1..], "system setup-status")?;
            let identities = ratspeak_db::get_all_identities(&profile.db);
            print_json(&json!({ "needs_setup": identities.is_empty() }), output)
        }
        "unread" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let identity = take_option(&mut rest, "--identity")?;
            ensure_no_extra_args(&rest, "system unread")?;
            let identity_id = profile::active_identity_id(profile, identity);
            let senders = unread_breakdown(profile, &identity_id);
            let total: i64 = senders
                .iter()
                .map(|sender| {
                    sender
                        .get("count")
                        .and_then(|value| value.as_i64())
                        .unwrap_or(0)
                })
                .sum();
            if output.jsonl {
                print_jsonl(&senders)
            } else {
                print_json(
                    &json!({
                        "identity_id": identity_id,
                        "total": total,
                        "senders": senders,
                    }),
                    output,
                )
            }
        }
        "db-stats" => {
            ensure_no_extra_args(&args[1..], "system db-stats")?;
            print_json(&ratspeak_db::get_database_stats(&profile.db), output)
        }
        other => Err(CliError::usage(format!("unknown system command: {other}"))),
    }
}

fn run_status(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    if !args.is_empty() {
        return Err(CliError::usage("status does not take positional arguments"));
    }
    if let Some(payload) = daemon_read(profile, "status.get", json!({}))? {
        return print_json(&payload, output);
    }
    let active_identity = ratspeak_db::get_active_identity(&profile.db);
    let identities = ratspeak_db::get_all_identities(&profile.db);
    let startup = "offline";
    print_json(
        &json!({
            "ok": true,
            "mode": "offline",
            "startup_stage": startup,
            "data_root": profile.data_root,
            "data_dir": profile.config.data_dir,
            "db_path": profile.config.db_path(),
            "active_identity": active_identity,
            "identity_count": identities.len(),
            "database": ratspeak_db::get_database_stats(&profile.db),
            "daemon_api": {
                "available": false,
                "expected_endpoint_path": profile.config.data_dir.join("ratspeakd-api.json"),
                "reason": "ratspeakd is not running for this profile"
            }
        }),
        output,
    )
}

fn run_agent(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "onboard" => {
            let mut onboard_args = args.get(1..).unwrap_or_default().to_vec();
            if !onboard_args.iter().any(|arg| arg == "--preset") {
                onboard_args.push("--preset".into());
                onboard_args.push("reply-assistant".into());
            }
            run_agent_create(profile, &onboard_args, output)
        }
        "create" => run_agent_create(profile, &args[1..], output),
        "list" => run_agent_list(profile, &args[1..], output),
        "show" => run_agent_show(profile, &args[1..], output),
        "grant" => run_agent_grant(profile, &args[1..], output),
        "revoke" => run_agent_revoke(profile, &args[1..], output),
        "rotate-token" => run_agent_rotate_token(profile, &args[1..], output),
        other => Err(CliError::usage(format!("unknown agent command: {other}"))),
    }
}

fn run_agent_create(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent create requires <name>"))?;
    validate_agent_name(name)?;

    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let identity_mode = take_option(&mut rest, "--identity")?.unwrap_or_else(|| "new".into());
    if identity_mode != "new" {
        return Err(CliError::usage(
            "agent create currently supports only --identity new",
        ));
    }
    let explicit_profile_dir = take_option(&mut rest, "--profile-dir")?.map(PathBuf::from);
    let mut requested_scopes = take_repeated_option(&mut rest, "--scope")?;
    for preset in take_repeated_option(&mut rest, "--preset")? {
        requested_scopes.extend(agent_preset_scopes(&preset)?);
    }
    let allowed_contacts = take_repeated_option(&mut rest, "--allow-contact")?;
    let allowed_conversations = take_repeated_option(&mut rest, "--allow-conversation")?;
    let unknown_contacts =
        take_option(&mut rest, "--unknown-contacts")?.unwrap_or_else(|| "deny".into());
    let nickname = take_option(&mut rest, "--nickname")?.unwrap_or_else(|| name.to_string());
    ensure_no_extra_args(&rest, "agent create")?;

    for contact in &allowed_contacts {
        if !ratspeak_runtime::helpers::validate_hex(contact, 16, 64) {
            return Err(CliError::usage(format!(
                "invalid --allow-contact hash: {contact}"
            )));
        }
    }
    if !matches!(unknown_contacts.as_str(), "deny" | "allow") {
        return Err(CliError::usage(
            "--unknown-contacts must be either deny or allow",
        ));
    }
    let allowed_conversations = normalize_conversation_grants(allowed_conversations)?;

    let agent_root =
        explicit_profile_dir.unwrap_or_else(|| profile.config.data_dir.join("agents").join(name));
    let agent_manifest_path = agent_manifest_path(&agent_root);
    if agent_manifest_path.exists() {
        return Err(CliError::failed(format!(
            "agent already exists: {}",
            agent_manifest_path.display()
        )));
    }

    let _owner_lock = ratspeak_runtime::profile_lock::try_acquire_profile_lock(
        &profile.config.data_dir,
        "ratspeakctl agent create",
    )
    .map_err(|e| CliError::failed(format!("failed to acquire owner profile lock: {e}")))?;

    let agent_profile = profile::open_profile(agent_root.clone())?;
    let _agent_lock = ratspeak_runtime::profile_lock::try_acquire_profile_lock(
        &agent_profile.config.data_dir,
        "ratspeakctl agent create",
    )
    .map_err(|e| CliError::failed(format!("failed to acquire agent profile lock: {e}")))?;

    let created = ratspeak_runtime::identity_service::create_recoverable_identity(
        &agent_profile.config.data_dir,
        &agent_profile.db,
        Some(&nickname),
        true,
    )
    .map_err(|e| CliError::failed(format!("failed to create agent identity: {e}")))?;

    let (effective_scopes, pending_scopes, normalized_requested) =
        normalize_agent_scopes(requested_scopes)?;
    let now = unix_now_secs();
    let credential = create_agent_credential(name, &created.hash, now);
    let token_path = agent_token_path(&agent_root);
    let write_policy = agent_actions::ensure_write_policy(&agent_profile.config.data_dir)?;

    let manifest = AgentManifest {
        format: AGENT_MANIFEST_FORMAT.into(),
        version: 1,
        name: name.to_string(),
        created_at_unix: now,
        profile_root: agent_root.clone(),
        profile_data_dir: agent_profile.config.data_dir.clone(),
        identity_hash: created.hash.clone(),
        lxmf_hash: created.lxmf_hash.clone(),
        display_name: created.display_name.clone(),
        requested_scopes: normalized_requested,
        effective_scopes: effective_scopes.clone(),
        pending_scopes: pending_scopes.clone(),
        allowed_contacts: sorted_unique(allowed_contacts.clone()),
        allowed_conversations: allowed_conversations.clone(),
        unknown_contacts: unknown_contacts.clone(),
        grant: AgentGrant {
            status: "active".into(),
            revision: 1,
            scopes: effective_scopes,
            pending_scopes,
            allowed_contacts: sorted_unique(allowed_contacts),
            allowed_conversations,
            unknown_contacts,
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
        commands: AgentCommandHints {
            start_daemon: vec![
                "ratspeakd".into(),
                "--data-dir".into(),
                agent_root.display().to_string(),
                "--events-jsonl".into(),
            ],
            status: vec![
                "ratspeakctl".into(),
                "--data-dir".into(),
                agent_root.display().to_string(),
                "status".into(),
            ],
            events_preview: vec![
                "ratspeakd".into(),
                "--data-dir".into(),
                agent_root.display().to_string(),
                "--events-jsonl".into(),
            ],
            events_stream: vec![
                "ratspeakctl".into(),
                "--data-dir".into(),
                agent_root.display().to_string(),
                "--jsonl".into(),
                "events".into(),
                "stream".into(),
            ],
        },
    };
    write_agent_manifest(&agent_manifest_path, &manifest)?;
    write_agent_credential(&token_path, &credential)?;
    append_agent_admin_audit(
        &agent_root,
        "grant.created",
        json!({
            "agent": name,
            "grant_revision": manifest.grant.revision,
            "scopes": manifest.grant.scopes,
            "allowed_contacts": manifest.grant.allowed_contacts,
            "allowed_conversations": manifest.grant.allowed_conversations,
            "unknown_contacts": manifest.grant.unknown_contacts,
        }),
    );

    let owner_root_arg = profile.data_root.display().to_string();
    let agent_root_arg = agent_root.display().to_string();
    let payload = json!({
        "agent": manifest,
        "credential": {
            "format": credential.format,
            "token_file": token_path,
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
                    "argv": ["ratspeakctl", "--data-dir", owner_root_arg.clone(), "approvals", "list", "--agent", name]
                },
                {
                    "actor": "owner",
                    "purpose": "approve an action after review",
                    "argv": ["ratspeakctl", "--data-dir", owner_root_arg.clone(), "approvals", "approve", "--agent", name, "<action-id>"]
                },
                {
                    "actor": "agent-runtime",
                    "purpose": "run the Ratspeak daemon for the agent identity",
                    "argv": ["ratspeakd", "--data-dir", agent_root_arg.clone(), "run"]
                },
                {
                    "actor": "agent",
                    "purpose": "stream grant-filtered events as JSONL",
                    "argv": ["ratspeakctl", "--data-dir", agent_root_arg.clone(), "--jsonl", "events", "stream"]
                }
            ],
            "write_policy": write_policy,
            "note": "read scopes, daemon API auth, allowlists, durable events, write proposals, owner approvals, audit, and rate limits are active"
        }
    });
    print_json(&payload, output)
}

fn run_agent_list(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    ensure_no_extra_args(args, "agent list")?;
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
    if output.jsonl {
        print_jsonl(&records)
    } else {
        print_json(&json!(records), output)
    }
}

fn run_agent_show(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent show requires <name>"))?;
    validate_agent_name(name)?;
    ensure_no_extra_args(&args[1..], "agent show")?;
    let agent_root = profile.config.data_dir.join("agents").join(name);
    let path = agent_manifest_path(&agent_root);
    let manifest = read_agent_manifest(&path)?
        .ok_or_else(|| CliError::failed(format!("agent not found: {name}")))?;
    print_json(&serde_json::to_value(manifest)?, output)
}

fn run_agent_grant(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent grant requires <name>"))?;
    validate_agent_name(name)?;
    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let scopes = take_repeated_option(&mut rest, "--scope")?;
    let contacts = take_repeated_option(&mut rest, "--allow-contact")?;
    let conversations = take_repeated_option(&mut rest, "--allow-conversation")?;
    let presets = take_repeated_option(&mut rest, "--preset")?;
    let unknown_contacts = take_option(&mut rest, "--unknown-contacts")?;
    let replace_scopes = take_flag(&mut rest, "--replace-scopes");
    let replace_contacts = take_flag(&mut rest, "--replace-contacts");
    let replace_conversations = take_flag(&mut rest, "--replace-conversations");
    let activate = take_flag(&mut rest, "--activate");
    ensure_no_extra_args(&rest, "agent grant")?;

    let path = agent_manifest_path(&agent_root_from_owner_data_dir(
        &profile.config.data_dir,
        name,
    ));
    let mut manifest = read_agent_manifest(&path)?
        .ok_or_else(|| CliError::failed(format!("agent not found: {name}")))?;

    let now = unix_now_secs();
    let mut grant = manifest.effective_grant();
    let mut changed = false;

    if !scopes.is_empty() || !presets.is_empty() {
        let mut expanded_scopes = scopes;
        for preset in presets {
            expanded_scopes.extend(agent_preset_scopes(&preset)?);
        }
        let (effective, pending, requested) = normalize_agent_scopes(expanded_scopes)?;
        if replace_scopes {
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

    if !contacts.is_empty() || replace_contacts {
        for contact in &contacts {
            if !ratspeak_runtime::helpers::validate_hex(contact, 16, 64) {
                return Err(CliError::usage(format!(
                    "invalid --allow-contact hash: {contact}"
                )));
            }
        }
        if replace_contacts {
            grant.allowed_contacts = Vec::new();
        }
        for contact in contacts {
            push_unique(&mut grant.allowed_contacts, contact);
        }
        grant.allowed_contacts = sorted_unique(grant.allowed_contacts);
        changed = true;
    }

    if !conversations.is_empty() || replace_conversations {
        let normalized = normalize_conversation_grants(conversations)?;
        if replace_conversations {
            grant.allowed_conversations = Vec::new();
        }
        for conversation in normalized {
            push_unique(&mut grant.allowed_conversations, conversation);
        }
        grant.allowed_conversations = sorted_unique(grant.allowed_conversations);
        changed = true;
    }

    if let Some(value) = unknown_contacts {
        if !matches!(value.as_str(), "deny" | "allow") {
            return Err(CliError::usage(
                "--unknown-contacts must be either deny or allow",
            ));
        }
        grant.unknown_contacts = value;
        changed = true;
    }

    if activate {
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
            &agent_root_from_owner_data_dir(&profile.config.data_dir, name),
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

    print_json(
        &json!({
            "agent": manifest.name,
            "grant": grant,
            "changed": changed,
            "runtime_note": "restart ratspeakd for this agent profile after changing grants",
        }),
        output,
    )
}

fn run_agent_revoke(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent revoke requires <name>"))?;
    validate_agent_name(name)?;
    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let reason = take_option(&mut rest, "--reason")?;
    ensure_no_extra_args(&rest, "agent revoke")?;

    let path = agent_manifest_path(&agent_root_from_owner_data_dir(
        &profile.config.data_dir,
        name,
    ));
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
        &agent_root_from_owner_data_dir(&profile.config.data_dir, name),
        "grant.revoked",
        json!({
            "agent": manifest.name,
            "grant_revision": grant.revision,
            "reason": grant.revoke_reason,
        }),
    );

    print_json(
        &json!({
            "agent": manifest.name,
            "grant": grant,
            "runtime_note": "restart ratspeakd for this agent profile after revoking grants",
        }),
        output,
    )
}

fn run_agent_rotate_token(
    profile: &Profile,
    args: &[String],
    output: OutputFormat,
) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent rotate-token requires <name>"))?;
    validate_agent_name(name)?;
    ensure_no_extra_args(&args[1..], "agent rotate-token")?;

    let agent_root = agent_root_from_owner_data_dir(&profile.config.data_dir, name);
    let path = agent_manifest_path(&agent_root);
    let mut manifest = read_agent_manifest(&path)?
        .ok_or_else(|| CliError::failed(format!("agent not found: {name}")))?;
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
    write_agent_credential(&token_path, &credential)?;
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

    print_json(
        &json!({
            "agent": manifest.name,
            "credential": {
                "token_file": token_path,
                "token_hash": token_hash(&credential.token),
                "rotated_at_unix": now,
            },
            "grant": grant,
            "runtime_note": "restart ratspeakd for this agent profile after rotating credentials",
        }),
        output,
    )
}

fn run_identity(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("get") {
        "get" => {
            if args.len() > 1 {
                return Err(CliError::usage(
                    "identity get does not take positional arguments",
                ));
            }
            if let Some(payload) = daemon_read(profile, "identity.current", json!({}))? {
                return print_json(&payload, output);
            }
            let active = ratspeak_db::get_active_identity(&profile.db);
            print_json(
                &json!({
                    "exists": active.is_some(),
                    "identity": active,
                }),
                output,
            )
        }
        "current" => {
            if args.len() > 1 {
                return Err(CliError::usage(
                    "identity current does not take positional arguments",
                ));
            }
            if let Some(payload) = daemon_read(profile, "identity.current", json!({}))? {
                return print_json(&payload, output);
            }
            let active = ratspeak_db::get_active_identity(&profile.db);
            print_json(
                &json!({
                    "exists": active.is_some(),
                    "identity": active,
                }),
                output,
            )
        }
        "list" => {
            if let Some(payload) = daemon_read(profile, "identity.list", json!({}))? {
                return print_json_or_jsonl_array(&payload, output);
            }
            let records = ratspeak_db::get_all_identities(&profile.db);
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(&json!(records), output)
            }
        }
        "create" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let nickname =
                take_option(&mut rest, "--nickname")?.or(take_option(&mut rest, "--display-name")?);
            let activate = take_flag(&mut rest, "--activate");
            ensure_no_extra_args(&rest, "identity create")?;
            let _profile_lock = ratspeak_runtime::profile_lock::try_acquire_profile_lock(
                &profile.config.data_dir,
                "ratspeakctl identity create",
            )
            .map_err(|e| CliError::failed(format!("failed to acquire profile lock: {e}")))?;
            let created = ratspeak_runtime::identity_service::create_recoverable_identity(
                &profile.config.data_dir,
                &profile.db,
                nickname.as_deref(),
                activate,
            )
            .map_err(|e| CliError::failed(format!("failed to create identity: {e}")))?;
            let mut payload = serde_json::to_value(created)?;
            if let Some(obj) = payload.as_object_mut() {
                obj.insert(
                    "runtime_note".to_string(),
                    json!("restart ratspeakd or the Tauri app if this profile is already running"),
                );
            }
            print_json(&payload, output)
        }
        "activate" => {
            let hash = args
                .get(1)
                .ok_or_else(|| CliError::usage("identity activate requires <hash>"))?;
            ensure_no_extra_args(&args[2..], "identity activate")?;
            if !ratspeak_runtime::helpers::validate_hex(hash, 16, 128) {
                return Err(CliError::usage("invalid identity hash"));
            }
            let _profile_lock = ratspeak_runtime::profile_lock::try_acquire_profile_lock(
                &profile.config.data_dir,
                "ratspeakctl identity activate",
            )
            .map_err(|e| CliError::failed(format!("failed to acquire profile lock: {e}")))?;
            let activated = ratspeak_runtime::identity_service::activate_identity(
                &profile.config.data_dir,
                &profile.db,
                hash,
            )
            .map_err(|e| CliError::failed(format!("failed to activate identity: {e}")))?;
            print_json(&serde_json::to_value(activated)?, output)
        }
        other => Err(CliError::usage(format!(
            "unknown identity command: {other}"
        ))),
    }
}

fn run_contacts(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or("list");
    let mut rest = args.get(1..).unwrap_or_default().to_vec();

    match subcommand {
        "list" => {
            let identity = take_option(&mut rest, "--identity")?;
            ensure_no_extra_args(&rest, "contacts list")?;
            let identity_id = profile::active_identity_id(profile, identity);
            if let Some(payload) =
                daemon_read(profile, "contacts.list", json!({ "identity": identity_id }))?
            {
                return print_json_or_jsonl_field(&payload, output, "contacts");
            }
            let records = ratspeak_db::get_all_contacts(&profile.db, &identity_id);
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(
                    &json!({
                        "identity_id": identity_id,
                        "contacts": records,
                    }),
                    output,
                )
            }
        }
        "blocked" => {
            let identity = take_option(&mut rest, "--identity")?;
            ensure_no_extra_args(&rest, "contacts blocked")?;
            let identity_id = profile::active_identity_id(profile, identity);
            if let Some(payload) = daemon_read(
                profile,
                "contacts.blocked",
                json!({ "identity": identity_id }),
            )? {
                return print_json_or_jsonl_field(&payload, output, "blocked");
            }
            let records = ratspeak_db::get_blocked_contacts(&profile.db, &identity_id);
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(
                    &json!({
                        "identity_id": identity_id,
                        "blocked": records,
                    }),
                    output,
                )
            }
        }
        "add" => {
            let dest_hash = rest
                .first()
                .ok_or_else(|| CliError::usage("contacts add requires <dest-hash>"))?
                .to_string();
            rest.remove(0);
            let display_name =
                take_option(&mut rest, "--display-name")?.or(take_option(&mut rest, "--name")?);
            let trust = take_option(&mut rest, "--trust")?.unwrap_or_else(|| "trusted".into());
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            ensure_no_extra_args(&rest, "contacts add")?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": "contact.add",
                    "dest_hash": dest_hash,
                    "display_name": display_name,
                    "trust": trust,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": true,
                }),
            )?;
            print_json(&payload, output)
        }
        "remove" | "delete" => {
            let dest_hash = rest
                .first()
                .ok_or_else(|| CliError::usage("contacts remove requires <dest-hash>"))?
                .to_string();
            rest.remove(0);
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            ensure_no_extra_args(&rest, "contacts remove")?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": "contact.remove",
                    "dest_hash": dest_hash,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": true,
                }),
            )?;
            print_json(&payload, output)
        }
        "block" | "unblock" => {
            let dest_hash = rest
                .first()
                .ok_or_else(|| {
                    CliError::usage(format!("contacts {subcommand} requires <dest-hash>"))
                })?
                .to_string();
            rest.remove(0);
            let display_name =
                take_option(&mut rest, "--display-name")?.or(take_option(&mut rest, "--name")?);
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            ensure_no_extra_args(&rest, &format!("contacts {subcommand}"))?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": if subcommand == "block" { "contact.block" } else { "contact.unblock" },
                    "dest_hash": dest_hash,
                    "display_name": display_name,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": true,
                }),
            )?;
            print_json(&payload, output)
        }
        other => Err(CliError::usage(format!(
            "unknown contacts command: {other}"
        ))),
    }
}

fn run_peers(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or("list");
    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let identity = take_option(&mut rest, "--identity")?;
    let recency_secs = take_f64_option(&mut rest, "--recency-secs", PEER_RECENCY_SECS)?;
    ensure_no_extra_args(&rest, "peers")?;
    let identity_id = profile::active_identity_id(profile, identity);

    match subcommand {
        "list" => {
            if let Some(payload) = daemon_read(
                profile,
                "peers.list",
                json!({ "identity": identity_id, "recency_secs": recency_secs }),
            )? {
                return print_json_or_jsonl_field(&payload, output, "peers");
            }
            let cutoff = unix_now_secs() - recency_secs;
            let records: Vec<Value> =
                ratspeak_db::get_peers_snapshot(&profile.db, cutoff, &identity_id)
                    .into_iter()
                    .map(peer_to_json)
                    .collect();
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(
                    &json!({
                        "identity_id": identity_id,
                        "recency_secs": recency_secs,
                        "peers": records,
                    }),
                    output,
                )
            }
        }
        other => Err(CliError::usage(format!("unknown peers command: {other}"))),
    }
}

async fn run_conversations(
    profile: &Profile,
    args: &[String],
    output: OutputFormat,
) -> CliResult<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or("list");
    match subcommand {
        "list" => {
            if args.len() > 1 {
                return Err(CliError::usage(
                    "conversations list does not take positional arguments",
                ));
            }
            if let Some(payload) = daemon_read(profile, "conversations.list", json!({}))? {
                return print_json_or_jsonl_array(&payload, output);
            }
            let state = profile::offline_state(profile);
            let payload = ratspeak_runtime::messaging::build_conversations_payload(&state)
                .await
                .ok_or_else(|| CliError::failed("database temporarily unavailable"))?;
            if output.jsonl {
                print_array_as_jsonl(&payload)
            } else {
                print_json(&payload, output)
            }
        }
        "read" => {
            let conversation_id = args
                .get(1)
                .ok_or_else(|| CliError::usage("conversations read requires <conversation-id>"))?
                .to_string();
            let mut rest = args.get(2..).unwrap_or_default().to_vec();
            let identity = take_option(&mut rest, "--identity")?;
            let limit = take_limit(&mut rest, 100)?;
            ensure_no_extra_args(&rest, "conversations read")?;
            let identity_id = profile::active_identity_id(profile, identity);
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation_id)
                .ok_or_else(|| CliError::usage("invalid conversation id"))?;
            if let Some(payload) = daemon_read(
                profile,
                "conversations.read",
                json!({
                    "identity": identity_id,
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "limit": limit,
                }),
            )? {
                return print_json(&payload, output);
            }
            let records =
                ratspeak_db::get_conversation(&profile.db, &dest_hash, &identity_id, limit);
            print_json(
                &json!({
                    "identity_id": identity_id,
                    "conversation": {
                        "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                        "peer_hash": dest_hash,
                    },
                    "messages": records,
                }),
                output,
            )
        }
        "mark-read" | "hide" | "unhide" | "delete" => {
            let conversation = args
                .get(1)
                .ok_or_else(|| {
                    CliError::usage(format!(
                        "conversations {subcommand} requires <conversation-id>"
                    ))
                })?
                .to_string();
            let mut rest = args.get(2..).unwrap_or_default().to_vec();
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            ensure_no_extra_args(&rest, &format!("conversations {subcommand}"))?;
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation)
                .ok_or_else(|| CliError::usage("invalid conversation id or destination hash"))?;
            let kind = match subcommand {
                "mark-read" => "conversation.mark_read",
                "hide" => "conversation.hide",
                "unhide" => "conversation.unhide",
                "delete" => "conversation.delete",
                _ => unreachable!(),
            };
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": kind,
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": true,
                }),
            )?;
            print_json(&payload, output)
        }
        other => Err(CliError::usage(format!(
            "unknown conversations command: {other}"
        ))),
    }
}

fn run_messages(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err(CliError::usage(
            "messages requires a subcommand: list or search",
        ));
    };
    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let identity = take_option(&mut rest, "--identity")?;
    let limit = take_limit(&mut rest, 100)?;
    let identity_id = profile::active_identity_id(profile, identity);

    match subcommand {
        "draft" => {
            let conversation = rest
                .first()
                .ok_or_else(|| CliError::usage("messages draft requires <conversation-id>"))?
                .to_string();
            rest.remove(0);
            let text = take_option(&mut rest, "--text")?
                .or(take_option(&mut rest, "--content")?)
                .ok_or_else(|| CliError::usage("messages draft requires --text"))?;
            let title = take_option(&mut rest, "--title")?;
            let delivery_method =
                take_option(&mut rest, "--delivery-method")?.unwrap_or_else(|| "auto".into());
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            let submit = take_flag(&mut rest, "--submit");
            ensure_no_extra_args(&rest, "messages draft")?;
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation)
                .ok_or_else(|| CliError::usage("invalid conversation id or destination hash"))?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": "message.send",
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "text": text,
                    "title": title,
                    "delivery_method": delivery_method,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": submit,
                }),
            )?;
            print_json(&payload, output)
        }
        "send" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("messages send requires <action-id>"))?
                .to_string();
            ensure_no_extra_args(&rest[1..], "messages send")?;
            let mut payload =
                daemon_required(profile, "actions.submit", json!({ "id": id.clone() }))?;
            if payload.get("state").and_then(Value::as_str) == Some("approved") {
                payload = daemon_required(profile, "actions.execute", json!({ "id": id }))?;
            }
            print_json(&payload, output)
        }
        "reply" => {
            let conversation = rest
                .first()
                .ok_or_else(|| CliError::usage("messages reply requires <conversation-id>"))?
                .to_string();
            rest.remove(0);
            let reply_to_id = take_option(&mut rest, "--reply-to")?
                .ok_or_else(|| CliError::usage("messages reply requires --reply-to"))?;
            let text = take_option(&mut rest, "--text")?
                .or(take_option(&mut rest, "--content")?)
                .ok_or_else(|| CliError::usage("messages reply requires --text"))?;
            let reply_to_preview = take_option(&mut rest, "--reply-preview")?;
            let delivery_method =
                take_option(&mut rest, "--delivery-method")?.unwrap_or_else(|| "auto".into());
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            let submit = take_flag(&mut rest, "--submit");
            ensure_no_extra_args(&rest, "messages reply")?;
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation)
                .ok_or_else(|| CliError::usage("invalid conversation id or destination hash"))?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": "message.reply",
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "text": text,
                    "reply_to_id": reply_to_id,
                    "reply_to_preview": reply_to_preview,
                    "delivery_method": delivery_method,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": submit,
                }),
            )?;
            print_json(&payload, output)
        }
        "send-file" | "send-image" => {
            let is_image = subcommand == "send-image";
            let conversation = rest
                .first()
                .ok_or_else(|| {
                    CliError::usage(format!("messages {subcommand} requires <conversation-id>"))
                })?
                .to_string();
            rest.remove(0);
            let file = take_option(&mut rest, "--file")?
                .or(take_option(&mut rest, "--path")?)
                .ok_or_else(|| CliError::usage(format!("messages {subcommand} requires --file")))?;
            let name = take_option(&mut rest, "--name")?.or_else(|| {
                PathBuf::from(&file)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
            });
            let mime = take_option(&mut rest, "--mime")?.unwrap_or_else(|| {
                if is_image {
                    "image/png".into()
                } else {
                    "application/octet-stream".into()
                }
            });
            let text = take_option(&mut rest, "--text")?.unwrap_or_default();
            let delivery_method =
                take_option(&mut rest, "--delivery-method")?.unwrap_or_else(|| "auto".into());
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            let submit = take_flag(&mut rest, "--submit");
            ensure_no_extra_args(&rest, &format!("messages {subcommand}"))?;
            let bytes = std::fs::read(&file)?;
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation)
                .ok_or_else(|| CliError::usage("invalid conversation id or destination hash"))?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": if is_image { "message.image" } else { "message.attachment" },
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "text": text,
                    "file_name": name,
                    "mime": mime,
                    "file_data_b64": B64.encode(bytes),
                    "delivery_method": delivery_method,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": submit,
                }),
            )?;
            print_json(&payload, output)
        }
        "react" => {
            let conversation = rest
                .first()
                .ok_or_else(|| CliError::usage("messages react requires <conversation-id>"))?
                .to_string();
            rest.remove(0);
            let message_id = take_option(&mut rest, "--message-id")?
                .ok_or_else(|| CliError::usage("messages react requires --message-id"))?;
            let emoji = take_option(&mut rest, "--emoji")?
                .ok_or_else(|| CliError::usage("messages react requires --emoji"))?;
            let remove = take_flag(&mut rest, "--remove");
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            ensure_no_extra_args(&rest, "messages react")?;
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation)
                .ok_or_else(|| CliError::usage("invalid conversation id or destination hash"))?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": "message.reaction",
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "message_id": message_id,
                    "emoji": emoji,
                    "action": if remove { "remove" } else { "add" },
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": true,
                }),
            )?;
            print_json(&payload, output)
        }
        "actions" => run_actions(profile, &rest, output),
        "list" => {
            let conversation = rest
                .first()
                .ok_or_else(|| CliError::usage("messages list requires <conversation-id>"))?
                .to_string();
            ensure_no_extra_args(&rest[1..], "messages list")?;
            let dest_hash = crate::agent_policy::dest_hash_from_conversation_id(&conversation)
                .ok_or_else(|| CliError::usage("invalid conversation id or destination hash"))?;
            if let Some(payload) = daemon_read(
                profile,
                "messages.list",
                json!({
                    "identity": identity_id,
                    "conversation_id": crate::agent_policy::conversation_id_for_dest(&dest_hash),
                    "limit": limit,
                }),
            )? {
                return print_json_or_jsonl_field(&payload, output, "messages");
            }
            let records =
                ratspeak_db::get_conversation(&profile.db, &dest_hash, &identity_id, limit);
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(
                    &json!({
                        "identity_id": identity_id,
                        "dest_hash": dest_hash,
                        "messages": records,
                    }),
                    output,
                )
            }
        }
        "search" => {
            let query = rest
                .first()
                .ok_or_else(|| CliError::usage("messages search requires <query>"))?
                .to_string();
            ensure_no_extra_args(&rest[1..], "messages search")?;
            if query.trim().len() < 2 {
                return Err(CliError::usage(
                    "messages search query must be at least 2 characters",
                ));
            }
            if let Some(payload) = daemon_read(
                profile,
                "messages.search",
                json!({ "identity": identity_id, "query": query, "limit": limit }),
            )? {
                return print_json_or_jsonl_field(&payload, output, "messages");
            }
            let records = ratspeak_db::search_messages(&profile.db, &query, &identity_id, limit);
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(
                    &json!({
                        "identity_id": identity_id,
                        "query": query,
                        "messages": records,
                    }),
                    output,
                )
            }
        }
        other => Err(CliError::usage(format!(
            "unknown messages command: {other}"
        ))),
    }
}

fn run_propagation(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("status") {
        "status" => {
            ensure_no_extra_args(&args[1..], "propagation status")?;
            if let Some(payload) = daemon_read(profile, "propagation.status", json!({}))? {
                return print_json(&payload, output);
            }
            let state = profile::offline_state(profile);
            print_json(
                &ratspeak_runtime::propagation::get_status_payload(&state),
                output,
            )
        }
        other => Err(CliError::usage(format!(
            "unknown propagation command: {other}"
        ))),
    }
}

fn run_network(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let state = profile::offline_state(profile);
    match args.first().map(String::as_str).unwrap_or("status") {
        "status" => {
            ensure_no_extra_args(&args[1..], "network status")?;
            if let Some(payload) = daemon_read(profile, "network.status", json!({}))? {
                return print_json(&payload, output);
            }
            let last_stats = state.last_stats.read().ok().and_then(|stats| stats.clone());
            print_json(
                &json!({
                    "mode": "offline",
                    "last_stats": last_stats,
                    "propagation": ratspeak_runtime::propagation::get_status_payload(&state),
                }),
                output,
            )
        }
        "alerts" => {
            ensure_no_extra_args(&args[1..], "network alerts")?;
            let records = state
                .alerts
                .lock()
                .map(|alerts| alerts.clone())
                .unwrap_or_default();
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(&json!(records), output)
            }
        }
        "announces" => {
            ensure_no_extra_args(&args[1..], "network announces")?;
            let records: Vec<Value> = state
                .announce_history
                .read()
                .map(|announces| announces.values().cloned().collect())
                .unwrap_or_default();
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(&json!(records), output)
            }
        }
        "announce" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let reason = take_option(&mut rest, "--reason")?;
            let client_action_id = take_option(&mut rest, "--client-action-id")?;
            let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
            let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
            let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
            ensure_no_extra_args(&rest, "network announce")?;
            let payload = daemon_required(
                profile,
                "actions.create",
                json!({
                    "kind": "identity.announce",
                    "reason": reason,
                    "client_action_id": client_action_id,
                    "causal_event_id": causal_event_id,
                    "causal_message_id": causal_message_id,
                    "expires_secs": expires_secs,
                    "submit": true,
                }),
            )?;
            print_json(&payload, output)
        }
        "path" => {
            let action = args.get(1).map(String::as_str).unwrap_or("request");
            match action {
                "request" => {
                    let hash = args
                        .get(2)
                        .ok_or_else(|| CliError::usage("network path request requires <hash>"))?;
                    let mut rest = args.get(3..).unwrap_or_default().to_vec();
                    let client_action_id = take_option(&mut rest, "--client-action-id")?;
                    let causal_event_id = take_u64_option_opt(&mut rest, "--causal-event-id")?;
                    let causal_message_id = take_option(&mut rest, "--causal-message-id")?;
                    let expires_secs = take_u64_option_opt(&mut rest, "--expires-secs")?;
                    ensure_no_extra_args(&rest, "network path request")?;
                    let payload = daemon_required(
                        profile,
                        "actions.create",
                        json!({
                            "kind": "network.path_request",
                            "hash": hash,
                            "client_action_id": client_action_id,
                            "causal_event_id": causal_event_id,
                            "causal_message_id": causal_message_id,
                            "expires_secs": expires_secs,
                            "submit": true,
                        }),
                    )?;
                    print_json(&payload, output)
                }
                other => Err(CliError::usage(format!(
                    "unknown network path command: {other}"
                ))),
            }
        }
        other => Err(CliError::usage(format!("unknown network command: {other}"))),
    }
}

fn run_events(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("stream") {
        "stream" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let agent = take_option(&mut rest, "--agent")?;
            let cursor = take_u64_option(&mut rest, "--cursor", 0)?;
            let limit = take_usize_option(&mut rest, "--limit", 100)?;
            let once = take_flag(&mut rest, "--once");
            let wait_ms = take_u64_option(&mut rest, "--wait-ms", if once { 0 } else { 30_000 })?;
            ensure_no_extra_args(&rest, "events stream")?;

            let target_profile = if let Some(agent_name) = agent {
                validate_agent_name(&agent_name)?;
                let agent_root =
                    agent_root_from_owner_data_dir(&profile.config.data_dir, &agent_name);
                profile::open_profile(agent_root)?
            } else {
                profile.clone()
            };

            let mut after_id = cursor;
            loop {
                let Some(payload) = daemon_read(
                    &target_profile,
                    "events.read",
                    json!({
                        "after_id": after_id,
                        "limit": limit,
                        "wait_ms": wait_ms,
                    }),
                )?
                else {
                    return Err(CliError::failed(
                        "events stream requires ratspeakd running for the selected profile",
                    ));
                };
                let events = payload
                    .get("events")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if output.jsonl || !once {
                    print_jsonl(&events)?;
                } else {
                    print_json(&payload, output)?;
                }
                after_id = payload
                    .get("next_cursor")
                    .and_then(Value::as_u64)
                    .unwrap_or(after_id);
                if once {
                    return Ok(());
                }
            }
        }
        other => Err(CliError::usage(format!("unknown events command: {other}"))),
    }
}

fn run_actions(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let state = take_option(&mut rest, "--state")?;
            ensure_no_extra_args(&rest, "messages actions list")?;
            let payload = daemon_required(
                profile,
                "actions.list",
                json!({
                    "state": state,
                }),
            )?;
            print_json_or_jsonl_field(&payload, output, "actions")
        }
        "show" | "read" => {
            let id = args
                .get(1)
                .ok_or_else(|| CliError::usage("messages actions show requires <action-id>"))?;
            ensure_no_extra_args(&args[2..], "messages actions show")?;
            let payload = daemon_required(profile, "actions.read", json!({ "id": id }))?;
            print_json(&payload, output)
        }
        "cancel" => {
            let id = args
                .get(1)
                .ok_or_else(|| CliError::usage("messages actions cancel requires <action-id>"))?;
            let mut rest = args.get(2..).unwrap_or_default().to_vec();
            let note = take_option(&mut rest, "--note")?;
            ensure_no_extra_args(&rest, "messages actions cancel")?;
            let payload =
                daemon_required(profile, "actions.cancel", json!({ "id": id, "note": note }))?;
            print_json(&payload, output)
        }
        other => Err(CliError::usage(format!(
            "unknown messages actions command: {other}"
        ))),
    }
}

fn run_approvals(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or("list");
    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let agent = take_option(&mut rest, "--agent")?;
    let target = approval_target_profile(profile, agent.as_deref())?;
    match subcommand {
        "list" => {
            let state = take_option(&mut rest, "--state")?
                .unwrap_or_else(|| agent_actions::STATE_PENDING_APPROVAL.into());
            ensure_no_extra_args(&rest, "approvals list")?;
            let records = agent_actions::list_actions(&target.config.data_dir, None, Some(&state))?
                .into_iter()
                .map(|record| agent_actions::public_action(record, false))
                .collect::<Vec<_>>();
            if output.jsonl {
                print_jsonl(&records)
            } else {
                print_json(&json!({ "actions": records }), output)
            }
        }
        "show" | "read" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals show requires <action-id>"))?;
            ensure_no_extra_args(&rest[1..], "approvals show")?;
            let record = agent_actions::read_action(&target.config.data_dir, id)?;
            print_json(&agent_actions::public_action(record, true), output)
        }
        "inspect-file" | "file" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals inspect-file requires <action-id>"))?
                .to_string();
            rest.remove(0);
            let file_id = take_option(&mut rest, "--file-id")?;
            let preview_bytes = take_usize_option(&mut rest, "--preview-bytes", 1000)?;
            ensure_no_extra_args(&rest, "approvals inspect-file")?;
            let payload = agent_actions::inspect_staged_file(
                &target.config.data_dir,
                &id,
                file_id.as_deref(),
                preview_bytes,
            )?;
            print_json(&payload, output)
        }
        "approve" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals approve requires <action-id>"))?
                .to_string();
            rest.remove(0);
            let note = take_option(&mut rest, "--note")?;
            let execute = take_flag(&mut rest, "--execute");
            ensure_no_extra_args(&rest, "approvals approve")?;
            let mut payload = agent_actions::public_action(
                agent_actions::approve_action(&target.config.data_dir, &id, note)?,
                true,
            );
            append_owner_action_event(&target.config.data_dir, "agent.action.approved", &id);
            if execute {
                payload = daemon_required(&target, "actions.execute", json!({ "id": id }))?;
            }
            print_json(&payload, output)
        }
        "reject" | "deny" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals reject requires <action-id>"))?
                .to_string();
            rest.remove(0);
            let note = take_option(&mut rest, "--note")?;
            ensure_no_extra_args(&rest, "approvals reject")?;
            let record = agent_actions::reject_action(&target.config.data_dir, &id, note)?;
            append_owner_action_event(&target.config.data_dir, "agent.action.rejected", &id);
            print_json(&agent_actions::public_action(record, true), output)
        }
        "cancel" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals cancel requires <action-id>"))?
                .to_string();
            rest.remove(0);
            let note = take_option(&mut rest, "--note")?;
            ensure_no_extra_args(&rest, "approvals cancel")?;
            let record =
                agent_actions::cancel_action(&target.config.data_dir, &id, Actor::owner(), note)?;
            append_owner_action_event(&target.config.data_dir, "agent.action.cancelled", &id);
            print_json(&agent_actions::public_action(record, true), output)
        }
        "execute" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals execute requires <action-id>"))?;
            ensure_no_extra_args(&rest[1..], "approvals execute")?;
            let payload = daemon_required(&target, "actions.execute", json!({ "id": id }))?;
            print_json(&payload, output)
        }
        "expire" => {
            ensure_no_extra_args(&rest, "approvals expire")?;
            let expired = agent_actions::expire_due_actions(&target.config.data_dir)?;
            print_json(&json!({ "expired": expired }), output)
        }
        other => Err(CliError::usage(format!(
            "unknown approvals command: {other}"
        ))),
    }
}

fn run_audit(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let agent = take_option(&mut rest, "--agent")?;
            let limit = take_usize_option(&mut rest, "--limit", 100)?;
            ensure_no_extra_args(&rest, "audit list")?;
            if let Some(agent_name) = agent {
                let target = approval_target_profile(profile, Some(&agent_name))?;
                let records = agent_actions::list_audit(&target.config.data_dir, limit)?
                    .into_iter()
                    .map(|record| serde_json::to_value(record).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                if output.jsonl {
                    print_jsonl(&records)
                } else {
                    print_json(&json!({ "audit": records }), output)
                }
            } else if let Some(payload) =
                daemon_read(profile, "audit.list", json!({ "limit": limit }))?
            {
                print_json_or_jsonl_field(&payload, output, "audit")
            } else {
                let records = agent_actions::list_audit(&profile.config.data_dir, limit)?
                    .into_iter()
                    .map(|record| serde_json::to_value(record).unwrap_or(Value::Null))
                    .collect::<Vec<_>>();
                if output.jsonl {
                    print_jsonl(&records)
                } else {
                    print_json(&json!({ "audit": records }), output)
                }
            }
        }
        other => Err(CliError::usage(format!("unknown audit command: {other}"))),
    }
}

fn version_payload() -> Value {
    json!({
        "name": "Ratspeak",
        "cli_crate": "ratspeak-cli",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": {
            "ratspeakctl": "profile inspection plus approval-gated agent read/write actions",
            "ratspeakd": "headless runtime owner with optional JSONL event emission"
        }
    })
}

fn daemon_contract_payload() -> Value {
    json!({
        "ok": true,
        "contract": {
            "name": "ratspeak-agent-cli",
            "version": 1,
            "transport": "local profile daemon API discovered through .ratspeak/ratspeakd-api.json",
            "error_envelope": {
                "ok": false,
                "error": { "code": "stable-code", "message": "human-readable text" },
                "exit_code": 1
            }
        },
        "cli": {
            "onboard": "ratspeakctl agent onboard NAME --preset reply-assistant --allow-contact HASH",
            "daemon_ready": "ratspeakctl daemon wait-ready --timeout-secs 30",
            "events": "ratspeakctl --jsonl events stream",
            "read_conversation": "ratspeakctl conversations read lxmf:<hash>",
            "draft": "ratspeakctl messages draft lxmf:<hash> --text TEXT --client-action-id ID",
            "submit_or_execute": "ratspeakctl messages send <action-id>",
            "actions": "ratspeakctl messages actions list|show|cancel",
            "approvals": "ratspeakctl approvals list|show|inspect-file|approve|reject|cancel|execute --agent NAME"
        },
        "presets": {
            "inbox-reader": agent_preset_scopes("inbox-reader").unwrap_or_default(),
            "reply-assistant": agent_preset_scopes("reply-assistant").unwrap_or_default(),
            "media-assistant": agent_preset_scopes("media-assistant").unwrap_or_default(),
            "network-helper": agent_preset_scopes("network-helper").unwrap_or_default(),
            "openclaw-basic": agent_preset_scopes("openclaw-basic").unwrap_or_default()
        },
        "daemon_methods": [
            {"method": "status.get", "scope": "status:read"},
            {"method": "identity.current", "scope": "identity:read"},
            {"method": "identity.list", "scope": "identity:read"},
            {"method": "contacts.list", "scope": "contacts:read"},
            {"method": "contacts.blocked", "scope": "contacts:read"},
            {"method": "peers.list", "scope": "network:read"},
            {"method": "conversations.list", "scope": "messages:read"},
            {"method": "conversations.read", "scope": "messages:read + allowed contact/conversation"},
            {"method": "messages.list", "scope": "messages:read + allowed contact/conversation"},
            {"method": "messages.search", "scope": "messages:read"},
            {"method": "events.read", "scope": "events:read"},
            {"method": "actions.create", "scope": "action kind dependent"},
            {"method": "actions.submit", "scope": "action kind dependent"},
            {"method": "actions.list", "scope": "actions:read or write scope"},
            {"method": "actions.read", "scope": "actions:read or write scope"},
            {"method": "actions.cancel", "scope": "actions:read or write scope"},
            {"method": "actions.execute", "scope": "action kind dependent"},
            {"method": "audit.list", "scope": "audit:read"},
            {"method": "propagation.status", "scope": "network:read"},
            {"method": "network.status", "scope": "network:read"}
        ],
        "action_kinds": [
            {"kind": "message.send", "create": ["drafts:write"], "submit_execute": ["messages:write"]},
            {"kind": "message.reply", "create": ["drafts:write"], "submit_execute": ["messages:write"]},
            {"kind": "message.attachment", "create": ["drafts:write", "attachments:write"], "submit_execute": ["messages:write", "attachments:write"]},
            {"kind": "message.image", "create": ["drafts:write", "images:write"], "submit_execute": ["messages:write", "images:write"]},
            {"kind": "message.reaction", "scopes": ["reactions:write"]},
            {"kind": "identity.announce", "scopes": ["announces:write or network:write"]},
            {"kind": "network.path_request", "scopes": ["paths:write or network:write"]},
            {"kind": "contact.add/remove/block/unblock", "scopes": ["contacts:write"]},
            {"kind": "conversation.mark_read/hide/unhide/delete", "scopes": ["conversations:write"]}
        ],
        "bot_requirements": {
            "use_client_action_id": true,
            "client_action_id": "required for safe retries; reuse only with identical payload",
            "causal_metadata": "send --causal-event-id and/or --causal-message-id when reacting to inbound events",
            "prompt_injection_boundary": "remote text appears under fields with {text, untrusted:true}"
        }
    })
}

fn parse_global(args: Vec<String>) -> CliResult<(GlobalOptions, Vec<String>)> {
    let mut global = GlobalOptions::default();
    let mut rest = Vec::new();
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--data-dir" | "--profile" => {
                let value = iter
                    .next()
                    .ok_or_else(|| CliError::usage(format!("{arg} requires a path")))?;
                global.data_root = Some(PathBuf::from(value));
            }
            "--pretty" => global.output.pretty = true,
            "--jsonl" => global.output.jsonl = true,
            "--json" => {}
            _ => rest.push(arg),
        }
    }
    Ok((global, rest))
}

fn take_option(args: &mut Vec<String>, name: &str) -> CliResult<Option<String>> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(None);
    };
    args.remove(index);
    if index >= args.len() {
        return Err(CliError::usage(format!("{name} requires a value")));
    }
    Ok(Some(args.remove(index)))
}

fn take_repeated_option(args: &mut Vec<String>, name: &str) -> CliResult<Vec<String>> {
    let mut values = Vec::new();
    while let Some(index) = args.iter().position(|arg| arg == name) {
        args.remove(index);
        if index >= args.len() {
            return Err(CliError::usage(format!("{name} requires a value")));
        }
        values.push(args.remove(index));
    }
    Ok(values)
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return false;
    };
    args.remove(index);
    true
}

fn take_limit(args: &mut Vec<String>, default_limit: i64) -> CliResult<i64> {
    let Some(index) = args.iter().position(|arg| arg == "--limit") else {
        return Ok(default_limit);
    };
    args.remove(index);
    if index >= args.len() {
        return Err(CliError::usage("--limit requires a value"));
    }
    let value = args.remove(index);
    let parsed = value
        .parse::<i64>()
        .map_err(|_| CliError::usage("--limit must be an integer"))?;
    if !(1..=1000).contains(&parsed) {
        return Err(CliError::usage("--limit must be between 1 and 1000"));
    }
    Ok(parsed)
}

fn take_u64_option(args: &mut Vec<String>, name: &str, default_value: u64) -> CliResult<u64> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(default_value);
    };
    args.remove(index);
    if index >= args.len() {
        return Err(CliError::usage(format!("{name} requires a value")));
    }
    args.remove(index)
        .parse::<u64>()
        .map_err(|_| CliError::usage(format!("{name} must be an unsigned integer")))
}

fn take_u64_option_opt(args: &mut Vec<String>, name: &str) -> CliResult<Option<u64>> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(None);
    };
    args.remove(index);
    if index >= args.len() {
        return Err(CliError::usage(format!("{name} requires a value")));
    }
    let value = args.remove(index);
    Ok(Some(value.parse::<u64>().map_err(|_| {
        CliError::usage(format!("{name} must be an unsigned integer"))
    })?))
}

fn take_usize_option(args: &mut Vec<String>, name: &str, default_value: usize) -> CliResult<usize> {
    let parsed = take_u64_option(args, name, default_value as u64)?;
    if !(1..=1000).contains(&parsed) {
        return Err(CliError::usage(format!(
            "{name} must be between 1 and 1000"
        )));
    }
    Ok(parsed as usize)
}

fn take_f64_option(args: &mut Vec<String>, name: &str, default_value: f64) -> CliResult<f64> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(default_value);
    };
    args.remove(index);
    if index >= args.len() {
        return Err(CliError::usage(format!("{name} requires a value")));
    }
    let value = args.remove(index);
    let parsed = value
        .parse::<f64>()
        .map_err(|_| CliError::usage(format!("{name} must be a number")))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        return Err(CliError::usage(format!("{name} must be positive")));
    }
    Ok(parsed)
}

fn ensure_no_extra_args(args: &[String], context: &str) -> CliResult<()> {
    if let Some(extra) = args.first() {
        return Err(CliError::usage(format!(
            "unexpected argument for {context}: {extra}"
        )));
    }
    Ok(())
}

fn is_help(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("help" | "-h" | "--help")
    )
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn validate_agent_name(name: &str) -> CliResult<()> {
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

fn agent_preset_scopes(preset: &str) -> CliResult<Vec<String>> {
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

fn normalize_conversation_grants(values: Vec<String>) -> CliResult<Vec<String>> {
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

fn unread_breakdown(profile: &Profile, identity_id: &str) -> Vec<Value> {
    ratspeak_db::get_unread_breakdown(&profile.db, identity_id)
        .into_iter()
        .map(|(hash, display_name, count, preview, timestamp)| {
            json!({
                "hash": hash,
                "display_name": display_name,
                "count": count,
                "preview": preview,
                "timestamp": timestamp,
            })
        })
        .collect()
}

fn peer_to_json(row: ratspeak_db::PeerRow) -> Value {
    json!({
        "hash": row.hash,
        "identity_hash": row.identity_hash,
        "telephony_hash": ratspeak_runtime::telephony_hash_for_identity_hex(&row.identity_hash),
        "last_seen": row.last_seen,
        "first_seen": row.first_seen,
        "display_name": row.display_name,
        "profile_status": row.profile_status,
        "is_contact": row.is_contact,
        "last_interface": row.last_interface,
        "services": row.services,
    })
}

fn print_array_as_jsonl(value: &Value) -> CliResult<()> {
    let records = value
        .as_array()
        .ok_or_else(|| CliError::failed("expected array payload for JSONL output"))?;
    print_jsonl(records)
}

fn daemon_read(profile: &Profile, method: &str, params: Value) -> CliResult<Option<Value>> {
    crate::daemon_api::request(&profile.config.data_dir, method, params)
}

fn daemon_required(profile: &Profile, method: &str, params: Value) -> CliResult<Value> {
    daemon_read(profile, method, params)?.ok_or_else(|| {
        CliError::failed(format!(
            "{method} requires ratspeakd running for the selected profile"
        ))
    })
}

fn approval_target_profile(profile: &Profile, agent: Option<&str>) -> CliResult<Profile> {
    if let Some(agent_name) = agent {
        validate_agent_name(agent_name)?;
        let agent_root = agent_root_from_owner_data_dir(&profile.config.data_dir, agent_name);
        profile::open_profile(agent_root)
    } else {
        Ok(profile.clone())
    }
}

fn append_owner_action_event(data_dir: &std::path::Path, event: &str, action_id: &str) {
    let _ = crate::event_store::EventStore::append_daemon_event(
        data_dir,
        event,
        json!({
            "action_id": action_id,
            "actor": "owner",
        }),
    );
}

fn append_agent_admin_audit(agent_root: &Path, event: &str, details: Value) {
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

fn print_json_or_jsonl_array(value: &Value, output: OutputFormat) -> CliResult<()> {
    if output.jsonl {
        print_array_as_jsonl(value)
    } else {
        print_json(value, output)
    }
}

fn print_json_or_jsonl_field(value: &Value, output: OutputFormat, field: &str) -> CliResult<()> {
    if output.jsonl {
        let records = value
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| CliError::failed(format!("expected array field for JSONL: {field}")))?;
        print_jsonl(records)
    } else {
        print_json(value, output)
    }
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn print_ctl_help() {
    println!(
        "\
ratspeakctl [--data-dir PATH] [--pretty] [--jsonl] <command>

Ratspeak CLI commands:
  version
  system status
  system startup
  system setup-status
  system unread [--identity HASH]
  system db-stats
  daemon wait-ready [--timeout-secs N]
  daemon methods
  profile show
  status
  agent onboard NAME [--preset PRESET] [--allow-contact HASH]
  agent create NAME [--identity new] [--scope SCOPE] [--allow-contact HASH]
  agent list
  agent show NAME
  agent grant NAME [--scope SCOPE] [--allow-contact HASH] [--allow-conversation ID]
  agent revoke NAME [--reason TEXT]
  agent rotate-token NAME
  identity get
  identity current
  identity list
  identity create [--nickname NAME] [--activate]
  identity activate HASH
  contacts list [--identity HASH]
  contacts blocked [--identity HASH]
  contacts add <dest-hash> [--display-name NAME]
  contacts remove <dest-hash>
  contacts block <dest-hash> [--display-name NAME]
  contacts unblock <dest-hash>
  peers list [--identity HASH] [--recency-secs N]
  conversations list
  conversations read <conversation-id> [--identity HASH] [--limit N]
  conversations mark-read <conversation-id>
  conversations hide <conversation-id>
  conversations unhide <conversation-id>
  conversations delete <conversation-id>
  messages list <conversation-id> [--identity HASH] [--limit N]
  messages search <query> [--identity HASH] [--limit N]
  messages draft <conversation-id> --text TEXT [--submit] [--client-action-id ID]
  messages send <action-id>
  messages reply <conversation-id> --reply-to MSG --text TEXT [--submit] [--client-action-id ID]
  messages send-file <conversation-id> --file PATH [--mime MIME]
  messages send-image <conversation-id> --file PATH [--mime MIME]
  messages react <conversation-id> --message-id MSG --emoji EMOJI
  messages actions list|show|cancel
  approvals list|show|inspect-file|approve|reject|cancel|execute --agent NAME
  audit list [--agent NAME] [--limit N]
  events stream [--agent NAME] [--cursor N] [--limit N] [--once]
  propagation status
  network status
  network alerts
  network announces
  network announce
  network path request <hash>

State commands emit JSON by default. Use --pretty for formatted JSON, or
--jsonl to stream list-like records one JSON object per line.
Set RATSPEAK_DATA_DIR or --data-dir to target a specific Ratspeak profile."
    );
}

fn print_daemon_help() {
    println!(
        "\
ratspeakd [--data-dir PATH] [run] [--events-jsonl] [--quiet]

Runs the Ratspeak runtime without the Tauri UI.
  --events-jsonl   emit runtime events and notifications as JSONL on stdout
  --quiet          suppress daemon lifecycle messages on stderr

ratspeakd publishes a profile-local daemon API endpoint at
.ratspeak/ratspeakd-api.json. ratspeakctl discovers that endpoint and routes
supported read commands through the live daemon when it is running."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_global_extracts_profile_and_pretty() {
        let (global, rest) = parse_global(vec![
            "--data-dir".into(),
            "/tmp/profile".into(),
            "--pretty".into(),
            "status".into(),
        ])
        .unwrap();
        assert_eq!(global.data_root, Some(PathBuf::from("/tmp/profile")));
        assert!(global.output.pretty);
        assert_eq!(rest, vec!["status"]);
    }

    #[test]
    fn take_limit_removes_limit_pair() {
        let mut args = vec!["abc".into(), "--limit".into(), "42".into()];
        assert_eq!(take_limit(&mut args, 100).unwrap(), 42);
        assert_eq!(args, vec!["abc"]);
    }

    #[test]
    fn take_f64_option_removes_pair() {
        let mut args = vec!["--recency-secs".into(), "10.5".into(), "tail".into()];
        assert_eq!(
            take_f64_option(&mut args, "--recency-secs", 20.0).unwrap(),
            10.5
        );
        assert_eq!(args, vec!["tail"]);
    }

    #[test]
    fn take_flag_removes_flag() {
        let mut args = vec!["--activate".into(), "tail".into()];
        assert!(take_flag(&mut args, "--activate"));
        assert!(!take_flag(&mut args, "--activate"));
        assert_eq!(args, vec!["tail"]);
    }
}
