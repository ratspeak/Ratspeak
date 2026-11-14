//! Background pruning of peers (`identity_activity` rows and their
//! `LxmfManager::known_identities` entries). Two passes per sweep: by age
//! (`known_identities_prune_days`, off at 0) and by watermark (always on:
//! past `HIGH_WATERMARK` unprotected peers, evict down to `LOW_WATERMARK`,
//! never one heard within `EVICTION_FLOOR_SECS`). Protection set: contacts,
//! blocked contacts, message peers, used and Ratspeak Offline Inbox nodes,
//! and the discovered inbox nodes kept by `propagation::trim_discovered_nodes`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rns_transport::messages::PathTableRpcEntry;
use serde_json::json;

use crate::db;
use crate::state::AppState;

const HIGH_WATERMARK: usize = 5_000;
const LOW_WATERMARK: usize = 4_000;
const EVICTION_FLOOR_SECS: f64 = 24.0 * 3600.0;
const STARTUP_SWEEP_DELAY: Duration = Duration::from_secs(5);
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// Set while the floor holds back overflow, so it is logged once per episode.
static FLOOR_HOLD_LOGGED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Default, PartialEq)]
struct WatermarkPlan {
    victims: Vec<String>,
    /// Overflow kept because it was heard within the floor.
    held: usize,
}

/// Ratspeak-capable or directly reachable peers are evicted last.
fn evicted_last(peer: &db::UnprotectedPeer, direct: &HashSet<String>) -> bool {
    peer.ratspeak || direct.contains(&peer.dest_hash)
}

fn plan_watermark_eviction(
    mut peers: Vec<db::UnprotectedPeer>,
    direct: &HashSet<String>,
    now: f64,
) -> WatermarkPlan {
    if peers.len() <= HIGH_WATERMARK {
        return WatermarkPlan::default();
    }
    let overflow = peers.len() - LOW_WATERMARK;
    let floor = now - EVICTION_FLOOR_SECS;
    peers.retain(|peer| peer.last_seen < floor);
    peers.sort_by(|a, b| {
        evicted_last(a, direct)
            .cmp(&evicted_last(b, direct))
            .then(a.last_seen.total_cmp(&b.last_seen))
            .then_with(|| a.dest_hash.cmp(&b.dest_hash))
    });
    let victims: Vec<String> = peers
        .into_iter()
        .take(overflow)
        .map(|peer| peer.dest_hash)
        .collect();
    WatermarkPlan {
        held: overflow - victims.len(),
        victims,
    }
}

/// Whether this sweep's floor hold starts a new episode worth a warning.
fn floor_hold_is_news(held: usize, logged: &AtomicBool) -> bool {
    if held == 0 {
        logged.store(false, Ordering::Relaxed);
        false
    } else {
        !logged.swap(true, Ordering::Relaxed)
    }
}

/// Destinations one hop away on a live path.
fn direct_destinations(paths: &[PathTableRpcEntry], now: f64) -> HashSet<String> {
    paths
        .iter()
        .filter(|path| path.hops <= 1 && path.expires > now)
        .map(|path| hex::encode(path.hash))
        .collect()
}

async fn current_direct_destinations(state: &AppState, now: f64) -> HashSet<String> {
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.clone()));
    let Some(handle) = handle else {
        return HashSet::new();
    };
    crate::transport_observation::authoritative_path_table(&handle)
        .await
        .map(|paths| direct_destinations(&paths, now))
        .unwrap_or_default()
}

/// A sweep runs shortly after start, then hourly.
fn next_sweep_delay(swept_before: bool) -> Duration {
    if swept_before {
        SWEEP_INTERVAL
    } else {
        STARTUP_SWEEP_DELAY
    }
}

/// Run one sweep (age pass + watermark pass). Returns `(pruned, kept)`.
pub async fn sweep_once(state: Arc<AppState>) -> (usize, usize) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let protected_extra = crate::propagation::trim_discovered_nodes(&state, now).await;

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
        tracing::debug!("time-based identity pruning disabled — watermark pass only");
    }

    // Pass 2: watermark.
    if let Some(current_len) = state
        .lxmf
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|m| m.known_identities.len()))
    {
        kept_count = current_len;
    }
    let peers = {
        let protected_extra = protected_extra.clone();
        db::spawn_db(state.db.clone(), move |p| {
            db::find_unprotected_peers(&p, &protected_extra)
        })
        .await
        .unwrap_or_default()
    };
    let unprotected = peers.len();
    if unprotected > HIGH_WATERMARK {
        let direct = current_direct_destinations(&state, now).await;
        let plan = plan_watermark_eviction(peers, &direct, now);
        if floor_hold_is_news(plan.held, &FLOOR_HOLD_LOGGED) {
            tracing::warn!(
                unprotected,
                held = plan.held,
                high_watermark = HIGH_WATERMARK,
                low_watermark = LOW_WATERMARK,
                "peers above the watermark were heard within the last 24 hours; kept"
            );
        }
        if !plan.victims.is_empty() {
            let (pruned, kept) = apply_eviction(
                &state,
                plan.victims,
                now - EVICTION_FLOOR_SECS,
                protected_extra.clone(),
                "watermark identity eviction",
            )
            .await;
            total_pruned += pruned;
            kept_count = kept;
        }
    } else {
        floor_hold_is_news(0, &FLOOR_HOLD_LOGGED);
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
            unprotected,
            cutoff_days = ?prune_days_opt,
            high_watermark = HIGH_WATERMARK,
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

/// Spawn the background sweeper (cadence: `next_sweep_delay`); never per
/// announce. Spawn after `set_startup_stage("ready")`.
pub fn spawn_scheduler(state: Arc<AppState>, shutdown: rns_runtime::lifecycle::ShutdownSignal) {
    tokio::spawn(async move {
        let mut swept = false;
        loop {
            tokio::select! {
                _ = shutdown.wait() => break,
                _ = tokio::time::sleep(next_sweep_delay(swept)) => {
                    sweep_once(state.clone()).await;
                    swept = true;
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

    fn make_state_with_pool_size(max_size: u32) -> (tempfile::TempDir, Arc<AppState>) {
        let temp = tempfile::TempDir::new().unwrap();
        crate::lxmf::initialize_storage_namespace(temp.path()).unwrap();
        let config = DashboardConfig::from_env_and_defaults(temp.path().to_path_buf());
        let pool = r2d2::Pool::builder()
            .max_size(max_size)
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

    fn make_state() -> (tempfile::TempDir, Arc<AppState>) {
        make_state_with_pool_size(2)
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

    #[allow(clippy::await_holding_lock)]
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
        // A single connection makes the observation deterministic: the query
        // cannot see the old snapshot while the prune transaction is active;
        // it can acquire the connection only after DELETE + COMMIT completes.
        let (_temp, state) = make_state_with_pool_size(1);
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

        // Keep the deliberately blocking manager guard off the async executor.
        // If the prune task resumes first after its DB transaction commits, it
        // takes the same manager lock to report its retained count. Holding the
        // guard in this async task would then deadlock a single-thread Tokio
        // runtime before this task could observe the commit and release it.
        let observer_state = Arc::clone(&state);
        let observer_release = Arc::clone(&release);
        let observer_victim = victim.clone();
        tokio::task::spawn_blocking(move || {
            let manager = observer_state.lxmf.lock().unwrap();
            let (lock, wake) = &*observer_release;
            *lock.lock().unwrap() = true;
            wake.notify_all();

            // A separate thread waits for the sole DB connection. The holder
            // can still release the manager on timeout, so a regression cannot
            // hang the entire test process.
            let observer_db = observer_state.db.clone();
            let (observed_tx, observed_rx) = std::sync::mpsc::sync_channel(1);
            let query = std::thread::spawn(move || {
                let observed = db::get_identity_activity_first_seen(&observer_db, &observer_victim);
                let _ = observed_tx.send(observed);
            });
            let observed = observed_rx.recv_timeout(Duration::from_secs(2));
            drop(manager);
            query.join().unwrap();
            assert_eq!(
                observed,
                Ok(None),
                "DB commit must not wait on the manager after snapshot capture"
            );
        })
        .await
        .unwrap();
        assert_eq!(task.await.unwrap(), (1, 0));
    }

    const NOW: f64 = 10_000_000.0;
    const DAY: f64 = 86_400.0;

    fn hash(index: usize) -> String {
        format!("{index:032x}")
    }

    fn peer(index: usize, age_secs: f64) -> db::UnprotectedPeer {
        db::UnprotectedPeer {
            dest_hash: hash(index),
            last_seen: NOW - age_secs,
            ratspeak: false,
        }
    }

    fn stale_peers(count: usize) -> Vec<db::UnprotectedPeer> {
        (0..count).map(|i| peer(i, 2.0 * DAY + i as f64)).collect()
    }

    #[test]
    fn overflow_past_the_high_watermark_is_trimmed_to_the_low_watermark() {
        let plan = plan_watermark_eviction(stale_peers(5_001), &HashSet::new(), NOW);
        assert_eq!(plan.victims.len(), 1_001);
        assert_eq!(plan.held, 0);
        for count in [4_999, 5_000] {
            assert_eq!(
                plan_watermark_eviction(stale_peers(count), &HashSet::new(), NOW),
                WatermarkPlan::default(),
                "{count} unprotected peers stay"
            );
        }
    }

    #[test]
    fn a_trimmed_list_takes_a_thousand_new_peers_before_trimming_again() {
        let mut peers = stale_peers(6_000);
        let victims: HashSet<String> = plan_watermark_eviction(peers.clone(), &HashSet::new(), NOW)
            .victims
            .into_iter()
            .collect();
        peers.retain(|peer| !victims.contains(&peer.dest_hash));
        assert_eq!(peers.len(), LOW_WATERMARK);
        for added in [999, 1_000] {
            let mut grown = peers.clone();
            grown.extend((0..added).map(|i| peer(10_000 + i, 2.0 * DAY)));
            assert!(
                plan_watermark_eviction(grown, &HashSet::new(), NOW)
                    .victims
                    .is_empty(),
                "{added} new peers evict nothing"
            );
        }
    }

    #[test]
    fn peers_heard_within_a_day_are_never_evicted() {
        let fresh: Vec<_> = (0..6_000).map(|i| peer(i, DAY - 60.0)).collect();
        let plan = plan_watermark_eviction(fresh, &HashSet::new(), NOW);
        assert!(plan.victims.is_empty());
        assert_eq!(plan.held, 2_000);

        let mut mixed: Vec<_> = (0..5_200).map(|i| peer(i, 60.0)).collect();
        mixed.extend((5_200..5_500).map(|i| peer(i, DAY + 60.0)));
        let plan = plan_watermark_eviction(mixed, &HashSet::new(), NOW);
        assert_eq!(plan.victims.len(), 300, "only peers older than a day go");
        assert_eq!(plan.held, 1_200);
    }

    #[test]
    fn plain_peers_go_before_ratspeak_and_direct_ones_oldest_first() {
        let mut peers = stale_peers(5_000);
        let mut ratspeak = peer(90_001, 30.0 * DAY);
        ratspeak.ratspeak = true;
        let direct = peer(90_002, 29.0 * DAY);
        let old_plain = peer(90_003, 3.0 * DAY);
        peers.extend([ratspeak.clone(), direct.clone(), old_plain.clone()]);
        let direct_set = HashSet::from([direct.dest_hash.clone()]);

        let plan = plan_watermark_eviction(peers.clone(), &direct_set, NOW);
        assert_eq!(plan.victims.len(), 1_003);
        assert_eq!(plan.victims[0], old_plain.dest_hash, "oldest plain peer first");
        assert!(!plan.victims.contains(&ratspeak.dest_hash));
        assert!(!plan.victims.contains(&direct.dest_hash));

        // Evicted last, not never: once plain peers run out, the oldest go.
        let mut crowd: Vec<_> = (0..4_500)
            .map(|i| db::UnprotectedPeer {
                ratspeak: true,
                ..peer(i, 2.0 * DAY + i as f64)
            })
            .collect();
        crowd.extend((5_000..5_600).map(|i| peer(i, 2.0 * DAY)));
        let plan = plan_watermark_eviction(crowd, &HashSet::new(), NOW);
        assert_eq!(plan.victims.len(), 1_100);
        let plain: HashSet<String> = (5_000..5_600).map(hash).collect();
        assert!(plan.victims[..600].iter().all(|h| plain.contains(h)));
        assert_eq!(plan.victims[600], hash(4_499), "then the oldest Ratspeak peer");
    }

    #[test]
    fn only_live_one_hop_paths_count_as_direct() {
        let path = |n: u8, hops: u8, expires: f64| {
            let mut entry = PathTableRpcEntry::default();
            entry.hash = [n; 16];
            entry.hops = hops;
            entry.expires = expires;
            entry
        };
        let direct = direct_destinations(
            &[
                path(1, 1, NOW + 60.0),
                path(2, 2, NOW + 60.0),
                path(3, 1, NOW - 60.0),
            ],
            NOW,
        );
        assert_eq!(direct, HashSet::from([hex::encode([1u8; 16])]));
    }

    #[test]
    fn sweeps_run_at_start_then_hourly() {
        assert_eq!(next_sweep_delay(false), Duration::from_secs(5));
        assert_eq!(next_sweep_delay(true), Duration::from_secs(3_600));
    }

    #[test]
    fn a_floor_hold_is_logged_once_per_episode() {
        let logged = AtomicBool::new(false);
        assert!(floor_hold_is_news(10, &logged));
        assert!(!floor_hold_is_news(12, &logged));
        assert!(!floor_hold_is_news(0, &logged));
        assert!(floor_hold_is_news(3, &logged));
    }

    #[tokio::test]
    async fn a_sweep_ages_out_week_old_peers_and_trims_to_the_low_watermark() {
        let (_temp, state) = make_state();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let contact = hash(1_000_000);
        let week_old = hash(1_000_001);
        let six_days = hash(1_000_002);
        let mut rows: Vec<(String, f64)> = (0..5_100).map(|i| (hash(i), now - 2.0 * DAY)).collect();
        rows.extend([
            (contact.clone(), now - 6.5 * DAY),
            (week_old.clone(), now - 8.0 * DAY),
            (six_days.clone(), now - 6.0 * DAY),
        ]);
        let updates: Vec<db::IdentityActivityUpdate> = rows
            .iter()
            .map(|(dest_hash, timestamp)| db::IdentityActivityUpdate {
                dest_hash: dest_hash.clone(),
                timestamp: *timestamp,
                display_name: None,
                status: None,
                last_interface: None,
                identity_hash: None,
                services: vec![db::PEER_SERVICE_LXMF_DELIVERY.into()],
                clear_ratspeak_services: false,
                lxmf_compression_support: None,
            })
            .collect();
        db::touch_identity_activity_updates(&state.db, &updates);
        db::save_contact(&state.db, &contact, Some("Friend"), "trusted", "identity");
        {
            let mut manager = state.lxmf.lock().unwrap();
            let manager = manager.as_mut().unwrap();
            for (dest_hash, _) in &rows {
                manager.known_identities.insert(dest_hash.clone(), [1; 64]);
            }
        }

        let (pruned, kept) = sweep_once(state.clone()).await;

        assert_eq!(pruned, 1 + 1_101, "one aged out, then 5,101 trimmed to 4,000");
        assert_eq!(kept, 4_000 + 1, "the low watermark plus the protected contact");
        let remaining = db::find_unprotected_peers(&state.db, &HashSet::new());
        assert_eq!(remaining.len(), LOW_WATERMARK);
        assert_eq!(db::get_identity_activity_first_seen(&state.db, &week_old), None);
        assert!(db::get_identity_activity_first_seen(&state.db, &contact).is_some());
        assert!(
            !remaining.iter().any(|peer| peer.dest_hash == six_days),
            "the oldest unprotected peer goes first"
        );
        let manager = state.lxmf.lock().unwrap();
        let manager = manager.as_ref().unwrap();
        assert!(manager.known_identities.contains_key(&contact));
        assert_eq!(manager.known_identities.len(), 4_001);
    }
}
