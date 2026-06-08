use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

fn temp_root(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("ratspeak-{name}-{}-{nanos}", std::process::id()))
}

fn ratspeakctl(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ratspeakctl"))
        .args(args)
        .output()
        .expect("ratspeakctl should execute")
}

fn spawn_ratspeakd(data_dir: &Path) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_ratspeakd"))
        .args(["--data-dir", &path_arg(data_dir), "--quiet"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ratspeakd should spawn")
}

fn run_json(args: &[&str]) -> Value {
    let output = ratspeakctl(args);
    assert!(
        output.status.success(),
        "ratspeakctl failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("ratspeakctl stdout should be JSON")
}

fn run_fail(args: &[&str]) -> String {
    let output = ratspeakctl(args);
    assert!(
        !output.status.success(),
        "ratspeakctl unexpectedly succeeded"
    );
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn path_arg(path: &Path) -> String {
    path.display().to_string()
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn agent_profile_bootstrap_smoke() {
    let root = temp_root("agent-profile-bootstrap");
    let version_profile = root.join("version-profile");
    let owner_profile = root.join("owner-profile");
    let owner_arg = path_arg(&owner_profile);

    let version_profile_arg = path_arg(&version_profile);
    let version = run_json(&["--data-dir", &version_profile_arg, "version"]);
    assert_eq!(version["name"], "Ratspeak");
    assert!(!version_profile.exists());

    let status = run_json(&["--data-dir", &owner_arg, "status"]);
    assert_eq!(status["ok"], true);
    assert_eq!(status["mode"], "offline");
    assert_eq!(status["identity_count"], 0);
    assert_eq!(status["daemon_api"]["available"], false);

    let create = run_json(&[
        "--data-dir",
        &owner_arg,
        "agent",
        "create",
        "agent-smoke",
        "--identity",
        "new",
        "--scope",
        "read:messages",
        "--scope",
        "write:drafts",
        "--allow-contact",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ]);
    assert_eq!(create["agent"]["name"], "agent-smoke");
    assert_eq!(create["agent"]["requested_scopes"][0], "messages:read");
    assert_eq!(create["agent"]["requested_scopes"][1], "write:drafts");
    assert_eq!(create["agent"]["pending_scopes"][0], "messages:read");
    assert_eq!(create["agent"]["pending_scopes"][1], "write:drafts");
    assert_eq!(create["agent"]["enforcement"]["local_daemon_api"], false);
    assert_eq!(create["identity"]["activated"], true);
    assert!(
        create["identity"]["hash"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        create["identity"]["lxmf_hash"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert!(
        create["identity"]["mnemonic"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );

    let agent_profile = create["agent"]["profile_root"]
        .as_str()
        .expect("agent profile root")
        .to_string();
    let agent_hash = create["agent"]["identity_hash"]
        .as_str()
        .expect("agent identity hash")
        .to_string();

    let list = run_json(&["--data-dir", &owner_arg, "agent", "list"]);
    assert_eq!(list.as_array().expect("agent list array").len(), 1);
    assert_eq!(list[0]["name"], "agent-smoke");

    let show = run_json(&["--data-dir", &owner_arg, "agent", "show", "agent-smoke"]);
    assert_eq!(show["identity_hash"], agent_hash);

    let current = run_json(&["--data-dir", &agent_profile, "identity", "current"]);
    assert_eq!(current["exists"], true);
    assert_eq!(current["identity"]["hash"], agent_hash);

    let profile = run_json(&["--data-dir", &agent_profile, "profile", "show"]);
    assert_eq!(profile["identity_count"], 1);
    assert_eq!(profile["profile_lock"]["locked"], false);

    for args in [
        vec!["--data-dir", &agent_profile, "contacts", "list"],
        vec!["--data-dir", &agent_profile, "contacts", "blocked"],
        vec!["--data-dir", &agent_profile, "peers", "list"],
        vec!["--data-dir", &agent_profile, "conversations", "list"],
        vec!["--data-dir", &agent_profile, "system", "unread"],
        vec!["--data-dir", &agent_profile, "messages", "search", "zz"],
        vec!["--data-dir", &agent_profile, "propagation", "status"],
        vec!["--data-dir", &agent_profile, "network", "status"],
        vec!["--data-dir", &agent_profile, "network", "alerts"],
        vec!["--data-dir", &agent_profile, "network", "announces"],
    ] {
        let _ = run_json(&args);
    }

    assert!(run_fail(&["--data-dir", &agent_profile, "messages", "send"]).contains("unknown"));
    assert!(run_fail(&["--data-dir", &agent_profile, "contacts", "add"]).contains("unknown"));
    assert!(run_fail(&["--data-dir", &agent_profile, "identity", "export"]).contains("unknown"));
    assert!(run_fail(&["--data-dir", &agent_profile, "events"]).contains("requires the daemon"));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn daemon_api_read_commands_smoke() {
    let root = temp_root("daemon-api-read");
    let profile = root.join("profile");
    let profile_arg = path_arg(&profile);

    let create = run_json(&[
        "--data-dir",
        &profile_arg,
        "identity",
        "create",
        "--nickname",
        "daemon-api-smoke",
        "--activate",
    ]);
    assert_eq!(create["activated"], true);
    let identity_hash = create["hash"].as_str().expect("identity hash").to_string();

    let mut daemon = spawn_ratspeakd(&profile);
    let endpoint = profile.join(".ratspeak").join("ratspeakd-api.json");
    if !wait_for_path(&endpoint, Duration::from_secs(10)) {
        let _ = daemon.kill();
        let _ = daemon.wait();
        panic!("ratspeakd API endpoint did not appear");
    }
    let endpoint_json: Value =
        serde_json::from_slice(&std::fs::read(&endpoint).expect("endpoint manifest should read"))
            .expect("endpoint manifest should be JSON");
    assert_eq!(endpoint_json["version"], 1);
    assert!(
        matches!(
            endpoint_json["transport"].as_str(),
            Some("unix" | "tcp" | "file")
        ),
        "unexpected daemon API transport: {endpoint_json}"
    );

    let status = run_json(&["--data-dir", &profile_arg, "status"]);
    assert_eq!(status["mode"], "daemon");
    assert_eq!(status["daemon_api"]["available"], true);
    assert_eq!(
        status["daemon_api"]["transport"],
        endpoint_json["transport"]
    );

    let current = run_json(&["--data-dir", &profile_arg, "identity", "current"]);
    assert_eq!(current["identity"]["hash"], identity_hash);

    let identities = run_json(&["--data-dir", &profile_arg, "identity", "list"]);
    assert_eq!(identities.as_array().expect("identity list").len(), 1);

    for args in [
        vec!["--data-dir", &profile_arg, "contacts", "list"],
        vec!["--data-dir", &profile_arg, "peers", "list"],
        vec!["--data-dir", &profile_arg, "conversations", "list"],
        vec![
            "--data-dir",
            &profile_arg,
            "messages",
            "list",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
        vec!["--data-dir", &profile_arg, "messages", "search", "zz"],
        vec!["--data-dir", &profile_arg, "propagation", "status"],
        vec!["--data-dir", &profile_arg, "network", "status"],
    ] {
        let _ = run_json(&args);
    }

    let _ = daemon.kill();
    let _ = daemon.wait();

    let _ = std::fs::remove_dir_all(root);
}
