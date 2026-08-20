//! Background pruning of stale entries from `LxmfManager::known_identities`.
//! Two passes per sweep: time-based (configurable, off when `prune_days = 0`)
//! and cap-based (always on, evicts oldest beyond `CAP_HARD_FLOOR_DAYS` once
//! over `SOFT_CAP_IDENTITIES`). Protection set: contacts, blocked contacts,
//! message peers, propagation_node identities, discovered PN cache.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::db;
use crate::state::AppState;

/// Run one sweep (time pass + cap pass). Returns `(pruned, kept)`.
pub async fn sweep_once(state: Arc<AppState>) -> (usize, usize) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let protected_extra: std::collections::HashSet<String> = state
        .discovered_propagation_nodes
        .lock()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    let mut total_pruned = 0usize;
    let mut kept_count = 0usize;

    // Pass 1: time-based.
    let prune_days_opt = db::spawn_db(state.db.clone(), |p| db::get_prune_days(&p))
        .await
        .ok()
        .flatten();
    if let Some(prune_days) = prune_days_opt {
        let cutoff = now - (prune_days as f64) * 86_400.0;
        let victims = {
            let protected_extra = protected_extra.clone();
            db::spawn_db(state.db.clone(), move |p| {
                db::find_prune_candidates(&p, cutoff, &protected_extra)
            })
            .await
            .unwrap_or_default()
        };
        if victims.is_empty() {
            tracing::debug!(
                prune_days,
                "identity prune sweep: no stale non-protected identities"
            );
        } else {
            let (pruned, kept) = apply_eviction(
                &state,
                victims,
                cutoff,
                protected_extra.clone(),
                "time-based identity prune",
            )
            .await;
            total_pruned += pruned;
            kept_count = kept;
        }
    } else {
        tracing::debug!("time-based identity pruning disabled — cap pass only");
    }

    // Pass 2: cap-based.
    let current_len = {
        state
            .lxmf
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|m| m.known_identities.len()))
    };
    if let Some(current_len) = current_len {
        kept_count = current_len;
        if current_len > db::SOFT_CAP_IDENTITIES {
            let overflow = current_len - db::SOFT_CAP_IDENTITIES;
            let cap_cutoff = now - (db::CAP_HARD_FLOOR_DAYS as f64) * 86_400.0;
            let cap_victims = {
                let protected_extra = protected_extra.clone();
                db::spawn_db(state.db.clone(), move |p| {
                    db::find_cap_eviction_candidates(&p, cap_cutoff, overflow, &protected_extra)
                })
                .await
                .unwrap_or_default()
            };
            if cap_victims.is_empty() {
                tracing::warn!(
                    current = current_len,
                    soft_cap = db::SOFT_CAP_IDENTITIES,
                    floor_days = db::CAP_HARD_FLOOR_DAYS,
                    "known_identities above soft cap but all overflow is within the recency floor — no cap eviction this pass"
                );
            } else {
                let (pruned, kept) = apply_eviction(
                    &state,
                    cap_victims,
                    cap_cutoff,
                    protected_extra.clone(),
                    "cap-based identity eviction",
                )
                .await;
                total_pruned += pruned;
                kept_count = kept;
            }
        }
    }

    if total_pruned > 0 {
        state.emit_to_all(
            "identity_prune_completed",
            json!({
                "pruned":       total_pruned,
                "kept":         kept_count,
                "cutoff_days":  prune_days_opt,
                "timestamp":    now,
            }),
        );
    } else {
        tracing::debug!(
            kept = kept_count,
            cutoff_days = ?prune_days_opt,
            soft_cap = db::SOFT_CAP_IDENTITIES,
            "identity prune sweep: nothing to prune"
        );
    }

    (total_pruned, kept_count)
}

/// Revalidate candidates in one immediate DB transaction, persist the exact
/// matching known-identity mutation, then commit those row deletions. Disk
/// rewrite MUST commit before DB delete: reverse order could strand a peer
/// with no activity row and make it appear freshly-seen forever.
async fn apply_eviction(
    state: &Arc<AppState>,
    victims: Vec<String>,
    cutoff: f64,
    protected_extra: std::collections::HashSet<String>,
    label: &'static str,
) -> (usize, usize) {
    apply_eviction_with_persist(state, victims, cutoff, protected_extra, label, |snapshot| {
        snapshot.persist()
    })
    .await
}

async fn apply_eviction_with_persist<F>(
    state: &Arc<AppState>,
    victims: Vec<String>,
    cutoff: f64,
    protected_extra: std::collections::HashSet<String>,
    label: &'static str,
    persist_snapshot: F,
) -> (usize, usize)
where
    F: FnOnce(&crate::lxmf::KnownIdentitiesSnapshot) -> std::io::Result<()> + Send + 'static,
{
    let phase_started = std::time::Instant::now();
    let persistence_queued_at = std::time::Instant::now();
    let persistence_owner = state.lxmf_persistence_lock.lock().await;
    let persistence_wait = persistence_queued_at.elapsed();
    let removed_entries = Arc::new(std::sync::Mutex::new(Vec::new()));
    let callback_removed = Arc::clone(&removed_entries);
    let persisted_snapshot = Arc::new(std::sync::Mutex::new(None));
    let callback_snapshot = Arc::clone(&persisted_snapshot);
    let callback_state = Arc::clone(state);
    let db_victims = victims.clone();
    let db_protected = protected_extra.clone();
    let deleted_result = db::spawn_db(state.db.clone(), move |pool| {
        db::delete_prunable_identity_activity_after(
            &pool,
            &db_victims,
            cutoff,
            &db_protected,
            |eligible| {
                if eligible.is_empty() {
                    return Ok(());
                }
                let eligible = eligible.iter().cloned().collect();
                // The DB transaction already owns SQLite write admission.
                // Never wait DB -> manager because user paths may hold the
                // manager while entering SQLite. Abort this prune pass and
                // retry later if the protocol owner is busy.
                let (removed, snapshot) = {
                    let mut manager =
                        callback_state
                            .lxmf
                            .try_lock()
                            .map_err(|error| match error {
                                std::sync::TryLockError::WouldBlock => {
                                    "LXMF manager busy during identity prune".to_string()
                                }
                                std::sync::TryLockError::Poisoned(_) => {
                                    "LXMF manager unavailable during identity prune".to_string()
                                }
                            })?;
                    let manager = manager.as_mut().ok_or_else(|| {
                        "LXMF manager unavailable during identity prune".to_string()
                    })?;
                    (
                        manager.remove_known_identities(&eligible),
                        manager.known_identities_snapshot(),
                    )
                };
                *callback_removed.lock().unwrap() = removed.clone();
                persist_snapshot(&snapshot)
                    .map_err(|error| format!("known-identity snapshot failed: {error}"))?;
                *callback_snapshot.lock().unwrap() = Some(snapshot);
                Ok(())
            },
        )
    })
    .await;
    let deleted = match deleted_result {
        Ok(Ok(deleted)) => {
            if let Some(snapshot) = persisted_snapshot.lock().unwrap().take()
                && let Ok(mut manager) = state.lxmf.try_lock()
                && let Some(manager) = manager.as_mut()
            {
                manager.acknowledge_known_identities_snapshot(&snapshot);
            }
            deleted
        }
        Ok(Err(error)) => {
            let removed = removed_entries.lock().unwrap().clone();
            if removed.is_empty() {
                tracing::debug!(
                    pass = label,
                    candidates = victims.len(),
                    %error,
                    "identity prune deferred before memory mutation"
                );
                return (0, 0);
            }
            let kept = restore_known_identities(state, &removed);
            if let Err(recovery_error) =
                crate::lxmf_persistence::persist_current_known_identities_under_owner(
                    state,
                    &persistence_owner,
                    "identity_prune_rollback",
                )
                .await
            {
                tracing::error!(%recovery_error, "identity prune rollback persistence deferred");
            }
            tracing::warn!(
                pass = label,
                candidates = victims.len(),
                kept,
                %error,
                "identity prune failed; DB transaction rolled back and memory restored"
            );
            return (0, kept);
        }
        Err(error) => {
            let removed = removed_entries.lock().unwrap().clone();
            if removed.is_empty() {
                tracing::warn!(
                    pass = label,
                    candidates = victims.len(),
                    %error,
                    "identity prune DB worker failed before memory mutation"
                );
                return (0, 0);
            }
            let kept = restore_known_identities(state, &removed);
            if let Err(recovery_error) =
                crate::lxmf_persistence::persist_current_known_identities_under_owner(
                    state,
                    &persistence_owner,
                    "identity_prune_worker_rollback",
                )
                .await
            {
                tracing::error!(%recovery_error, "identity prune worker rollback persistence deferred");
            }
            tracing::warn!(
                pass = label,
                candidates = victims.len(),
                kept,
                %error,
                "identity prune DB worker failed; memory restored"
            );
            return (0, kept);
        }
    };
    let removed_count = removed_entries.lock().unwrap().len();
    let kept_count = state
        .lxmf
        .lock()
        .ok()
        .and_then(|manager| {
            manager
                .as_ref()
                .map(|manager| manager.known_identities.len())
        })
        .unwrap_or(0);
    drop(persistence_owner);

    for hash in &deleted {
        state.emit_to_all("peer_removed", json!({ "hash": hash }));
    }

    tracing::info!(
        pass = label,
        candidates = victims.len(),
        removed_from_map = removed_count,
        pruned = deleted.len(),
        kept = kept_count,
        removed_rows = deleted.len(),
        persistence_wait_ms = persistence_wait.as_millis() as u64,
        total_ms = phase_started.elapsed().as_millis() as u64,
        "identity prune pass complete"
    );

    (deleted.len(), kept_count)
}

fn restore_known_identities(state: &AppState, entries: &[(String, [u8; 64])]) -> usize {
    state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut manager| {
            manager
                .as_mut()
                .map(|manager| manager.restore_known_identities(entries))
        })
        .unwrap_or(0)
}

/// Spawn the background sweeper: one post-ready cleanup, then a 24h tick.
/// Spawn after `set_startup_stage("ready")`.
pub fn spawn_scheduler(state: Arc<AppState>, shutdown: rns_runtime::lifecycle::ShutdownSignal) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;

        if shutdown.is_triggered() {
            return;
        }
        sweep_once(state.clone()).await;

        let mut ticker = tokio::time::interval(Duration::from_secs(24 * 3600));
        ticker.tick().await; // discard immediate first tick

        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                _ = ticker.tick() => {
                    sweep_once(state.clone()).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use crate::lxmf::LxmfManager;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

    fn make_state() -> (tempfile::TempDir, Arc<AppState>) {
        let temp = tempfile::TempDir::new().unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let pool = r2d2::Pool::builder()
            .max_size(2)
            .build(SqliteConnectionManager::memory())
            .unwrap();
        db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        *state.lxmf.lock().unwrap() = Some(
            LxmfManager::load_or_create(temp.path(), None, None).expect("temporary LXMF manager"),
        );
        (temp, state)
    }

    #[tokio::test]
    async fn failed_identity_snapshot_restores_memory_and_preserves_activity_row() {
        let (_temp, state) = make_state();
        let victim = "33".repeat(16);
        let ratchets_dir = {
            let mut manager = state.lxmf.lock().unwrap();
            let manager = manager.as_mut().unwrap();
            manager.known_identities.insert(victim.clone(), [9; 64]);
            manager.ratchets_dir()
        };
        db::touch_identity_activity_for_service(
            &state.db,
            &[(victim.clone(), 1.0, None, None)],
            None,
            db::PEER_SERVICE_LXMF_DELIVERY,
        );

        // Make the snapshot parent invalid so the durable write deterministically
        // fails without relying on platform permissions.
        std::fs::remove_dir_all(&ratchets_dir).unwrap();
        std::fs::write(&ratchets_dir, b"not a directory").unwrap();
        let (pruned, kept) = apply_eviction(
            &state,
            vec![victim.clone()],
            10.0,
            std::collections::HashSet::new(),
            "test_prune_failure",
        )
        .await;

        assert_eq!(pruned, 0);
        assert_eq!(kept, 1);
        assert!(
            state
                .lxmf
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .known_identities
                .contains_key(&victim)
        );
        assert_eq!(
            db::get_identity_activity_first_seen(&state.db, &victim),
            Some(1.0)
        );
    }

    #[tokio::test]
    async fn successful_prune_commits_the_same_exact_memory_disk_and_db_set() {
        let (_temp, state) = make_state();
        let victim = "55".repeat(16);
        {
            let mut manager = state.lxmf.lock().unwrap();
            manager
                .as_mut()
                .unwrap()
                .known_identities
                .insert(victim.clone(), [5; 64]);
        }
        db::touch_identity_activity_for_service(
            &state.db,
            &[(victim.clone(), 1.0, None, None)],
            None,
            db::PEER_SERVICE_LXMF_DELIVERY,
        );

        let (pruned, kept) = apply_eviction(
            &state,
            vec![victim.clone()],
            10.0,
            std::collections::HashSet::new(),
            "test_prune_success",
        )
        .await;
        assert_eq!((pruned, kept), (1, 0));
        let manager = state.lxmf.lock().unwrap();
        let manager = manager.as_ref().unwrap();
        assert!(!manager.known_identities.contains_key(&victim));
        assert!(!manager.known_identities_dirty());
        assert!(manager.known_identities_blob().is_empty());
        assert!(
            std::fs::read(manager.ratchets_dir().join("known_identities"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db::get_identity_activity_first_seen(&state.db, &victim),
            None
        );
    }

    #[tokio::test]
    async fn busy_manager_causes_prompt_prune_rollback_without_disk_or_db_mutation() {
        let (_temp, state) = make_state();
        let victim = "77".repeat(16);
        let snapshot_path = {
            let mut manager = state.lxmf.lock().unwrap();
            let manager = manager.as_mut().unwrap();
            manager.known_identities.insert(victim.clone(), [7; 64]);
            let snapshot = manager.known_identities_snapshot();
            snapshot.persist().unwrap();
            manager.acknowledge_known_identities_snapshot(&snapshot);
            manager.ratchets_dir().join("known_identities")
        };
        db::touch_identity_activity_for_service(
            &state.db,
            &[(victim.clone(), 1.0, None, None)],
            None,
            db::PEER_SERVICE_LXMF_DELIVERY,
        );

        let manager = state.lxmf.lock().unwrap();
        let started = std::time::Instant::now();
        let result = apply_eviction(
            &state,
            vec![victim.clone()],
            10.0,
            std::collections::HashSet::new(),
            "test_prune_busy_manager",
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(result.0, 0);
        assert!(
            manager
                .as_ref()
                .unwrap()
                .known_identities
                .contains_key(&victim)
        );
        assert_eq!(std::fs::read(snapshot_path).unwrap().len(), 80);
        assert_eq!(
            db::get_identity_activity_first_seen(&state.db, &victim),
            Some(1.0)
        );
    }

    #[tokio::test]
    async fn manager_acquired_during_snapshot_io_cannot_block_db_commit() {
        let (_temp, state) = make_state();
        let victim = "88".repeat(16);
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .known_identities
            .insert(victim.clone(), [8; 64]);
        db::touch_identity_activity_for_service(
            &state.db,
            &[(victim.clone(), 1.0, None, None)],
            None,
            db::PEER_SERVICE_LXMF_DELIVERY,
        );

        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let task_state = Arc::clone(&state);
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        let task_victim = victim.clone();
        let task = tokio::spawn(async move {
            apply_eviction_with_persist(
                &task_state,
                vec![task_victim],
                10.0,
                std::collections::HashSet::new(),
                "test_prune_snapshot_window",
                move |_snapshot| {
                    task_started.store(true, Ordering::Release);
                    let (lock, wake) = &*task_release;
                    let released = lock.lock().unwrap();
                    drop(wake.wait_while(released, |released| !*released).unwrap());
                    Ok(())
                },
            )
            .await
        });
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }

        let manager = state.lxmf.lock().unwrap();
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while db::get_identity_activity_first_seen(&state.db, &victim).is_some()
            && std::time::Instant::now() < deadline
        {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            db::get_identity_activity_first_seen(&state.db, &victim),
            None,
            "DB commit must not wait on the manager after snapshot capture"
        );
        drop(manager);
        assert_eq!(task.await.unwrap(), (1, 0));
    }
}
