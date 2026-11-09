//! User-requested offline relay migration and trusted recovery. The task owns
//! lifecycle locks through the actual blocking operation and subsequent restart,
//! even if the invoking UI disappears.
use crate::{AppState, state::PropagationHostReadiness};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn recover_host(
    state: Arc<AppState>,
    identity: String,
    supervisor_writers_stopped: bool,
    recovery: Option<(PathBuf, [u8; 32])>,
) -> Result<Value, String> {
    if !supervisor_writers_stopped {
        return Err(
            "Stop other relay applications and disable their automatic restart before continuing"
                .into(),
        );
    }
    let generation = state.current_identity_session_generation();
    tokio::spawn(async move {
        let _lifecycle=state.identity_switch_lock.lock_owned().await;
        let _interfaces=state.auto_interface_lock.clone().lock_owned().await;
        let path= {
            let manager=state.lxmf.lock().map_err(|_|"Messaging unavailable")?;
            let manager=manager.as_ref().ok_or("Unlock the selected identity first")?;
            if manager.identity_hash!=identity || generation!=state.current_identity_session_generation() {
                return Err("The selected identity changed; reopen its inbox settings".into());
            }
            if manager.is_hardware { return Err("Hosted inbox recovery is unavailable for hardware identities".into()); }
            manager.data_dir.join("identities").join(&identity).join("propagation")
        };
        crate::shutdown_rns_lxmf(&state).await.map_err(|_|"Could not stop the current messaging session safely")?;
        state.set_propagation_host_readiness(identity.clone(),generation,PropagationHostReadiness::Recovering,None);
        crate::propagation::emit_propagation_update(&state);
        let cost=state.propagation_node_stamp_cost.load(std::sync::atomic::Ordering::Relaxed);
        let result=tokio::task::spawn_blocking(move || {
            use lxmf_core::propagation_store_v2::{migrate_store_offline,import_trusted_recovery_manifest,RecoveryPolicy};
            let guard=begin_native_migration(&path,supervisor_writers_stopped)?;
            let report=migrate_store_offline(&guard)?;
            let recovered=if let Some((manifest,digest))=recovery {
                let config=lxmf_core::propagation_node::PropagationNodeConfig::default();
                Some(import_trusted_recovery_manifest(&guard,&manifest,digest,RecoveryPolicy {
                    maximum_store_bytes:config.max_storage, maximum_message_bytes:config.max_message_size,
                    minimum_stamp_cost:cost,
                })?)
            } else {None};
            Ok::<_,std::io::Error>(json!({
                "quarantined_files":report.quarantine.files,"quarantined_bytes":report.quarantine.bytes,
                "quarantine_reason":report.quarantine.reason,
                "imported":recovered.as_ref().map_or(0,|r|r.imported),
                "already_present":recovered.as_ref().map_or(0,|r|r.already_present)
            }))
        }).await.map_err(|_|"Relay recovery worker failed".to_string()).and_then(|r|r.map_err(|error|error.to_string()));
        // Reopen through the same gate on both success and failure. An incomplete
        // migration keeps hosting unavailable while direct messaging resumes.
        if let Ok(mut shutdown)=state.session_shutdown.write() {
            *shutdown=rns_runtime::lifecycle::ShutdownSignal::new();
        }
        state.set_startup_stage("checking");
        crate::init_rns_lxmf(state.clone(),state.config.data_root.clone()).await;
        crate::propagation::emit_propagation_update(&state);
        result
    }).await.map_err(|_|"Relay recovery task failed".to_string())?
}

/// Revoke any already selected purge batches before changing retained effects.
pub fn invalidate_downloads(state: &AppState) {
    if let Ok(mut manager) = state.lxmf.lock()
        && let Some(manager) = manager.as_mut()
    {
        manager.invalidate_download_state();
    }
}

/// Move this guard into the actual deletion worker. Both revocations occur
/// inside one lifecycle span, even when its awaiting command is cancelled.
pub struct DownloadDeletionGuard {
    state: Arc<AppState>,
    _lifecycle: crate::state::OwnedRoutineGuard,
}
impl Drop for DownloadDeletionGuard {
    fn drop(&mut self) {
        invalidate_downloads(&self.state);
    }
}
pub async fn begin_download_deletion(state: Arc<AppState>) -> DownloadDeletionGuard {
    let lifecycle = state.identity_switch_lock.lock_routine_owned().await;
    invalidate_downloads(&state);
    DownloadDeletionGuard {
        state,
        _lifecycle: lifecycle,
    }
}
pub use crate::inbound_admission::DeletedMessages;

/// Invoke inside the blocking worker while retaining its deletion lifecycle
/// guard. The rows are gone when this returns; remove their files after.
pub fn delete_messages(
    state: &AppState,
    identity: &str,
    peer: Option<&str>,
) -> Result<DeletedMessages, crate::db::admission::AdmissionDbError> {
    let sending = crate::db::outbound_on_its_way(&state.db, identity, peer, None);
    let deleted = crate::inbound_admission::delete_messages(state, identity, peer)?;
    stop_deleted_sends(state, identity, &sending);
    Ok(deleted)
}

/// `delete_messages` for one conversation with `peer`; `""` is Main.
pub fn delete_thread_messages(
    state: &AppState,
    identity: &str,
    peer: &str,
    thread: &str,
) -> Result<DeletedMessages, crate::db::admission::AdmissionDbError> {
    let sending = crate::db::outbound_on_its_way(&state.db, identity, Some(peer), Some(thread));
    let deleted = crate::inbound_admission::delete_thread_messages(state, identity, peer, thread)?;
    stop_deleted_sends(state, identity, &sending);
    Ok(deleted)
}

/// A deleted message is not sent on: its queued, resumed or retried send stops
/// with its row.
fn stop_deleted_sends(state: &AppState, identity: &str, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(manager) = lxmf.as_mut()
        && manager.identity_hash == identity
    {
        for id in ids {
            manager.cancel_outbound_message(id);
        }
    }
    for id in ids {
        state.release_attachment_delivery_lease(id);
    }
    if let Ok(mut times) = state.message_send_times.lock() {
        for id in ids {
            times.remove(id);
        }
    }
}

/// `delete_messages` for chosen `ids` with `peer`; also returns the ids deleted.
pub fn delete_selected_messages(
    state: &AppState,
    identity: &str,
    peer: &str,
    ids: &[String],
) -> Result<(DeletedMessages, Vec<String>), crate::db::admission::AdmissionDbError> {
    crate::inbound_admission::delete_selected_messages(state, identity, peer, ids)
}

pub async fn retry_deferred_downloads(state: Arc<AppState>) -> Result<(), String> {
    let generation = state.current_identity_session_generation();
    tokio::spawn(async move {
        let _lifecycle = state.identity_switch_lock.lock_routine_owned().await;
        if state.current_identity_session_generation() != generation {
            return Err("Selected identity changed".into());
        }
        let (identity, node) = {
            let mut manager = state.lxmf.lock().map_err(|_| "Messaging unavailable")?;
            let manager = manager.as_mut().ok_or("Unlock an identity first")?;
            let node = manager
                .configured_propagation_node
                .ok_or("Select an offline inbox first")?;
            let identity = manager.identity_hash.clone();
            manager.invalidate_download_state();
            (identity, node)
        };
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let store = crate::db::admission::DurableInboundStore::from_pool(&pool)?;
            let parent = store
                .path()
                .parent()
                .ok_or(crate::db::admission::AdmissionDbError::MissingEffect)?;
            rns_identity::persistence::validate_durable_storage_directory(parent)
                .map_err(|e| crate::db::admission::AdmissionDbError::Unavailable(e.to_string()))?;
            store.clear_suppression(&identity, node)
        })
        .await
        .map_err(|_| "Download recovery worker failed".to_string())?
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "Download recovery task failed".to_string())?
}

fn begin_native_migration(
    path: &std::path::Path,
    supervisor_writers_stopped: bool,
) -> std::io::Result<lxmf_core::propagation_store_v2::OfflineMigrationGuard> {
    if !supervisor_writers_stopped {
        return Err(std::io::Error::other("Relay writer exclusion is required"));
    }
    #[cfg(target_os = "ios")]
    {
        lxmf_core::propagation_store_v2::begin_ios_container_migration(path)
    }
    #[cfg(target_os = "android")]
    {
        let vm = rns_interface::android_usb::java_vm()
            .ok_or_else(|| std::io::Error::other("Android application VM is unavailable"))?;
        let env = vm.attach_current_thread().map_err(std::io::Error::other)?;
        let result = (|| {
            let thread = env
                .find_class("android/app/ActivityThread")
                .map_err(std::io::Error::other)?;
            let context = env
                .call_static_method(
                    thread,
                    "currentApplication",
                    "()Landroid/app/Application;",
                    &[],
                )
                .and_then(|value| value.l())
                .map_err(std::io::Error::other)?;
            lxmf_core::propagation_store_v2::begin_android_container_migration(path, &env, context)
        })();
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        result
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        lxmf_core::propagation_store_v2::begin_offline_migration(path, supervisor_writers_stopped)
    }
}
