//! Serialized LXMF persistence outside the live protocol-manager lock.

use std::io;
use std::path::PathBuf;
use std::time::Instant;

use crate::lxmf::{KnownIdentitiesSnapshot, LxmfCheckpointSnapshot};
use crate::state::AppState;

async fn run_blocking<T, F>(reason: &'static str, artifacts: usize, work: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| io::Error::other(format!("LXMF persistence worker failed: {error}")))?;
    let elapsed = started.elapsed();
    match &result {
        Ok(_) => tracing::info!(
            reason,
            artifacts,
            io_ms = elapsed.as_millis() as u64,
            "LXMF persistence completed"
        ),
        Err(error) => tracing::warn!(
            reason,
            artifacts,
            io_ms = elapsed.as_millis() as u64,
            %error,
            "LXMF persistence failed"
        ),
    }
    result
}

/// Persist a current coherent delta. Snapshot capture occurs only after this
/// session's persistence turn is acquired, so two writers cannot complete out
/// of order and replace newer state with an older snapshot.
pub async fn persist_current_delta(
    state: &AppState,
    identities_changed: bool,
    changed_ratchet_hashes: &[String],
    router_changed: bool,
    reason: &'static str,
) -> io::Result<bool> {
    persist_current_delta_with(
        state,
        identities_changed,
        changed_ratchet_hashes,
        router_changed,
        reason,
        |snapshot| snapshot.persist(),
    )
    .await
}

async fn persist_current_delta_with<F>(
    state: &AppState,
    identities_changed: bool,
    changed_ratchet_hashes: &[String],
    router_changed: bool,
    reason: &'static str,
    persist: F,
) -> io::Result<bool>
where
    F: FnOnce(&crate::lxmf::LxmfPersistenceDelta) -> io::Result<()> + Send + 'static,
{
    let queued_at = Instant::now();
    let _owner = state.lxmf_persistence_lock.lock().await;
    let queued = queued_at.elapsed();
    let snapshot = state.lxmf.lock().ok().and_then(|manager| {
        manager.as_ref().map(|manager| {
            manager.persistence_delta_snapshot(
                identities_changed,
                changed_ratchet_hashes,
                router_changed,
            )
        })
    });
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    if snapshot.is_empty() {
        return Ok(true);
    }
    let artifacts = snapshot.artifact_count();
    tracing::debug!(
        reason,
        artifacts,
        queued_ms = queued.as_millis() as u64,
        "LXMF persistence snapshot captured"
    );
    let persisted = run_blocking(reason, artifacts, move || {
        persist(&snapshot)?;
        Ok(snapshot)
    })
    .await?;
    if let Ok(mut manager) = state.lxmf.lock()
        && let Some(manager) = manager.as_mut()
    {
        manager.acknowledge_persistence_delta(&persisted);
    }
    Ok(true)
}

/// Persist the current periodic/shutdown checkpoint without replaying received
/// ratchet files. Changed ratchets are write-through deltas.
pub async fn persist_current_checkpoint(
    state: &AppState,
    reason: &'static str,
) -> io::Result<bool> {
    // Retry any failed per-destination writes before the compact checkpoint.
    // A failed ratchet remains dirty and will be attempted again on the next
    // periodic/shutdown pass. Its failure must not prevent the independent
    // known-identity/router checkpoint from advancing.
    let delta_result = persist_current_dirty_received_ratchets(state, reason).await;
    let checkpoint_result =
        persist_current_checkpoint_with(state, reason, |snapshot| snapshot.persist()).await;
    match (delta_result, checkpoint_result) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(delta_present), Ok(checkpoint_present)) => Ok(delta_present || checkpoint_present),
    }
}

async fn persist_current_dirty_received_ratchets(
    state: &AppState,
    reason: &'static str,
) -> io::Result<bool> {
    let _owner = state.lxmf_persistence_lock.lock().await;
    let snapshot = state.lxmf.lock().ok().and_then(|manager| {
        manager
            .as_ref()
            .map(|manager| manager.dirty_received_ratchets_snapshot())
    });
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    if snapshot.is_empty() {
        return Ok(true);
    }
    let artifacts = snapshot.artifact_count();
    let persisted = run_blocking(reason, artifacts, move || {
        snapshot.persist()?;
        Ok(snapshot)
    })
    .await?;
    if let Ok(mut manager) = state.lxmf.lock()
        && let Some(manager) = manager.as_mut()
    {
        manager.acknowledge_persistence_delta(&persisted);
    }
    Ok(true)
}

/// Remove only ratchet files whose destinations are still absent after this
/// cleanup job acquires the serialized persistence turn. A ratchet learned
/// while the job was queued is preserved; one learned after validation queues
/// its write behind this owner and therefore lands after the old file removal.
pub async fn delete_expired_received_ratchets(
    state: &AppState,
    hashes: &[String],
) -> io::Result<usize> {
    let _owner = state.lxmf_persistence_lock.lock().await;
    let cleanup = state.lxmf.lock().ok().and_then(|manager| {
        manager.as_ref().map(|manager| {
            (
                manager.identity_hash.clone(),
                hashes
                    .iter()
                    .filter(|hash| {
                        crate::helpers::is_protocol_hash_16(hash)
                            && !manager.received_ratchets.contains_key(*hash)
                    })
                    .map(|hash| {
                        (
                            hash.clone(),
                            manager
                                .received_ratchets_dir
                                .join(format!("{hash}.ratchet")),
                        )
                    })
                    .collect::<Vec<(String, PathBuf)>>(),
            )
        })
    });
    let Some((identity_hash, paths)) = cleanup else {
        return Ok(0);
    };
    if paths.is_empty() {
        return Ok(0);
    }
    let artifacts = paths.len();
    let (removed, failed) = run_blocking("ratchet_cleanup", artifacts, move || {
        let mut removed = 0usize;
        let mut failed = Vec::new();
        for (hash, path) in paths {
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to remove expired received ratchet");
                    failed.push(hash);
                }
            }
        }
        Ok((removed, failed))
    })
    .await?;
    if !failed.is_empty()
        && let Ok(mut manager) = state.lxmf.lock()
        && let Some(manager) = manager.as_mut()
        && manager.identity_hash == identity_hash
    {
        manager.requeue_expired_received_ratchets(failed);
    }
    Ok(removed)
}

async fn persist_current_checkpoint_with<F>(
    state: &AppState,
    reason: &'static str,
    persist: F,
) -> io::Result<bool>
where
    F: FnOnce(&LxmfCheckpointSnapshot) -> io::Result<()> + Send + 'static,
{
    let queued_at = Instant::now();
    let _owner = state.lxmf_persistence_lock.lock().await;
    let queued = queued_at.elapsed();
    let snapshot: Option<LxmfCheckpointSnapshot> = state.lxmf.lock().ok().and_then(|manager| {
        manager
            .as_ref()
            .map(|manager| manager.checkpoint_snapshot())
    });
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    let identities = snapshot.known_identities_count();
    tracing::debug!(
        reason,
        identities,
        queued_ms = queued.as_millis() as u64,
        "LXMF checkpoint snapshot captured"
    );
    let persisted = run_blocking(reason, 2, move || {
        persist(&snapshot)?;
        Ok(snapshot)
    })
    .await?;
    if let Ok(mut manager) = state.lxmf.lock()
        && let Some(manager) = manager.as_mut()
    {
        manager.acknowledge_checkpoint_snapshot(&persisted);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use crate::lxmf::LxmfManager;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    fn make_state() -> Arc<AppState> {
        let tmp = tempfile::TempDir::new().unwrap().keep();
        let config = DashboardConfig::from_env_and_defaults(tmp.clone());
        let pool = r2d2::Pool::builder()
            .max_size(2)
            .build(SqliteConnectionManager::memory())
            .unwrap();
        crate::db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        *state.lxmf.lock().unwrap() = Some(LxmfManager::load_or_create(&tmp, None, None).unwrap());
        state
    }

    #[tokio::test]
    async fn blocked_checkpoint_io_never_holds_live_lxmf_manager() {
        let state = make_state();
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let task_state = state.clone();
        let task_started = started.clone();
        let task_release = release.clone();
        let task = tokio::spawn(async move {
            persist_current_checkpoint_with(&task_state, "test_blocked", move |_snapshot| {
                task_started.store(true, Ordering::Release);
                let (lock, wake) = &*task_release;
                let released = lock.lock().unwrap();
                drop(wake.wait_while(released, |released| !*released).unwrap());
                Ok(())
            })
            .await
        });

        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        assert!(
            state.lxmf.try_lock().is_ok(),
            "blocking persistence must not retain protocol-manager ownership"
        );
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        assert_eq!(task.await.unwrap().unwrap(), true);
    }

    #[tokio::test]
    async fn failed_identity_and_ratchet_delta_remain_dirty_for_retry() {
        let state = make_state();
        let hash = "11".repeat(16);
        {
            let mut manager = state.lxmf.lock().unwrap();
            manager
                .as_mut()
                .unwrap()
                .update_remote_crypto(&hash, &[7; 64], Some(&[1; 32]));
        }

        let error =
            persist_current_delta_with(&state, false, &[], false, "test_failure", |_snapshot| {
                Err(io::Error::other("injected"))
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        let still_dirty = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .persistence_delta_snapshot(false, &[], false);
        assert_eq!(still_dirty.artifact_count(), 2);

        persist_current_delta_with(&state, false, &[], false, "test_retry", |_snapshot| Ok(()))
            .await
            .unwrap();
        let clean = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .persistence_delta_snapshot(false, &[], false);
        assert_eq!(clean.artifact_count(), 0);
    }

    #[tokio::test]
    async fn newer_ratchet_version_is_not_cleared_by_older_write_ack() {
        let state = make_state();
        let hash = "22".repeat(16);
        {
            let mut manager = state.lxmf.lock().unwrap();
            manager
                .as_mut()
                .unwrap()
                .update_remote_crypto(&hash, &[8; 64], Some(&[1; 32]));
        }

        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let task_state = state.clone();
        let task_started = started.clone();
        let task_release = release.clone();
        let task = tokio::spawn(async move {
            persist_current_delta_with(
                &task_state,
                false,
                &[],
                false,
                "test_superseded",
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
        {
            let mut manager = state.lxmf.lock().unwrap();
            manager
                .as_mut()
                .unwrap()
                .update_remote_crypto(&hash, &[8; 64], Some(&[2; 32]));
        }
        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        assert_eq!(task.await.unwrap().unwrap(), true);

        let newer_is_dirty = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .persistence_delta_snapshot(false, &[], false);
        assert_eq!(newer_is_dirty.artifact_count(), 1);
        persist_current_delta_with(&state, false, &[], false, "test_newer_retry", |_snapshot| {
            Ok(())
        })
        .await
        .unwrap();
        let clean = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .persistence_delta_snapshot(false, &[], false);
        assert_eq!(clean.artifact_count(), 0);
    }

    #[tokio::test]
    async fn ratchet_cleanup_revalidates_live_memory_before_file_removal() {
        let state = make_state();
        let hash = "44".repeat(16);
        let ratchet = rns_identity::ratchet::ReceivedRatchet::new([4; 32]);
        let path = {
            let mut manager = state.lxmf.lock().unwrap();
            let manager = manager.as_mut().unwrap();
            manager.received_ratchets.insert(hash.clone(), ratchet);
            manager
                .received_ratchets_dir
                .join(format!("{hash}.ratchet"))
        };
        ratchet.save(&path).unwrap();

        assert_eq!(
            delete_expired_received_ratchets(&state, std::slice::from_ref(&hash))
                .await
                .unwrap(),
            0
        );
        assert!(path.exists());
        state
            .lxmf
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .received_ratchets
            .remove(&hash);
        assert_eq!(
            delete_expired_received_ratchets(&state, &[hash])
                .await
                .unwrap(),
            1
        );
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn failed_ratchet_file_removal_is_requeued() {
        let state = make_state();
        let hash = "66".repeat(16);
        let path = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .received_ratchets_dir
            .join(format!("{hash}.ratchet"));
        std::fs::create_dir(&path).unwrap();

        assert_eq!(
            delete_expired_received_ratchets(&state, std::slice::from_ref(&hash))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            state
                .lxmf
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .take_expired_received_ratchets(),
            vec![hash]
        );
    }
}

/// Persist a known-identities snapshot while the caller owns the serialized
/// persistence turn. Used by pruning so its memory mutation, durable artifact,
/// and conditional DB cleanup cannot be interleaved with another snapshot.
pub async fn persist_known_identities_under_owner(
    _owner: &tokio::sync::MutexGuard<'_, ()>,
    snapshot: KnownIdentitiesSnapshot,
    reason: &'static str,
) -> io::Result<KnownIdentitiesSnapshot> {
    let count = snapshot.count;
    tracing::debug!(
        reason,
        identities = count,
        "LXMF identity snapshot captured"
    );
    run_blocking(reason, 1, move || {
        snapshot.persist()?;
        Ok(snapshot)
    })
    .await
}

pub async fn persist_current_known_identities_under_owner(
    state: &AppState,
    owner: &tokio::sync::MutexGuard<'_, ()>,
    reason: &'static str,
) -> io::Result<bool> {
    let snapshot = state.lxmf.lock().ok().and_then(|manager| {
        manager
            .as_ref()
            .map(|manager| manager.known_identities_snapshot())
    });
    let Some(snapshot) = snapshot else {
        return Ok(false);
    };
    let persisted = persist_known_identities_under_owner(owner, snapshot, reason).await?;
    if let Ok(mut manager) = state.lxmf.lock()
        && let Some(manager) = manager.as_mut()
    {
        manager.acknowledge_known_identities_snapshot(&persisted);
    }
    Ok(true)
}
