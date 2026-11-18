//! Serialized LXMF persistence outside the live protocol-manager lock.

use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::lxmf::{KnownIdentitiesSnapshot, LxmfCheckpointSnapshot};
use crate::state::AppState;

/// A hub delivers announces in bursts, and each write rewrites every known
/// identity with several fsyncs; announce paths share one write per window.
const ANNOUNCE_PERSISTENCE_COALESCE: Duration = Duration::from_secs(10);

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
    router_changed: bool,
    reason: &'static str,
) -> io::Result<bool> {
    persist_current_delta_with(
        state,
        identities_changed,
        router_changed,
        reason,
        |snapshot| snapshot.persist(),
    )
    .await
}

async fn persist_current_delta_with<F>(
    state: &AppState,
    identities_changed: bool,
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
        manager
            .as_ref()
            .map(|manager| manager.persistence_delta_snapshot(identities_changed, router_changed))
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

/// Persist announce-learned identities and router state within
/// [`ANNOUNCE_PERSISTENCE_COALESCE`]. The session shutdown checkpoint writes
/// whatever a pending flush has not.
pub fn schedule_announce_persistence(state: &Arc<AppState>, router_changed: bool) {
    if router_changed {
        state
            .announce_router_persistence_pending
            .store(true, Ordering::Release);
    }
    if state
        .announce_persistence_scheduled
        .swap(true, Ordering::AcqRel)
    {
        return;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        tokio::time::sleep(ANNOUNCE_PERSISTENCE_COALESCE).await;
        state
            .announce_persistence_scheduled
            .store(false, Ordering::Release);
        let router_changed = state
            .announce_router_persistence_pending
            .swap(false, Ordering::AcqRel);
        // Identity keys are written when dirty; a failed write stays dirty.
        if let Err(error) = persist_current_delta(&state, false, router_changed, "announces").await
        {
            if router_changed {
                state
                    .announce_router_persistence_pending
                    .store(true, Ordering::Release);
            }
            tracing::warn!(%error, "announce-derived LXMF persistence failed");
        }
    });
}

/// Persist the current periodic/shutdown checkpoint.
pub async fn persist_current_checkpoint(
    state: &AppState,
    reason: &'static str,
) -> io::Result<bool> {
    persist_current_checkpoint_with(state, reason, |snapshot| snapshot.persist()).await
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
        crate::lxmf::initialize_storage_namespace(&tmp).unwrap();
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
        assert!(task.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn failed_identity_delta_remains_dirty_for_retry() {
        let state = make_state();
        let hash = "11".repeat(16);
        {
            let mut manager = state.lxmf.lock().unwrap();
            manager
                .as_mut()
                .unwrap()
                .update_remote_identity(&hash, &[7; 64]);
        }

        let error = persist_current_delta_with(&state, false, false, "test_failure", |_snapshot| {
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
            .persistence_delta_snapshot(false, false);
        assert_eq!(still_dirty.artifact_count(), 1);

        persist_current_delta_with(&state, false, false, "test_retry", |_snapshot| Ok(()))
            .await
            .unwrap();
        let clean = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .persistence_delta_snapshot(false, false);
        assert_eq!(clean.artifact_count(), 0);
    }
    /// Feed `announce` to Reticulum as heard on a new interface.
    async fn hear(
        rns: &crate::rns::RnsManager,
        announce: &[u8],
    ) -> tokio::sync::mpsc::Receiver<bytes::Bytes> {
        use rns_transport::messages::{InboundPacket, InterfaceEntry, TransportMessage};
        let (driver_tx, driver_rx) = tokio::sync::mpsc::channel(64);
        let entry = InterfaceEntry::new(
            "heard".into(),
            rns_transport::constants::InterfaceMode::Full,
            rns_transport::constants::InterfaceDirection::bidirectional(),
            10_000_000,
            500,
            driver_tx,
        );
        let owner = entry.owner.clone();
        let transport = &rns.handle.transport_tx;
        transport
            .send(TransportMessage::RegisterInterface { id: 9001, entry })
            .await
            .unwrap();
        transport
            .send(TransportMessage::InterfaceEvent {
                interface_id: 9001,
                owner,
                message: Box::new(TransportMessage::Inbound(InboundPacket {
                    raw: bytes::Bytes::copy_from_slice(announce),
                    interface_id: 9001,
                    rssi: None,
                    snr: None,
                    q: None,
                })),
            })
            .await
            .unwrap();
        driver_rx
    }

    async fn reticulum(dir: &std::path::Path) -> crate::rns::RnsManager {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("config"),
            "[reticulum]\nshare_instance = No\nenable_transport = No\n\n[interfaces]\n",
        )
        .unwrap();
        crate::rns::RnsManager::init(
            dir.to_str().unwrap(),
            Some(dir.join("cache")),
            Arc::new(AtomicBool::new(true)),
        )
        .await
        .unwrap()
    }

    /// Reticulum hears the recipient's announce and stops. After a restart in
    /// which nothing is heard, the runtime must encrypt for the recipient with
    /// `ratchet`.
    #[tokio::test]
    async fn a_ratchet_learned_before_a_restart_encrypts_the_next_message() {
        use rns_identity::destination::{DestType, Destination, Direction};
        use rns_transport::messages::{TransportQuery, TransportQueryResponse};

        let state = make_state();
        let data_dir = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .data_dir
            .clone();
        let rns_dir = data_dir.join("reticulum-under-test");
        let recipient = rns_identity::identity::Identity::new();
        let ratchet_key = rns_crypto::x25519::X25519PrivateKey::generate().to_bytes();
        let ratchet = rns_identity::ratchet::ratchet_public_bytes(&ratchet_key);
        let mut destination = Destination::new(
            Some(&recipient),
            Direction::In,
            DestType::Single,
            "lxmf.delivery",
        )
        .unwrap();
        let announce = destination
            .announce_packet(&recipient, None, Some(&ratchet), false, None, now())
            .unwrap();
        let recipient_hash = destination.hash;
        let recipient_hex = hex::encode(recipient_hash);

        let rns = reticulum(&rns_dir).await;
        let _driver = hear(&rns, &announce).await;
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            rns.handle
                .query_transport(TransportQuery::RecallDestination {
                    dest: recipient_hash
                })
                .await,
            Some(TransportQueryResponse::RecalledDestination(Some(_)))
        ) {
            assert!(
                Instant::now() < deadline,
                "Reticulum validates the announce"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        rns.shutdown().await;

        state.set_rns(reticulum(&rns_dir).await);
        submit(
            &state,
            &recipient_hex,
            &recipient.get_public_key(),
            "after the restart",
        );
        // Before Reticulum has answered for this tick nothing is encrypted.
        assert!(
            state
                .lxmf
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .encrypt_for_destination(&recipient_hex, b"x")
                .is_none()
        );
        let ciphertext = encrypt_after_refresh(&state, &recipient_hex, b"after the restart").await;
        // Enforcing ratchets, the recipient opens it only with that ratchet key.
        let opened = recipient
            .decrypt_with_ratchet_id(&ciphertext, Some(&[&ratchet_key]), true)
            .unwrap();
        assert_eq!(opened.plaintext, b"after the restart");
        assert!(opened.ratchet_id.is_some());
        let rns = state.rns.write().unwrap().take().unwrap();
        rns.shutdown().await;
    }

    /// An earlier release kept a contact's ratchet in its own file, which
    /// Reticulum, as upstream, does not import: the file goes, a message sent
    /// before the contact announces again is encrypted to its identity key, and
    /// the next announce's ratchet encrypts the message after it.
    #[tokio::test]
    async fn an_earlier_release_ratchet_file_goes_and_the_next_announce_brings_the_ratchet() {
        use rns_identity::destination::{DestType, Destination, Direction};
        use rns_transport::messages::{TransportQuery, TransportQueryResponse};

        let state = make_state();
        let (data_dir, identity_hash) = {
            let lxmf = state.lxmf.lock().unwrap();
            let manager = lxmf.as_ref().unwrap();
            (manager.data_dir.clone(), manager.identity_hash.clone())
        };
        let rns_dir = data_dir.join("reticulum-under-test");
        let recipient = rns_identity::identity::Identity::new();
        let mut destination = Destination::new(
            Some(&recipient),
            Direction::In,
            DestType::Single,
            "lxmf.delivery",
        )
        .unwrap();
        let recipient_hash = destination.hash;
        let recipient_hex = hex::encode(recipient_hash);

        let received = data_dir
            .join("identities")
            .join(&identity_hash)
            .join("ratchets/received");
        std::fs::create_dir_all(&received).unwrap();
        let earlier_key = rns_crypto::x25519::X25519PrivateKey::generate().to_bytes();
        rns_identity::ratchet::ReceivedRatchet::new(rns_identity::ratchet::ratchet_public_bytes(
            &earlier_key,
        ))
        .save(&received.join(format!("{recipient_hex}.ratchet")))
        .unwrap();
        crate::lxmf::remove_earlier_received_ratchets(&data_dir, &identity_hash).unwrap();
        assert!(!received.exists());

        state.set_rns(reticulum(&rns_dir).await);
        submit(
            &state,
            &recipient_hex,
            &recipient.get_public_key(),
            "before it announces",
        );
        let ciphertext =
            encrypt_after_refresh(&state, &recipient_hex, b"before it announces").await;
        assert!(
            recipient
                .decrypt_with_ratchet_id(&ciphertext, Some(&[&earlier_key]), true)
                .is_err(),
            "the earlier release's ratchet is not used"
        );
        let opened = recipient
            .decrypt_with_ratchet_id(&ciphertext, None, false)
            .unwrap();
        assert_eq!(opened.plaintext, b"before it announces");
        assert!(opened.ratchet_id.is_none(), "encrypted to the identity key");

        let ratchet_key = rns_crypto::x25519::X25519PrivateKey::generate().to_bytes();
        let ratchet = rns_identity::ratchet::ratchet_public_bytes(&ratchet_key);
        let announce = destination
            .announce_packet(&recipient, None, Some(&ratchet), false, None, now())
            .unwrap();
        let rns = state.rns.write().unwrap().take().unwrap();
        let _driver = hear(&rns, &announce).await;
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !matches!(
            rns.handle
                .query_transport(TransportQuery::RecallDestination {
                    dest: recipient_hash
                })
                .await,
            Some(TransportQueryResponse::RecalledDestination(Some(ref recalled)))
                if recalled.ratchet == Some(ratchet)
        ) {
            assert!(
                Instant::now() < deadline,
                "Reticulum keeps the announced ratchet"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        state.set_rns(rns);
        submit(
            &state,
            &recipient_hex,
            &recipient.get_public_key(),
            "after it announces",
        );
        let ciphertext = encrypt_after_refresh(&state, &recipient_hex, b"after it announces").await;
        let opened = recipient
            .decrypt_with_ratchet_id(&ciphertext, Some(&[&ratchet_key]), true)
            .unwrap();
        assert_eq!(opened.plaintext, b"after it announces");
        assert!(opened.ratchet_id.is_some());
        let rns = state.rns.write().unwrap().take().unwrap();
        rns.shutdown().await;
    }

    fn now() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    /// Queue an opportunistic message to `recipient_hex`, whose key the runtime knows.
    fn submit(state: &AppState, recipient_hex: &str, public_key: &[u8; 64], content: &str) {
        let mut lxmf = state.lxmf.lock().unwrap();
        let manager = lxmf.as_mut().unwrap();
        manager.update_remote_identity(recipient_hex, public_key);
        let message = manager
            .create_message(
                recipient_hex,
                content,
                "",
                lxmf_core::constants::DeliveryMethod::Opportunistic,
            )
            .unwrap();
        manager.router.submit_signed(message).unwrap();
    }

    /// Encrypt as a tick does, once Reticulum has answered for the recipient.
    async fn encrypt_after_refresh(
        state: &AppState,
        recipient_hex: &str,
        plaintext: &[u8],
    ) -> Vec<u8> {
        crate::refresh_outbound_ratchets(state).await;
        state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .encrypt_for_destination(recipient_hex, plaintext)
            .expect("Reticulum answered for the recipient")
    }

    #[test]
    fn received_ratchet_files_of_earlier_releases_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join(".ratspeak");
        let identity = "ab".repeat(16);
        let ratchets = data_dir.join("identities").join(&identity).join("ratchets");
        let received = ratchets.join("received");
        std::fs::create_dir_all(&received).unwrap();
        // The identity's own ratchet keys and known identities sit beside it and stay.
        std::fs::write(ratchets.join("ring"), b"own keys").unwrap();
        std::fs::write(ratchets.join("known_identities"), b"known").unwrap();
        std::fs::write(received.join(format!("{}.ratchet", "11".repeat(16))), b"r").unwrap();
        std::fs::write(received.join("human-label.ratchet"), b"not a destination").unwrap();

        crate::lxmf::remove_earlier_received_ratchets(&data_dir, &identity).unwrap();

        assert!(!received.exists());
        assert_eq!(std::fs::read(ratchets.join("ring")).unwrap(), b"own keys");
        assert_eq!(
            std::fs::read(ratchets.join("known_identities")).unwrap(),
            b"known"
        );
        // Nothing left to remove.
        crate::lxmf::remove_earlier_received_ratchets(&data_dir, &identity).unwrap();
        assert!(crate::lxmf::remove_earlier_received_ratchets(&data_dir, "not-a-hash").is_err());
    }
}
