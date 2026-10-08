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
        Self::with_emitter(Arc::new(ratspeak_core::NoopEmitter))
    }
    fn with_emitter(emitter: Arc<dyn ratspeak_core::Emitter>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init_pool(dir.path()).unwrap();
        db::init_schema(&pool).unwrap();
        db::save_identity(&pool, "alice", &"11".repeat(16), "Alice", "Alice");
        db::save_identity(&pool, "bob", &"22".repeat(16), "Bob", "Bob");
        db::set_active_identity(&pool, "alice").unwrap();
        let state = Arc::new(AppState::new(
            DashboardConfig::from_env_and_defaults(dir.path().to_owned()),
            pool,
            emitter,
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
fn voice_format_ipc_is_automatic_and_ignores_legacy_preferences() {
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
            json!({"dest_hash":peer,"identity_id":"alice","audio_mode":3})
        )
        .is_err()
    );
    assert!(
        f.invoke("voice_memo_format", json!({"dest_hash":"my T-Deck"}))
            .is_err()
    );
    db::set_voice_message_format(&f.state.db, "alice", &peer, 3).unwrap();
    assert_eq!(
        f.invoke("voice_memo_format", json!({"dest_hash":peer}))
            .unwrap()["audio_mode"],
        16
    );
    db::touch_identity_activity(&f.state.db, &[(peer.clone(), 1.0, None, None)]);
    assert!(db::set_identity_lxmf_compression_support(
        &f.state.db,
        &peer,
        db::LXMF_COMPRESSION_SUPPORT_UNSUPPORTED
    ));
    assert_eq!(
        f.invoke("voice_memo_format", json!({"dest_hash":peer}))
            .unwrap()["audio_mode"],
        3
    );
    db::set_active_identity(&f.state.db, "bob").unwrap();
    f.state.bump_identity_session_generation();
    assert!(
        f.invoke(
            "voice_memo_format",
            json!({"dest_hash":peer,"identity_id":"alice"})
        )
        .is_err()
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

fn stage_compact(f: &Fixture) -> String {
    let token = f
        .state
        .begin_attachment_staging(
            "Voice message.c2raw".into(),
            "application/octet-stream".into(),
            100,
            false,
        )
        .unwrap();
    f.state
        .append_attachment_staging(&token, 0, &[0x42; 100])
        .unwrap();
    token
}
fn send_args(token: String, client: bool) -> SendLxmfVoiceMessageArgs {
    SendLxmfVoiceMessageArgs {
        dest_hash: "ab".repeat(16),
        staging_token: token,
        delivery_method: Some("direct".into()),
        client_msg_id: client.then(|| format!("out_{}", "01".repeat(16))),
    }
}
#[tokio::test]
async fn voice_stage_cannot_cross_identity_changes_with_or_without_client_id() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use tauri::Manager;
    for (client, return_to_alice) in [(true, false), (false, false), (true, true), (false, true)] {
        let f = Fixture::new();
        let token = stage_compact(&f);
        let mut send = Box::pin(send_lxmf_voice_message(
            f.window.state(),
            send_args(token.clone(), client),
        ));
        poll_fn(|cx| {
            assert!(send.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(f.state.take_completed_attachment_staging(&token).is_none());
        f.state.bump_identity_session_generation();
        f.state.clear_identity_scoped_runtime_state();
        db::set_active_identity(&f.state.db, "bob").unwrap();
        let replacement = f._dir.path().join("replacement-runtime");
        std::fs::create_dir_all(&replacement).unwrap();
        *f.state.lxmf.lock().unwrap() =
            Some(LxmfManager::load_or_create(&replacement, None, None).unwrap());
        if return_to_alice {
            f.state.bump_identity_session_generation();
            db::set_active_identity(&f.state.db, "alice").unwrap();
        }
        assert!(send.await.is_err());
        for identity in ["alice", "bob"] {
            assert!(db::get_conversation(&f.state.db, &"ab".repeat(16), identity, 10).is_empty());
        }
        assert!(f.state.take_completed_attachment_staging(&token).is_none());
    }
}
#[tokio::test]
async fn definite_voice_prequeue_failure_keeps_exact_clip_then_queues_once() {
    use tauri::Manager;
    let f = Fixture::new();
    let token = stage_compact(&f);
    let manager = f.state.lxmf.lock().unwrap().take();
    let error = send_lxmf_voice_message(f.window.state(), send_args(token.clone(), true))
        .await
        .unwrap_err();
    assert_eq!(error.code, "voice_retryable");
    assert!(db::get_conversation(&f.state.db, &"ab".repeat(16), "alice", 10).is_empty());
    *f.state.lxmf.lock().unwrap() = manager;
    let result = send_lxmf_voice_message(f.window.state(), send_args(token.clone(), true))
        .await
        .unwrap();
    assert!(result["msg_id"].is_string());
    let rows = db::get_conversation(&f.state.db, &"ab".repeat(16), "alice", 10);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["audio"]["mode"], 3);
    let name = rows[0]["audio"]["stored_name"].as_str().unwrap();
    let path = f
        .state
        .lxmf
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .get_received_file(name)
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), [0x42; 100]);
    assert!(
        send_lxmf_voice_message(f.window.state(), send_args(token, true))
            .await
            .is_err()
    );
    assert_eq!(
        db::get_conversation(&f.state.db, &"ab".repeat(16), "alice", 10).len(),
        1
    );
}
#[tokio::test]
async fn cancelled_taken_voice_stage_never_reappears_after_failure() {
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use tauri::Manager;
    let f = Fixture::new();
    let token = stage_compact(&f);
    let mut send = Box::pin(send_lxmf_voice_message(
        f.window.state(),
        send_args(token.clone(), false),
    ));
    poll_fn(|cx| {
        assert!(send.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(f.state.cancel_attachment_staging(&token));
    assert!(send.await.is_err());
    assert!(f.state.take_completed_attachment_staging(&token).is_none());
    assert!(db::get_conversation(&f.state.db, &"ab".repeat(16), "alice", 10).is_empty());
}

#[tokio::test]
async fn admitted_voice_worker_retains_identity_through_notification_after_ipc_drop() {
    use std::sync::Mutex;
    use std::time::Duration;
    use tauri::Manager;
    struct GateEmitter {
        entered: tokio::sync::Notify,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl ratspeak_core::Emitter for GateEmitter {
        fn try_emit(&self, event: &str, payload: Value) -> Result<(), ratspeak_core::EmitError> {
            if event == "lxmf_step" && payload["step"] == "sending" {
                self.entered.notify_one();
                self.release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            Ok(())
        }
    }
    for with_client_id in [false, true] {
        let (release, receive) = std::sync::mpsc::channel();
        let emitter = Arc::new(GateEmitter {
            entered: tokio::sync::Notify::new(),
            release: Mutex::new(receive),
        });
        let f = Fixture::with_emitter(emitter.clone());
        let token = stage_compact(&f);
        let mut send = Box::pin(send_lxmf_voice_message(
            f.window.state(),
            send_args(token.clone(), with_client_id),
        ));
        tokio::select! {
            result = &mut send => panic!("send completed before its publication gate: {result:?}"),
            _ = emitter.entered.notified() => {}
        }
        assert_eq!(
            db::get_conversation(&f.state.db, &"ab".repeat(16), "alice", 10).len(),
            1
        );
        drop(send); // Dropping IPC must not drop the detached worker's lifecycle protection.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                f.state.identity_switch_lock.lock()
            )
            .await
            .is_err(),
            "identity replacement must wait until queued voice publication completes"
        );
        release.send(()).unwrap();
        let _identity =
            tokio::time::timeout(Duration::from_secs(2), f.state.identity_switch_lock.lock())
                .await
                .unwrap();
        f.state.bump_identity_session_generation();
        f.state.clear_identity_scoped_runtime_state();
        db::set_active_identity(&f.state.db, "bob").unwrap();
        assert!(db::get_conversation(&f.state.db, &"ab".repeat(16), "bob", 10).is_empty());
        assert!(f.state.take_completed_attachment_staging(&token).is_none());
    }
}
