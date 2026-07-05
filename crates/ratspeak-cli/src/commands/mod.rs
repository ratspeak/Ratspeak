use std::path::PathBuf;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use crate::agent_actions;
use crate::agent_admin;
use crate::agent_policy::{agent_root_from_owner_data_dir, push_unique};
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
        "interface" | "interfaces" => run_interface(&profile, &args[1..], global.output),
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
    let mut force = false;
    let mut share_instance = false;
    let mut instance_name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "run" => {}
            "--events-jsonl" => emit_jsonl = true,
            "--quiet" => quiet = true,
            "--force" => force = true,
            "--no-share-instance" => share_instance = false,
            "--share-instance" => share_instance = true,
            "--instance-name" => {
                i += 1;
                let Some(name) = args.get(i) else {
                    return Err(CliError::usage(
                        "--instance-name requires a value".to_string(),
                    ));
                };
                instance_name = Some(name.clone());
                share_instance = true;
            }
            other => {
                return Err(CliError::usage(format!(
                    "unknown ratspeakd option: {other}"
                )));
            }
        }
        i += 1;
    }
    if let Some(name) = &instance_name {
        if name.trim().is_empty() || !name.bytes().all(is_instance_name_byte) {
            return Err(CliError::usage(
                "invalid --instance-name: use letters, digits, '.', '_', or '-'".to_string(),
            ));
        }
        if name.eq_ignore_ascii_case("default") && !force {
            return Err(CliError::usage(
                "refusing --instance-name default: it collides with Python rnsd's default \
                 instance. Choose another name or pass --force."
                    .to_string(),
            ));
        }
    }
    let rns_policy = ratspeak_runtime::bootstrap::HeadlessRnsPolicy {
        share_instance,
        instance_name,
    };

    init_tracing();
    let data_root = profile::resolve_data_root(global.data_root);
    if profile::is_desktop_app_root(&data_root) {
        if !force {
            return Err(CliError::usage(format!(
                "refusing to run ratspeakd against the desktop app profile at {} — \
                 a headless daemon here would co-own the app's database, identity, and \
                 Reticulum config. Point --data-dir at a separate bot profile, or pass \
                 --force to override.",
                data_root.display()
            )));
        }
        eprintln!(
            "warning: running ratspeakd against the desktop app profile at {} (--force); \
             concurrent access with the GUI app can corrupt state",
            data_root.display()
        );
    }
    let lock_data_dir = data_root.join(".ratspeak");
    let profile_lock =
        ratspeak_runtime::profile_lock::try_acquire_profile_lock(&lock_data_dir, "ratspeakd")
            .map_err(|e| CliError::failed(format!("failed to acquire profile lock: {e}")))?;
    let state =
        crate::runtime_host::init_headless_runtime(data_root.clone(), emit_jsonl, rns_policy)
            .await?;
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

    let signal = wait_for_shutdown_signal().await;
    if !quiet {
        eprintln!("ratspeakd shutting down ({signal})");
    }
    // Persist crypto/ratchet state and release the lock on both SIGINT and
    // SIGTERM so `systemctl stop`/`kill` don't skip shutdown or leak the lock.
    ratspeak_runtime::shutdown_rns_lxmf(&state).await;
    drop(profile_lock);
    Ok(())
}

/// Resolve when the daemon should shut down. Handles SIGTERM (service stop) in
/// addition to SIGINT so an always-on daemon persists state on every clean exit.
async fn wait_for_shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => "SIGINT",
                    _ = term.recv() => "SIGTERM",
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "ctrl-c"
    }
}

fn is_instance_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-'
}

/// The Reticulum config dir the daemon actually reads for this profile: the
/// active identity's dir when one exists (seeded from app-private), else the
/// app-private dir.
fn runtime_rns_config_dir(profile: &Profile) -> PathBuf {
    if profile.config.uses_app_private_rns_config_dir() {
        if let Some(hash) = ratspeak_db::get_active_identity(&profile.db)
            .and_then(|id| id.get("hash").and_then(|h| h.as_str()).map(str::to_string))
            .filter(|h| !h.is_empty())
        {
            return profile.config.identity_rns_config_dir(&hash);
        }
    }
    profile.config.rns_config_dir.clone()
}

fn run_interface(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    use ratspeak_runtime::rns_config;
    let config_dir = runtime_rns_config_dir(profile);
    let interface_result = |ok: bool, action: &str, name: &str| -> CliResult<()> {
        if !ok {
            return Err(CliError::failed(format!(
                "interface {action} failed for '{name}' (invalid name/args or write error)"
            )));
        }
        print_json(
            &json!({
                "ok": true,
                "action": action,
                "name": name,
                "config_dir": config_dir,
                "note": "restart ratspeakd for interface changes to take effect",
            }),
            output,
        )
    };

    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => print_json(
            &json!({
                "ok": true,
                "config_dir": config_dir,
                "interfaces": rns_config::get_all_interfaces(&config_dir),
            }),
            output,
        ),
        "add-auto" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let name = take_option(&mut rest, "--name")?.unwrap_or_else(|| "LAN".to_string());
            let group_id = take_option(&mut rest, "--group-id")?;
            let discovery_scope = take_option(&mut rest, "--discovery-scope")?;
            ensure_no_extra_args(&rest, "interface add-auto")?;
            let opts = rns_config::AutoInterfaceOptions {
                group_id,
                discovery_scope,
                ..Default::default()
            };
            let ok = rns_config::add_auto_interface(&config_dir, &name, &opts);
            interface_result(ok, "add-auto", &name)
        }
        "add-tcp" => {
            let mut rest = args.get(1..).unwrap_or_default().to_vec();
            let name = take_option(&mut rest, "--name")?.unwrap_or_else(|| "TCP".to_string());
            let host = take_option(&mut rest, "--host")?
                .ok_or_else(|| CliError::usage("interface add-tcp requires --host".to_string()))?;
            let port = take_u64_option_opt(&mut rest, "--port")?
                .ok_or_else(|| CliError::usage("interface add-tcp requires --port".to_string()))?;
            let port = u16::try_from(port)
                .map_err(|_| CliError::usage("--port must be 1-65535".to_string()))?;
            ensure_no_extra_args(&rest, "interface add-tcp")?;
            let ok = rns_config::add_tcp_client(&config_dir, &name, &host, port);
            interface_result(ok, "add-tcp", &name)
        }
        "remove" => {
            let name = args
                .get(1)
                .ok_or_else(|| CliError::usage("interface remove requires a name".to_string()))?;
            let ok = rns_config::remove_interface(&config_dir, name);
            interface_result(ok, "remove", name)
        }
        other => Err(CliError::usage(format!(
            "unknown interface command: {other} (expected list|add-auto|add-tcp|remove)"
        ))),
    }
}

fn run_profile(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "show" => print_json(&profile::profile_summary(profile), output),
        "unlock" => run_profile_unlock(profile, &args[1..], output),
        other => Err(CliError::usage(format!("unknown profile command: {other}"))),
    }
}

fn run_profile_unlock(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let mut rest = args.to_vec();
    let force = take_flag(&mut rest, "--force");
    ensure_no_extra_args(&rest, "profile unlock")?;
    let data_dir = &profile.config.data_dir;
    let lock_path = ratspeak_runtime::profile_lock::lock_path(data_dir);
    if !force {
        return Err(CliError::usage(
            "profile unlock removes the profile lock and requires --force; \
             stop any running ratspeakd for this profile first"
                .to_string(),
        ));
    }
    // On Unix a held lock means a live process still owns the advisory flock;
    // yanking the file would let a second daemon co-own the profile. Refuse.
    if ratspeak_runtime::profile_lock::is_profile_locked(data_dir) {
        return Err(CliError::failed(format!(
            "refusing to unlock: a running process still holds the profile lock at {}. \
             Stop that ratspeakd first.",
            lock_path.display()
        )));
    }
    let removed = ratspeak_runtime::profile_lock::remove_lock_file(data_dir)
        .map_err(|e| CliError::failed(format!("failed to remove lock file: {e}")))?;
    print_json(
        &json!({
            "ok": true,
            "unlocked": removed,
            "lock_path": lock_path,
        }),
        output,
    )
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
        "policy" => run_agent_policy(profile, &args[1..], output),
        "adapter" => run_agent_adapter(profile, &args[1..], output),
        "revoke" => run_agent_revoke(profile, &args[1..], output),
        "remove" | "delete" => run_agent_remove(profile, &args[1..], output),
        "rotate-token" => run_agent_rotate_token(profile, &args[1..], output),
        other => Err(CliError::usage(format!("unknown agent command: {other}"))),
    }
}

fn run_agent_adapter(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "show" => {
            let name = args
                .get(1)
                .ok_or_else(|| CliError::usage("agent adapter show requires <name>"))?;
            print_json(&agent_admin::show_agent_adapter(profile, name)?, output)
        }
        "catalog" => print_json(&agent_admin::agent_adapter_catalog_payload(), output),
        "set" => {
            let name = args
                .get(1)
                .ok_or_else(|| CliError::usage("agent adapter set requires <name>"))?
                .clone();
            let mut rest = args.get(2..).unwrap_or_default().to_vec();
            let provider = take_option(&mut rest, "--provider")?.unwrap_or_else(|| "venice".into());
            let label = take_option(&mut rest, "--label")?;
            let model = take_option(&mut rest, "--model")?;
            let base_url = take_option(&mut rest, "--base-url")?;
            let secret_env = take_option(&mut rest, "--secret-env")?;
            let secret_file = take_option(&mut rest, "--secret-file")?.map(PathBuf::from);
            let notes = take_option(&mut rest, "--notes")?;
            ensure_no_extra_args(&rest, "agent adapter set")?;
            // The adapter's launch `command` is reserved: ratspeakd does not spawn
            // runners, so the CLI never records one (an external runner attaches
            // over the daemon API with the agent token).
            let update = agent_admin::AgentAdapterUpdate {
                name,
                provider,
                label,
                model,
                base_url,
                command: Vec::new(),
                secret_env,
                secret_file,
                notes,
            };
            print_json(&agent_admin::set_agent_adapter(profile, update)?, output)
        }
        other => Err(CliError::usage(format!(
            "unknown agent adapter command: {other} (expected show|set|catalog)"
        ))),
    }
}

fn run_agent_create(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent create requires <name>"))?;

    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let identity_mode = take_option(&mut rest, "--identity")?.unwrap_or_else(|| "new".into());
    let explicit_profile_dir = take_option(&mut rest, "--profile-dir")?.map(PathBuf::from);
    let requested_scopes = take_repeated_option(&mut rest, "--scope")?;
    let presets = take_repeated_option(&mut rest, "--preset")?;
    let allowed_contacts = take_repeated_option(&mut rest, "--allow-contact")?;
    let allowed_conversations = take_repeated_option(&mut rest, "--allow-conversation")?;
    let unknown_contacts =
        take_option(&mut rest, "--unknown-contacts")?.unwrap_or_else(|| "deny".into());
    let nickname = take_option(&mut rest, "--nickname")?.unwrap_or_else(|| name.to_string());
    let include_recovery = take_flag(&mut rest, "--show-recovery");
    ensure_no_extra_args(&rest, "agent create")?;

    let payload = agent_admin::create_agent(
        profile,
        agent_admin::AgentCreateOptions {
            name: name.clone(),
            identity_mode,
            explicit_profile_dir,
            requested_scopes,
            presets,
            allowed_contacts,
            allowed_conversations,
            unknown_contacts,
            nickname: Some(nickname),
            include_recovery,
        },
    )?;
    print_json(&payload, output)
}

fn run_agent_list(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    ensure_no_extra_args(args, "agent list")?;
    let records = agent_admin::list_agent_manifests(profile)?;
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
    ensure_no_extra_args(&args[1..], "agent show")?;
    let manifest = agent_admin::show_agent_manifest(profile, name)?;
    print_json(&serde_json::to_value(manifest)?, output)
}

fn run_agent_grant(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent grant requires <name>"))?;
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

    let payload = agent_admin::update_agent_grant(
        profile,
        agent_admin::AgentGrantUpdate {
            name: name.clone(),
            scopes,
            presets,
            contacts,
            conversations,
            unknown_contacts,
            replace_scopes,
            replace_contacts,
            replace_conversations,
            activate,
        },
    )?;
    print_json(&payload, output)
}

fn run_agent_policy(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let subcommand = args.first().map(String::as_str).unwrap_or("show");
    if subcommand == "defaults" {
        ensure_no_extra_args(&args[1..], "agent policy defaults")?;
        return print_json(
            &serde_json::to_value(agent_actions::AgentWritePolicy::default())?,
            output,
        );
    }
    let name = args
        .get(1)
        .ok_or_else(|| CliError::usage(format!("agent policy {subcommand} requires <name>")))?;
    agent_admin::validate_agent_name(name)?;
    match subcommand {
        "show" => {
            ensure_no_extra_args(&args[2..], "agent policy show")?;
            print_json(&agent_admin::show_agent_policy(profile, name)?, output)
        }
        "validate" => {
            ensure_no_extra_args(&args[2..], "agent policy validate")?;
            print_json(&agent_admin::validate_agent_policy(profile, name)?, output)
        }
        "set" => {
            let mut rest = args.get(2..).unwrap_or_default().to_vec();
            let current = agent_admin::show_agent_policy(profile, name)?;
            let mut policy: agent_actions::AgentWritePolicy =
                serde_json::from_value(current["policy"].clone())?;

            for pair in take_repeated_option(&mut rest, "--set")? {
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| CliError::usage("--set requires key=value"))?;
                apply_policy_key_value(&mut policy, key, value)?;
            }

            apply_policy_bool_option(
                &mut rest,
                "--require-owner-approval",
                &mut policy.require_owner_approval,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--auto-approval",
                &mut policy.auto_approval_enabled,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--require-causal-context",
                &mut policy.require_causal_context_for_outbound,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--require-verified-causal-context",
                &mut policy.require_verified_causal_context,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-agent-file-paths",
                &mut policy.allow_agent_file_paths,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-attachments",
                &mut policy.allow_message_attachments,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-images",
                &mut policy.allow_message_images,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-reactions",
                &mut policy.allow_message_reactions,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-contact-mutations",
                &mut policy.allow_contact_mutations,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-conversation-mutations",
                &mut policy.allow_conversation_mutations,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-conversation-delete",
                &mut policy.allow_conversation_delete,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-identity-announce",
                &mut policy.allow_identity_announce,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-path-request",
                &mut policy.allow_path_request,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--allow-forced-propagated-delivery",
                &mut policy.allow_forced_propagated_delivery,
            )?;
            apply_policy_bool_option(
                &mut rest,
                "--static-propagation-nodes-only",
                &mut policy.allow_static_propagation_nodes_only,
            )?;

            apply_policy_usize_option(&mut rest, "--max-text-chars", &mut policy.max_text_chars)?;
            apply_policy_usize_option(&mut rest, "--max-text-bytes", &mut policy.max_text_bytes)?;
            apply_policy_usize_option(
                &mut rest,
                "--max-attachment-bytes",
                &mut policy.max_attachment_bytes,
            )?;
            apply_policy_usize_option(&mut rest, "--max-file-bytes", &mut policy.max_file_bytes)?;
            apply_policy_usize_option(&mut rest, "--max-image-bytes", &mut policy.max_image_bytes)?;
            apply_policy_usize_option(
                &mut rest,
                "--max-actions-per-hour",
                &mut policy.max_actions_per_hour,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-actions-per-day",
                &mut policy.max_actions_per_day,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-messages-per-contact-hour",
                &mut policy.max_messages_per_contact_hour,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-messages-per-contact-day",
                &mut policy.max_messages_per_contact_day,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-announces-per-hour",
                &mut policy.max_announces_per_hour,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-announces-per-day",
                &mut policy.max_announces_per_day,
            )?;
            apply_policy_u64_option(
                &mut rest,
                "--min-announce-interval-secs",
                &mut policy.min_announce_interval_secs,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-path-requests-per-hour",
                &mut policy.max_path_requests_per_hour,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--max-path-requests-per-day",
                &mut policy.max_path_requests_per_day,
            )?;
            apply_policy_u64_option(
                &mut rest,
                "--min-path-request-interval-secs",
                &mut policy.min_path_request_interval_secs,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--auto-max-text-chars",
                &mut policy.auto_approval_max_text_chars,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--auto-max-text-bytes",
                &mut policy.auto_approval_max_text_bytes,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--auto-max-actions-per-hour",
                &mut policy.auto_approval_max_actions_per_hour,
            )?;
            apply_policy_usize_option(
                &mut rest,
                "--auto-max-actions-per-day",
                &mut policy.auto_approval_max_actions_per_day,
            )?;

            if take_flag(&mut rest, "--clear-source-roots") {
                policy.allowed_source_roots.clear();
            }
            for root in take_repeated_option(&mut rest, "--allow-source-root")? {
                push_unique_path(&mut policy.allowed_source_roots, PathBuf::from(root));
            }
            if take_flag(&mut rest, "--clear-delivery-methods") {
                policy.allowed_delivery_methods.clear();
            }
            for method in take_repeated_option(&mut rest, "--allowed-delivery-method")? {
                push_unique(&mut policy.allowed_delivery_methods, method);
            }
            if take_flag(&mut rest, "--clear-mime-prefixes") {
                policy.allowed_attachment_mime_prefixes.clear();
            }
            for prefix in take_repeated_option(&mut rest, "--allow-mime-prefix")? {
                push_unique(&mut policy.allowed_attachment_mime_prefixes, prefix);
            }
            for prefix in take_repeated_option(&mut rest, "--deny-mime-prefix")? {
                push_unique(&mut policy.denied_attachment_mime_prefixes, prefix);
            }
            for kind in take_repeated_option(&mut rest, "--block-kind")? {
                push_unique(&mut policy.blocked_action_kinds, kind);
            }
            for kind in take_repeated_option(&mut rest, "--unblock-kind")? {
                policy
                    .blocked_action_kinds
                    .retain(|candidate| candidate != &kind);
            }
            for kind in take_repeated_option(&mut rest, "--auto-allow-kind")? {
                push_unique(&mut policy.auto_approval_allowed_action_kinds, kind);
            }
            for contact in take_repeated_option(&mut rest, "--auto-allow-contact")? {
                push_unique(&mut policy.auto_approval_allowed_contacts, contact);
            }
            for conversation in take_repeated_option(&mut rest, "--auto-allow-conversation")? {
                push_unique(
                    &mut policy.auto_approval_allowed_conversations,
                    conversation,
                );
            }
            for hash in take_repeated_option(&mut rest, "--allow-path-request-hash")? {
                push_unique(&mut policy.allowed_path_request_hashes, hash);
            }
            for hash in take_repeated_option(&mut rest, "--allow-propagation-node-hash")? {
                push_unique(&mut policy.allowed_propagation_node_hashes, hash);
            }
            ensure_no_extra_args(&rest, "agent policy set")?;

            let payload = agent_admin::set_agent_policy(
                profile,
                name,
                agent_admin::AgentPolicyPatch {
                    policy: Some(policy),
                    set: Vec::new(),
                },
            )?;
            print_json(&payload, output)
        }
        other => Err(CliError::usage(format!(
            "unknown agent policy command: {other}"
        ))),
    }
}

fn run_agent_revoke(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent revoke requires <name>"))?;
    let mut rest = args.get(1..).unwrap_or_default().to_vec();
    let reason = take_option(&mut rest, "--reason")?;
    ensure_no_extra_args(&rest, "agent revoke")?;

    print_json(&agent_admin::revoke_agent(profile, name, reason)?, output)
}

fn run_agent_remove(profile: &Profile, args: &[String], output: OutputFormat) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent remove requires <name>"))?;
    ensure_no_extra_args(&args[1..], "agent remove")?;

    print_json(&agent_admin::remove_agent(profile, name)?, output)
}

fn run_agent_rotate_token(
    profile: &Profile,
    args: &[String],
    output: OutputFormat,
) -> CliResult<()> {
    let name = args
        .first()
        .ok_or_else(|| CliError::usage("agent rotate-token requires <name>"))?;
    ensure_no_extra_args(&args[1..], "agent rotate-token")?;

    print_json(&agent_admin::rotate_agent_token(profile, name)?, output)
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
            enforce_agent_file_source_policy(profile, &file, is_image)?;
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
                agent_admin::validate_agent_name(&agent_name)?;
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
    match subcommand {
        "list" => {
            let state = take_option(&mut rest, "--state")?
                .unwrap_or_else(|| agent_actions::STATE_PENDING_APPROVAL.into());
            ensure_no_extra_args(&rest, "approvals list")?;
            let payload =
                agent_admin::list_agent_approvals(profile, agent.as_deref(), Some(&state))?;
            if output.jsonl {
                let records = payload
                    .get("actions")
                    .and_then(Value::as_array)
                    .ok_or_else(|| CliError::failed("expected actions array"))?;
                print_jsonl(records)
            } else {
                print_json(&payload, output)
            }
        }
        "show" | "read" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals show requires <action-id>"))?;
            ensure_no_extra_args(&rest[1..], "approvals show")?;
            print_json(
                &agent_admin::show_agent_approval(profile, agent.as_deref(), id)?,
                output,
            )
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
            let payload = agent_admin::inspect_agent_staged_file(
                profile,
                agent.as_deref(),
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
            let payload =
                agent_admin::approve_agent_action(profile, agent.as_deref(), &id, note, execute)?;
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
            print_json(
                &agent_admin::reject_agent_action(profile, agent.as_deref(), &id, note)?,
                output,
            )
        }
        "cancel" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals cancel requires <action-id>"))?
                .to_string();
            rest.remove(0);
            let note = take_option(&mut rest, "--note")?;
            ensure_no_extra_args(&rest, "approvals cancel")?;
            print_json(
                &agent_admin::cancel_agent_action(profile, agent.as_deref(), &id, note)?,
                output,
            )
        }
        "execute" => {
            let id = rest
                .first()
                .ok_or_else(|| CliError::usage("approvals execute requires <action-id>"))?;
            ensure_no_extra_args(&rest[1..], "approvals execute")?;
            let payload = agent_admin::execute_agent_action(profile, agent.as_deref(), id)?;
            print_json(&payload, output)
        }
        "expire" => {
            ensure_no_extra_args(&rest, "approvals expire")?;
            print_json(
                &agent_admin::expire_agent_actions(profile, agent.as_deref())?,
                output,
            )
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
                let payload = agent_admin::list_agent_audit(profile, Some(&agent_name), limit)?;
                if output.jsonl {
                    let records = payload
                        .get("audit")
                        .and_then(Value::as_array)
                        .ok_or_else(|| CliError::failed("expected audit array"))?;
                    print_jsonl(records)
                } else {
                    print_json(&payload, output)
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
            "approvals": "ratspeakctl approvals list|show|inspect-file|approve|reject|cancel|execute --agent NAME",
            "policy": "ratspeakctl agent policy show|validate|set NAME"
        },
        "presets": {
            "inbox-reader": agent_admin::agent_preset_scopes("inbox-reader").unwrap_or_default(),
            "reply-assistant": agent_admin::agent_preset_scopes("reply-assistant").unwrap_or_default(),
            "media-assistant": agent_admin::agent_preset_scopes("media-assistant").unwrap_or_default(),
            "network-helper": agent_admin::agent_preset_scopes("network-helper").unwrap_or_default()
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
        },
        "write_policy": {
            "default_auto_approval_enabled": false,
            "auto_approval": "opt-in policy auto-approves only matching action kind, contact/conversation, delivery method, causal context, size, and rate limits",
            "policy_commands": ["agent policy show NAME", "agent policy validate NAME", "agent policy set NAME"],
            "guardrail_categories": [
                "owner approval defaults and high-risk approval requirements",
                "action-kind blocklists",
                "text byte/char caps and denied substrings",
                "file/image size, MIME, and source-root controls",
                "delivery-method controls including forced propagated delivery",
                "causal context verification and loop-prevention counters",
                "per-contact, per-kind, announce, path-request, and network rate limits",
                "path request and propagation node allowlists",
                "grant/policy revision recheck at execute time"
            ]
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

fn apply_policy_bool_option(
    args: &mut Vec<String>,
    name: &str,
    target: &mut bool,
) -> CliResult<()> {
    if let Some(value) = take_option(args, name)? {
        *target = parse_bool_value(name, &value)?;
    }
    Ok(())
}

fn apply_policy_usize_option(
    args: &mut Vec<String>,
    name: &str,
    target: &mut usize,
) -> CliResult<()> {
    if let Some(value) = take_option(args, name)? {
        *target = value
            .parse::<usize>()
            .map_err(|_| CliError::usage(format!("{name} must be an unsigned integer")))?;
    }
    Ok(())
}

fn apply_policy_u64_option(args: &mut Vec<String>, name: &str, target: &mut u64) -> CliResult<()> {
    if let Some(value) = take_option(args, name)? {
        *target = value
            .parse::<u64>()
            .map_err(|_| CliError::usage(format!("{name} must be an unsigned integer")))?;
    }
    Ok(())
}

fn apply_policy_key_value(
    policy: &mut agent_actions::AgentWritePolicy,
    key: &str,
    value: &str,
) -> CliResult<()> {
    agent_admin::apply_policy_value(policy, key, json!(value))
}

fn parse_bool_value(name: &str, value: &str) -> CliResult<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "on" => Ok(true),
        "false" | "no" | "0" | "off" => Ok(false),
        _ => Err(CliError::usage(format!("{name} must be true or false"))),
    }
}

fn push_unique_path(values: &mut Vec<PathBuf>, value: PathBuf) {
    if !values.contains(&value) {
        values.push(value);
    }
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

fn enforce_agent_file_source_policy(
    profile: &Profile,
    file: &str,
    is_image: bool,
) -> CliResult<()> {
    let policy = agent_actions::ensure_write_policy(&profile.config.data_dir)?;
    if !policy.allow_agent_file_paths {
        return Err(CliError::failed(
            "agent policy blocks reading local file paths",
        ));
    }
    let path = PathBuf::from(file);
    let metadata = std::fs::metadata(&path)?;
    let size = metadata.len() as usize;
    let cap = if is_image {
        policy.max_image_bytes
    } else {
        policy.max_file_bytes
    };
    if size > cap {
        return Err(CliError::failed(format!(
            "local file exceeds policy cap ({cap} bytes)"
        )));
    }
    if policy.allowed_source_roots.is_empty() {
        return Ok(());
    }
    let canonical_file = path.canonicalize()?;
    let allowed = policy.allowed_source_roots.iter().any(|root| {
        root.canonicalize()
            .map(|canonical_root| canonical_file.starts_with(canonical_root))
            .unwrap_or(false)
    });
    if allowed {
        Ok(())
    } else {
        Err(CliError::failed(
            "local file is outside the agent policy allowed_source_roots",
        ))
    }
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
  agent policy show|validate|set NAME
  agent policy defaults
  agent revoke NAME [--reason TEXT]
  agent remove NAME
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
ratspeakd [--data-dir PATH] [run] [--events-jsonl] [--quiet] [--force]
          [--share-instance | --no-share-instance] [--instance-name NAME]

Runs the Ratspeak runtime without the Tauri UI. With no --data-dir it uses a
headless CLI profile distinct from the desktop app. By default the daemon runs a
Standalone Reticulum instance, isolated from the desktop app and other bots.
  --events-jsonl        emit runtime events and notifications as JSONL on stdout
  --quiet               suppress daemon lifecycle messages on stderr
  --force               allow running against the desktop app profile, or an
                        --instance-name of 'default' (both unsafe)
  --no-share-instance   force a Standalone instance (default)
  --share-instance      join a machine-local shared instance with a per-profile
                        derived name and ports
  --instance-name NAME  join the named shared instance (implies --share-instance)

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
