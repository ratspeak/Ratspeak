//! Releasing what controls an identity: through the IPC surface and its
//! invoke gate, as the window asks for it. The vault runs one job per process,
//! so these tests take turns.

use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ratspeak_tauri::{config::DashboardConfig, db, state::AppState};
use rns_identity::identity::Identity;
use serde_json::{Value, json};
use tauri::test::{
    INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
};

const PASSCODE: &str = "correct horse";

static VAULT: Mutex<()> = Mutex::new(());

struct Fixture {
    state: Arc<AppState>,
    window: tauri::WebviewWindow<MockRuntime>,
    app: tauri::App<MockRuntime>,
    _root: tempfile::TempDir,
    _vault: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let vault = VAULT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().to_path_buf();
        ratspeak_runtime::lxmf::initialize_storage_namespace(&data_root).unwrap();
        let config = DashboardConfig {
            data_root: data_root.clone(),
            data_dir: data_root.join(".ratspeak"),
            rns_config_dir: data_root.join("rns"),
            rns_config_dir_overridden: true,
            max_log_entries: 20,
        };
        let pool = db::init_pool(&data_root).unwrap();
        db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        let app = mock_builder()
            .manage(Arc::clone(&state))
            .invoke_handler(ratspeak_tauri::invoke_gate::gated(
                tauri::generate_handler![
                    ratspeak_tauri::commands::identity::set_identity_passcode,
                    ratspeak_tauri::commands::identity::reveal_identity_mnemonic,
                    ratspeak_tauri::commands::identity::api_export_identity_reticulum_base64,
                    ratspeak_tauri::commands::identity::api_export_identity_reticulum_base32,
                    ratspeak_tauri::commands::identity::api_export_identity_backup_base64,
                    ratspeak_tauri::commands::network_ownership::export_shared_access,
                ],
            ))
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        Self {
            state,
            window,
            app,
            _root: root,
            _vault: vault,
        }
    }

    fn call(&self, command: &str, args: Value) -> Result<Value, Value> {
        get_ipc_response(
            &self.window,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(any(windows, target_os = "android")) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(args),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.into(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    }

    /// A plain identity on disk and in the identity table, made the active one.
    fn activate_new(&self, nickname: &str) -> String {
        let key = Identity::new().get_private_key().unwrap().to_vec();
        let (hash, _) = ratspeak_runtime::lxmf::LxmfManager::import_identity_to_data_dir(
            &self.state.config.data_dir,
            &key,
            nickname,
            &self.state.db,
        )
        .unwrap();
        db::set_active_identity(&self.state.db, &hash).unwrap();
        hash
    }

    /// `identity` on disk and in the identity table, with `phrase` stored as
    /// its recovery phrase.
    fn add(&self, identity: &Identity, phrase: Option<&str>) -> String {
        let key = identity.get_private_key().unwrap().to_vec();
        let (hash, _) = ratspeak_runtime::lxmf::LxmfManager::import_identity_to_data_dir(
            &self.state.config.data_dir,
            &key,
            "Fir",
            &self.state.db,
        )
        .unwrap();
        if let Some(phrase) = phrase {
            ratspeak_runtime::vault::store_plaintext_seed(&self.profile(&hash), phrase).unwrap();
        }
        hash
    }

    fn profile(&self, identity: &str) -> std::path::PathBuf {
        self.state.config.data_dir.join("identities").join(identity)
    }

    /// `hash` active and loaded: its key in memory, as after an unlock.
    fn load(&self, hash: &str) {
        db::set_active_identity(&self.state.db, hash).unwrap();
        let manager = ratspeak_runtime::lxmf::LxmfManager::load_or_create(
            &self.state.config.data_root,
            Some(hash),
            None,
        )
        .unwrap();
        self.state.set_lxmf(manager);
    }

    /// The active `identity` locked: no key in memory.
    fn lock(&self, hash: &str) {
        *self.state.lxmf.lock().unwrap() = None;
        self.state.set_hw_locked(Some(hash.to_owned()));
    }

    /// A backup's key and recovery phrase, opened with its own PIN.
    fn open_backup(&self, backup: &Value) -> (Vec<u8>, Option<String>) {
        let bytes = B64
            .decode(backup["backup_base64"].as_str().unwrap())
            .unwrap();
        let envelope: Value = serde_json::from_slice(&bytes).unwrap();
        let sealed: ratspeak_runtime::vault::EncryptedVault =
            serde_json::from_value(envelope["vault"].clone()).unwrap();
        let opened = ratspeak_runtime::vault::decrypt_identity(BACKUP_PIN, &sealed).unwrap();
        (opened.key().to_vec(), opened.mnemonic().map(str::to_owned))
    }

    fn protect(&self, identity: &str) {
        self.call(
            "set_identity_passcode",
            json!({ "args": { "hash": identity, "passcode": PASSCODE } }),
        )
        .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // MockRuntime never destroys the window, and its cycle with the app keeps this state alive.
        #[allow(deprecated)]
        drop(tauri::Manager::unmanage::<Arc<AppState>>(&self.app));
    }
}

/// Refused before the network is asked: the export's authorization failed.
fn refused(result: Result<Value, Value>) -> Value {
    let error = result.expect_err("released without authorization");
    assert_ne!(error["message"], "Network is not running", "{error}");
    error
}

/// Authorized: the export goes on to the network, which these tests don't run.
fn authorized(result: Result<Value, Value>) {
    let error = result.expect_err("no network runs here");
    assert_eq!(error["message"], "Network is not running", "{error}");
}

// The shared instance's control key is an admin credential: copying it takes
// the active identity's PIN, or a confirmation when it has none.
#[test]
fn the_shared_instance_key_needs_the_pin_or_a_confirmation() {
    let f = Fixture::new();
    db::set_setting(&f.state.db, "developer_mode_enabled", "true");

    let protected = f.activate_new("Protected");
    f.protect(&protected);
    assert_eq!(
        refused(f.call("export_shared_access", json!({})))["code"],
        "unauthorized"
    );
    assert_eq!(
        refused(f.call(
            "export_shared_access",
            json!({ "passcode": "wrong horse", "confirmed": true })
        ))["code"],
        "unauthorized"
    );
    authorized(f.call("export_shared_access", json!({ "passcode": PASSCODE })));

    f.activate_new("Plain");
    refused(f.call("export_shared_access", json!({})));
    refused(f.call("export_shared_access", json!({ "confirmed": false })));
    authorized(f.call("export_shared_access", json!({ "confirmed": true })));
}

const BACKUP_PIN: &str = "backup horse";
const PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// The private key an export carries, decoded.
fn exported_key(export: &Value) -> Vec<u8> {
    B64.decode(export["data_base64"].as_str().unwrap()).unwrap()
}

// An unlocked passcode identity's key leaves only with its passcode, from its
// vault, so a locked one exports the same way; a backup also takes the
// identity's passcode, beside its own, and keeps the recovery phrase.
#[test]
fn a_passcode_identitys_key_and_backup_need_its_passcode() {
    let f = Fixture::new();
    let identity = Identity::new();
    let protected = f.add(&identity, Some(PHRASE));
    // Loaded, then protected: unlocked, as after its passcode was entered.
    f.load(&protected);
    f.protect(&protected);
    let key = identity.get_private_key().unwrap().to_vec();

    for command in [
        "api_export_identity_reticulum_base64",
        "api_export_identity_reticulum_base32",
    ] {
        let refused = f
            .call(command, json!({ "hashHex": protected }))
            .unwrap_err();
        assert_eq!(refused["code"], "unauthorized", "{command}");
        let refused = f
            .call(
                command,
                json!({ "hashHex": protected, "passcode": "wrong horse", "confirmed": true }),
            )
            .unwrap_err();
        assert_eq!(refused["code"], "unauthorized", "{command}");
    }
    let export = f
        .call(
            "api_export_identity_reticulum_base64",
            json!({ "hashHex": protected, "passcode": PASSCODE }),
        )
        .unwrap();
    assert_eq!(exported_key(&export), key);

    let backup = |identity_passcode: Option<&str>| {
        f.call(
            "api_export_identity_backup_base64",
            json!({ "hashHex": protected, "passcode": BACKUP_PIN, "identityPasscode": identity_passcode }),
        )
    };
    assert_eq!(backup(None).unwrap_err()["code"], "unauthorized");
    assert_eq!(
        backup(Some("wrong horse")).unwrap_err()["code"],
        "unauthorized"
    );
    let sealed = backup(Some(PASSCODE)).unwrap();
    let (backup_key, phrase) = f.open_backup(&sealed);
    assert_eq!(backup_key, key);
    assert_eq!(
        phrase.as_deref(),
        Some(PHRASE),
        "the backup lost the phrase"
    );

    // Locked: no key in memory, the vault still answers its passcode.
    f.lock(&protected);
    let export = f
        .call(
            "api_export_identity_reticulum_base32",
            json!({ "hashHex": protected, "passcode": PASSCODE }),
        )
        .unwrap();
    assert!(export["text"].as_str().is_some_and(|text| !text.is_empty()));
}

// An identity without a passcode has no secret to check, so its key leaves
// only after an explicit confirmation.
#[test]
fn an_open_identitys_key_and_backup_need_a_confirmation() {
    let f = Fixture::new();
    let identity = Identity::new();
    let open = f.add(&identity, None);
    f.load(&open);
    let key = identity.get_private_key().unwrap().to_vec();

    for confirmed in [None, Some(false)] {
        let refused = f
            .call(
                "api_export_identity_reticulum_base64",
                json!({ "hashHex": open, "confirmed": confirmed }),
            )
            .unwrap_err();
        assert_eq!(refused["code"], "bad_request");
        let refused = f
            .call(
                "api_export_identity_backup_base64",
                json!({ "hashHex": open, "passcode": BACKUP_PIN, "confirmed": confirmed }),
            )
            .unwrap_err();
        assert_eq!(refused["code"], "bad_request");
    }
    let export = f
        .call(
            "api_export_identity_reticulum_base64",
            json!({ "hashHex": open, "confirmed": true }),
        )
        .unwrap();
    assert_eq!(exported_key(&export), key);
    let sealed = f
        .call(
            "api_export_identity_backup_base64",
            json!({ "hashHex": open, "passcode": BACKUP_PIN, "confirmed": true }),
        )
        .unwrap();
    assert_eq!(f.open_backup(&sealed).0, key);
}

// A recovery phrase is shown only for the active identity: with its passcode,
// or after a confirmation when it has none.
#[test]
fn only_the_active_identitys_phrase_is_shown() {
    let f = Fixture::new();
    let other = f.add(&Identity::new(), Some(PHRASE));
    let active = f.add(&Identity::new(), Some(PHRASE));
    db::set_active_identity(&f.state.db, &active).unwrap();

    let reveal = |hash: &str, confirmed: Option<bool>| {
        f.call(
            "reveal_identity_mnemonic",
            json!({ "args": { "hash": hash, "confirmed": confirmed } }),
        )
    };
    assert_eq!(reveal(&other, Some(true)).unwrap_err()["code"], "forbidden");
    assert_eq!(reveal(&active, None).unwrap_err()["code"], "bad_request");
    assert_eq!(reveal(&active, Some(true)).unwrap()["mnemonic"], PHRASE);

    f.protect(&active);
    let refused = reveal(&active, Some(true)).unwrap_err();
    assert_eq!(
        refused["code"], "unauthorized",
        "a confirmation stood in for the PIN"
    );
    let shown = f
        .call(
            "reveal_identity_mnemonic",
            json!({ "args": { "hash": active, "passcode": PASSCODE } }),
        )
        .unwrap();
    assert_eq!(shown["mnemonic"], PHRASE);
}

// A hardware identity's key never leaves its token, confirmed or not.
#[test]
fn a_hardware_identitys_key_is_never_exported() {
    let f = Fixture::new();
    let hash = "cccccccccccccccccccccccccccccccc";
    std::fs::create_dir_all(f.profile(hash)).unwrap();
    std::fs::write(f.profile(hash).join("identity.hwid"), b"{}").unwrap();
    db::save_identity(
        &f.state.db,
        hash,
        "dddddddddddddddddddddddddddddddd",
        "H",
        "H",
    );
    db::set_active_identity(&f.state.db, hash).unwrap();
    for command in [
        "api_export_identity_reticulum_base64",
        "api_export_identity_reticulum_base32",
        "api_export_identity_backup_base64",
    ] {
        let refused = f
            .call(
                command,
                json!({ "hashHex": hash, "confirmed": true, "passcode": BACKUP_PIN }),
            )
            .unwrap_err();
        assert_eq!(refused["code"], "bad_request", "{command}");
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|message| message.contains("Hardware-backed identity")),
            "{command}: {refused}"
        );
    }
}
