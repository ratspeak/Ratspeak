//! Background pruning of peers (`identity_activity` rows and their
//! `LxmfManager::known_identities` entries). Two passes per sweep: by age
//! (`known_identities_prune_days`, off at 0) and by watermark (always on:
//! past `HIGH_WATERMARK` unprotected peers, evict down to `LOW_WATERMARK`,
//! never one heard within `EVICTION_FLOOR_SECS`). Protection set: contacts,
//! blocked contacts, message peers, used and Ratspeak Offline Inbox nodes,
//! and the discovered inbox nodes kept by `propagation::trim_discovered_nodes`.
//!
//! A third pass reads Reticulum's known destinations: it retains the user's
//! contacts and selected inbox nodes there, then drops keys with no peer row
//! that Reticulum has forgotten and no protection or feature keeps.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rns_runtime::reticulum::ReticulumHandle;
use rns_transport::messages::{PathTableRpcEntry, TransportQuery, TransportQueryResponse};
use serde_json::json;

use crate::db;
use crate::state::AppState;

const HIGH_WATERMARK: usize = 5_000;
const LOW_WATERMARK: usize = 4_000;
const EVICTION_FLOOR_SECS: f64 = 24.0 * 3600.0;
const STARTUP_SWEEP_DELAY: Duration = Duration::from_secs(45);
const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);
const RETAIN_TIMEOUT: Duration = Duration::from_millis(250);
/// Candidates per write transaction; other writers get SQLite between batches.
const PRUNE_BATCH: usize = 250;
/// Longer than SQLite's longest busy-handler sleep (100 ms), so a waiting
/// writer always wakes inside the gap instead of starving behind the pass.
const PRUNE_BATCH_PAUSE: Duration = Duration::from_millis(150);
/// Attempts per batch while the LXMF manager is busy before the pass defers.
const PRUNE_BUSY_ATTEMPTS: usize = 5;

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

/// A sweep runs once startup has settled, then hourly.
fn next_sweep_delay(swept_before: bool) -> Duration {
    if swept_before {
        SWEEP_INTERVAL
    } else {
        STARTUP_SWEEP_DELAY
    }
}

/// Reticulum's known destinations (`Identity.known_destinations`) with whether
/// each is retained, and the keys of the identity whose runtime answered.
struct ReticulumView {
    handle: ReticulumHandle,
    identity: String,
    keys: Vec<String>,
    known: HashMap<String, bool>,
}

/// `None` when there is no runtime or no full answer; the pass then removes
/// nothing. Keys are read first, so a key learned meanwhile is no candidate.
async fn reticulum_view(state: &AppState) -> Option<ReticulumView> {
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.clone()))?;
    let (identity, keys) = state.lxmf.lock().ok().and_then(|manager| {
        manager.as_ref().map(|manager| {
            (
                manager.identity_hash.clone(),
                manager.known_identities.keys().cloned().collect::<Vec<_>>(),
            )
        })
    })?;
    // The whole local table, as Ratspeak recalls from it, not a recent subset.
    let Some(TransportQueryResponse::Announces(entries)) = handle
        .query_transport(TransportQuery::GetRecentAnnounces)
        .await
    else {
        return None;
    };
    // An empty table is a failed load as often as a quiet network.
    if entries.is_empty() || !crate::is_current_network_runtime(state, &handle) {
        return None;
    }
    let known = entries
        .into_iter()
        .map(|entry| (hex::encode(entry.dest_hash), entry.retained))
        .collect();
    Some(ReticulumView {
        handle,
        identity,
        keys,
        known,
    })
}

/// Retain the contacts and selected inbox nodes Reticulum knows but has not
/// pinned. Never released: the pins grow only with the user's own choices.
async fn retain_user_pins(state: &AppState, view: &ReticulumView) {
    let Some(pins) = db::spawn_db(state.db.clone(), |p| db::user_pinned_destinations(&p))
        .await
        .ok()
        .and_then(Result::ok)
    else {
        return;
    };
    let mut retained = 0usize;
    for pin in pins {
        let pin = pin.to_ascii_lowercase();
        if view.known.get(&pin) != Some(&false) {
            continue;
        }
        let Some(dest) = hex::decode(&pin)
            .ok()
            .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        else {
            continue;
        };
        let answer = tokio::time::timeout(
            RETAIN_TIMEOUT,
            view.handle
                .query_control(TransportQuery::RetainDestination { dest }),
        )
        .await;
        if matches!(answer, Ok(Some(TransportQueryResponse::BoolResult(true)))) {
            retained += 1;
        } else {
            tracing::debug!(
                dest = %crate::short_id(&pin),
                "Reticulum did not retain a contact or inbox node"
            );
        }
    }
    if retained > 0 {
        tracing::info!(
            retained,
            "retained contacts and Offline Inbox nodes in Reticulum"
        );
    }
}

/// Remove the keys with no peer row that Reticulum no longer knows and that
/// no protection or feature keeps.
async fn prune_orphan_keys(
    state: &Arc<AppState>,
    view: ReticulumView,
    protected_extra: HashSet<String>,
) {
    let candidates: Vec<String> = view
        .keys
        .into_iter()
        .filter(|key| !view.known.contains_key(key) && !protected_extra.contains(key))
        .collect();
    if candidates.is_empty() {
        tracing::info!(removed = 0, "orphan key prune: nothing to remove");
        return;
    }
    apply_prune(
        state,
        PruneScope::OrphanKeys {
            identity: view.identity,
        },
        candidates,
        protected_extra,
        "orphan key prune",
    )
    .await;
}

/// Run one sweep (age, watermark, then Reticulum passes). Returns `(pruned
/// peer rows, kept keys)`.
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

    // Pass 3: against Reticulum's known destinations.
    if let Some(view) = reticulum_view(&state).await {
        retain_user_pins(&state, &view).await;
        prune_orphan_keys(&state, view, protected_extra).await;
        if let Some(current_len) = state
            .lxmf
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|m| m.known_identities.len()))
        {
            kept_count = current_len;
        }
    } else {
        tracing::debug!("orphan key prune skipped: Reticulum's known destinations unavailable");
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

/// What a prune pass removes.
#[derive(Clone)]
enum PruneScope {
    /// Peer rows last heard before `cutoff`, with their keys.
    Peers { cutoff: f64 },
    /// Keys without a peer row, from `identity`'s map only.
    OrphanKeys { identity: String },
}

async fn apply_eviction(
    state: &Arc<AppState>,
    victims: Vec<String>,
    cutoff: f64,
    protected_extra: std::collections::HashSet<String>,
    label: &'static str,
) -> (usize, usize) {
    apply_prune(
        state,
        PruneScope::Peers { cutoff },
        victims,
        protected_extra,
        label,
    )
    .await
}

async fn apply_prune(
    state: &Arc<AppState>,
    scope: PruneScope,
    victims: Vec<String>,
    protected_extra: std::collections::HashSet<String>,
    label: &'static str,
) -> (usize, usize) {
    apply_prune_with_persist(state, scope, victims, protected_extra, label, |snapshot| {
        snapshot.persist()
    })
    .await
}

/// How one prune batch ended.
enum BatchOutcome {
    Committed {
        deleted: Vec<String>,
        removed: usize,
    },
    /// Nothing changed; `busy` when the LXMF manager was held, which a retry may clear.
    Deferred { busy: bool },
    /// Failed after the memory mutation, which was restored.
    RolledBack,
}

/// Prune `victims` in batches of `PRUNE_BATCH`, each its own short write
/// transaction, pausing between them so no other writer waits on the pass.
async fn apply_prune_with_persist<F>(
    state: &Arc<AppState>,
    scope: PruneScope,
    victims: Vec<String>,
    protected_extra: std::collections::HashSet<String>,
    label: &'static str,
    persist_snapshot: F,
) -> (usize, usize)
where
    F: Fn(&crate::lxmf::KnownIdentitiesSnapshot) -> std::io::Result<()> + Send + Sync + 'static,
{
    let peer_rows = matches!(scope, PruneScope::Peers { .. });
    let phase_started = std::time::Instant::now();
    let persist_snapshot = Arc::new(persist_snapshot);
    let protected_extra = Arc::new(protected_extra);
    let mut deleted = Vec::new();
    let mut removed_count = 0usize;
    let mut batches = 0usize;
    let mut longest_batch = Duration::ZERO;
    let mut stopped = false;
    'batches: for batch in victims.chunks(PRUNE_BATCH) {
        for attempt in 1..=PRUNE_BUSY_ATTEMPTS {
            if batches > 0 || attempt > 1 {
                tokio::time::sleep(PRUNE_BATCH_PAUSE).await;
            }
            let batch_started = std::time::Instant::now();
            let outcome = prune_batch(
                state,
                scope.clone(),
                batch.to_vec(),
                Arc::clone(&protected_extra),
                label,
                Arc::clone(&persist_snapshot),
            )
            .await;
            longest_batch = longest_batch.max(batch_started.elapsed());
            match outcome {
                BatchOutcome::Committed {
                    deleted: mut batch_deleted,
                    removed,
                } => {
                    batches += 1;
                    removed_count += removed;
                    if peer_rows && !batch_deleted.is_empty() {
                        state.emit_to_all("peers_removed", json!({ "hashes": &batch_deleted }));
                    }
                    deleted.append(&mut batch_deleted);
                    continue 'batches;
                }
                BatchOutcome::Deferred { busy: true } if attempt < PRUNE_BUSY_ATTEMPTS => {}
                // Nothing done: return without locking the manager found busy.
                BatchOutcome::Deferred { .. } if batches == 0 => return (0, 0),
                BatchOutcome::Deferred { .. } | BatchOutcome::RolledBack => {
                    stopped = true;
                    break 'batches;
                }
            }
        }
    }
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

    tracing::info!(
        pass = label,
        candidates = victims.len(),
        removed_from_map = removed_count,
        pruned = deleted.len(),
        kept = kept_count,
        removed_rows = if peer_rows { deleted.len() } else { 0 },
        batches,
        stopped,
        longest_batch_ms = longest_batch.as_millis() as u64,
        total_ms = phase_started.elapsed().as_millis() as u64,
        "identity prune pass complete"
    );

    (deleted.len(), kept_count)
}

/// Revalidate one batch in an immediate DB transaction, persist the exact
/// matching known-identity mutation, then commit those row deletions. Disk
/// rewrite MUST commit before DB delete: reverse order could strand a peer
/// with no activity row and make it appear freshly-seen forever.
async fn prune_batch<F>(
    state: &Arc<AppState>,
    scope: PruneScope,
    batch: Vec<String>,
    protected_extra: Arc<std::collections::HashSet<String>>,
    label: &'static str,
    persist_snapshot: Arc<F>,
) -> BatchOutcome
where
    F: Fn(&crate::lxmf::KnownIdentitiesSnapshot) -> std::io::Result<()> + Send + Sync + 'static,
{
    let persistence_owner = state.lxmf_persistence_lock.lock().await;
    let removed_entries = Arc::new(std::sync::Mutex::new(Vec::new()));
    let callback_removed = Arc::clone(&removed_entries);
    let persisted_snapshot = Arc::new(std::sync::Mutex::new(None));
    let callback_snapshot = Arc::clone(&persisted_snapshot);
    let busy = Arc::new(AtomicBool::new(false));
    let callback_busy = Arc::clone(&busy);
    let callback_state = Arc::clone(state);
    let candidates = batch.len();
    let deleted_result = db::spawn_db(state.db.clone(), move |pool| {
        let remove_keys = |eligible: &[String]| -> Result<(), String> {
            if eligible.is_empty() {
                return Ok(());
            }
            let eligible = eligible.iter().cloned().collect();
            // The DB transaction already owns SQLite write admission.
            // Never wait DB -> manager because user paths may hold the
            // manager while entering SQLite. Retry the batch shortly, then
            // defer the pass, if the protocol owner is busy.
            let (removed, snapshot) = {
                let mut manager = callback_state
                    .lxmf
                    .try_lock()
                    .map_err(|error| match error {
                        std::sync::TryLockError::WouldBlock => {
                            callback_busy.store(true, Ordering::Relaxed);
                            "LXMF manager busy during identity prune".to_string()
                        }
                        std::sync::TryLockError::Poisoned(_) => {
                            "LXMF manager unavailable during identity prune".to_string()
                        }
                    })?;
                let manager = manager
                    .as_mut()
                    .ok_or_else(|| "LXMF manager unavailable during identity prune".to_string())?;
                if let PruneScope::OrphanKeys { identity } = &scope
                    && manager.identity_hash != *identity
                {
                    return Err("identity changed during key prune".to_string());
                }
                (
                    manager.remove_known_identities(&eligible),
                    manager.known_identities_snapshot(),
                )
            };
            *callback_removed.lock().unwrap() = removed.clone();
            (*persist_snapshot)(&snapshot)
                .map_err(|error| format!("known-identity snapshot failed: {error}"))?;
            *callback_snapshot.lock().unwrap() = Some(snapshot);
            Ok(())
        };
        match &scope {
            PruneScope::Peers { cutoff } => db::delete_prunable_identity_activity_after(
                &pool,
                &batch,
                *cutoff,
                &protected_extra,
                remove_keys,
            ),
            PruneScope::OrphanKeys { .. } => {
                db::confirm_orphan_identity_keys_after(&pool, &batch, &protected_extra, remove_keys)
            }
        }
    })
    .await;
    let (reason, error) = match deleted_result {
        Ok(Ok(deleted)) => {
            if let Some(snapshot) = persisted_snapshot.lock().unwrap().take()
                && let Ok(mut manager) = state.lxmf.try_lock()
                && let Some(manager) = manager.as_mut()
            {
                manager.acknowledge_known_identities_snapshot(&snapshot);
            }
            let removed = removed_entries.lock().unwrap().len();
            return BatchOutcome::Committed { deleted, removed };
        }
        Ok(Err(error)) => ("identity_prune_rollback", error),
        Err(error) => ("identity_prune_worker_rollback", error.to_string()),
    };
    let removed = removed_entries.lock().unwrap().clone();
    if removed.is_empty() {
        tracing::debug!(
            pass = label,
            candidates,
            %error,
            "identity prune batch deferred before memory mutation"
        );
        return BatchOutcome::Deferred {
            busy: busy.load(Ordering::Relaxed),
        };
    }
    let kept = restore_known_identities(state, &removed);
    if let Err(recovery_error) =
        crate::lxmf_persistence::persist_current_known_identities_under_owner(
            state,
            &persistence_owner,
            reason,
        )
        .await
    {
        tracing::error!(%recovery_error, reason, "identity prune rollback persistence deferred");
    }
    tracing::warn!(
        pass = label,
        candidates,
        kept,
        %error,
        "identity prune batch failed; DB transaction rolled back and memory restored"
    );
    BatchOutcome::RolledBack
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
        make_state_with(max_size, Arc::new(ratspeak_core::NoopEmitter))
    }

    fn make_state_with(
        max_size: u32,
        emitter: Arc<dyn ratspeak_core::Emitter>,
    ) -> (tempfile::TempDir, Arc<AppState>) {
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
            emitter,
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

    #[tokio::test]
    async fn a_large_prune_commits_and_reports_one_batch_at_a_time() {
        let emitter = Arc::new(CountingEmitter::default());
        let (_temp, state) = make_state_with(2, emitter.clone());
        let victims: Vec<String> = (0..PRUNE_BATCH + 1).map(|i| hash(3_000_000 + i)).collect();
        let rows: Vec<_> = victims
            .iter()
            .map(|v| (v.clone(), 1.0, None, None))
            .collect();
        db::touch_identity_activity(&state.db, &rows);
        insert_keys(&state, &victims);

        let (pruned, kept) =
            apply_eviction(&state, victims.clone(), 10.0, HashSet::new(), "test").await;

        assert_eq!((pruned, kept), (PRUNE_BATCH + 1, 0));
        assert_eq!(emitter.0.lock().unwrap().get("peers_removed"), Some(&2));
        assert!(db::find_unprotected_peers(&state.db, &HashSet::new()).is_empty());
        let manager = state.lxmf.lock().unwrap();
        let manager = manager.as_ref().unwrap();
        assert!(!manager.known_identities_dirty());
        assert!(
            std::fs::read(manager.ratchets_dir().join("known_identities"))
                .unwrap()
                .is_empty()
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
            apply_prune_with_persist(
                &task_state,
                PruneScope::Peers { cutoff: 10.0 },
                vec![task_victim],
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
        assert_eq!(
            plan.victims[0], old_plain.dest_hash,
            "oldest plain peer first"
        );
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
        assert_eq!(
            plan.victims[600],
            hash(4_499),
            "then the oldest Ratspeak peer"
        );
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
    fn sweeps_run_after_start_then_hourly() {
        assert_eq!(next_sweep_delay(false), Duration::from_secs(45));
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
    async fn a_sweep_ages_out_peers_past_a_stored_week_and_trims_to_the_low_watermark() {
        let (_temp, state) = make_state();
        db::set_setting(&state.db, "known_identities_prune_days", "7");
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
                announce_emitted: None,
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

        assert_eq!(
            pruned,
            1 + 1_101,
            "one aged out, then 5,101 trimmed to 4,000"
        );
        assert_eq!(
            kept,
            4_000 + 1,
            "the low watermark plus the protected contact"
        );
        let remaining = db::find_unprotected_peers(&state.db, &HashSet::new());
        assert_eq!(remaining.len(), LOW_WATERMARK);
        assert_eq!(
            db::get_identity_activity_first_seen(&state.db, &week_old),
            None
        );
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

    #[tokio::test]
    async fn a_sweep_ages_out_peers_after_a_day_by_default() {
        let (_temp, state) = make_state();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let (stale, fresh) = (hash(2_000_001), hash(2_000_002));
        db::touch_identity_activity(
            &state.db,
            &[
                (stale.clone(), now - 1.5 * DAY, None, None),
                (fresh.clone(), now - 0.5 * DAY, None, None),
            ],
        );
        insert_keys(&state, [&stale, &fresh]);

        assert_eq!(sweep_once(state.clone()).await, (1, 1));
        assert_eq!(
            db::get_identity_activity_first_seen(&state.db, &stale),
            None
        );
        assert!(db::get_identity_activity_first_seen(&state.db, &fresh).is_some());
        assert_eq!(keys(&state), vec![fresh]);
    }

    fn dest(hash: &str) -> [u8; 16] {
        hex::decode(hash).unwrap().try_into().unwrap()
    }

    fn insert_keys<'a>(state: &AppState, hashes: impl IntoIterator<Item = &'a String>) {
        let mut manager = state.lxmf.lock().unwrap();
        let manager = manager.as_mut().unwrap();
        for hash in hashes {
            manager.known_identities.insert(hash.clone(), [3; 64]);
        }
    }

    fn keys(state: &AppState) -> Vec<String> {
        let manager = state.lxmf.lock().unwrap();
        let mut keys: Vec<String> = manager
            .as_ref()
            .unwrap()
            .known_identities
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    type Retained = Arc<Mutex<Vec<[u8; 16]>>>;

    /// A runtime whose transport lists `known` (destination, retained), or
    /// fails the listing on `None`, and retains whatever it is asked to.
    async fn install_reticulum(
        state: &AppState,
        known: Option<Vec<([u8; 16], bool)>>,
    ) -> (tempfile::TempDir, Retained, tokio::task::JoinHandle<()>) {
        use rns_transport::messages::{AnnounceRpcEntry, TransportMessage};
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("config"),
            "[reticulum]\nshare_instance = No\nenable_transport = No\n[interfaces]\n",
        )
        .unwrap();
        let mut network = crate::rns::RnsManager::init(
            root.path().to_str().unwrap(),
            Some(root.path().join("cache")),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap();
        let (tx, mut queries) = tokio::sync::mpsc::channel(8);
        network.handle.transport_tx = tx;
        *state.rns.write().unwrap() = Some(network);
        let retained = Retained::default();
        let answer = tokio::spawn({
            let retained = Arc::clone(&retained);
            async move {
                while let Some(message) = queries.recv().await {
                    let TransportMessage::Rpc { query, response_tx } = message else {
                        continue;
                    };
                    let response = match (query, &known) {
                        (TransportQuery::GetRecentAnnounces, Some(known)) => {
                            TransportQueryResponse::Announces(
                                known
                                    .iter()
                                    .map(|(dest, pinned)| {
                                        let mut entry = AnnounceRpcEntry::default();
                                        entry.dest_hash = *dest;
                                        entry.retained = *pinned;
                                        entry
                                    })
                                    .collect(),
                            )
                        }
                        (TransportQuery::RetainDestination { dest }, _) => {
                            retained.lock().unwrap().push(dest);
                            TransportQueryResponse::BoolResult(true)
                        }
                        // Dropping the reply fails the query.
                        _ => continue,
                    };
                    let _ = response_tx.send(response);
                }
            }
        });
        (root, retained, answer)
    }

    fn remove_reticulum(state: &AppState, answer: tokio::task::JoinHandle<()>) {
        if let Some(network) = state.rns.write().unwrap().take() {
            network.handle.shutdown.trigger();
        }
        answer.abort();
    }

    #[tokio::test]
    async fn a_sweep_drops_only_keys_that_nothing_and_no_reticulum_entry_keeps() {
        let (_temp, state) = make_state();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let [orphan, contact, known, peer, hub] = [1, 2, 3, 4, 5].map(|n| hash(2_100_000 + n));
        insert_keys(&state, [&orphan, &contact, &known, &peer, &hub]);
        db::save_contact(&state.db, &contact, None, "pending", "identity");
        db::touch_identity_activity(&state.db, &[(peer.clone(), now, None, None)]);
        db::save_identity(&state.db, "identity", "", "Me", "Me");
        db::save_channel_hub(&state.db, "identity", &hub, "Hub", "me", false).unwrap();
        let (_root, _, answer) = install_reticulum(&state, Some(vec![(dest(&known), false)])).await;

        let (pruned, kept) = sweep_once(state.clone()).await;

        remove_reticulum(&state, answer);
        assert_eq!((pruned, kept), (0, 4), "a key is not a peer row");
        let mut expected = vec![contact, known, peer.clone(), hub];
        expected.sort();
        assert_eq!(keys(&state), expected);
        assert!(db::get_identity_activity_first_seen(&state.db, &peer).is_some());
        let manager = state.lxmf.lock().unwrap();
        let manager = manager.as_ref().unwrap();
        assert!(!manager.known_identities_dirty());
        assert_eq!(
            std::fs::read(manager.ratchets_dir().join("known_identities"))
                .unwrap()
                .len(),
            4 * 80
        );
    }

    #[tokio::test]
    async fn a_sweep_without_a_full_reticulum_listing_removes_no_key() {
        let (_temp, state) = make_state();
        let orphan = hash(2_200_001);
        insert_keys(&state, [&orphan]);
        sweep_once(state.clone()).await;
        assert_eq!(keys(&state), vec![orphan.clone()], "no runtime");

        for listing in [None, Some(Vec::new())] {
            let (_root, _, answer) = install_reticulum(&state, listing).await;
            sweep_once(state.clone()).await;
            remove_reticulum(&state, answer);
            assert_eq!(keys(&state), vec![orphan.clone()]);
        }
    }

    #[tokio::test]
    async fn an_orphan_key_prune_never_touches_another_identitys_keys() {
        let (_temp, state) = make_state();
        let orphan = hash(2_300_001);
        insert_keys(&state, [&orphan]);
        let scope = PruneScope::OrphanKeys {
            identity: "another".into(),
        };
        let (pruned, _) =
            apply_prune(&state, scope, vec![orphan.clone()], HashSet::new(), "test").await;
        assert_eq!(pruned, 0);
        assert_eq!(keys(&state), vec![orphan]);
    }

    #[derive(Default)]
    struct CountingEmitter(Mutex<HashMap<String, usize>>);

    impl ratspeak_core::Emitter for CountingEmitter {
        fn try_emit(
            &self,
            event: &str,
            _payload: serde_json::Value,
        ) -> Result<(), ratspeak_core::EmitError> {
            *self.0.lock().unwrap().entry(event.into()).or_default() += 1;
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_sweep_retains_known_contacts_and_selected_inbox_nodes_in_reticulum() {
        let (_temp, state) = make_state();
        let [contact, pinned, unheard, node, stranger] =
            [1, 2, 3, 4, 5].map(|n| hash(2_400_000 + n));
        db::save_identity(&state.db, "identity", "", "Me", "Me");
        db::set_identity_propagation_node(&state.db, "identity", &node).unwrap();
        for hash in [&contact, &pinned, &unheard] {
            db::save_contact(&state.db, hash, None, "pending", "identity");
        }
        let listing = vec![
            (dest(&contact), false),
            (dest(&pinned), true),
            (dest(&node), false),
            (dest(&stranger), false),
        ];
        let (_root, retained, answer) = install_reticulum(&state, Some(listing)).await;

        sweep_once(state.clone()).await;

        remove_reticulum(&state, answer);
        let mut asked = retained.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, vec![dest(&contact), dest(&node)]);
    }
}
