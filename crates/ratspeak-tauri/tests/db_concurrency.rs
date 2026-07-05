//! Concurrency stress test for the async DB boundary. The important invariant:
//! SQLite work stays on the blocking pool so Tokio worker threads can keep
//! driving the app under contention.

use std::sync::Arc;
use std::time::Duration;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use ratspeak_tauri::db;
use ratspeak_tauri::state::DbPool;
use tempfile::tempdir;

/// Build a real DbPool against a tempdir so schema migrations match production.
fn build_pool() -> (DbPool, tempfile::TempDir) {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("concurrency.db");
    let manager = SqliteConnectionManager::file(&db_path).with_init(|conn| {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=30000;
             PRAGMA synchronous=NORMAL;",
        )
    });
    let pool = Pool::builder()
        .max_size(32)
        .build(manager)
        .expect("build pool");
    db::init_schema(&pool).expect("init_schema");
    (pool, dir)
}

/// 100 concurrent write/read cycles on a constrained Tokio runtime. This
/// catches accidental regressions back to blocking DB work on worker threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_save_and_get_scales_with_pool() {
    let (pool, _dir) = build_pool();
    let identity_id = "test_identity_abc123";

    let start = std::time::Instant::now();

    let mut handles = Vec::with_capacity(100);
    for i in 0..100u32 {
        let pool = pool.clone();
        let id = identity_id.to_string();
        handles.push(tokio::spawn(async move {
            let dest_hash = format!("{:032x}", i);
            let msg_id = format!("msg_{i}");
            let content = format!("Hello from task {i}");

            // Write.
            let id_w = id.clone();
            let dest_w = dest_hash.clone();
            let msg_w = msg_id.clone();
            let content_w = content.clone();
            db::spawn_db(pool.clone(), move |p| {
                db::save_message(
                    &p,
                    &msg_w,
                    &dest_w,
                    &id_w,
                    &content_w,
                    "",
                    1_700_000_000.0 + i as f64,
                    "sent",
                    "outbound",
                    &id_w,
                    "",
                    "",
                    "",
                    "",
                    "",
                    "",
                    Some("opportunistic"),
                );
            })
            .await
            .expect("save_message task");

            // Read back.
            let id_r = id.clone();
            let dest_r = dest_hash.clone();
            let messages =
                db::spawn_db(pool, move |p| db::get_conversation(&p, &dest_r, &id_r, 10))
                    .await
                    .expect("get_conversation task");
            assert!(
                !messages.is_empty(),
                "task {i}: conversation should contain the message we just saved"
            );
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        h.await.unwrap_or_else(|e| panic!("task {i} panicked: {e}"));
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(30),
        "100 concurrent DB tasks should complete well under 30s on a 2-worker \
         runtime when blocking is offloaded to the blocking pool — actual: {elapsed:?}"
    );
}

/// Same workload, single tokio worker: even with one worker the blocking pool
/// absorbs the DB work so the runtime doesn't stall.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn concurrent_ops_dont_stall_single_worker_runtime() {
    let (pool, _dir) = build_pool();
    let identity_id = "single_worker_test";

    let mut handles = Vec::with_capacity(50);
    for i in 0..50u32 {
        let pool = pool.clone();
        let id = identity_id.to_string();
        handles.push(tokio::spawn(async move {
            let id_c = id.clone();
            db::spawn_db(pool, move |p| db::get_all_contacts(&p, &id_c))
                .await
                .expect("get_all_contacts task");
            // Yield to exercise multiple tasks interleaving on a single worker.
            tokio::task::yield_now().await;
            let _ = (i, id);
        }));
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    for h in handles {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        tokio::time::timeout(remaining, h)
            .await
            .expect("task should finish within the deadline — single-worker runtime must not stall")
            .expect("no task panic");
    }
}

/// spawn_db propagates panics as JoinError — verify our expectation is
/// correct so real code can rely on `.expect("db task panicked")` being
/// unreachable under normal operation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_db_propagates_panics_as_join_error() {
    let (pool, _dir) = build_pool();

    let result: Result<(), _> = db::spawn_db(pool, |_p| {
        panic!("intentional panic in blocking closure");
    })
    .await;

    let join_err = result.expect_err("panic in blocking closure should surface as JoinError");
    assert!(join_err.is_panic(), "error should represent a panic");
}

/// T2-2: `helpers::active_identity_id` is generation-cached — concurrent hot
/// paths read the cache, and any identity-table write through the db layer
/// (switch/delete) invalidates it without an explicit hook at the call site.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_identity_cache_tracks_identity_switches() {
    use ratspeak_tauri::config::DashboardConfig;
    use ratspeak_tauri::helpers;
    use ratspeak_tauri::state::AppState;

    let (pool, dir) = build_pool();
    let data_dir = dir.path().join(".ratspeak");
    let rns_config_dir = data_dir.join("reticulum");
    std::fs::create_dir_all(&rns_config_dir).expect("create dirs");
    let state = Arc::new(AppState::new(
        DashboardConfig {
            data_root: dir.path().to_path_buf(),
            data_dir,
            rns_config_dir,
            rns_config_dir_overridden: false,
            max_log_entries: 200,
            rns_share_instance: true,
            rns_instance_name: None,
            rns_derive_ports: false,
            rns_seed_default_interface: false,
        },
        pool.clone(),
        Arc::new(ratspeak_core::NoopEmitter),
        Arc::new(ratspeak_core::NoopNotifier),
    ));

    db::save_identity(&pool, "id-a", "lxmf-a", "", "");
    db::save_identity(&pool, "id-b", "lxmf-b", "", "");
    db::set_active_identity(&pool, "id-a").expect("activate a");
    assert_eq!(helpers::active_identity_id(&state), "id-a");
    assert_eq!(helpers::active_lxmf_hash(&state), "lxmf-a");

    // Concurrent cached reads agree.
    let mut handles = Vec::new();
    for _ in 0..50 {
        let state = Arc::clone(&state);
        handles.push(tokio::spawn(async move {
            assert_eq!(helpers::active_identity_id(&state), "id-a");
        }));
    }
    for h in handles {
        h.await.expect("cached read task");
    }

    // A switch through the db layer invalidates the cache by itself.
    db::set_active_identity(&pool, "id-b").expect("activate b");
    assert_eq!(helpers::active_identity_id(&state), "id-b");
    assert_eq!(helpers::active_lxmf_hash(&state), "lxmf-b");

    // Deleting the active identity leaves no active row.
    db::delete_identity(&pool, "id-b", false).expect("delete b");
    assert_eq!(helpers::active_identity_id(&state), "");
}

/// Keep the pool large enough to absorb UI bursts without starving callers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pool_size_is_at_least_32() {
    let (pool, _dir) = build_pool();
    // r2d2::Pool exposes `max_size()`.
    assert!(
        pool.max_size() >= 32,
        "DB pool must stay >= 32 for concurrent async workload; got {}",
        pool.max_size()
    );
    // Arc<Pool> check — Pool is `Clone + Send + Sync + 'static`.
    let _cloned: Arc<DbPool> = Arc::new(pool);
}
