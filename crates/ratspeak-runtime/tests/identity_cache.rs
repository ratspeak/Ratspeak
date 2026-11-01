//! The active-identity cache is keyed by a process-global generation. Any
//! identity write elsewhere in the process bumps it and hides a stale entry,
//! so these tests run in their own binary, one at a time.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use r2d2_sqlite::SqliteConnectionManager;
use ratspeak_runtime::config::DashboardConfig;
use ratspeak_runtime::db::{self, DbPool};
use ratspeak_runtime::helpers::{active_identity_id, active_lxmf_hash};
use ratspeak_runtime::state::AppState;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const POOL_SIZE: u32 = 2;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Fixture {
    _root: tempfile::TempDir,
    db_path: PathBuf,
    state: Arc<AppState>,
}

/// A and B exist, A is active.
fn fixture(pool_size: u32) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let config = DashboardConfig::from_env_and_defaults(root.path().to_path_buf());
    let db_path = config.db_path();
    let manager = SqliteConnectionManager::file(&db_path).with_init(|conn| {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=30000;")
    });
    let pool = r2d2::Pool::builder()
        .max_size(pool_size)
        .connection_timeout(Duration::from_millis(200))
        .build_unchecked(manager);
    // Open every connection without the 200 ms read bound, which a loaded runner can miss.
    let opened: Vec<_> = (0..pool_size)
        .map(|_| pool.get_timeout(Duration::from_secs(30)).unwrap())
        .collect();
    drop(opened);
    db::init_schema(&pool).unwrap();
    db::save_identity(&pool, A, "a-lxmf", "A", "A");
    db::save_identity(&pool, B, "b-lxmf", "B", "B");
    db::set_active_identity(&pool, A).unwrap();
    let state = Arc::new(AppState::new(
        config,
        pool,
        Arc::new(ratspeak_core::NoopEmitter),
        Arc::new(ratspeak_core::NoopNotifier),
    ));
    Fixture {
        _root: root,
        db_path,
        state,
    }
}

fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Runs `write` while another connection holds the write lock, and reads the
/// active identity while the write waits for it.
fn read_while_write_waits(fixture: &Fixture, write: impl FnOnce(DbPool) + Send + 'static) {
    assert_eq!(active_identity_id(&fixture.state), A);
    let blocker = rusqlite::Connection::open(&fixture.db_path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let pool = fixture.state.db.clone();
    let writer = std::thread::spawn(move || write(pool));
    // The writer holds a pooled connection only once it is past any bump that
    // precedes its write, and its write cannot commit until the blocker ends.
    wait_until(|| fixture.state.db.state().idle_connections < POOL_SIZE);
    assert_eq!(active_identity_id(&fixture.state), A);
    blocker.execute_batch("ROLLBACK").unwrap();
    writer.join().unwrap();
}

#[test]
fn a_read_during_an_activation_never_keeps_the_old_identity() {
    let _serial = SERIAL.blocking_lock();
    let fixture = fixture(POOL_SIZE);
    read_while_write_waits(&fixture, |pool| {
        db::set_active_identity(&pool, B).unwrap();
    });
    assert_eq!(active_identity_id(&fixture.state), B);
}

#[test]
fn a_read_during_an_identity_save_never_keeps_the_old_row() {
    let _serial = SERIAL.blocking_lock();
    let fixture = fixture(POOL_SIZE);
    read_while_write_waits(&fixture, |pool| {
        db::save_identity(&pool, A, "a-lxmf-new", "A", "A");
    });
    assert_eq!(active_lxmf_hash(&fixture.state), "a-lxmf-new");
}

#[test]
fn a_read_during_a_deletion_never_keeps_the_deleted_identity() {
    let _serial = SERIAL.blocking_lock();
    let fixture = fixture(POOL_SIZE);
    read_while_write_waits(&fixture, |pool| {
        db::delete_identity(&pool, A, true).unwrap();
    });
    assert_eq!(active_identity_id(&fixture.state), "");
}

#[test]
fn a_rolled_back_activation_still_invalidates_the_cache() {
    let _serial = SERIAL.blocking_lock();
    let fixture = fixture(POOL_SIZE);
    assert_eq!(active_identity_id(&fixture.state), A);
    let generation = db::identity_generation();
    assert!(
        db::set_active_identity(&fixture.state.db, "cccccccccccccccccccccccccccccccc").is_err()
    );
    assert_ne!(db::identity_generation(), generation);
    assert_eq!(active_identity_id(&fixture.state), A);
}

#[test]
fn a_read_error_is_never_cached_as_no_identity() {
    let _serial = SERIAL.blocking_lock();
    let fixture = fixture(1);
    let held = fixture.state.db.get().unwrap();
    assert_eq!(active_identity_id(&fixture.state), "", "the read must fail");
    drop(held);
    assert_eq!(active_identity_id(&fixture.state), A);
}

#[tokio::test]
async fn startup_does_not_take_a_failed_active_identity_read_for_no_identity() {
    let _serial = SERIAL.lock().await;
    let fixture = fixture(1);
    // Protected material only: read as "no active row", startup would open A locked.
    let profile = fixture.state.config.data_dir.join("identities").join(A);
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("identity.enc"), b"sealed").unwrap();
    let held = fixture.state.db.get().unwrap();
    ratspeak_runtime::init_rns_lxmf(
        Arc::clone(&fixture.state),
        fixture.state.config.data_root.clone(),
    )
    .await;
    drop(held);
    assert_eq!(fixture.state.get_startup_stage(), "error");
}
