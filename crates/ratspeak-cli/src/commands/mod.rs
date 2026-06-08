use std::path::PathBuf;

use serde_json::{Value, json};

use crate::error::{CliError, CliResult};
use crate::output::{OutputFormat, print_json};
use crate::profile::{self, Profile};

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
    if args[0] == "version" {
        return print_json(&version_payload(), global.output);
    }

    let data_root = profile::resolve_data_root(global.data_root);
    let profile = profile::open_profile(data_root)?;

    match args[0].as_str() {
        "profile" => run_profile(&profile, &args[1..], global.output),
        "status" => run_status(&profile, &args[1..], global.output),
        "identity" => run_identity(&profile, &args[1..], global.output),
        "contacts" => run_contacts(&profile, &args[1..], global.output),
        "conversations" => run_conversations(&profile, &args[1..], global.output).await,
        "messages" => run_messages(&profile, &args[1..], global.output),
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
    let state = crate::runtime_host::init_headless_runtime(data_root.clone(), emit_jsonl).await?;
    if !quiet {
        eprintln!(
            "ratspeakd running; data_root={}; events_jsonl={}",
            data_root.display(),
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
        "list" => print_json(&json!(ratspeak_db::get_all_identities(&profile.db)), output),
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
        "list" => print_json(
            &json!({
                "identity_id": identity_id,
                "contacts": ratspeak_db::get_all_contacts(&profile.db, &identity_id),
            }),
            output,
        ),
        "blocked" => print_json(
            &json!({
                "identity_id": identity_id,
                "blocked": ratspeak_db::get_blocked_contacts(&profile.db, &identity_id),
            }),
            output,
        ),
        other => Err(CliError::usage(format!(
            "unknown contacts command: {other}"
        ))),
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
        "list" => {
            let dest_hash = rest
                .first()
                .ok_or_else(|| CliError::usage("messages list requires <dest_hash>"))?
                .to_string();
            ensure_no_extra_args(&rest[1..], "messages list")?;
            if !ratspeak_runtime::helpers::validate_hex(&dest_hash, 16, 64) {
                return Err(CliError::usage("invalid destination hash"));
            }
            print_json(
                &json!({
                    "identity_id": identity_id,
                    "dest_hash": dest_hash,
                    "messages": ratspeak_db::get_conversation(&profile.db, &dest_hash, &identity_id, limit),
                }),
                output,
            )
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
            print_json(
                &json!({
                    "identity_id": identity_id,
                    "query": query,
                    "messages": ratspeak_db::search_messages(&profile.db, &query, &identity_id, limit),
                }),
                output,
            )
        }
        other => Err(CliError::usage(format!(
            "unknown messages command: {other}"
        ))),
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

fn print_ctl_help() {
    println!(
        "\
ratspeakctl [--data-dir PATH] [--pretty] <command>

Read-only Ratspeak CLI commands:
  version
  profile show
  status
  identity get
  identity list
  contacts list [--identity HASH]
  contacts blocked [--identity HASH]
  conversations list
  messages list <dest_hash> [--identity HASH] [--limit N]
  messages search <query> [--identity HASH] [--limit N]

State commands emit JSON by default. Use --pretty for formatted JSON.
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
}
