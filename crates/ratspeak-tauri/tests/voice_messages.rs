//! Serialized voice IPC, without opening a microphone or physical speaker.
#![cfg(feature = "lxst-voice")]
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ratspeak_tauri::{
    commands::voice::*, config::DashboardConfig, db, lxmf::LxmfManager, state::AppState,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::test::{MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets};

struct Fixture {
    window: tauri::WebviewWindow<MockRuntime>,
    _app: tauri::App<MockRuntime>,
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init_pool(dir.path()).unwrap();
        db::init_schema(&pool).unwrap();
        db::save_identity(&pool, "alice", &"11".repeat(16), "Alice", "Alice");
        db::save_identity(&pool, "bob", &"22".repeat(16), "Bob", "Bob");
        db::set_active_identity(&pool, "alice").unwrap();
        let state = Arc::new(AppState::new(
            DashboardConfig::from_env_and_defaults(dir.path().to_owned()),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        *state.lxmf.lock().unwrap() =
            Some(LxmfManager::load_or_create(dir.path(), None, None).unwrap());
        let app = mock_builder()
            .manage(state.clone())
            .invoke_handler(tauri::generate_handler![
                voice_memo_format,
                voice_memo_decode_data,
                voice_memo_decode_stored,
                voice_memo_inspect_stored,
            ])
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        Self {
            window,
            _app: app,
            state,
            _dir: dir,
        }
    }
    fn invoke(&self, cmd: &str, args: Value) -> Result<Value, Value> {
        get_ipc_response(
            &self.window,
            tauri::webview::InvokeRequest {
                cmd: cmd.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(any(windows, target_os = "android")) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(json!({"args":args})),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    }
}
#[test]
fn voice_format_ipc_is_explicit_and_rejects_stale_identity_and_invalid_destination() {
    let f = Fixture::new();
    let peer = "ab".repeat(16);
    let current = f
        .invoke("voice_memo_format", json!({"dest_hash":peer}))
        .unwrap();
    assert_eq!(current["audio_mode"], 16);
    assert_eq!(current["identity_id"], "alice");
    assert!(
        f.invoke(
            "voice_memo_format",
            json!({"dest_hash":peer,"audio_mode":3})
        )
        .is_err()
    );
    assert!(
        f.invoke(
            "voice_memo_format",
            json!({"dest_hash":"my T-Deck","identity_id":"alice","audio_mode":3})
        )
        .is_err()
    );
    assert!(
        f.invoke(
            "voice_memo_format",
            json!({"dest_hash":peer,"identity_id":"alice","audio_mode":9})
        )
        .is_err()
    );
    assert_eq!(
        f.invoke(
            "voice_memo_format",
            json!({"dest_hash":peer,"identity_id":"alice","audio_mode":3})
        )
        .unwrap()["audio_mode"],
        3
    );
    db::set_active_identity(&f.state.db, "bob").unwrap();
    f.state.bump_identity_session_generation();
    assert!(
        f.invoke(
            "voice_memo_format",
            json!({"dest_hash":peer,"identity_id":"alice","audio_mode":16})
        )
        .is_err()
    );
    assert_eq!(
        f.invoke("voice_memo_format", json!({"dest_hash":peer}))
            .unwrap()["audio_mode"],
        16
    );
    assert_eq!(
        db::voice_message_format(&f.state.db, "alice", &peer).unwrap(),
        3
    );
}
#[test]
fn voice_decode_ipc_uses_explicit_draft_mode_and_stored_identity_metadata() {
    let f = Fixture::new();
    let bytes = vec![0x42; 100];
    let encoded = B64.encode(&bytes);
    assert!(
        f.invoke("voice_memo_decode_data", json!({"data_base64":encoded}))
            .is_err()
    );
    assert!(
        f.invoke(
            "voice_memo_decode_data",
            json!({"data_base64":encoded,"audio_mode":4})
        )
        .is_err()
    );
    assert!(
        f.invoke(
            "voice_memo_decode_data",
            json!({"data_base64":B64.encode([1,2,3]),"audio_mode":3})
        )
        .is_err()
    );
    let decoded = f
        .invoke(
            "voice_memo_decode_data",
            json!({"data_base64":encoded,"audio_mode":3}),
        )
        .unwrap();
    assert_eq!(decoded["duration_ms"], 1000);
    assert_eq!(decoded["sample_rate_hz"], 8000);
    assert_eq!(
        B64.decode(decoded["data_base64"].as_str().unwrap())
            .unwrap()
            .len(),
        16044
    );
    let stored = f
        .state
        .lxmf
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .save_attachment("Voice message.c2raw", &bytes)
        .unwrap();
    assert!(
        f.invoke(
            "voice_memo_decode_stored",
            json!({"stored_name":stored,"audio_mode":3})
        )
        .is_err()
    );
    f.state.db.get().unwrap().execute("INSERT INTO messages(id,source,destination,timestamp,identity_id,audio_mode,audio_stored_name) VALUES('clip','s','d',1,'alice',3,?1)",[&stored]).unwrap();
    assert_eq!(
        f.invoke("voice_memo_inspect_stored", json!({"stored_name":stored}))
            .unwrap()["duration_ms"],
        1000
    );
    // Supplied format cannot relabel a signed message's stored audio field.
    assert_eq!(
        f.invoke(
            "voice_memo_decode_stored",
            json!({"stored_name":stored,"audio_mode":16})
        )
        .unwrap()["sample_rate_hz"],
        8000
    );
    db::set_active_identity(&f.state.db, "bob").unwrap();
    f.state.bump_identity_session_generation();
    assert!(
        f.invoke(
            "voice_memo_decode_stored",
            json!({"stored_name":stored,"audio_mode":3})
        )
        .is_err()
    );
}
