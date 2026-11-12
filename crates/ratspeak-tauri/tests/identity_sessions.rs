//! Identity switches and removals with real runtimes. Reticulum runs with no
//! interfaces and shares no instance. Runtimes take process-wide locks, so
//! these tests live in their own binary.

use std::sync::{Arc, Mutex};

use ratspeak_core::EmitError;
use ratspeak_tauri::{config::DashboardConfig, db, state::AppState};
use serde_json::Value;
use tauri::Manager;
use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
    /// Holds the emitting thread at this event until the test lets it go.
    hold: Mutex<Option<(&'static str, Arc<std::sync::Barrier>)>>,
}

impl ratspeak_core::Emitter for Recorder {
    fn try_emit(&self, event: &str, payload: Value) -> Result<(), EmitError> {
        self.events
            .lock()
            .unwrap()
            .push((event.to_owned(), payload));
        let hold = self
            .hold
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(held, _)| *held == event)
            .map(|(_, barrier)| Arc::clone(barrier));
        if let Some(barrier) = hold {
            barrier.wait();
            barrier.wait();
        }
        Ok(())
    }
}

impl Recorder {
    fn last(&self, name: &str) -> Option<Value> {
        let events = self.events.lock().unwrap();
        events
            .iter()
            .rev()
            .find(|(event, _)| event == name)
            .map(|(_, payload)| payload.clone())
    }
}

/// Counts how often the shown alerts were withdrawn.
#[derive(Default)]
struct Withdrawals(std::sync::atomic::AtomicUsize);

impl ratspeak_core::NativeNotifier for Withdrawals {
    fn notify(&self, _notification: ratspeak_core::NativeNotification) {}
    fn withdraw_all(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Withdrawals {
    fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

struct Fixture {
    app: tauri::App<MockRuntime>,
    state: Arc<AppState>,
    recorder: Arc<Recorder>,
    withdrawals: Arc<Withdrawals>,
    runtime: tokio::runtime::Runtime,
    _root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().to_path_buf();
        ratspeak_runtime::lxmf::initialize_storage_namespace(&data_root).unwrap();
        let rns_dir = data_root.join("rns");
        std::fs::create_dir_all(&rns_dir).unwrap();
        std::fs::write(
            rns_dir.join("config"),
            "[reticulum]\nshare_instance = No\nenable_transport = No\n\n[interfaces]\n",
        )
        .unwrap();
        let config = DashboardConfig {
            data_root: data_root.clone(),
            data_dir: data_root.join(".ratspeak"),
            rns_config_dir: rns_dir,
            rns_config_dir_overridden: true,
            max_log_entries: 20,
        };
        let pool = db::init_pool(&data_root).unwrap();
        db::init_schema(&pool).unwrap();
        let recorder = Arc::new(Recorder::default());
        let withdrawals = Arc::new(Withdrawals::default());
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::clone(&recorder) as Arc<dyn ratspeak_core::Emitter>,
            Arc::clone(&withdrawals) as Arc<dyn ratspeak_core::NativeNotifier>,
        ));
        let app = mock_builder()
            .manage(Arc::clone(&state))
            .build(mock_context(noop_assets()))
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        Self {
            app,
            state,
            recorder,
            withdrawals,
            runtime,
            _root: root,
        }
    }

    /// A plain identity on disk and in the identity table, never activated.
    fn import(&self, nickname: &str) -> String {
        let key = rns_identity::identity::Identity::new()
            .get_private_key()
            .unwrap()
            .to_vec();
        ratspeak_runtime::lxmf::LxmfManager::import_identity_to_data_dir(
            &self.state.config.data_dir,
            &key,
            nickname,
            &self.state.db,
        )
        .unwrap()
        .0
    }

    /// Protect `identity` with `passcode`; from then on it opens locked.
    fn protect(&self, identity: &str, passcode: &str) {
        self.runtime
            .block_on(ratspeak_tauri::commands::identity::set_identity_passcode(
                self.app.state(),
                ratspeak_tauri::commands::identity::SetPasscodeArgs {
                    hash: identity.to_owned(),
                    passcode: passcode.to_owned(),
                    current: None,
                },
            ))
            .unwrap();
    }

    /// Start whichever identity is active, as the app does at launch.
    fn launch(&self) {
        self.runtime.block_on(ratspeak_runtime::init_rns_lxmf(
            Arc::clone(&self.state),
            self.state.config.data_root.clone(),
        ));
    }

    fn start(&self, identity: &str) {
        db::set_active_identity(&self.state.db, identity).unwrap();
        self.runtime.block_on(ratspeak_runtime::init_rns_lxmf(
            Arc::clone(&self.state),
            self.state.config.data_root.clone(),
        ));
        assert_eq!(self.loaded().as_deref(), Some(identity));
    }

    fn profile(&self, identity: &str) -> std::path::PathBuf {
        self.state.config.data_dir.join("identities").join(identity)
    }

    fn active(&self) -> Option<String> {
        db::get_active_identity(&self.state.db)
            .unwrap()
            .and_then(|row| row["hash"].as_str().map(str::to_owned))
    }

    fn loaded(&self) -> Option<String> {
        self.state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .map(|mgr| mgr.identity_hash.clone())
    }

    fn running(&self) -> bool {
        self.state.rns.read().unwrap().is_some()
    }

    #[cfg(feature = "hardware")]
    fn set_last_used(&self, identity: &str, at: f64) {
        self.state
            .db
            .get()
            .unwrap()
            .execute(
                "UPDATE identities SET last_used = ?1 WHERE hash = ?2",
                rusqlite::params![at, identity],
            )
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self
            .runtime
            .block_on(ratspeak_runtime::shutdown_rns_lxmf(&self.state));
    }
}

const PASSCODE: &str = "correct horse";

/// The vault runs one encryption job per process: tests that use it take turns.
static VAULT: Mutex<()> = Mutex::new(());

// A passcode identity unlocked once asks for its passcode again after a
// switch away and back; a restart of the same identity keeps the unlock.
#[test]
fn switching_back_to_an_unlocked_passcode_identity_asks_for_its_passcode() {
    let _vault = VAULT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let a = f.import("A");
    let b = f.import("B");
    f.protect(&a, PASSCODE);
    db::set_active_identity(&f.state.db, &a).unwrap();
    f.launch();
    assert_eq!(f.state.hw_locked_hash().as_deref(), Some(a.as_str()));
    let unlocked = f
        .runtime
        .block_on(ratspeak_tauri::commands::identity::unlock_identity(
            f.app.state(),
            PASSCODE.into(),
        ))
        .unwrap();
    assert_eq!(unlocked["ok"], true, "{unlocked}");
    assert_eq!(f.loaded().as_deref(), Some(a.as_str()));

    f.runtime
        .block_on(ratspeak_runtime::restart_rns_lxmf(Arc::clone(&f.state)))
        .unwrap();
    assert_eq!(
        f.loaded().as_deref(),
        Some(a.as_str()),
        "a restart asked for the passcode"
    );

    f.runtime
        .block_on(ratspeak_tauri::commands::identity::switch_identity(
            f.app.state(),
            b.clone(),
        ))
        .unwrap();
    assert_eq!(f.loaded().as_deref(), Some(b.as_str()));
    let back = f
        .runtime
        .block_on(ratspeak_tauri::commands::identity::switch_identity(
            f.app.state(),
            a.clone(),
        ))
        .unwrap();

    assert_eq!(back["locked"], true, "A opened without its passcode");
    assert_eq!(f.state.hw_locked_hash().as_deref(), Some(a.as_str()));
    assert!(f.loaded().is_none());
    let locked = f.recorder.last("hardware_locked").unwrap();
    assert_eq!(locked["hash"], a.as_str());
    assert_eq!(locked["kind"], "passcode");
}

// A secret staged for one identity never opens another, and serves a single
// load.
#[test]
fn a_staged_secret_opens_only_the_identity_it_was_staged_for() {
    let _vault = VAULT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let f = Fixture::new();
    let a = f.import("A");
    let b = f.import("B");
    f.protect(&a, PASSCODE);
    f.protect(&b, PASSCODE);
    let secret = || ratspeak_runtime::Zeroizing::new(PASSCODE.to_owned());

    f.state.stage_pending_hw_pin(&a, secret());
    db::set_active_identity(&f.state.db, &b).unwrap();
    f.launch();
    assert_eq!(f.state.hw_locked_hash().as_deref(), Some(b.as_str()));
    assert!(f.loaded().is_none(), "B opened with A's secret");
    db::set_active_identity(&f.state.db, &a).unwrap();
    f.launch();
    assert_eq!(f.state.hw_locked_hash().as_deref(), Some(a.as_str()));
    assert!(f.loaded().is_none(), "the secret served a second load");

    f.state.stage_pending_hw_pin(&a, secret());
    f.launch();
    assert_eq!(f.loaded().as_deref(), Some(a.as_str()));
}

// A switch whose activation fails ends with its closing event and leaves
// the previous identity running, in a session of its own.
#[test]
fn a_failed_switch_ends_with_its_event_and_the_previous_identity_running() {
    for failure in ["an unloadable key", "a refused activation"] {
        let f = Fixture::new();
        let a = f.import("A");
        let b = f.import("B");
        f.start(&a);
        if failure == "an unloadable key" {
            std::fs::write(f.profile(&b).join("identity"), b"not a key").unwrap();
        } else {
            f.state
                .db
                .get()
                .unwrap()
                .execute_batch(&format!(
                    "CREATE TRIGGER refuse_b BEFORE UPDATE OF is_active ON identities \
                     WHEN NEW.hash = '{b}' AND NEW.is_active = 1 \
                     BEGIN SELECT RAISE(ABORT, 'refused'); END;"
                ))
                .unwrap();
        }
        let generation = f.state.current_identity_session_generation();
        let result = f
            .runtime
            .block_on(ratspeak_tauri::commands::identity::switch_identity(
                f.app.state(),
                b.clone(),
            ));
        assert!(result.is_err(), "{failure}");
        let closing = f
            .recorder
            .last("identity_switch_failed")
            .unwrap_or_else(|| panic!("{failure}: the switch never ended"));
        assert_eq!(closing["hash"], b.as_str(), "{failure}");
        assert!(
            closing["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty()),
            "{failure}"
        );
        assert!(f.recorder.last("identity_switched").is_none(), "{failure}");
        assert_eq!(f.active().as_deref(), Some(a.as_str()), "{failure}");
        assert_eq!(f.loaded().as_deref(), Some(a.as_str()), "{failure}");
        assert!(f.running(), "{failure}: the network stayed down");
        let resumed = f.state.current_identity_session_generation();
        assert!(
            resumed > generation + 1,
            "{failure}: the restored session reuses the failed switch's generation"
        );
        assert_eq!(closing["generation"], resumed.to_string(), "{failure}");
    }
}

// A switch ends the previous identity's session, and its alerts go as it
// begins.
#[test]
fn a_switch_withdraws_the_previous_identitys_alerts() {
    let f = Fixture::new();
    let a = f.import("A");
    let b = f.import("B");
    f.start(&a);
    assert_eq!(f.withdrawals.count(), 0);
    f.runtime
        .block_on(ratspeak_tauri::commands::identity::switch_identity(
            f.app.state(),
            b.clone(),
        ))
        .unwrap();
    assert_eq!(f.loaded().as_deref(), Some(b.as_str()));
    assert_eq!(f.withdrawals.count(), 1);
}

// The reset runs before the switch begins, so a refused one changes
// nothing: no new session, no switching state, the same identity running.
#[test]
fn a_switch_whose_activity_reset_is_refused_changes_nothing() {
    let f = Fixture::new();
    let a = f.import("A");
    let b = f.import("B");
    f.start(&a);
    f.runtime.block_on(f.state.activity.shutdown()).unwrap();
    let generation = f.state.current_identity_session_generation();
    let error = f
        .runtime
        .block_on(ratspeak_tauri::commands::identity::switch_identity(
            f.app.state(),
            b,
        ))
        .unwrap_err();
    assert_eq!(error.code, "service_unavailable");
    assert_eq!(f.state.current_identity_session_generation(), generation);
    assert!(f.recorder.last("identity_switching").is_none());
    assert!(f.recorder.last("identity_switch_failed").is_none());
    assert_eq!(f.active().as_deref(), Some(a.as_str()));
    assert_eq!(f.loaded().as_deref(), Some(a.as_str()));
    assert!(f.running());
}

// The Activity reset comes before the switch's session. Activity going
// away once the switch has begun cannot strand the previous runtime in that
// session: nothing after the reset refuses, and the switch completes.
#[test]
fn a_switch_that_began_completes_when_activity_goes_away() {
    let f = Fixture::new();
    let a = f.import("A");
    let b = f.import("B");
    f.start(&a);
    let previous_session = f.state.session_shutdown.read().unwrap().clone();
    let generation = f.state.current_identity_session_generation();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    *f.recorder.hold.lock().unwrap() = Some(("identity_switching", Arc::clone(&barrier)));
    let stopper = std::thread::spawn({
        let state = Arc::clone(&f.state);
        let runtime = f.runtime.handle().clone();
        move || {
            barrier.wait();
            runtime.block_on(state.activity.shutdown()).unwrap();
            barrier.wait();
        }
    });
    let result = f
        .runtime
        .block_on(ratspeak_tauri::commands::identity::switch_identity(
            f.app.state(),
            b.clone(),
        ));
    stopper.join().unwrap();
    *f.recorder.hold.lock().unwrap() = None;
    let switched = result.expect("a switch that began was refused");
    assert_eq!(switched["hash"], b.as_str());
    assert!(
        previous_session.is_triggered(),
        "the previous runtime kept running"
    );
    assert!(f.state.current_identity_session_generation() > generation);
    assert_eq!(f.active().as_deref(), Some(b.as_str()));
    assert_eq!(f.loaded().as_deref(), Some(b.as_str()));
    assert!(f.running());
    assert!(f.recorder.last("identity_switch_failed").is_none());
}

// A hardware activation that fails after the previous runtime stopped brings
// the previous identity back, in a session of its own, and ends with the
// switch's closing event, as a failed switch does.
#[cfg(feature = "hardware")]
#[test]
fn a_failed_hardware_activation_resumes_the_previous_identity() {
    let f = Fixture::new();
    let a = f.import("A");
    f.start(&a);
    let h = "cccccccccccccccccccccccccccccccc".to_string();
    std::fs::create_dir_all(f.profile(&h)).unwrap();
    std::fs::write(f.profile(&h).join("identity.hwid"), b"{}").unwrap();
    db::save_identity(
        &f.state.db,
        &h,
        "dddddddddddddddddddddddddddddddd",
        "H",
        "H",
    );
    f.state
        .db
        .get()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER refuse_h BEFORE UPDATE OF is_active ON identities \
             WHEN NEW.hash = '{h}' AND NEW.is_active = 1 \
             BEGIN SELECT RAISE(ABORT, 'refused'); END;"
        ))
        .unwrap();
    let generation = f.state.current_identity_session_generation();
    let result = f
        .runtime
        .block_on(ratspeak_tauri::commands::hardware::hw_activate_and_unlock(
            f.app.state(),
            h.clone(),
            "123456".into(),
        ));
    assert!(result.is_err());
    assert_eq!(
        f.withdrawals.count(),
        1,
        "the previous identity's alerts stayed"
    );
    let closing = f
        .recorder
        .last("identity_switch_failed")
        .expect("the failed activation never ended");
    assert_eq!(closing["hash"], h.as_str());
    assert_eq!(f.active().as_deref(), Some(a.as_str()));
    assert_eq!(f.loaded().as_deref(), Some(a.as_str()));
    assert!(f.running(), "the previous identity stayed down");
    let resumed = f.state.current_identity_session_generation();
    assert!(
        resumed > generation + 1,
        "the restored session reuses the failed activation's generation"
    );
    assert_eq!(closing["generation"], resumed.to_string());
}

/// What `hw_remove` finds: H, a hardware identity, is the active one.
#[cfg(feature = "hardware")]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Removal {
    /// S is plain and the most recently used.
    PlainS,
    /// S is passcode-protected.
    ProtectedS,
    /// S's key no longer loads; an older identity is next.
    UnusableS,
    /// H is the active row but neither loaded nor locked.
    ActiveOnly,
}

// After hw_remove(H), a plain S is active and running; a passcode S is
// active and locked. An S whose key doesn't load is passed over, and H counts
// as live while it is the active row.
#[cfg(feature = "hardware")]
#[test]
fn removing_the_live_hardware_identity_activates_the_most_recent_one() {
    for case in [
        Removal::PlainS,
        Removal::ProtectedS,
        Removal::UnusableS,
        Removal::ActiveOnly,
    ] {
        let f = Fixture::new();
        let older = f.import("Older");
        let s = f.import("S");
        match case {
            Removal::ProtectedS => std::fs::rename(
                f.profile(&s).join("identity"),
                f.profile(&s).join("identity.enc"),
            )
            .unwrap(),
            Removal::UnusableS => std::fs::write(f.profile(&s).join("identity"), b"short").unwrap(),
            _ => {}
        }
        let h = "cccccccccccccccccccccccccccccccc".to_string();
        std::fs::create_dir_all(f.profile(&h)).unwrap();
        std::fs::write(f.profile(&h).join("identity.hwid"), b"{}").unwrap();
        db::save_identity(
            &f.state.db,
            &h,
            "dddddddddddddddddddddddddddddddd",
            "H",
            "H",
        );
        f.set_last_used(&older, 1.0);
        f.set_last_used(&s, 2.0);
        db::set_active_identity(&f.state.db, &h).unwrap();
        if case != Removal::ActiveOnly {
            f.state.set_hw_locked(Some(h.clone()));
            f.state.set_startup_stage("hw_locked");
        }
        let generation = f.state.current_identity_session_generation();
        let removed = f
            .runtime
            .block_on(ratspeak_tauri::commands::hardware::hw_remove(
                f.app.state(),
                h.clone(),
            ))
            .unwrap();
        assert_eq!(removed["needs_setup"], false, "{case:?}");
        assert!(f.withdrawals.count() >= 1, "{case:?}: H's alerts stayed");
        assert!(db::get_identity(&f.state.db, &h).is_none(), "{case:?}");
        assert!(
            f.state.current_identity_session_generation() > generation,
            "{case:?}"
        );
        let expected = if case == Removal::UnusableS {
            &older
        } else {
            &s
        };
        assert_eq!(f.active().as_deref(), Some(expected.as_str()), "{case:?}");
        if case == Removal::ProtectedS {
            assert_eq!(f.state.hw_locked_hash().as_deref(), Some(s.as_str()));
        } else {
            assert_eq!(f.loaded().as_deref(), Some(expected.as_str()), "{case:?}");
            assert!(f.running(), "{case:?}: {expected} is not running");
        }
    }
}

// When the identity taking over from a removed live hardware identity can't
// load, no identity is left active without a runtime.
#[cfg(feature = "hardware")]
#[test]
fn a_takeover_that_cannot_load_leaves_no_identity_active() {
    let f = Fixture::new();
    let s = f.import("S");
    // Its key loads, but its ratchet store cannot open.
    let ratchets = f.profile(&s).join("ratchets");
    std::fs::remove_dir_all(&ratchets).unwrap();
    std::fs::write(&ratchets, b"not a directory").unwrap();
    let h = "cccccccccccccccccccccccccccccccc".to_string();
    std::fs::create_dir_all(f.profile(&h)).unwrap();
    std::fs::write(f.profile(&h).join("identity.hwid"), b"{}").unwrap();
    db::save_identity(
        &f.state.db,
        &h,
        "dddddddddddddddddddddddddddddddd",
        "H",
        "H",
    );
    db::set_active_identity(&f.state.db, &h).unwrap();
    f.state.set_hw_locked(Some(h.clone()));
    f.state.set_startup_stage("hw_locked");

    let removed = f
        .runtime
        .block_on(ratspeak_tauri::commands::hardware::hw_remove(
            f.app.state(),
            h.clone(),
        ))
        .unwrap();

    assert_eq!(removed["needs_setup"], false);
    let failed = f
        .recorder
        .last("identity_switch_failed")
        .expect("the takeover never ended");
    assert_eq!(failed["hash"], s.as_str());
    assert_eq!(f.active(), None, "S was left active with no runtime");
    assert!(f.loaded().is_none());
    assert!(!f.running());
    assert_eq!(f.state.get_startup_stage(), "ready");
}

/// `identity`'s key loads, but its ratchet store cannot open.
fn break_ratchets(f: &Fixture, identity: &str) {
    let ratchets = f.profile(identity).join("ratchets");
    std::fs::remove_dir_all(&ratchets).unwrap();
    std::fs::write(&ratchets, b"not a directory").unwrap();
}

// A plain identity that cannot load ends its start in an error: the network
// never runs without it, as if the app were ready.
#[test]
fn a_plain_identity_that_cannot_load_ends_its_start_in_an_error() {
    let f = Fixture::new();
    let s = f.import("S");
    break_ratchets(&f, &s);
    db::set_active_identity(&f.state.db, &s).unwrap();

    f.launch();

    assert!(f.loaded().is_none());
    assert_eq!(f.state.get_startup_stage(), "error");
    assert!(!f.running(), "the network ran without its identity");
}

// A switch to an identity that cannot load does not count as its use, so it
// never becomes the one a later start without an active identity opens.
#[test]
fn a_switch_to_an_identity_that_cannot_load_is_not_counted_as_its_use() {
    let f = Fixture::new();
    let a = f.import("A");
    let s = f.import("S");
    f.start(&a);
    break_ratchets(&f, &s);
    let unused = db::get_identity(&f.state.db, &s).unwrap()["last_used"].clone();

    let result = f
        .runtime
        .block_on(ratspeak_tauri::commands::identity::switch_identity(
            f.app.state(),
            s.clone(),
        ));

    assert!(result.is_err());
    assert_eq!(f.active().as_deref(), Some(a.as_str()));
    assert_eq!(f.loaded().as_deref(), Some(a.as_str()));
    assert_eq!(
        db::get_identity(&f.state.db, &s).unwrap()["last_used"],
        unused
    );
}

// A takeover that cannot load and cannot be made inactive again ends in an
// error, not ready with an active identity and no runtime.
#[cfg(feature = "hardware")]
#[test]
fn a_takeover_that_cannot_be_made_inactive_ends_in_an_error() {
    let f = Fixture::new();
    let s = f.import("S");
    break_ratchets(&f, &s);
    f.state
        .db
        .get()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER keep_s BEFORE UPDATE OF is_active ON identities \
             WHEN OLD.hash = '{s}' AND OLD.is_active = 1 AND NEW.is_active = 0 \
             BEGIN SELECT RAISE(ABORT, 'refused'); END;"
        ))
        .unwrap();
    let h = "cccccccccccccccccccccccccccccccc".to_string();
    std::fs::create_dir_all(f.profile(&h)).unwrap();
    std::fs::write(f.profile(&h).join("identity.hwid"), b"{}").unwrap();
    db::save_identity(
        &f.state.db,
        &h,
        "dddddddddddddddddddddddddddddddd",
        "H",
        "H",
    );
    db::set_active_identity(&f.state.db, &h).unwrap();
    f.state.set_hw_locked(Some(h.clone()));
    f.state.set_startup_stage("hw_locked");

    f.runtime
        .block_on(ratspeak_tauri::commands::hardware::hw_remove(
            f.app.state(),
            h.clone(),
        ))
        .unwrap();

    assert!(f.recorder.last("identity_switch_failed").is_some());
    assert!(f.loaded().is_none());
    assert!(!f.running());
    assert_eq!(f.state.get_startup_stage(), "error");
}

fn send_to(f: &Fixture, dest: &str) -> Result<Value, ratspeak_tauri::error::AppError> {
    let args = serde_json::from_value(serde_json::json!({
        "dest_hash": dest,
        "content": "hello",
        "delivery_method": "direct",
    }))
    .unwrap();
    f.runtime
        .block_on(ratspeak_tauri::commands::messaging::send_lxmf_message(
            f.app.state(),
            args,
        ))
}

/// A destination nobody has announced: a send to it is queued, not delivered.
const STRANGER: &str = "0123456789abcdef0123456789abcdef";

// A panic while the messaging manager is held (a tick, or a command) poisons
// its lock. The session restarts by itself and a send issued meanwhile waits
// for it; before, every send failed until the app was restarted.
#[test]
fn a_panic_under_the_messaging_lock_restarts_messaging_and_sends_work_again() {
    let f = Fixture::new();
    let a = f.import("A");
    f.start(&a);
    assert!(send_to(&f, STRANGER).is_ok(), "a send before the panic");

    let state = Arc::clone(&f.state);
    std::thread::spawn(move || {
        let _held = state.lxmf.lock().unwrap();
        panic!("injected panic under the messaging lock");
    })
    .join()
    .unwrap_err();
    assert!(f.state.lxmf.is_poisoned());

    // A send waits for the restart. Other runtimes in this test process can
    // hold it past the wait, and then the send says to try again.
    let asked = std::time::Instant::now();
    let sent = loop {
        match send_to(&f, STRANGER) {
            Err(refused)
                if refused.message == "Messaging is still starting. Try again shortly."
                    && asked.elapsed() < std::time::Duration::from_secs(90) => {}
            other => break other,
        }
    };

    assert!(sent.is_ok(), "the send after the panic: {sent:?}");
    assert!(!f.state.lxmf.is_poisoned());
    assert_eq!(f.loaded().as_deref(), Some(a.as_str()));
    // The send returns once the manager is back; the network follows.
    let restarted = std::time::Instant::now();
    while f.state.get_startup_stage() != "ready"
        && restarted.elapsed() < std::time::Duration::from_secs(30)
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(f.state.get_startup_stage(), "ready");
    assert!(f.running());
}

// A send issued while the runtime is still starting waits for the manager.
#[test]
fn a_send_issued_during_start_up_waits_for_messaging() {
    let f = Fixture::new();
    let a = f.import("A");
    db::set_active_identity(&f.state.db, &a).unwrap();
    f.state.set_startup_stage("checking");
    let state = Arc::clone(&f.state);
    let start = f.runtime.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        ratspeak_runtime::init_rns_lxmf(Arc::clone(&state), state.config.data_root.clone()).await;
    });

    let sent = send_to(&f, STRANGER);

    f.runtime.block_on(start).unwrap();
    assert!(sent.is_ok(), "the send during start-up: {sent:?}");
}

// When messaging cannot start, a send says why at once instead of waiting.
#[test]
fn a_send_says_at_once_why_messaging_is_unavailable() {
    let f = Fixture::new();
    let a = f.import("A");
    db::set_active_identity(&f.state.db, &a).unwrap();
    for (stage, reason) in [
        ("error", "Messaging failed to start. Restart Ratspeak."),
        ("hw_locked", "Unlock your identity to send messages."),
        ("stopped", "Messaging is stopped."),
    ] {
        f.state.set_startup_stage(stage);
        let asked = std::time::Instant::now();

        let refused = send_to(&f, STRANGER).unwrap_err();

        assert_eq!(refused.code, "lxmf_not_initialized", "{stage}");
        assert_eq!(refused.message, reason, "{stage}");
        assert!(
            asked.elapsed() < std::time::Duration::from_secs(2),
            "{stage}"
        );
    }
}
