use std::path::PathBuf;

use serde_json::{Value, json};

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
        "profile" => run_profile(&profile, &args[1..], global.output),
        "status" => run_status(&profile, &args[1..], global.output),
        "identity" => run_identity(&profile, &args[1..], global.output),
        "contacts" => run_contacts(&profile, &args[1..], global.output),
        "peers" | "peer" => run_peers(&profile, &args[1..], global.output),
        "conversations" => run_conversations(&profile, &args[1..], global.output).await,
        "messages" => run_messages(&profile, &args[1..], global.output),
        "propagation" => run_propagation(&profile, &args[1..], global.output),
        "network" => run_network(&profile, &args[1..], global.output),
        "events" => Err(CliError::usage(
            "ratspeakctl events requires the daemon local API; use ratspeakd --events-jsonl for live events in milestone 1",
        )),
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
    if !quiet {
        eprintln!(
            "ratspeakd running; data_root={}; lock={}; events_jsonl={}",
            data_root.display(),
            profile_lock.path().display(),
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
                "note": "local daemon API is planned after the read-only CLI foundation"
            }
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
    let identity = take_option(&mut rest, "--identity")?;
    ensure_no_extra_args(&rest, "contacts")?;
    let identity_id = profile::active_identity_id(profile, identity);

    match subcommand {
        "list" => {
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
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            if args.len() > 1 {
                return Err(CliError::usage(
                    "conversations list does not take positional arguments",
                ));
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
        "list" => {
            let dest_hash = rest
                .first()
                .ok_or_else(|| CliError::usage("messages list requires <dest_hash>"))?
                .to_string();
            ensure_no_extra_args(&rest[1..], "messages list")?;
            if !ratspeak_runtime::helpers::validate_hex(&dest_hash, 16, 64) {
                return Err(CliError::usage("invalid destination hash"));
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
        other => Err(CliError::usage(format!("unknown network command: {other}"))),
    }
}

fn version_payload() -> Value {
    json!({
        "name": "Ratspeak",
        "cli_crate": "ratspeak-cli",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": {
            "ratspeakctl": "read-only profile/status/identity/contact/conversation/message inspection",
            "ratspeakd": "headless runtime owner with optional JSONL event emission"
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
  profile show
  status
  identity get
  identity current
  identity list
  identity create [--nickname NAME] [--activate]
  identity activate HASH
  contacts list [--identity HASH]
  contacts blocked [--identity HASH]
  peers list [--identity HASH] [--recency-secs N]
  conversations list
  messages list <dest_hash> [--identity HASH] [--limit N]
  messages search <query> [--identity HASH] [--limit N]
  propagation status
  network status
  network alerts
  network announces

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

Milestone 1 does not expose a local control API yet; use ratspeakctl for
read-only profile/database inspection and ratspeakd --events-jsonl for live
runtime event streaming."
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
