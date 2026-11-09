//! Propagation downloads commit their application effect and ciphertext receipt
//! together. Transport completion alone never authorizes a remote purge.
mod game;

use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use lxmf_core::admission::{
    AdmissionEpochs, AdmissionError, AdmissionRecord, AdmissionScope, DownloadAdmissionOutcome,
    DownloadedPropagationMessage, DurableAdmissionBackend, DurableAdmissionReceipt,
    DurableDisposition, DurableHave, ProcessingLease, SuppressionLedger, SuppressionReason,
    SuppressionRecord,
};
use lxmf_core::message_api::{LxMessage, MessageDecodeLimits};
use ratspeak_db::admission::{
    AdmissionDbError, AdmissionGuard, AdmissionReceipt, Disposition, DurableInboundStore,
    EffectCommit, FileReference, PreparedAdmission, try_reserve_admission,
};
use rns_crypto::sha::sha256;
use rns_identity::persistence::{
    durable_create_dir_all, durable_reconcile_hash, durable_remove, durable_write,
    validate_durable_storage_directory,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{ActivityRequestFence, AppState, db, lxmf};

fn jobs() -> &'static Arc<tokio::sync::RwLock<()>> {
    static JOBS: std::sync::OnceLock<Arc<tokio::sync::RwLock<()>>> = std::sync::OnceLock::new();
    JOBS.get_or_init(|| Arc::new(tokio::sync::RwLock::new(())))
}
/// Trigger session shutdown first. Guards follow actual blocking workers.
pub(crate) async fn quiesce() -> tokio::sync::OwnedRwLockWriteGuard<()> {
    jobs().clone().write_owned().await
}

const SUPPRESSION_REASONS: [SuppressionReason; 13] = [
    SuppressionReason::Malformed,
    SuppressionReason::WrongDestination,
    SuppressionReason::InvalidSignature,
    SuppressionReason::InvalidStamp,
    SuppressionReason::DecryptionFailed,
    SuppressionReason::TooLarge,
    SuppressionReason::MissingKey,
    SuppressionReason::StorageUnavailable,
    SuppressionReason::StorageBusy,
    SuppressionReason::Quota,
    SuppressionReason::WorkerUnavailable,
    SuppressionReason::DispositionLedgerFull,
    SuppressionReason::Cancelled,
];

fn encode_suppression(record: &SuppressionRecord) -> Result<Vec<u8>, AdmissionDbError> {
    use rmpv::Value as V;
    let reason = SUPPRESSION_REASONS
        .iter()
        .position(|reason| *reason == record.reason)
        .ok_or(AdmissionDbError::MissingEffect)?;
    let value = V::Array(vec![
        1u8.into(),
        V::Binary(record.scope.identity.to_vec()),
        V::Binary(record.scope.node.to_vec()),
        V::Binary(record.scope.transient_id.to_vec()),
        (reason as u8).into(),
        V::Binary(record.epochs.policy.to_vec()),
        V::Binary(record.epochs.keys.to_vec()),
        V::Binary(record.epochs.budget.to_vec()),
        V::Binary(record.epochs.storage_health.to_vec()),
        record.attempts.into(),
        record.last_failure.into(),
        record.next_retry.into(),
        record.expires_at.into(),
    ]);
    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &value).map_err(storage)?;
    if encoded.len() > 512 {
        return Err(AdmissionDbError::TooLarge);
    }
    Ok(encoded)
}
fn decode_suppression(encoded: &[u8]) -> Result<SuppressionRecord, AdmissionDbError> {
    if encoded.len() > 512 {
        return Err(AdmissionDbError::MissingEffect);
    }
    // Validate structure before the owned decoder can trust a declared length.
    lxmf_core::message_api::EncodedFieldValue::new(Bytes::copy_from_slice(encoded))
        .map_err(storage)?;
    let value = rmpv::decode::read_value(&mut &encoded[..]).map_err(storage)?;
    let array = value
        .as_array()
        .filter(|array| array.len() == 13)
        .ok_or(AdmissionDbError::MissingEffect)?;
    let integer = |index: usize| array[index].as_u64().ok_or(AdmissionDbError::MissingEffect);
    fn binary<const N: usize>(value: &rmpv::Value) -> Result<[u8; N], AdmissionDbError> {
        value
            .as_slice()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(AdmissionDbError::MissingEffect)
    }
    if integer(0)? != 1 {
        return Err(AdmissionDbError::MissingEffect);
    }
    let reason = *SUPPRESSION_REASONS
        .get(usize::try_from(integer(4)?).map_err(storage)?)
        .ok_or(AdmissionDbError::MissingEffect)?;
    let record = SuppressionRecord {
        scope: AdmissionScope {
            identity: binary(&array[1])?,
            node: binary(&array[2])?,
            transient_id: binary(&array[3])?,
        },
        reason,
        epochs: AdmissionEpochs {
            policy: binary(&array[5])?,
            keys: binary(&array[6])?,
            budget: binary(&array[7])?,
            storage_health: binary(&array[8])?,
        },
        attempts: u16::try_from(integer(9)?).map_err(storage)?,
        last_failure: integer(10)?,
        next_retry: integer(11)?,
        expires_at: integer(12)?,
    };
    if record.attempts == 0
        || record.next_retry < record.last_failure
        || record.expires_at <= record.last_failure
        || record.expires_at > record.last_failure.saturating_add(30 * 86400)
    {
        return Err(AdmissionDbError::MissingEffect);
    }
    Ok(record)
}

fn epochs(
    state: &AppState,
    store: &DurableInboundStore,
    identity: [u8; 16],
    keys: [u8; 32],
) -> Result<AdmissionEpochs, AdmissionDbError> {
    let (blocked, storage_revision) = store.policy_state(&hex::encode(identity))?;
    let mut policy = b"ratspeak-inbound-validation-v1".to_vec();
    policy.push(u8::from(
        state
            .enforce_stamps
            .load(std::sync::atomic::Ordering::Relaxed),
    ));
    policy.push(
        state
            .required_stamp_cost
            .load(std::sync::atomic::Ordering::Relaxed),
    );
    for peer in blocked {
        policy.extend_from_slice(&(peer.len() as u64).to_be_bytes());
        policy.extend_from_slice(peer.as_bytes());
    }
    let mut budget = b"ratspeak-inbound-budget-v1".to_vec();
    for size in [
        db::admission::MAX_PROJECTION_BINDINGS,
        lxmf_core::admission::MAX_PROCESSING_BYTES,
        lxmf_core::admission::MAX_RETAINED_DOWNLOAD_BYTES,
    ] {
        budget.extend_from_slice(&(size as u64).to_be_bytes());
    }
    Ok(AdmissionEpochs {
        policy: sha256(&policy),
        keys,
        budget: sha256(&budget),
        storage_health: sha256(&storage_revision.to_be_bytes()),
    })
}

fn persist_suppression(
    store: &DurableInboundStore,
    snapshot: &lxmf::DownloadSuppressionSnapshot,
) -> Result<(), AdmissionDbError> {
    let identity = hex::encode(snapshot.identity);
    let old = store
        .suppression_rows(&identity, now())?
        .into_iter()
        .map(|bytes| decode_suppression(&bytes))
        .collect::<Result<Vec<_>, _>>()?;
    let remove = old
        .iter()
        .filter(|record| {
            record.scope.node == snapshot.node && snapshot.ledger.get(&record.scope).is_none()
        })
        .map(|record| {
            (
                identity.clone(),
                record.scope.node,
                record.scope.transient_id,
            )
        })
        .collect::<Vec<_>>();
    for batch in remove.chunks(256) {
        store.write_suppression(&[], batch, now())?;
    }
    let mut batch = Vec::with_capacity(256);
    for record in snapshot.ledger.records().filter(|record| {
        record.scope.identity == snapshot.identity && record.scope.node == snapshot.node
    }) {
        batch.push(db::admission::SuppressionRow {
            identity: identity.clone(),
            node: record.scope.node,
            transient_id: record.scope.transient_id,
            encoded: encode_suppression(record)?,
            expires_at: record.expires_at,
        });
        if batch.len() == 256 {
            store.write_suppression(&batch, &[], now())?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        store.write_suppression(&batch, &[], now())?;
    }
    Ok(())
}

#[derive(Default)]
pub(crate) struct DownloadCoordinator {
    installed: Option<(u64, [u8; 16], [u8; 16], AdmissionEpochs)>,
    next_check: u64,
    storage_failed: bool,
    intent_cursor: usize,
    deletion_cursor: i64,
    next_cleanup: u64,
    #[cfg(test)]
    hydration_pause: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
}
impl DownloadCoordinator {
    pub(crate) async fn synchronize(&mut self, state: &Arc<AppState>) {
        let shutdown = state
            .session_shutdown
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let job = Arc::new(jobs().clone().read_owned().await);
        if shutdown.is_triggered() {
            return;
        }
        // User deletion recovery also runs when inbox downloads are disabled
        // or no remote node is selected.
        if now() >= self.next_cleanup {
            self.next_cleanup = now().saturating_add(30);
            let worker_state = state.clone();
            let cleanup_job = job.clone();
            let cursor = self.deletion_cursor;
            match tokio::task::spawn_blocking(move || {
                let _job = cleanup_job;
                let guard = try_reserve_admission()?;
                let store = DurableInboundStore::from_pool(&worker_state.db)?;
                cleanup_deleted_batch(&worker_state.config.data_dir, &store, &guard, None, cursor)
            })
            .await
            {
                Ok(Ok((cursor, _))) => self.deletion_cursor = cursor,
                Ok(Err(AdmissionDbError::Busy)) => {}
                other => tracing::warn!(?other, "deleted message cleanup will be retried"),
            }
            if shutdown.is_triggered() {
                return;
            }
        }
        let generation = state.current_identity_session_generation();
        let Some((identity, node, keys, has_state)) = state.lxmf.lock().ok().and_then(|manager| {
            let manager = manager.as_ref()?;
            Some((
                manager.identity.hash,
                manager.configured_propagation_node?,
                manager.download_key_epoch(),
                manager.download_state_installed(),
            ))
        }) else {
            self.installed = None;
            return;
        };
        let initial = !has_state
            || self.installed.as_ref().is_none_or(
                |&(old_generation, old_identity, old_node, _)| {
                    (old_generation, old_identity, old_node) != (generation, identity, node)
                },
            );
        if now() < self.next_check {
            return;
        }
        self.next_check = now().saturating_add(5);
        // A delete/reset can revoke cached haves while the database work is in
        // flight, even without switching identity or node. Capture after the
        // snapshot call, which can itself advance the revision.
        let Some((snapshot, revision)) = state.lxmf.lock().ok().and_then(|mut manager| {
            let manager = manager.as_mut()?;
            if manager.identity.hash != identity
                || manager.configured_propagation_node != Some(node)
            {
                return None;
            }
            let snapshot = if initial {
                None
            } else {
                manager.download_suppression_snapshot()
            };
            Some((snapshot, manager.download_state_revision()))
        }) else {
            return;
        };
        let state_worker = Arc::clone(state);
        let recovering = self.storage_failed;
        let intent_cursor = self.intent_cursor;
        self.intent_cursor = self.intent_cursor.wrapping_add(256);
        let outcome = tokio::task::spawn_blocking(move || -> Result<_, AdmissionDbError> {
            let _job = job;
            let guard = try_reserve_admission()?;
            let store = DurableInboundStore::from_pool(&state_worker.db)?;
            validate_durable_storage_directory(
                store
                    .path()
                    .parent()
                    .ok_or(AdmissionDbError::MissingEffect)?,
            )
            .map_err(storage)?;
            let archive_dir = state_worker
                .config
                .data_dir
                .join("identities")
                .join(hex::encode(identity))
                .join("propagation-admissions");
            reconcile_intents(&store, &archive_dir, &hex::encode(identity), intent_cursor)?;
            if recovering {
                store.reconcile_barrier()?;
                store.storage_recovered(&guard, &hex::encode(identity))?;
            }
            let current_epochs = epochs(&state_worker, &store, identity, keys)?;
            if let Some(snapshot) = snapshot.as_ref() {
                persist_suppression(&store, snapshot)?;
            }
            let initial_state = if initial {
                store.reconcile_barrier()?;
                let mut records = Vec::new();
                for bytes in store.suppression_rows(&hex::encode(identity), now())? {
                    let record = decode_suppression(&bytes)?;
                    if record.scope.identity != identity {
                        return Err(AdmissionDbError::MissingEffect);
                    }
                    records.push(record);
                }
                let mut ledger = SuppressionLedger::from_records(records).map_err(storage)?;
                ledger.prune(now(), current_epochs);
                let archive_dir = state_worker
                    .config
                    .data_dir
                    .join("identities")
                    .join(hex::encode(identity))
                    .join("propagation-admissions");
                let mut haves = Vec::new();
                for (receipt, archive, files) in store.receipts(&hex::encode(identity), now())? {
                    if receipt.node != node {
                        continue;
                    }
                    let verified = match (receipt.stored_wire_hash, archive) {
                        (Some(hash), Some(path)) => {
                            verify_archive(&archive_dir, &path, hash, &files).is_ok()
                        }
                        (None, _) => matches!(
                            receipt.disposition,
                            Disposition::PolicyDiscard | Disposition::UserDeleted
                        ),
                        _ => false,
                    };
                    if verified {
                        haves.push(DurableHave {
                            transient_id: receipt.transient_id,
                            expires_at: matches!(
                                receipt.disposition,
                                Disposition::PolicyDiscard | Disposition::UserDeleted
                            )
                            .then_some(
                                receipt
                                    .committed_at
                                    .saturating_add(db::admission::POLICY_RECEIPT_SECONDS),
                            ),
                        });
                    }
                }
                Some((ledger, haves))
            } else {
                None
            };
            Ok((current_epochs, initial_state, snapshot))
        })
        .await;
        #[cfg(test)]
        if let Some((ready, resume)) = self.hydration_pause.take() {
            let _ = ready.send(());
            let _ = resume.await;
        }
        let _lifecycle = state.identity_switch_lock.lock_routine().await;
        if state.current_identity_session_generation() != generation || shutdown.is_triggered() {
            return;
        }
        let result = outcome.map_err(storage).and_then(|value| value);
        if let Ok(mut manager) = state.lxmf.lock()
            && let Some(manager) = manager.as_mut()
            && manager.identity.hash == identity
            && manager.configured_propagation_node == Some(node)
            && manager.download_state_revision() == revision
        {
            match result {
                Ok((current_epochs, initial_state, snapshot)) => {
                    self.storage_failed = false;
                    if let Some((ledger, haves)) = initial_state {
                        manager.install_download_state(ledger, current_epochs, haves);
                    } else {
                        if let Some(snapshot) = snapshot {
                            manager.acknowledge_download_suppression(&snapshot);
                        }
                        if self
                            .installed
                            .as_ref()
                            .is_some_and(|entry| entry.3 != current_epochs)
                        {
                            manager.update_download_epochs(identity, current_epochs);
                        }
                    }
                    self.installed = Some((generation, identity, node, current_epochs));
                }
                Err(error) => {
                    self.storage_failed = true;
                    manager.pause_download_admission();
                    self.next_check = now().saturating_add(60);
                    tracing::warn!(%error, "propagation download persistence unavailable; remote messages retained");
                }
            }
        }
    }
}

fn storage(error: impl std::fmt::Display) -> AdmissionDbError {
    AdmissionDbError::Unavailable(error.to_string())
}
fn reason(error: &AdmissionDbError) -> SuppressionReason {
    match error {
        AdmissionDbError::TooLarge => SuppressionReason::TooLarge,
        AdmissionDbError::Busy => SuppressionReason::StorageBusy,
        AdmissionDbError::LedgerFull => SuppressionReason::DispositionLedgerFull,
        _ => SuppressionReason::StorageUnavailable,
    }
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Constructed only from the return value of the acknowledged SQLite commit.
struct Committed(AdmissionReceipt);
impl DurableAdmissionBackend for Committed {
    fn commit(self) -> Result<AdmissionRecord, AdmissionError> {
        let receipt = self.0;
        let identity = hex::decode(&receipt.identity)
            .ok()
            .and_then(|value| value.try_into().ok())
            .ok_or(AdmissionError::WrongScope)?;
        Ok(AdmissionRecord {
            scope: AdmissionScope {
                identity,
                node: receipt.node,
                transient_id: receipt.transient_id,
            },
            message_id: receipt.message_id,
            received_wire_hash: receipt.received_wire_hash,
            stored_message_wire_hash: receipt.stored_wire_hash,
            disposition: match receipt.disposition {
                Disposition::Committed => DurableDisposition::Committed,
                Disposition::Duplicate => DurableDisposition::Duplicate,
                Disposition::PolicyDiscard => DurableDisposition::PolicyDiscard,
                Disposition::UserDeleted => DurableDisposition::UserDeleted,
            },
            committed_at: receipt.committed_at,
        })
    }
}

type StagedFile = FileReference;
#[derive(Serialize, Deserialize)]
struct StagingIntent {
    version: u8,
    identity: String,
    transient_id: [u8; 32],
    message_id: [u8; 32],
    received_wire_hash: [u8; 32],
    archive: String,
    media: Vec<StagedFile>,
}

const MAX_STAGING_INTENTS: usize = 4096;
const ORPHAN_GRACE: u64 = 86400;
fn staging_intents(root: &Path) -> Result<Vec<std::path::PathBuf>, AdmissionDbError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut intents = Vec::new();
    for entry in std::fs::read_dir(root).map_err(storage)? {
        let entry = entry.map_err(storage)?;
        if entry.path().extension().and_then(|s| s.to_str()) == Some("intent") {
            if intents.len() == MAX_STAGING_INTENTS {
                return Err(AdmissionDbError::LedgerFull);
            }
            intents.push(entry.path());
        }
    }
    intents.sort();
    Ok(intents)
}
/// Renew database authority before removing any orphan. Maintenance touches at
/// most 256 intents per pass and never mints an acceptance receipt.
fn read_intent(
    root: &Path,
    path: &Path,
    identity: &str,
) -> Result<(StagingIntent, std::fs::Metadata), AdmissionDbError> {
    let meta = std::fs::symlink_metadata(path).map_err(storage)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 16384 {
        return Err(AdmissionDbError::MissingEffect);
    }
    let bytes = rns_identity::persistence::read_file_bounded(path, 16384)
        .map_err(storage)?
        .ok_or(AdmissionDbError::MissingEffect)?;
    let value = crate::vault::parse_unique_json(&bytes, 16384).map_err(storage)?;
    let intent: StagingIntent = serde_json::from_value(value).map_err(storage)?;
    let expected = root.join(format!(
        "{}-{}.lxm",
        hex::encode(intent.message_id),
        hex::encode(intent.received_wire_hash)
    ));
    if intent.version != 1
        || intent.identity != identity
        || Path::new(&intent.archive) != expected
        || path.file_stem().and_then(|v| v.to_str())
            != Some(hex::encode(intent.transient_id).as_str())
        || intent.media.len() > 2
        || intent.media.iter().any(|f| {
            f.name.len() > 128
                || f.name.is_empty()
                || matches!(f.name.as_str(), "." | "..")
                || !f
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        })
    {
        return Err(AdmissionDbError::MissingEffect);
    }
    Ok((intent, meta))
}

/// The caller owns the admission guard through logical deletion and cleanup.
/// Failed/interrupted admissions may share content-addressed files with a
/// deleted message, so their intents remain authoritative owners too.
pub(crate) fn cleanup_deleted_artifacts(
    data_dir: &Path,
    store: &DurableInboundStore,
    guard: &AdmissionGuard,
    artifacts: &db::admission::MessageDeletionArtifacts,
) -> Result<(), AdmissionDbError> {
    let identities = artifacts
        .media
        .iter()
        .chain(&artifacts.archives)
        .map(|a| a.identity.as_str())
        .collect::<std::collections::HashSet<_>>();
    for identity in identities {
        let mut cursor = 0;
        loop {
            let (next, count) =
                cleanup_deleted_batch(data_dir, store, guard, Some(identity), cursor)?;
            if count < 256 {
                break;
            }
            cursor = next;
        }
    }
    Ok(())
}
fn cleanup_deleted_batch(
    data_dir: &Path,
    store: &DurableInboundStore,
    guard: &AdmissionGuard,
    identity: Option<&str>,
    after: i64,
) -> Result<(i64, usize), AdmissionDbError> {
    use db::admission::DeletedArtifactKind;
    let pending = store.pending_deleted_artifacts(guard, identity, after, 256)?;
    if pending.is_empty() {
        return Ok((0, 0));
    }
    // Reestablish an acknowledged FULL barrier after a previous uncertain
    // deletion transaction before treating its journal as cleanup authority.
    store.reconcile_barrier()?;
    let parent = store
        .path()
        .parent()
        .ok_or(AdmissionDbError::MissingEffect)?;
    let witness = durable_write(
        &parent.join(".lxmf-admission-witness"),
        b"ratspeak-admission-v1\n",
    )
    .map_err(storage)?;
    let identities = pending
        .iter()
        .map(|row| row.artifact.identity.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut completed = Vec::new();
    // Keep at most one identity's bounded intent index in memory.
    for identity in identities {
        if identity.len() != 32 || !identity.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(AdmissionDbError::MissingEffect);
        }
        let root = data_dir
            .join("identities")
            .join(identity)
            .join("propagation-admissions");
        let mut protected_media = std::collections::HashSet::new();
        let mut protected_archives = std::collections::HashSet::new();
        for path in staging_intents(&root)? {
            let (intent, _) = read_intent(&root, &path, identity)?;
            protected_archives.insert(intent.archive);
            for media in intent.media {
                protected_media.insert(media.name);
            }
        }
        for row in pending.iter().filter(|r| r.artifact.identity == identity) {
            let artifact = &row.artifact;
            let path = match row.kind {
                DeletedArtifactKind::Archive => {
                    let path = Path::new(&artifact.path);
                    if path.parent() != Some(root.as_path())
                        || path.extension().and_then(|s| s.to_str()) != Some("lxm")
                    {
                        return Err(AdmissionDbError::MissingEffect);
                    }
                    if protected_archives.contains(&artifact.path)
                        || store.references_archive(identity, &artifact.path)?
                    {
                        continue;
                    }
                    path.to_path_buf()
                }
                DeletedArtifactKind::Media => {
                    let name = lxmf::sanitize_stored_file_name(&artifact.path)
                        .ok_or(AdmissionDbError::MissingEffect)?;
                    if protected_media.contains(&artifact.path)
                        || store.references_media(identity, &artifact.path)?
                    {
                        continue;
                    }
                    data_dir
                        .join("identities")
                        .join(identity)
                        .join("files")
                        .join(name)
                }
            };
            // durable_remove also renews the native deletion barrier when the
            // file is already absent after an interrupted previous attempt.
            durable_remove(&path, &witness).map_err(storage)?;
            completed.push(row.id);
        }
    }
    store.complete_deleted_artifacts(guard, &completed)?;
    Ok((pending.last().unwrap().id, pending.len()))
}

/// Messages whose delete has committed; their files are still on disk.
#[must_use = "files left here wait for the cleanup sweep"]
pub struct DeletedMessages {
    guard: AdmissionGuard,
    store: DurableInboundStore,
    artifacts: db::admission::MessageDeletionArtifacts,
    data_dir: std::path::PathBuf,
}

impl DeletedMessages {
    /// A file that cannot be removed now stays journaled for the cleanup sweep.
    pub fn remove_files(self) -> Result<(), AdmissionDbError> {
        cleanup_deleted_artifacts(&self.data_dir, &self.store, &self.guard, &self.artifacts)
    }
}

pub(crate) fn delete_messages(
    state: &AppState,
    identity: &str,
    peer: Option<&str>,
) -> Result<DeletedMessages, AdmissionDbError> {
    delete_with(state, |store, guard| {
        store
            .delete_messages_guarded(guard, identity, peer, now())
            .map(|artifacts| (artifacts, ()))
    })
    .map(|(deleted, ())| deleted)
}

pub(crate) fn delete_thread_messages(
    state: &AppState,
    identity: &str,
    peer: &str,
    thread: &str,
) -> Result<DeletedMessages, AdmissionDbError> {
    delete_with(state, |store, guard| {
        store
            .delete_thread_messages_guarded(guard, identity, peer, thread, now())
            .map(|artifacts| (artifacts, ()))
    })
    .map(|(deleted, ())| deleted)
}

pub(crate) fn delete_selected_messages(
    state: &AppState,
    identity: &str,
    peer: &str,
    ids: &[String],
) -> Result<(DeletedMessages, Vec<String>), AdmissionDbError> {
    delete_with(state, |store, guard| {
        store.delete_selected_messages_guarded(guard, identity, peer, ids, now())
    })
}

fn delete_with<T>(
    state: &AppState,
    delete: impl FnOnce(
        &DurableInboundStore,
        &AdmissionGuard,
    ) -> Result<(db::admission::MessageDeletionArtifacts, T), AdmissionDbError>,
) -> Result<(DeletedMessages, T), AdmissionDbError> {
    let guard = try_reserve_admission()?;
    let store = DurableInboundStore::from_pool(&state.db)?;
    let (artifacts, value) = match delete(&store, &guard) {
        Ok(deleted) => deleted,
        Err(error) => {
            // An uncertain commit may have deleted them all the same.
            if matches!(error, AdmissionDbError::CommitUncertain(_)) {
                state.invalidate_conversation_lists();
            }
            return Err(error);
        }
    };
    // A list read before the commit is stale now, whatever happens to the files.
    state.invalidate_conversation_lists();
    Ok((
        DeletedMessages {
            guard,
            store,
            artifacts,
            data_dir: state.config.data_dir.clone(),
        },
        value,
    ))
}
fn reconcile_intents(
    store: &DurableInboundStore,
    root: &Path,
    identity: &str,
    cursor: usize,
) -> Result<(), AdmissionDbError> {
    let intents = staging_intents(root)?;
    if intents.is_empty() {
        return Ok(());
    }
    // The admission guard excludes concurrent staging. Count every active
    // intent before deleting shared content; retain only bounded file names and
    // hashes, rather than all 4096 serialized records.
    let mut media_owners = std::collections::HashMap::<String, usize>::new();
    let mut archive_owners = std::collections::HashMap::<([u8; 32], [u8; 32]), usize>::new();
    for path in &intents {
        let (intent, _) = read_intent(root, path, identity)?;
        *archive_owners
            .entry((intent.message_id, intent.received_wire_hash))
            .or_default() += 1;
        for file in intent.media {
            *media_owners.entry(file.name).or_default() += 1;
        }
    }
    store.reconcile_barrier()?;
    let parent = store
        .path()
        .parent()
        .ok_or(AdmissionDbError::MissingEffect)?;
    let witness = durable_write(
        &parent.join(".lxmf-admission-witness"),
        b"ratspeak-admission-v1\n",
    )
    .map_err(storage)?;
    for path in intents
        .iter()
        .cycle()
        .skip(cursor % intents.len())
        .take(intents.len().min(256))
    {
        let (intent, meta) = read_intent(root, path, identity)?;
        let expected = root.join(format!(
            "{}-{}.lxm",
            hex::encode(intent.message_id),
            hex::encode(intent.received_wire_hash)
        ));
        let retained = store.references_archive(identity, &intent.archive)?;
        if retained {
            verify_archive(
                root,
                &intent.archive,
                intent.received_wire_hash,
                &intent.media,
            )?;
        } else {
            let age = meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map_or(0, |age| age.as_secs());
            if age < ORPHAN_GRACE {
                continue;
            }
            // These are only this intent's content-addressed staging files.
            if archive_owners.get(&(intent.message_id, intent.received_wire_hash)) == Some(&1)
                && expected.exists()
            {
                durable_remove(&expected, &witness).map_err(storage)?;
            }
            let files = root
                .parent()
                .ok_or(AdmissionDbError::MissingEffect)?
                .join("files");
            for file in &intent.media {
                if media_owners.get(&file.name) == Some(&1)
                    && !store.references_media(identity, &file.name)?
                    && files.join(&file.name).exists()
                {
                    durable_remove(&files.join(&file.name), &witness).map_err(storage)?;
                }
            }
        }
        durable_remove(path, &witness).map_err(storage)?;
        if let Some(count) = archive_owners.get_mut(&(intent.message_id, intent.received_wire_hash))
        {
            *count -= 1;
        }
        for file in &intent.media {
            if let Some(count) = media_owners.get_mut(&file.name) {
                *count -= 1;
            }
        }
    }
    Ok(())
}

struct Media<'a> {
    name: String,
    stored: String,
    bytes: &'a [u8],
    image: bool,
    audio: Option<u8>,
}
impl Media<'_> {
    fn descriptor(&self) -> StagedFile {
        StagedFile {
            name: self.stored.clone(),
            hash: sha256(self.bytes),
            length: self.bytes.len(),
        }
    }
}

fn media(msg: &LxMessage) -> Vec<Media<'_>> {
    let mut output = Vec::with_capacity(2);
    if let Ok(Some((name, bytes))) = msg.first_file_attachment() {
        output.push(Media {
            name,
            stored: format!("{}.bin", hex::encode(sha256(bytes))),
            bytes,
            image: false,
            audio: None,
        });
    } else if let Ok(Some((mime, bytes))) = msg.image_attachment() {
        // The UI detects the actual image bytes. A peer-controlled MIME cannot
        // select a directory or turn a saved attachment into executable code.
        let extension = match mime.as_str() {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        };
        output.push(Media {
            name: format!("image.{extension}"),
            stored: format!("{}.{extension}", hex::encode(sha256(bytes))),
            bytes,
            image: true,
            audio: None,
        });
    }
    if output.is_empty()
        && (msg
            .fields()
            .contains(lxmf_core::constants::FIELD_FILE_ATTACHMENTS)
            || msg.fields().contains(lxmf_core::constants::FIELD_IMAGE))
    {
        let image = msg.fields().contains(lxmf_core::constants::FIELD_IMAGE);
        output.push(Media {
            name: if image { "Image" } else { "Attachment" }.into(),
            stored: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.into(),
            bytes: &[],
            image,
            audio: None,
        });
    }
    if let Ok(Some(audio)) = msg.audio_field()
        && valid_audio(audio.mode, audio.bytes)
    {
        let extension = match audio.mode {
            lxmf_core::constants::AM_OPUS_OGG => "opus",
            lxmf_core::constants::AM_CODEC2_700C => "c2",
            _ => "bin",
        };
        output.push(Media {
            name: "Audio message".into(),
            stored: format!("{}.{extension}", hex::encode(sha256(audio.bytes))),
            bytes: audio.bytes,
            image: false,
            audio: Some(audio.mode),
        });
    }
    if msg.fields().contains(lxmf_core::constants::FIELD_AUDIO)
        && !output.iter().any(|item| item.audio.is_some())
    {
        output.push(Media {
            name: "Audio message".into(),
            stored: db::ATTACHMENT_UNAVAILABLE_STORED_NAME.into(),
            bytes: &[],
            image: false,
            audio: Some(lxmf_core::constants::AM_CUSTOM),
        });
    }
    output
}

fn valid_audio(mode: u8, bytes: &[u8]) -> bool {
    if bytes.len() > lxmf::MAX_AUDIO_FIELD_BYTES {
        return false;
    }
    #[cfg(feature = "lxst-voice")]
    if matches!(
        mode,
        lxmf_core::constants::AM_OPUS_OGG | lxmf_core::constants::AM_CODEC2_700C
    ) {
        return crate::voice_memo::MemoFormat::from_mode(mode)
            .and_then(|format| crate::voice_memo::inspect_voice_memo_format(bytes, format))
            .is_ok();
    }
    let _ = mode;
    true
}

enum Projection {
    Game(Box<game::PreparedGame>),
    Chat {
        title: String,
        content: String,
        reply: String,
        preview: String,
        renderer: Option<u8>,
        thread: String,
    },
    Reaction {
        target: String,
        emoji: String,
        remove: bool,
    },
    Service,
}
impl Projection {
    fn prepare(
        state: &AppState,
        msg: &LxMessage,
        authenticated: bool,
    ) -> Result<Self, SuppressionReason> {
        let extension = lxmf::decode_ratspeak_chat_extension_checked(msg)?;
        if let Some(lxmf::RatspeakChatExtension::Reaction {
            target,
            emoji,
            action,
        }) = extension.as_ref()
        {
            let emoji =
                crate::sanitize_reaction_emoji(emoji).ok_or(SuppressionReason::Malformed)?;
            if target.is_empty() || target.len() > 128 {
                return Err(SuppressionReason::Malformed);
            }
            return Ok(Self::Reaction {
                target: target.clone(),
                emoji,
                remove: action == "remove",
            });
        }
        if let Some(game) = game::PreparedGame::prepare(
            state,
            msg,
            &hex::encode(msg.destination_hash()),
            authenticated,
        )? {
            return Ok(Self::Game(Box::new(game)));
        }
        if !crate::inbound_has_chat_content(msg, extension.as_ref()) {
            return Ok(Self::Service);
        }
        let (reply, preview) = crate::inbound_reply_fields(extension.as_ref());
        Ok(Self::Chat {
            title: msg.title_text().into_owned(),
            content: msg.content_text().into_owned(),
            reply,
            preview,
            renderer: lxmf::message_renderer(msg),
            thread: lxmf::message_thread(msg),
        })
    }
    fn bindings(&self) -> Vec<usize> {
        match self {
            Self::Chat {
                title,
                content,
                reply,
                preview,
                ..
            } => vec![title.len(), content.len(), reply.len(), preview.len()],
            Self::Reaction { target, emoji, .. } => vec![target.len(), emoji.len()],
            Self::Service => Vec::new(),
            Self::Game(game) => game.binding_lengths(),
        }
    }
}

fn verify_archive(
    root: &Path,
    path: &str,
    hash: [u8; 32],
    files: &[FileReference],
) -> Result<(), AdmissionDbError> {
    let path = Path::new(path);
    // Stored paths are private metadata, but corruption must not make recovery
    // flush or trust a different identity's files or an arbitrary external path.
    let parent = path.parent().ok_or(AdmissionDbError::MissingEffect)?;
    if parent != root || path.file_name().is_none() {
        return Err(AdmissionDbError::MissingEffect);
    }
    durable_reconcile_hash(path, hash, MessageDecodeLimits::default().max_wire_bytes)
        .map_err(storage)?;
    let media_dir = root
        .parent()
        .ok_or(AdmissionDbError::MissingEffect)?
        .join("files");
    for file in files {
        let receipt = durable_reconcile_hash(&media_dir.join(&file.name), file.hash, file.length)
            .map_err(storage)?;
        if receipt.len() != file.length {
            return Err(AdmissionDbError::MissingEffect);
        }
    }
    Ok(())
}

struct CommitOutput {
    receipt: DurableAdmissionReceipt,
    projection: Projection,
    display_media: serde_json::Map<String, serde_json::Value>,
    timestamp: f64,
}

fn commit_message(
    state: &AppState,
    guard: AdmissionGuard,
    scope: AdmissionScope,
    msg: &LxMessage,
    mut projection: Projection,
) -> Result<CommitOutput, AdmissionDbError> {
    let identity = hex::encode(scope.identity);
    let source = hex::encode(msg.source_hash());
    let destination = hex::encode(msg.destination_hash());
    let id = hex::encode(msg.message_id());
    let wire_hash = sha256(msg.wire_bytes());
    let base = state.config.data_dir.join("identities").join(&identity);
    let archive_dir = base.join("propagation-admissions");
    let files_dir = base.join("files");
    let archive_path = archive_dir.join(format!("{}-{}.lxm", id, hex::encode(wire_hash)));
    let archive_string = archive_path
        .to_str()
        .ok_or_else(|| storage("invalid archive path"))?
        .to_string();
    let media = if matches!(projection, Projection::Chat { .. }) {
        media(msg)
    } else {
        Vec::new()
    };
    let mut bindings = projection.bindings();
    // Hashes and short constants are already included in the plan's fixed
    // allowance. Each variable media string is bound exactly once below.
    bindings.extend(
        media
            .iter()
            .flat_map(|item| [item.name.len(), item.stored.len()]),
    );
    let timestamp = db::next_conversation_observed_timestamp(
        &state.db,
        &source,
        &identity,
        crate::local_now_ts(),
    );
    let mut plan = PreparedAdmission::new(
        identity.clone(),
        scope.transient_id,
        scope.node,
        msg.message_id(),
        wire_hash,
        archive_string.clone(),
        now(),
        bindings,
    )?
    .with_files(
        &media
            .iter()
            .filter(|item| item.stored != db::ATTACHMENT_UNAVAILABLE_STORED_NAME)
            .map(Media::descriptor)
            .collect::<Vec<_>>(),
    )?;
    if let Projection::Game(game) = &projection {
        plan = plan.with_game(&game.effect_reference())?;
    }
    let store = DurableInboundStore::from_pool(&state.db)?;
    let database_parent = store
        .path()
        .parent()
        .ok_or_else(|| storage("missing database parent"))?;
    validate_durable_storage_directory(database_parent).map_err(storage)?;
    // The witness creates the complete identity/archive/media parent chain
    // through the platform's checked durable namespace operations.
    let witness = durable_write(
        &database_parent.join(".lxmf-admission-witness"),
        b"ratspeak-admission-v1\n",
    )
    .map_err(storage)?;
    durable_create_dir_all(&archive_dir, &witness).map_err(storage)?;
    durable_create_dir_all(&files_dir, &witness).map_err(storage)?;
    let descriptors = media
        .iter()
        .filter(|item| item.stored != db::ATTACHMENT_UNAVAILABLE_STORED_NAME)
        .map(Media::descriptor)
        .collect::<Vec<_>>();
    let intent = StagingIntent {
        version: 1,
        identity: identity.clone(),
        transient_id: scope.transient_id,
        message_id: msg.message_id(),
        received_wire_hash: wire_hash,
        archive: archive_string,
        media: descriptors,
    };
    let intent_bytes = db::admission::bounded_json(&intent, 16 * 1024)?;
    let intent_path = archive_dir.join(format!("{}.intent", hex::encode(scope.transient_id)));
    let intents = staging_intents(&archive_dir)?;
    if intents.len() >= MAX_STAGING_INTENTS && !intents.contains(&intent_path) {
        return Err(AdmissionDbError::LedgerFull);
    }
    durable_write(&intent_path, intent_bytes.as_bytes()).map_err(storage)?;
    durable_write(&archive_path, msg.wire_bytes()).map_err(storage)?;
    for item in media
        .iter()
        .filter(|item| item.stored != db::ATTACHMENT_UNAVAILABLE_STORED_NAME)
    {
        durable_write(&files_dir.join(&item.stored), item.bytes).map_err(storage)?;
    }
    let receipt = store.commit(guard, plan,
        |path, hash, files| verify_archive(&archive_dir, path, hash, files),
        |connection| {
            let blocked: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM blocked_contacts WHERE identity_id=?1 AND dest_hash=?2)", params![identity,source], |row| row.get(0))?;
            if blocked { return Ok(EffectCommit::PolicyDiscard); }
            match &mut projection {
                Projection::Service => Ok(EffectCommit::Applied("service")),
                Projection::Game(game) => game.apply(connection),
                Projection::Reaction { target, emoji, remove } => {
                    if *remove {
                        connection.execute("DELETE FROM reactions WHERE message_id=?1 AND sender=?2 AND emoji=?3 AND identity_id=?4", params![target.as_str(),source,emoji.as_str(),identity])?;
                    } else {
                        connection.execute("INSERT OR IGNORE INTO reactions(message_id,sender,emoji,timestamp,identity_id) VALUES(?1,?2,?3,?4,?5)",params![target.as_str(),source,emoji.as_str(),timestamp,identity])?;
                    }
                    Ok(EffectCommit::Applied("reaction"))
                }
                Projection::Chat { title, content, reply, preview, renderer, thread } => {
                    let attachment = media.iter().find(|item| !item.image && item.audio.is_none());
                    let image = media.iter().find(|item| item.image);
                    let audio = media.iter().find(|item| item.audio.is_some());
                    db::try_save_message_with_audio_conn(connection, &id, &source, &destination,
                        content, title, timestamp, "received", "inbound", &identity,
                        attachment.map_or("", |v| v.name.as_str()), attachment.map_or("", |v| v.stored.as_str()),
                        image.map_or("", |v| v.name.as_str()), image.map_or("", |v| v.stored.as_str()), reply, preview,
                        Some("propagated"), audio.and_then(|v| v.audio), audio.map_or("", |v| v.stored.as_str()), *renderer, thread).map_err(storage)?;
                    connection.execute("DELETE FROM hidden_conversations WHERE identity_id=?1 AND dest_hash=?2",params![identity,source])?;
                    Ok(EffectCommit::Applied("message"))
                }
            }
        })?;
    let receipt = DurableAdmissionReceipt::commit(Committed(receipt)).map_err(storage)?;
    if let Projection::Game(game) = &mut projection {
        game.committed(state)?;
    }
    // A failed cleanup leaves a resumable intent. It cannot undo an already
    // acknowledged database commit or cause a second visible insertion.
    if store.references_archive(
        &identity,
        archive_path
            .to_str()
            .ok_or(AdmissionDbError::MissingEffect)?,
    )? && let Err(error) = durable_remove(&intent_path, &witness)
    {
        tracing::warn!(%error, "committed propagation staging intent awaits cleanup");
    }
    let mut display_media = serde_json::Map::new();
    for item in media {
        let unavailable = item.stored == db::ATTACHMENT_UNAVAILABLE_STORED_NAME;
        if let Some(mode) = item.audio {
            display_media.insert("audio".into(), if unavailable { json!({"mode":mode,"supported":false,"unavailable":true}) } else {
                json!({"mode":mode,"supported":cfg!(feature="lxst-voice") && matches!(mode,lxmf_core::constants::AM_OPUS_OGG|lxmf_core::constants::AM_CODEC2_700C),"stored_name":item.stored})
            });
        } else {
            let mut value = serde_json::Map::new();
            value.insert("filename".into(), serde_json::Value::String(item.name));
            if unavailable {
                value.insert("unavailable".into(), json!(true));
            } else {
                value.insert("stored_name".into(), serde_json::Value::String(item.stored));
            }
            if item.image {
                display_media.insert("image".into(), value.into());
            } else {
                display_media.insert(
                    "attachments".into(),
                    serde_json::Value::Array(vec![value.into()]),
                );
            }
        }
    }
    Ok(CommitOutput {
        receipt,
        projection,
        display_media,
        timestamp,
    })
}

pub(crate) async fn admit_download(
    state: &Arc<AppState>,
    downloaded: DownloadedPropagationMessage,
    activity: ActivityRequestFence,
) {
    let generation = state.current_identity_session_generation();
    let shutdown = state
        .session_shutdown
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let job = jobs().clone().read_owned().await;
    if shutdown.is_triggered() {
        return;
    }
    let scope = downloaded.scope();
    let identity = hex::encode(scope.identity);
    let context = state.lxmf.lock().ok().and_then(|manager| {
        let manager = manager.as_ref()?;
        (manager.identity.hash == scope.identity).then(|| manager.propagation_decrypt_context())
    });
    let (raw, token) = downloaded.into_parts();
    let Some(context) = context else {
        let _ = token.resolve(DownloadAdmissionOutcome::Rejected(
            SuppressionReason::Cancelled,
        ));
        return;
    };
    let lease = match ProcessingLease::try_acquire(raw.len()) {
        Ok(lease) => lease,
        Err(reason) => {
            let _ = token.resolve(DownloadAdmissionOutcome::Rejected(reason));
            return;
        }
    };
    let worker_state = Arc::clone(state);
    let worker_shutdown = shutdown.clone();
    let worker_identity = identity.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _job = job;
        let state = worker_state;
        let shutdown = worker_shutdown;
        let identity = worker_identity;
        // Cancellation cannot free the raw image or processing reservation
        // while this actual worker still owns decrypted bytes and effects.
        let process = || -> Result<(LxMessage, CommitOutput, Option<lxmf_core::message::OwnedVerifiedLxMessage>), SuppressionReason> {
            if shutdown.is_triggered() || !token.is_current() || state.current_identity_session_generation() != generation { return Err(SuppressionReason::Cancelled); }
            let clear = context.decrypt(&raw).ok_or(SuppressionReason::DecryptionFailed)?;
            let msg = LxMessage::unpack_owned(Bytes::from(clear), MessageDecodeLimits::default()).map_err(|_| SuppressionReason::Malformed)?;
            let key = crate::with_receiver(&state, &identity, |manager| manager.inbound_source_public_key(&msg.source_hash())).flatten();
            let verified = key.as_ref().map(|key| msg.verify(key)).transpose().map_err(|_| SuppressionReason::InvalidSignature)?;
            if !crate::inbound_stamp_allowed(&state, &identity, &msg) { return Err(SuppressionReason::InvalidStamp); }
            let guard = try_reserve_admission().map_err(|error| reason(&error))?;
            let projection = Projection::prepare(&state, &msg, key.is_some())?;
            let output = commit_message(&state, guard, scope, &msg, projection).map_err(|error| {
                tracing::warn!(%error, "propagation download was not durably admitted"); reason(&error)
            })?;
            let authenticated_ticket = verified.as_ref().filter(|v|v.message().fields().get(lxmf_core::constants::FIELD_TICKET).is_some()).map(|v|v.to_owned());
            Ok((msg, output, authenticated_ticket))
        };
        let completed = match process() {
            Ok((msg,output,ticket)) => Some((output,token,msg.message_id(),msg.source_hash(),msg.destination_hash(),msg.wire_bytes().len(),lease,ticket)),
            Err(reason) => { let _ = token.resolve(DownloadAdmissionOutcome::Rejected(reason)); None }
        };
        drop(raw);
        completed
    }).await;
    let completed = match result {
        Ok(Some(completed)) => completed,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(%error,"propagation admission worker failed");
            return;
        }
    };
    let (output, token, message_id, source_hash, destination_hash, wire_len, _lease, ticket) =
        completed;
    // Taken once the message is stored: a bulk delete after it moves it, so
    // the chat refresh then publishes no lists and checks the row again.
    let revision = state.conversation_list_revision();
    // What the publication shows is read now, off the lifecycle lock.
    let reactions = match &output.projection {
        Projection::Reaction { target, .. } => {
            let target = target.clone();
            let identity = identity.clone();
            db::spawn_db(state.db.clone(), move |p| {
                db::get_reactions_for_message(&p, &target, &identity)
            })
            .await
            .unwrap_or_default()
        }
        _ => Vec::new(),
    };
    let game_publication = match &output.projection {
        Projection::Game(game) => game.read_publication(state, &identity).await,
        _ => None,
    };
    // The worker has exited and released quiescence ownership before waiting
    // for this lock. Shutdown can therefore never deadlock on UI publication.
    let _lifecycle = state.identity_switch_lock.lock_routine().await;
    if shutdown.is_triggered()
        || !token.is_current()
        || state.current_identity_session_generation() != generation
    {
        return;
    }
    let disposition = output.receipt.record().disposition;
    crate::with_receiver(state, &identity, |manager| {
        manager.remember_committed_download(&output.receipt);
        if matches!(
            disposition,
            DurableDisposition::Committed | DurableDisposition::Duplicate
        ) && let Some(ticket) = ticket.as_ref()
        {
            manager
                .router
                .learn_ticket_from_inbound(&ticket.as_verified());
        }
    });
    if token
        .resolve(DownloadAdmissionOutcome::Accepted(output.receipt))
        .is_err()
    {
        return;
    }
    if disposition != DurableDisposition::Committed {
        return;
    }
    let mut chat_refresh = None;
    match output.projection {
        Projection::Chat {
            title,
            content,
            reply,
            preview,
            renderer,
            thread,
        } => {
            let notification: String = if content.trim().is_empty() {
                &title
            } else {
                &content
            }
            .chars()
            .take(160)
            .collect();
            let has_attachment = !output.display_media.is_empty();
            let source = hex::encode(source_hash);
            let message_id = hex::encode(message_id);
            let mut event = json!({"id":message_id,"source":source,
                "destination":hex::encode(destination_hash),"timestamp":output.timestamp,
                "state":"received","direction":"inbound"});
            event["content"] = serde_json::Value::String(content);
            event["title"] = serde_json::Value::String(title);
            event["reply_to_id"] = serde_json::Value::String(reply);
            event["reply_to_preview"] = serde_json::Value::String(preview);
            event["renderer"] = json!(renderer);
            event["thread"] = json!(thread);
            if let Some(fields) = event.as_object_mut() {
                fields.extend(output.display_media);
            }
            // A bulk delete that committed since the admission revoked the
            // token checked above, so the row is still there.
            state.emit_to_all("lxmf_message", event);
            chat_refresh = Some(ChatRefresh {
                generation,
                shutdown: shutdown.clone(),
                revision,
                message_id,
                source,
                identity: identity.clone(),
                preview: notification,
                renderer,
                attachment: has_attachment,
                thread,
            });
            crate::record_activity_if_current(state, activity, || {
                Ok(crate::producer::lxmf_inbound_accepted(
                    crate::producer::LxmfInboundAccepted {
                        source: crate::producer::DestinationHash::new(source_hash),
                        method: crate::InboundLxmfSource::Propagated.activity_method(),
                        encoded_bytes: wire_len.min(u32::MAX as usize) as u32,
                    },
                ))
            });
        }
        Projection::Reaction { target, .. } => {
            state.emit_to_all(
                "reaction_update",
                json!({"message_id":target,"reactions":reactions}),
            );
        }
        Projection::Service => {}
        Projection::Game(game) => {
            if let Some(publication) = game_publication {
                game.emit(state, publication);
            }
        }
    }
    drop(ticket);
    drop(_lease);
    drop(_lifecycle);
    // Keep presentation work bounded to this admitted job rather than spawning
    // an unbounded contact/FTS refresh for every item in a large download.
    if let Some(refresh) = chat_refresh {
        refresh_chat(state.clone(), refresh).await;
    }
}

/// Shared strict schema adapter for ordinary and propagated game messages.
pub(crate) fn decode_game(
    msg: &lxmf_core::message_api::LxMessage,
    authenticated: bool,
) -> Result<Option<lrgp::protocol::Envelope>, lxmf_core::admission::SuppressionReason> {
    game::decode(msg, authenticated)
}

/// A committed download's chat refresh, for the sessions it was admitted in.
/// `revision` is the list revision taken once the message was stored.
struct ChatRefresh {
    generation: u64,
    shutdown: rns_runtime::lifecycle::ShutdownSignal,
    revision: u64,
    message_id: String,
    source: String,
    identity: String,
    preview: String,
    renderer: Option<u8>,
    attachment: bool,
    thread: String,
}

async fn refresh_chat(state: Arc<AppState>, refresh: ChatRefresh) {
    let ChatRefresh {
        generation,
        shutdown,
        revision,
        message_id,
        source,
        identity,
        preview,
        renderer,
        attachment,
        thread,
    } = refresh;
    let lists = crate::read_inbound_lists(&state, &source, &thread, &identity, &message_id).await;
    let _lifecycle = state.identity_switch_lock.lock_routine().await;
    if state.current_identity_session_generation() != generation || shutdown.is_triggered() {
        return;
    }
    if let Some(lists) = lists {
        // Only a bulk delete since the admission moves the revision; its own
        // lists replace these, and the row is checked again before an alert.
        let current = state.conversation_list_revision() == revision;
        let label = lists.alert_label();
        if current {
            state.emit_to_all("contacts_update", lists.contacts.into());
            state.emit_to_all("unread_total", json!({"count":lists.unread}));
        }
        let stored = if current {
            lists.stored
        } else {
            crate::inbound_message_stored(&state, &message_id, &identity).await
        };
        if stored {
            crate::notify_inbound_message_if_background(
                &state, &source, &label, &preview, renderer, attachment,
            );
        }
    }
    crate::messaging::broadcast_conversations(state.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxmf_core::constants::DeliveryMethod;
    use lxmf_core::message::LxMessageDraft;
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    fn fixture() -> (tempfile::TempDir, Arc<AppState>) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        crate::lxmf::initialize_storage_namespace(&root).unwrap();
        let pool = db::init_pool(&root).unwrap();
        db::init_schema(&pool).unwrap();
        let config = crate::config::DashboardConfig {
            data_root: root.clone(),
            data_dir: root.join(".ratspeak"),
            rns_config_dir: root.join(".ratspeak/reticulum"),
            rns_config_dir_overridden: false,
            max_log_entries: 20,
        };
        let state = AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        );
        (temp, Arc::new(state))
    }
    fn scope(tid: u8) -> AdmissionScope {
        AdmissionScope {
            identity: [1; 16],
            node: [2; 16],
            transient_id: [tid; 32],
        }
    }
    fn message(content: &str, attachment: bool) -> LxMessage {
        let signer = rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(&[7; 32]);
        let mut draft =
            LxMessageDraft::new([3; 16], [4; 16], "", content, DeliveryMethod::Direct).unwrap();
        if attachment {
            draft
                .set_file_attachment_field("note.txt", b"retained attachment")
                .unwrap();
        }
        draft.sign(&signer).unwrap().finalize(None).unwrap()
    }
    #[test]
    fn suppression_codec_is_bounded_and_rejects_unknown_layout() {
        let record = SuppressionRecord {
            scope: scope(3),
            reason: SuppressionReason::InvalidSignature,
            epochs: AdmissionEpochs {
                policy: [4; 32],
                keys: [5; 32],
                budget: [6; 32],
                storage_health: [7; 32],
            },
            attempts: 1,
            last_failure: 1000,
            next_retry: 2000,
            expires_at: 3000,
        };
        let encoded = encode_suppression(&record).unwrap();
        assert!(encoded.len() <= 512);
        let restored = decode_suppression(&encoded).unwrap();
        assert_eq!(restored.scope, record.scope);
        assert_eq!(restored.reason, record.reason);
        let mut corrupt = encoded;
        corrupt[0] = 0x90;
        assert!(decode_suppression(&corrupt).is_err());
        assert!(decode_suppression(&[0; 513]).is_err());
    }
    #[tokio::test]
    async fn committed_chat_retains_exact_archive_media_and_rejects_missing_media() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let msg = message("hello", true);
        let output = commit_message(
            &state,
            try_reserve_admission().unwrap(),
            scope(3),
            &msg,
            Projection::prepare(&state, &msg, false).unwrap(),
        )
        .unwrap();
        assert_eq!(
            output.receipt.record().disposition,
            DurableDisposition::Committed
        );
        let store = DurableInboundStore::from_pool(&state.db).unwrap();
        let rows = store.receipts(&hex::encode([1; 16]), now()).unwrap();
        assert_eq!(rows.len(), 1);
        let (_, archive, files) = &rows[0];
        assert_eq!(files.len(), 1);
        assert_eq!(
            std::fs::read(archive.as_ref().unwrap()).unwrap(),
            msg.wire_bytes()
        );
        let base = state
            .config
            .data_dir
            .join("identities")
            .join(hex::encode([1; 16]));
        verify_archive(
            &base.join("propagation-admissions"),
            archive.as_ref().unwrap(),
            sha256(msg.wire_bytes()),
            files,
        )
        .unwrap();
        std::fs::remove_file(base.join("files").join(&files[0].name)).unwrap();
        assert!(
            verify_archive(
                &base.join("propagation-admissions"),
                archive.as_ref().unwrap(),
                sha256(msg.wire_bytes()),
                files
            )
            .is_err()
        );
        assert_eq!(
            state
                .db
                .get()
                .unwrap()
                .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn duplicate_and_deleted_downloads_never_reapply_chat() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let msg = message("hello", false);
        let admit = |tid| {
            commit_message(
                &state,
                try_reserve_admission().unwrap(),
                scope(tid),
                &msg,
                Projection::prepare(&state, &msg, false).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            admit(3).receipt.record().disposition,
            DurableDisposition::Committed
        );
        assert_eq!(
            admit(4).receipt.record().disposition,
            DurableDisposition::Duplicate
        );
        let store = DurableInboundStore::from_pool(&state.db).unwrap();
        store
            .delete_messages(&hex::encode([1; 16]), None, now())
            .unwrap();
        assert_eq!(
            admit(5).receipt.record().disposition,
            DurableDisposition::UserDeleted
        );
        assert_eq!(
            state
                .db
                .get()
                .unwrap()
                .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn oversized_projection_has_no_staged_files_or_database_effect() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let identities = state.config.data_dir.join("identities");
        let namespace_before = std::fs::read_dir(&identities)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert!(namespace_before.is_empty());
        let msg = message(&"x".repeat(1024 * 1024), false);
        let error = commit_message(
            &state,
            try_reserve_admission().unwrap(),
            scope(3),
            &msg,
            Projection::prepare(&state, &msg, false).unwrap(),
        )
        .err()
        .unwrap();
        assert!(matches!(error, AdmissionDbError::TooLarge));
        let namespace_after = std::fs::read_dir(&identities)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(
            namespace_after, namespace_before,
            "oversized rejection must not create staged files under the initialized namespace"
        );
        assert_eq!(
            state
                .db
                .get()
                .unwrap()
                .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[tokio::test]
    async fn hosting_readiness_is_scoped_to_identity_generation_and_configured_desire() {
        let (_temp, state) = fixture();
        let generation = state.current_identity_session_generation();
        state
            .propagation_node_hosting_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(!state.effective_propagation_hosting());
        assert!(state.set_propagation_host_readiness(
            "profile".into(),
            generation,
            crate::state::PropagationHostReadiness::Ready,
            None
        ));
        assert!(state.effective_propagation_hosting());
        state
            .propagation_node_hosting_enabled
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!state.effective_propagation_hosting());
        assert!(!state.set_propagation_host_readiness(
            "old".into(),
            generation.wrapping_add(1),
            crate::state::PropagationHostReadiness::Ready,
            None
        ));
    }
    #[tokio::test]
    async fn orphan_cleanup_preserves_files_owned_by_another_pending_intent() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let _guard = try_reserve_admission().unwrap();
        let identity = hex::encode([1; 16]);
        let base = state.config.data_dir.join("identities").join(&identity);
        let root = base.join("propagation-admissions");
        let files = base.join("files");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&files).unwrap();
        let archive = root.join(format!(
            "{}-{}.lxm",
            hex::encode([3; 32]),
            hex::encode([4; 32])
        ));
        std::fs::write(&archive, b"uncommitted").unwrap();
        std::fs::write(files.join("shared.bin"), b"media").unwrap();
        for tid in [5u8, 6] {
            let intent = StagingIntent {
                version: 1,
                identity: identity.clone(),
                transient_id: [tid; 32],
                message_id: [3; 32],
                received_wire_hash: [4; 32],
                archive: archive.to_string_lossy().into_owned(),
                media: vec![FileReference {
                    name: "shared.bin".into(),
                    hash: sha256(b"media"),
                    length: 5,
                }],
            };
            let path = root.join(format!("{}.intent", hex::encode([tid; 32])));
            std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
            if tid == 5 {
                std::fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(
                        std::time::SystemTime::now()
                            - std::time::Duration::from_secs(ORPHAN_GRACE + 60),
                    ))
                    .unwrap();
            }
        }
        let store = DurableInboundStore::from_pool(&state.db).unwrap();
        reconcile_intents(&store, &root, &identity, 0).unwrap();
        assert!(archive.exists());
        assert!(files.join("shared.bin").exists());
        assert_eq!(staging_intents(&root).unwrap().len(), 1);
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deleted_message_cannot_regain_purge_authority_from_stale_hydration() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let mut manager =
            lxmf::LxmfManager::load_or_create(&state.config.data_root, None, None).unwrap();
        manager.set_runtime_propagation_node(Some([2; 16]));
        let identity = manager.identity.hash;
        *state.lxmf.lock().unwrap() = Some(manager);
        let msg = message("delete after database hydration", false);
        let scope = AdmissionScope {
            identity,
            node: [2; 16],
            transient_id: [3; 32],
        };
        commit_message(
            &state,
            try_reserve_admission().unwrap(),
            scope,
            &msg,
            Projection::prepare(&state, &msg, false).unwrap(),
        )
        .unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        let mut coordinator = DownloadCoordinator {
            hydration_pause: Some((ready_tx, resume_rx)),
            ..Default::default()
        };
        let worker_state = state.clone();
        let task = tokio::spawn(async move {
            coordinator.synchronize(&worker_state).await;
        });
        ready_rx.await.unwrap();
        let deletion = crate::propagation_recovery::begin_download_deletion(state.clone()).await;
        delete_messages(&state, &hex::encode(identity), None)
            .unwrap()
            .remove_files()
            .unwrap();
        drop(deletion);
        resume_tx.send(()).unwrap();
        task.await.unwrap();
        assert!(
            !state
                .lxmf
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .download_state_installed()
        );
        assert!(
            DurableInboundStore::from_pool(&state.db)
                .unwrap()
                .receipts(&hex::encode(identity), now())
                .unwrap()
                .is_empty()
        );
        assert!(
            !state
                .config
                .data_dir
                .join("identities")
                .join(hex::encode(identity))
                .join("propagation-admissions")
                .join(format!(
                    "{}-{}.lxm",
                    hex::encode(msg.message_id()),
                    hex::encode(sha256(msg.wire_bytes()))
                ))
                .exists()
        );
        DownloadCoordinator::default().synchronize(&state).await;
        assert!(
            state
                .lxmf
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .download_state_installed()
        );
    }
    #[tokio::test]
    async fn interrupted_user_deletion_recovers_files_without_a_selected_node() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let identity = hex::encode([1; 16]);
        let msg = message("durable deletion recovery", true);
        commit_message(
            &state,
            try_reserve_admission().unwrap(),
            scope(3),
            &msg,
            Projection::prepare(&state, &msg, false).unwrap(),
        )
        .unwrap();
        let (archive, media) = {
            let store = DurableInboundStore::from_pool(&state.db).unwrap();
            let rows = store.receipts(&identity, now()).unwrap();
            let (_, archive, files) = &rows[0];
            let media = state
                .config
                .data_dir
                .join("identities")
                .join(&identity)
                .join("files")
                .join(&files[0].name);
            let guard = try_reserve_admission().unwrap();
            store
                .delete_messages_guarded(&guard, &identity, None, now())
                .unwrap();
            assert_eq!(
                store
                    .pending_deleted_artifacts(&guard, None, 0, 256)
                    .unwrap()
                    .len(),
                2
            );
            (std::path::PathBuf::from(archive.as_ref().unwrap()), media)
        };
        // Simulate exit immediately after the logical transaction. Recovery
        // must run without an LXMF manager, selected node, or enabled sync.
        assert!(archive.exists());
        assert!(media.exists());
        DownloadCoordinator::default().synchronize(&state).await;
        assert!(!archive.exists());
        assert!(!media.exists());
        let guard = try_reserve_admission().unwrap();
        let reopened = DurableInboundStore::from_pool(&state.db).unwrap();
        assert!(
            reopened
                .pending_deleted_artifacts(&guard, None, 0, 256)
                .unwrap()
                .is_empty()
        );
        assert!(reopened.receipts(&identity, now()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_messages_stops_their_sends() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let mut manager =
            lxmf::LxmfManager::load_or_create(&state.config.data_root, None, None).unwrap();
        let identity = manager.identity_hash.clone();
        let send = |manager: &mut lxmf::LxmfManager, dest: &str, thread: &str| {
            manager
                .send_message_with_preference_report(lxmf::MessageSendRequest {
                    dest_hash_hex: dest,
                    content: "on its way",
                    title: "",
                    db_pool: &state.db,
                    identity_id: &identity,
                    preference: lxmf::DeliveryPreference::Direct,
                    profile: lxmf::DeliveryProfile::Message,
                    renderer: None,
                    thread,
                })
                .unwrap()
                .message_id
        };
        let (peer, other) = ("e1".repeat(16), "e2".repeat(16));
        let thread = "ab".repeat(16);
        send(&mut manager, &peer, "");
        send(&mut manager, &peer, &thread);
        let kept = send(&mut manager, &other, "");
        *state.lxmf.lock().unwrap() = Some(manager);
        let queued = || {
            state
                .lxmf
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .router
                .pending_outbound
                .len()
        };
        assert_eq!(queued(), 3);

        crate::propagation_recovery::delete_thread_messages(&state, &identity, &peer, &thread)
            .unwrap()
            .remove_files()
            .unwrap();
        assert_eq!(queued(), 2, "only that conversation's send stops");
        crate::propagation_recovery::delete_messages(&state, &identity, Some(&peer))
            .unwrap()
            .remove_files()
            .unwrap();
        assert_eq!(queued(), 1);
        let left = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .router
            .pending_outbound[0]
            .message_id();
        assert_eq!(hex::encode(left), kept);
        crate::propagation_recovery::delete_messages(&state, &identity, None)
            .unwrap()
            .remove_files()
            .unwrap();
        assert_eq!(queued(), 0, "Delete All Messages stops every send");
    }

    #[tokio::test]
    async fn a_committed_delete_invalidates_conversation_lists_even_when_a_file_stays() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let identity = hex::encode([1; 16]);
        let msg = message("a file that stays", true);
        commit_message(
            &state,
            try_reserve_admission().unwrap(),
            scope(3),
            &msg,
            Projection::prepare(&state, &msg, false).unwrap(),
        )
        .unwrap();
        let media = {
            let store = DurableInboundStore::from_pool(&state.db).unwrap();
            let rows = store.receipts(&identity, now()).unwrap();
            state
                .config
                .data_dir
                .join("identities")
                .join(&identity)
                .join("files")
                .join(&rows[0].2[0].name)
        };
        std::fs::remove_file(&media).unwrap();
        std::fs::create_dir(&media).unwrap();
        std::fs::write(media.join("held"), b"media").unwrap();
        let before = state.conversation_list_revision();
        let _ = delete_messages(&state, &identity, None);
        let messages: i64 = state
            .db
            .get()
            .unwrap()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 0);
        assert_ne!(
            state.conversation_list_revision(),
            before,
            "a list read before the delete stays publishable"
        );
    }

    // A delete whose commit is uncertain may have happened: lists read before
    // it are stale either way.
    #[tokio::test]
    async fn an_uncertain_delete_still_makes_lists_stale() {
        let _serial = SERIAL.lock().await;
        let (_temp, state) = fixture();
        let identity = hex::encode([1; 16]);
        db::try_save_message(
            &state.db, "m1", "src", "dst", "kept?", "", 1.0, "received", "inbound", &identity, "",
            "", "", "", "", "", None,
        )
        .unwrap();
        // A deferred reference fails the delete's commit, not its statements.
        state
            .db
            .get()
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE commit_guard(id TEXT, identity_id TEXT, \
                 FOREIGN KEY(id, identity_id) REFERENCES messages(id, identity_id) \
                 DEFERRABLE INITIALLY DEFERRED); \
                 INSERT INTO commit_guard VALUES('m1', '{identity}');"
            ))
            .unwrap();
        let before = state.conversation_list_revision();
        let error = match delete_messages(&state, &identity, None) {
            Err(error) => error,
            Ok(_) => panic!("the delete committed"),
        };
        assert!(
            matches!(error, AdmissionDbError::CommitUncertain(_)),
            "{error}"
        );
        assert_ne!(
            state.conversation_list_revision(),
            before,
            "a list read before an uncertain delete stays publishable"
        );
    }

    /// A downloaded message's sender, its unread row, and the identity it is for.
    fn unread_download() -> (
        Arc<AppState>,
        Arc<crate::inbound_pipeline_tests::RecordingEmitter>,
        String,
        String,
    ) {
        let (state, emitter) = crate::inbound_pipeline_tests::pipeline_state();
        let identity = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .identity_hash
            .clone();
        let source = "e4".repeat(16);
        db::try_save_message(
            &state.db,
            &"e5".repeat(32),
            &source,
            &identity,
            "downloaded",
            "",
            1.0,
            "received",
            "inbound",
            &identity,
            "",
            "",
            "",
            "",
            "",
            "",
            Some("propagated"),
        )
        .unwrap();
        (state, emitter, source, identity)
    }

    fn chat_refresh(state: &AppState, source: String, identity: String) -> ChatRefresh {
        ChatRefresh {
            generation: state.current_identity_session_generation(),
            shutdown: state.session_shutdown.read().unwrap().clone(),
            revision: state.conversation_list_revision(),
            message_id: "e5".repeat(32),
            source,
            identity,
            preview: "downloaded".into(),
            renderer: None,
            attachment: false,
            thread: String::new(),
        }
    }

    // A refresh admitted before a bulk delete publishes no lists: the delete
    // re-sends its own.
    #[tokio::test]
    async fn a_refresh_admitted_before_a_delete_publishes_no_lists() {
        let (state, emitter, source, identity) = unread_download();
        let refresh = chat_refresh(&state, source, identity.clone());
        state
            .db
            .get()
            .unwrap()
            .execute("DELETE FROM messages WHERE identity_id = ?1", [&identity])
            .unwrap();
        state.invalidate_conversation_lists();
        refresh_chat(state.clone(), refresh).await;
        assert_eq!(
            emitter.count("unread_total"),
            0,
            "lists published after the delete"
        );
        assert_eq!(emitter.count("contacts_update"), 0);
    }

    // While its lists are current, a refresh is published without the
    // database under the lifecycle lock.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_current_refresh_is_published_without_the_database() {
        let (state, emitter, source, identity) = unread_download();
        let lifecycle = state.identity_switch_lock.lock_routine_owned().await;
        let mut refresh = Box::pin(refresh_chat(
            state.clone(),
            chat_refresh(&state, source, identity),
        ));
        crate::inbound_pipeline_tests::drive_until_parked(&mut refresh).await;
        let held: Vec<_> = (0..2).map(|_| state.db.get().unwrap()).collect();
        drop(lifecycle);
        let published = crate::inbound_pipeline_tests::finish_within(
            &mut refresh,
            std::time::Duration::from_secs(2),
        )
        .await;
        drop(held);
        assert!(
            published,
            "the refresh waited on the database under the lock"
        );
        assert_eq!(emitter.count("unread_total"), 1);
    }

    // A refresh belongs to the runtime session its download was admitted
    // in, even when a restart keeps the identity session.
    #[tokio::test]
    async fn a_refresh_from_an_ended_runtime_session_is_not_published() {
        let (state, emitter, source, identity) = unread_download();
        let refresh = chat_refresh(&state, source, identity);
        state.session_shutdown.read().unwrap().trigger();
        *state.session_shutdown.write().unwrap() = rns_runtime::lifecycle::ShutdownSignal::new();
        refresh_chat(state.clone(), refresh).await;
        assert_eq!(
            emitter.count("contacts_update"),
            0,
            "a refresh from the ended run"
        );
        assert_eq!(emitter.count("unread_total"), 0);
    }
}
