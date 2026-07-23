use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

fn command_body<'a>(source: &'a str, name: &str) -> &'a str {
    let marker = format!("pub async fn {name}");
    let start = source.find(&marker).expect("command must exist");
    let remainder = &source[start..];
    let end = remainder
        .find("\n#[tauri::command]")
        .unwrap_or(remainder.len());
    &remainder[..end]
}

#[test]
fn typed_activity_commands_are_unconditionally_registered() {
    let root = repo_root();
    let app = fs::read_to_string(root.join("src-tauri/src/lib.rs")).expect("app command registry");
    let commands = fs::read_to_string(root.join("crates/ratspeak-tauri/src/commands/mod.rs"))
        .expect("command modules");

    assert!(commands.lines().any(|line| line == "pub mod activity;"));
    let command_names = [
        "activity_status",
        "activity_start",
        "activity_stop",
        "activity_resume",
        "activity_set_profile",
        "activity_replay",
        "activity_clear",
        "activity_detail",
        "activity_reveal",
        "activity_safe_copy",
    ];
    for command in command_names {
        let registration = format!("ratspeak_tauri::commands::activity::{command},");
        assert!(
            app.lines().any(|line| line.trim() == registration),
            "missing unconditional registration for {command}"
        );
    }

    let mut contiguous =
        String::from("            ratspeak_tauri::commands::network::api_hub_interfaces,\n");
    for command in command_names {
        contiguous.push_str(&format!(
            "            ratspeak_tauri::commands::activity::{command},\n"
        ));
    }
    contiguous.push_str("            ratspeak_tauri::commands::channels::api_channels,");
    assert!(
        app.contains(&contiguous),
        "Activity commands must remain an unconditional registry block"
    );
    assert!(!app.contains("commands::network::enable_network_log,"));
    assert!(!app.contains("commands::network::set_network_log_level,"));
}

#[test]
fn activity_lifecycle_commands_follow_identity_then_activity_lock_order() {
    let root = repo_root();
    let activity = fs::read_to_string(root.join("crates/ratspeak-tauri/src/commands/activity.rs"))
        .expect("activity commands");
    let system = fs::read_to_string(root.join("crates/ratspeak-tauri/src/commands/system.rs"))
        .expect("foreground command");

    for command in [
        "activity_status",
        "activity_start",
        "activity_stop",
        "activity_resume",
        "activity_set_profile",
        "activity_clear",
    ] {
        let body = command_body(&activity, command);
        let identity = body
            .find("identity_switch_lock.lock().await")
            .expect("identity lifecycle lock");
        let control = body
            .find("activity_control_lock.lock().await")
            .expect("activity control lock");
        assert!(identity < control, "wrong lock order in {command}");
        if command == "activity_status" {
            continue;
        }
        let snapshot = body
            .find("activity_request_fence()")
            .expect("Activity request-fence snapshot");
        let validation = body
            .find("ensure_activity_request_fence")
            .expect("Activity request-fence validation");
        assert!(
            snapshot < identity,
            "late request-fence snapshot in {command}"
        );
        assert!(
            control < validation,
            "request fence checked before both locks in {command}"
        );
    }

    let foreground = command_body(&system, "api_set_foreground");
    let identity = foreground
        .find("identity_switch_lock.lock().await")
        .expect("identity lifecycle lock");
    let control = foreground
        .find("activity_control_lock.lock().await")
        .expect("activity control lock");
    assert!(identity < control, "wrong foreground lock order");
}
