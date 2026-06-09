use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("set private permissions");
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) {}

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

fn seed_contact_and_message(
    profile_root: &Path,
    identity_id: &str,
    peer_hash: &str,
    display_name: &str,
    content: &str,
) {
    let db = ratspeak_db::init_pool(profile_root).expect("test db pool");
    ratspeak_db::init_schema(&db).expect("test db schema");
    ratspeak_db::save_contact(&db, peer_hash, Some(display_name), "trusted", identity_id);
    ratspeak_db::save_message(
        &db,
        &format!("msg-{peer_hash}"),
        peer_hash,
        identity_id,
        content,
        "",
        now_secs(),
        "received",
        "inbound",
        identity_id,
        "",
        "",
        "",
        "",
        "",
        "",
        None,
    );
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
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
    assert_eq!(create["agent"]["requested_scopes"][1], "drafts:write");
    assert_eq!(create["agent"]["effective_scopes"][0], "messages:read");
    assert_eq!(create["agent"]["effective_scopes"][1], "drafts:write");
    assert_eq!(
        create["agent"]["pending_scopes"]
            .as_array()
            .expect("pending scopes")
            .len(),
        0
    );
    assert_eq!(create["agent"]["grant"]["status"], "active");
    assert_eq!(create["agent"]["grant"]["scopes"][0], "messages:read");
    assert_eq!(create["agent"]["enforcement"]["local_daemon_api"], true);
    assert_eq!(create["agent"]["enforcement"]["contact_allowlist"], true);
    assert!(
        create["credential"]["token_file"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
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

    assert!(run_fail(&["--data-dir", &agent_profile, "messages", "send"]).contains("requires"));
    assert!(run_fail(&["--data-dir", &agent_profile, "contacts", "add"]).contains("requires"));
    assert!(run_fail(&["--data-dir", &agent_profile, "identity", "export"]).contains("unknown"));
    assert!(run_fail(&["--data-dir", &agent_profile, "events"]).contains("requires ratspeakd"));

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

#[test]
fn daemon_agent_grants_auth_events_and_safe_reads_smoke() {
    let root = temp_root("daemon-agent-policy");
    let owner_profile = root.join("owner");
    let owner_arg = path_arg(&owner_profile);
    let allowed = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let denied = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let create = run_json(&[
        "--data-dir",
        &owner_arg,
        "agent",
        "create",
        "agent-policy",
        "--identity",
        "new",
        "--scope",
        "read:status",
        "--scope",
        "read:identity",
        "--scope",
        "read:contacts",
        "--scope",
        "read:messages",
        "--scope",
        "read:network",
    ]);
    let agent_profile = create["agent"]["profile_root"]
        .as_str()
        .expect("agent profile")
        .to_string();
    let agent_hash = create["agent"]["identity_hash"]
        .as_str()
        .expect("agent hash")
        .to_string();
    let token_file = create["credential"]["token_file"]
        .as_str()
        .expect("token file")
        .to_string();

    let grant = run_json(&[
        "--data-dir",
        &owner_arg,
        "agent",
        "grant",
        "agent-policy",
        "--scope",
        "read:events",
        "--allow-contact",
        allowed,
    ]);
    assert_eq!(grant["grant"]["status"], "active");
    assert_eq!(grant["grant"]["revision"], 2);
    assert_eq!(grant["grant"]["allowed_contacts"][0], allowed);
    assert!(
        grant["grant"]["scopes"]
            .as_array()
            .expect("scopes")
            .iter()
            .any(|scope| scope == "events:read")
    );

    seed_contact_and_message(
        Path::new(&agent_profile),
        &agent_hash,
        allowed,
        "Allowed Human",
        "allowed prompt-injection-looking text",
    );
    seed_contact_and_message(
        Path::new(&agent_profile),
        &agent_hash,
        denied,
        "Denied Human",
        "denied private text",
    );

    let mut daemon = spawn_ratspeakd(Path::new(&agent_profile));
    let endpoint = Path::new(&agent_profile)
        .join(".ratspeak")
        .join("ratspeakd-api.json");
    if !wait_for_path(&endpoint, Duration::from_secs(10)) {
        let _ = daemon.kill();
        let _ = daemon.wait();
        panic!("ratspeakd API endpoint did not appear");
    }

    let status = run_json(&["--data-dir", &agent_profile, "status"]);
    assert_eq!(status["mode"], "daemon");
    assert_eq!(status["access"]["mode"], "agent");
    assert_eq!(status["access"]["agent"], "agent-policy");

    let contacts = run_json(&["--data-dir", &agent_profile, "contacts", "list"]);
    let contact_rows = contacts["contacts"].as_array().expect("contacts array");
    assert_eq!(contact_rows.len(), 1);
    assert_eq!(contact_rows[0]["dest_hash"], allowed);

    let conversations = run_json(&["--data-dir", &agent_profile, "conversations", "list"]);
    let conversation_rows = conversations.as_array().expect("conversation array");
    assert_eq!(conversation_rows.len(), 1);
    assert_eq!(conversation_rows[0]["peer_hash"], allowed);
    assert_eq!(
        conversation_rows[0]["conversation_id"],
        format!("lxmf:{allowed}")
    );
    assert_eq!(conversation_rows[0]["last_message"]["untrusted"], true);

    let read = run_json(&[
        "--data-dir",
        &agent_profile,
        "conversations",
        "read",
        &format!("lxmf:{allowed}"),
    ]);
    let message = &read["messages"][0];
    assert_eq!(message["peer_hash"], allowed);
    assert_eq!(message["content"]["untrusted"], true);
    assert_eq!(message["agent_safety"]["stored_file_paths_redacted"], true);

    assert!(
        run_fail(&[
            "--data-dir",
            &agent_profile,
            "messages",
            "list",
            &format!("lxmf:{denied}"),
        ])
        .contains("forbidden")
    );

    let events = run_json(&["--data-dir", &agent_profile, "events", "stream", "--once"]);
    let event_rows = events["events"].as_array().expect("events array");
    assert!(
        event_rows
            .iter()
            .any(|event| event["event"] == "daemon.started")
    );
    let latest_id = events["latest_id"].as_u64().expect("latest id");
    let replay = run_json(&[
        "--data-dir",
        &agent_profile,
        "events",
        "stream",
        "--once",
        "--cursor",
        &latest_id.to_string(),
    ]);
    assert!(
        replay["events"]
            .as_array()
            .expect("replay events")
            .iter()
            .all(|event| event["id"].as_u64().unwrap_or(0) > latest_id)
    );

    let bad_credential = serde_json::json!({
        "format": "ratspeak.agent-token.v1",
        "version": 1,
        "agent_name": "agent-policy",
        "identity_hash": agent_hash,
        "token": "wrong-token",
        "created_at_unix": 0.0,
    });
    std::fs::write(
        &token_file,
        serde_json::to_vec_pretty(&bad_credential).expect("bad credential json"),
    )
    .expect("write bad credential");
    set_private_file_permissions(Path::new(&token_file));
    assert!(run_fail(&["--data-dir", &agent_profile, "status"]).contains("unauthorized"));

    let audit = run_json(&[
        "--data-dir",
        &owner_arg,
        "audit",
        "list",
        "--agent",
        "agent-policy",
        "--limit",
        "50",
    ]);
    assert!(
        audit["audit"]
            .as_array()
            .expect("audit rows")
            .iter()
            .any(|row| row["event"] == "auth.failed")
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn daemon_agent_write_approval_audit_and_limits_smoke() {
    let root = temp_root("daemon-agent-writes");
    let owner_profile = root.join("owner");
    let owner_arg = path_arg(&owner_profile);
    let allowed_text = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let allowed_image = "cccccccccccccccccccccccccccccccc";
    let denied = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let create = run_json(&[
        "--data-dir",
        &owner_arg,
        "agent",
        "create",
        "agent-writer",
        "--identity",
        "new",
        "--scope",
        "read:status",
        "--scope",
        "read:messages",
        "--scope",
        "read:actions",
        "--scope",
        "read:audit",
        "--scope",
        "write:drafts",
        "--scope",
        "write:messages",
        "--scope",
        "write:attachments",
        "--scope",
        "write:images",
        "--scope",
        "write:announces",
        "--scope",
        "write:contacts",
        "--allow-contact",
        allowed_text,
        "--allow-contact",
        allowed_image,
    ]);
    assert_eq!(create["agent"]["enforcement"]["write_actions"], true);
    assert_eq!(create["agent"]["enforcement"]["owner_approval"], true);
    assert_eq!(create["agent"]["enforcement"]["audit_log"], true);
    assert_eq!(create["agent"]["enforcement"]["rate_limits"], true);
    let agent_profile = create["agent"]["profile_root"]
        .as_str()
        .expect("agent profile")
        .to_string();

    let mut daemon = spawn_ratspeakd(Path::new(&agent_profile));
    let endpoint = Path::new(&agent_profile)
        .join(".ratspeak")
        .join("ratspeakd-api.json");
    if !wait_for_path(&endpoint, Duration::from_secs(10)) {
        let _ = daemon.kill();
        let _ = daemon.wait();
        panic!("ratspeakd API endpoint did not appear");
    }

    let draft = run_json(&[
        "--data-dir",
        &agent_profile,
        "messages",
        "draft",
        &format!("lxmf:{allowed_text}"),
        "--text",
        "owner-reviewed outbound text",
        "--client-action-id",
        "draft-allowed-text-1",
        "--causal-event-id",
        "100",
    ]);
    assert_eq!(draft["kind"], "message.send");
    assert_eq!(draft["state"], "draft");
    assert_eq!(draft["subject_hash"], allowed_text);
    assert_eq!(draft["policy"]["approval_required"], true);
    let draft_id = draft["id"].as_str().expect("draft id").to_string();

    let retry = run_json(&[
        "--data-dir",
        &agent_profile,
        "messages",
        "draft",
        &format!("lxmf:{allowed_text}"),
        "--text",
        "owner-reviewed outbound text",
        "--client-action-id",
        "draft-allowed-text-1",
        "--causal-event-id",
        "100",
    ]);
    assert_eq!(retry["id"], draft_id);
    assert!(
        run_fail(&[
            "--data-dir",
            &agent_profile,
            "messages",
            "draft",
            &format!("lxmf:{allowed_text}"),
            "--text",
            "different text",
            "--client-action-id",
            "draft-allowed-text-1",
            "--causal-event-id",
            "100",
        ])
        .contains("idempotency_conflict")
    );

    let submitted = run_json(&["--data-dir", &agent_profile, "messages", "send", &draft_id]);
    assert_eq!(submitted["state"], "pending_approval");

    let queue = run_json(&[
        "--data-dir",
        &owner_arg,
        "approvals",
        "list",
        "--agent",
        "agent-writer",
    ]);
    let queued = queue["actions"].as_array().expect("approval queue");
    assert!(queued.iter().any(|item| item["id"] == draft_id));
    assert_eq!(queued[0]["payload"]["redacted"], true);

    let approved = run_json(&[
        "--data-dir",
        &owner_arg,
        "approvals",
        "approve",
        "--agent",
        "agent-writer",
        &draft_id,
        "--note",
        "smoke test",
    ]);
    assert_eq!(approved["state"], "approved");
    assert_eq!(approved["approval"]["actor"], "owner");

    let image_path = root.join("tiny.png");
    std::fs::write(&image_path, b"not really a png but enough bytes").expect("write tiny image");
    let image = run_json(&[
        "--data-dir",
        &agent_profile,
        "messages",
        "send-image",
        &format!("lxmf:{allowed_image}"),
        "--file",
        &path_arg(&image_path),
        "--name",
        "tiny",
        "--mime",
        "image/png",
        "--client-action-id",
        "image-allowed-1",
    ]);
    assert_eq!(image["kind"], "message.image");
    assert_eq!(image["state"], "draft");
    assert_eq!(image["staged_files"][0]["stored_path"], "<redacted>");
    assert_eq!(image["staged_files"][0]["mime"], "image/png");
    let image_id = image["id"].as_str().expect("image action id").to_string();
    let inspected = run_json(&[
        "--data-dir",
        &owner_arg,
        "approvals",
        "inspect-file",
        "--agent",
        "agent-writer",
        &image_id,
    ]);
    assert_eq!(inspected["file"]["mime"], "image/png");
    assert_eq!(
        inspected["file"]["sha256"],
        image["staged_files"][0]["sha256"]
    );

    let staged_root = Path::new(&agent_profile)
        .join(".ratspeak")
        .join("agent-actions")
        .join("staged-files");
    let staged_count = std::fs::read_dir(&staged_root)
        .map(|entries| entries.count())
        .unwrap_or(0);
    let image_retry = run_json(&[
        "--data-dir",
        &agent_profile,
        "messages",
        "send-image",
        &format!("lxmf:{allowed_image}"),
        "--file",
        &path_arg(&image_path),
        "--name",
        "tiny",
        "--mime",
        "image/png",
        "--client-action-id",
        "image-allowed-1",
    ]);
    assert_eq!(image_retry["id"], image_id);
    let staged_after_retry = std::fs::read_dir(&staged_root)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(staged_after_retry, staged_count);
    assert!(
        run_fail(&[
            "--data-dir",
            &agent_profile,
            "messages",
            "send-file",
            &format!("lxmf:{allowed_image}"),
            "--file",
            &path_arg(&image_path),
            "--mime",
            "application/x-ratspeak-test",
        ])
        .contains("policy_denied")
    );
    let staged_after_reject = std::fs::read_dir(&staged_root)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(staged_after_reject, staged_count);

    let announce = run_json(&[
        "--data-dir",
        &agent_profile,
        "network",
        "announce",
        "--reason",
        "smoke",
    ]);
    assert_eq!(announce["kind"], "identity.announce");
    assert_eq!(announce["state"], "pending_approval");

    let contact_add = run_json(&[
        "--data-dir",
        &agent_profile,
        "contacts",
        "add",
        allowed_image,
        "--display-name",
        "new contact",
    ]);
    assert_eq!(contact_add["kind"], "contact.add");
    assert_eq!(contact_add["state"], "pending_approval");

    assert!(
        run_fail(&[
            "--data-dir",
            &agent_profile,
            "messages",
            "draft",
            &format!("lxmf:{denied}"),
            "--text",
            "not allowed",
        ])
        .contains("forbidden")
    );

    let audit = run_json(&[
        "--data-dir",
        &owner_arg,
        "audit",
        "list",
        "--agent",
        "agent-writer",
        "--limit",
        "100",
    ]);
    let audit_rows = audit["audit"].as_array().expect("audit rows");
    assert!(
        audit_rows
            .iter()
            .any(|row| row["event"] == "action.submitted")
    );
    assert!(
        audit_rows
            .iter()
            .any(|row| row["event"] == "action.approved")
    );
    assert!(audit_rows.iter().any(|row| row["event"] == "grant.created"));
    assert!(audit_rows.iter().any(|row| row["event"] == "policy.denied"));

    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(root);
}
