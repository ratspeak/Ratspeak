//! Profile-scoped network ownership. Settings contain selectors, never RPC keys.

use std::path::Path;
use std::sync::Arc;

use rns_runtime::reticulum::{InstanceMode, ReticulumConfig, ReticulumHandle, SharedInstanceType};
use rns_runtime::shared_instance::{
    InstancePolicy, SharedInstanceCredentials, SharedInstanceEndpoint, SharedInstanceState,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{AppState, db, helpers};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "carrier", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalEndpoint {
    Tcp { packet_port: u16, control_port: u16 },
    Unix { instance_name: String },
}
impl LocalEndpoint {
    fn runtime(&self) -> Result<SharedInstanceEndpoint, String> {
        let endpoint = match self {
            Self::Tcp {
                packet_port,
                control_port,
            } => SharedInstanceEndpoint::Tcp {
                packet_port: *packet_port,
                control_port: *control_port,
            },
            Self::Unix { instance_name } => {
                if cfg!(target_os = "android") {
                    return Err("Use TCP sharing for another Android app; its Unix socket is not an accessible cross-app service.".into());
                }
                SharedInstanceEndpoint::Unix {
                    instance_name: instance_name.clone(),
                }
            }
        };
        endpoint.validate().map_err(|error| error.to_string())?;
        Ok(endpoint)
    }
    fn from_config(config: &ReticulumConfig) -> Self {
        match config.shared_instance_type {
            SharedInstanceType::Tcp => Self::Tcp {
                packet_port: config.shared_instance_port,
                control_port: config.control_port,
            },
            SharedInstanceType::Unix => Self::Unix {
                instance_name: config.instance_name.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Managed,
    Existing,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    mode: Ownership,
    #[serde(default)]
    share: bool,
    endpoint: Option<LocalEndpoint>,
    credential_ref: Option<String>,
}

/// The command input intentionally has no Debug implementation. Keys are
/// consumed into zeroizing storage before any asynchronous work.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRequest {
    pub mode: Ownership,
    #[serde(default)]
    pub share: bool,
    pub endpoint: Option<LocalEndpoint>,
    pub rpc_key: Option<String>,
}

#[derive(Default)]
pub(crate) struct NetworkSession {
    pub(crate) proposed: Option<(String, Selection, Option<SharedInstanceCredentials>)>,
    pub(crate) error: Option<String>,
    pub(crate) retained_identity: Option<(rns_identity::identity::Identity, bool)>,
}

// Normal failures are rolled back asynchronously below. An unwinding or
// internally cancelled transition must still drop retained key material and
// stop traffic; it must never leave a transient selection active in memory.
struct TransitionGuard {
    state: Arc<AppState>,
    armed: bool,
}
impl Drop for TransitionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut session = self
            .state
            .network_session
            .write()
            .unwrap_or_else(|e| e.into_inner());
        session.proposed = None;
        if let Some((identity, true)) = session.retained_identity.take() {
            identity.lock();
        }
        session.error = Some("Network change was interrupted. The last committed selection is retained; restart the network before retrying.".into());
        drop(session);
        self.state
            .session_shutdown
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .trigger();
        if let Ok(rns) = self.state.rns.read() {
            if let Some(manager) = rns.as_ref() {
                manager.handle.shutdown.trigger();
            }
        }
        self.state.set_startup_stage("error");
    }
}

fn profile_key(state: &AppState) -> Result<String, String> {
    let identity = helpers::active_identity_id(state);
    if identity.is_empty() {
        return Err("Unlock or select an identity before changing network ownership.".into());
    }
    Ok(format!("network_ownership.v1.{identity}"))
}

fn saved(state: &AppState, key: &str) -> Result<Option<Selection>, String> {
    let values = db::get_settings(&state.db, &[key])
        .map_err(|_| "Cannot read network ownership settings".to_string())?;
    values
        .get(key)
        .map(|value| {
            serde_json::from_str(value).map_err(|_| {
                "Network ownership settings are invalid; choose a mode again.".to_string()
            })
        })
        .transpose()
}

pub fn require_developer_mode(state: &AppState) -> Result<(), String> {
    if db::get_setting(&state.db, "developer_mode_enabled").as_deref() != Some("true") {
        return Err("Enable Developer Mode in Settings to change network ownership.".into());
    }
    Ok(())
}

pub(crate) fn fail_startup(state: &AppState, error: String) {
    state
        .network_session
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .error = Some(error.clone());
    state.set_startup_stage("error");
    state.emit_to_all(
        "network_ownership",
        json!({"status":"error", "error":error}),
    );
    state.emit_to_all("system_status", json!({"status":"error", "error":error}));
}

async fn read_credentials(
    state: &AppState,
    selection: &Selection,
) -> Result<SharedInstanceCredentials, String> {
    let endpoint = selection
        .endpoint
        .as_ref()
        .ok_or("Select the existing local service endpoints")?
        .runtime()?;
    let id = selection
        .credential_ref
        .clone()
        .ok_or("Enter the existing instance's RPC key")?;
    let store = state.network_credentials.clone();
    let key = tokio::task::spawn_blocking(move || store.read(&id))
        .await
        .map_err(|_| "Credential reader failed".to_string())??;
    SharedInstanceCredentials::new(endpoint, key.to_vec()).map_err(|e| e.to_string())
}

/// Startup does not change the selected profile or the external daemon's files.
pub(crate) async fn startup_policy(
    state: &AppState,
    config_dir: &Path,
) -> Result<InstancePolicy, String> {
    let key = profile_key(state)?;
    let proposed = state
        .network_session
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .proposed
        .clone();
    let (selection, credentials) =
        if let Some((profile, selection, credentials)) = proposed.filter(|p| p.0 == key) {
            let _ = profile;
            (Some(selection), credentials)
        } else {
            (saved(state, &key)?, None)
        };
    if let Some(selection) = selection {
        return match selection.mode {
            Ownership::Managed => Ok(if selection.share {
                InstancePolicy::SharedOwnerAt(SharedInstanceEndpoint::Tcp {
                    packet_port: ratspeak_core::config::RATSPEAK_RNS_SHARED_INSTANCE_PORT,
                    control_port: ratspeak_core::config::RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
                })
            } else {
                InstancePolicy::Standalone
            }),
            Ownership::Existing => Ok(InstancePolicy::SharedClient(match credentials {
                Some(credentials) => credentials,
                None => read_credentials(state, &selection).await?,
            })),
        };
    }
    // Preserve explicitly operated configs until the user chooses ownership.
    let content =
        crate::rns_config::read_config(config_dir).ok_or("Cannot read Reticulum configuration")?;
    if !state.config.uses_app_private_rns_config_dir()
        || crate::rns_config::has_operator_shared_selector(&content)
    {
        return Ok(InstancePolicy::Configured);
    }
    let config = rns_runtime::config::Config::parse(&content)
        .map_err(|_| "Invalid Reticulum configuration")?;
    let config =
        ReticulumConfig::try_from_config(&config).map_err(|_| "Invalid Reticulum configuration")?;
    Ok(if config.share_instance {
        InstancePolicy::SharedOwner
    } else {
        InstancePolicy::Standalone
    })
}

pub fn local_interfaces_allowed(state: &AppState) -> bool {
    if let Ok(key) = profile_key(state) {
        let session = state
            .network_session
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((profile, selection, _)) = &session.proposed {
            if profile == &key {
                return selection.mode == Ownership::Managed;
            }
        }
        drop(session);
        match saved(state, &key) {
            Ok(Some(selection)) if selection.mode == Ownership::Existing => return false,
            Err(_) => return false,
            _ => {}
        }
    }
    state
        .rns
        .read()
        .ok()
        .and_then(|r| {
            r.as_ref()
                .map(|r| r.handle.instance_mode != InstanceMode::Client)
        })
        .unwrap_or(true)
}

pub fn require_local_interfaces(state: &AppState) -> Result<(), String> {
    if local_interfaces_allowed(state) {
        Ok(())
    } else {
        Err("Interfaces are managed by the existing Reticulum instance. Configure them in that app, or switch to Managed by Ratspeak in Settings → Network.".into())
    }
}

pub fn snapshot(state: &AppState) -> Value {
    let selection = profile_key(state).and_then(|key| saved(state, &key));
    let error = state
        .network_session
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .error
        .clone();
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|r| r.handle.clone()));
    let (selection, settings_error) = match selection {
        Ok(s) => (s, None),
        Err(e) => (None, Some(e)),
    };
    let mode = selection
        .as_ref()
        .map(|s| s.mode.clone())
        .unwrap_or_else(|| {
            if handle
                .as_ref()
                .is_some_and(|h| h.instance_mode == InstanceMode::Client)
            {
                Ownership::Existing
            } else {
                Ownership::Managed
            }
        });
    let share = selection.as_ref().map(|s| s.share).unwrap_or_else(|| {
        handle
            .as_ref()
            .is_some_and(|h| h.instance_mode == InstanceMode::Shared)
    });
    let endpoint = selection
        .as_ref()
        .and_then(|s| s.endpoint.clone())
        .or_else(|| {
            handle
                .as_ref()
                .map(|h| LocalEndpoint::from_config(&h.config))
        });
    let status = handle
        .as_ref()
        .map(|h| match h.shared_instance_state() {
            Some(state) => serde_json::to_value(state).unwrap_or(json!("unknown")),
            None if h.instance_mode == InstanceMode::Client => json!("legacy_client"),
            None => json!("ready"),
        })
        .unwrap_or(json!("unavailable"));
    let warnings: Vec<Value> = handle.as_ref().map(|handle| {
        handle.startup_interface_failures().iter().map(|(name, error)| {
            let auto_port = handle.interface_configs.iter().find_map(|config| match config {
                rns_runtime::interface_factory::InterfaceConfig::Auto(auto) if &auto.name == name => Some(auto.data_port),
                _ => None,
            });
            json!({"interface":name,"message":auto_port.map(|port| auto_startup_error_message(error, port)).unwrap_or_else(|| "This configured interface could not start. Check its settings and application logs; other interfaces can still be used.".into())})
        }).collect()
    }).unwrap_or_default();
    json!({"mode": mode, "share": share, "endpoint": endpoint,
        "configured":selection.is_some(), "credential_saved":selection.as_ref().is_some_and(|s| s.credential_ref.is_some()),
        "status":status, "error":error.or(settings_error), "local_interfaces_allowed":local_interfaces_allowed(state),
        "unix_supported":cfg!(target_os="linux"), "startup_stage":state.get_startup_stage(), "warnings":warnings})
}

/// Interpret the existing interface factory's diagnostic without changing the
/// interoperable AutoInterface data port or claiming which process owns it.
pub fn auto_startup_error_message(error: &str, data_port: u16) -> String {
    if error.contains("socket bind")
        && (error.contains("Address already in use")
            || error.contains("os error 98")
            || error.contains("os error 48")
            || error.contains("os error 10048"))
    {
        format!(
            "Local Network could not start: UDP port {data_port} is already in use. Another Reticulum stack or Local Network interface may own it. Use that existing stack in Settings → Network (Developer Mode), or disable its AutoInterface before enabling Ratspeak's. Ordinary TCP connections can still be used. The LAN port has not been changed."
        )
    } else {
        error.to_string()
    }
}

fn decode_key(input: String) -> Result<Zeroizing<Vec<u8>>, String> {
    let input = Zeroizing::new(input);
    let value = input.trim();
    if value.is_empty() || value.len() > 2048 || !value.len().is_multiple_of(2) {
        return Err("RPC key must be 1–1024 bytes written as hexadecimal.".into());
    }
    hex::decode(value)
        .map(Zeroizing::new)
        .map_err(|_| "RPC key must contain only hexadecimal digits.".into())
}

async fn prepare(
    state: &AppState,
    request: NetworkRequest,
) -> Result<
    (
        Selection,
        Option<SharedInstanceCredentials>,
        Option<Zeroizing<Vec<u8>>>,
    ),
    String,
> {
    let supplied = request.rpc_key.map(decode_key).transpose()?;
    let mut selection = Selection {
        mode: request.mode,
        share: request.share,
        endpoint: request.endpoint,
        credential_ref: None,
    };
    if selection.mode == Ownership::Managed {
        if supplied.is_some() {
            return Err("A managed stack does not use another instance's key.".into());
        }
        selection.endpoint = None;
        return Ok((selection, None, None));
    }
    if selection.share {
        return Err("An existing-instance client cannot also host a shared instance.".into());
    }
    let endpoint = selection
        .endpoint
        .as_ref()
        .ok_or("Choose the existing instance endpoints")?
        .runtime()?;
    let credentials = if let Some(key) = &supplied {
        SharedInstanceCredentials::new(endpoint, key.to_vec()).map_err(|e| e.to_string())?
    } else {
        selection.credential_ref =
            saved(state, &profile_key(state)?)?.and_then(|s| s.credential_ref);
        read_credentials(state, &selection).await?
    };
    credentials
        .test()
        .await
        .map_err(|e| format!("{e}. The current network has not been changed."))?;
    Ok((selection, Some(credentials), supplied))
}

pub async fn test_connection(
    state: Arc<AppState>,
    request: NetworkRequest,
) -> Result<Value, String> {
    require_developer_mode(&state)?;
    if request.mode != Ownership::Existing {
        return Err("Connection testing is for an existing local instance.".into());
    }
    let generation = state.current_identity_session_generation();
    prepare(&state, request).await?;
    if state.current_identity_session_generation() != generation {
        return Err("The identity changed; test again.".into());
    }
    Ok(
        json!({"status":"ready", "message":"Packet service and authenticated RPC are available. Nothing has been changed."}),
    )
}

async fn ready(state: &AppState) -> Result<(), String> {
    if state.get_startup_stage() != "ready" {
        return Err(state
            .network_session
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .clone()
            .unwrap_or("The network did not finish starting.".into()));
    }
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|r| r.handle.clone()))
        .ok_or("The network runtime is unavailable.")?;
    if handle.instance_mode == InstanceMode::Client
        && handle.shared_instance_state() != Some(SharedInstanceState::Ready)
    {
        return Err("The shared instance is not authenticated and ready.".into());
    }
    handle
        .query_control_result(rns_transport::messages::TransportQuery::GetInterfaceStats)
        .await
        .map_err(|_| "The network control service is not responding.".to_string())?;
    Ok(())
}

async fn start_selected(state: &Arc<AppState>) -> Result<(), String> {
    *state
        .session_shutdown
        .write()
        .unwrap_or_else(|e| e.into_inner()) = rns_runtime::lifecycle::ShutdownSignal::new();
    state.set_startup_stage("checking");
    crate::init_rns_lxmf(state.clone(), state.config.data_root.clone()).await;
    ready(state).await
}

/// Runs inside a retained command task: dropping the WebView request cannot
/// abandon a half-applied mode transition. Committed settings change last.
pub async fn apply(state: Arc<AppState>, request: NetworkRequest) -> Result<Value, String> {
    let requested_profile = profile_key(&state)?;
    let generation = state.current_identity_session_generation();
    let _lifecycle = state.identity_switch_lock.lock().await;
    let _auto_ownership = state.auto_interface_lock.lock().await;
    if profile_key(&state)? != requested_profile
        || state.current_identity_session_generation() != generation
    {
        return Err("The identity changed while this request was waiting. Review its network settings and try again.".into());
    }
    require_developer_mode(&state)?;
    let previous = saved(&state, &requested_profile)?;
    let retained_identity = state
        .lxmf
        .lock()
        .ok()
        .and_then(|m| m.as_ref().map(|m| (m.identity.clone(), m.is_hardware)))
        .ok_or("Unlock the active identity before changing its network ownership.")?;
    let (mut selection, credentials, new_key) = prepare(&state, request).await?;
    let staged_id = if let Some(key) = new_key {
        let id = uuid::Uuid::new_v4().to_string();
        let save_id = id.clone();
        let store = state.network_credentials.clone();
        tokio::task::spawn_blocking(move || {
            store.write(&save_id, &key)?;
            // Do not tear down a working stack for a credential that cannot
            // be recovered on the next launch.
            match store.read(&save_id) {
                Ok(saved) if *saved == *key => Ok(()),
                _ => { let _ = store.delete(&save_id); Err("Protected storage could not verify the saved key. The current network has not changed.".to_string()) }
            }
        })
            .await
            .map_err(|_| "Credential writer failed")??;
        selection.credential_ref = Some(id.clone());
        Some(id)
    } else {
        None
    };

    let mut transition = TransitionGuard {
        state: state.clone(),
        armed: true,
    };
    if crate::shutdown_rns_lxmf_inner(&state, false).await.is_err() {
        transition.armed = false;
        let cleaned = match staged_id {
            Some(id) => delete_credential(&state, id).await,
            None => true,
        };
        return Err(format!(
            "Could not shut down the previous runtime safely. Its network selection has not changed.{}",
            if cleaned {
                ""
            } else {
                " An unused credential could not be removed from protected storage."
            }
        ));
    }
    state
        .network_session
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .retained_identity = Some(retained_identity);
    let result = async {
        state
            .network_session
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .proposed = Some((requested_profile.clone(), selection.clone(), credentials));
        start_selected(&state).await?;
        let value =
            serde_json::to_string(&selection).map_err(|_| "Cannot encode network settings")?;
        db::try_set_setting(&state.db, &requested_profile, &value)
            .map_err(|_| "Cannot commit network settings".to_string())?;
        Ok::<(), String>(())
    }
    .await;

    if let Err(error) = result {
        let stopped = crate::shutdown_rns_lxmf_inner(&state, false).await.is_ok();
        state
            .network_session
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .proposed = None;
        let restored = if stopped {
            start_selected(&state).await.is_ok()
        } else {
            false
        };
        let retained = state
            .network_session
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retained_identity
            .take();
        if !restored {
            if let Some((identity, true)) = retained {
                identity.lock();
            }
        }
        let cleaned = match staged_id {
            Some(id) => delete_credential(&state, id).await,
            None => true,
        };
        let mut message = format!(
            "{error} {}",
            if restored {
                "The previous network selection was restored."
            } else {
                "The previous selection is still saved, but its runtime could not be restored. The network is unavailable; retry or choose another mode."
            }
        );
        if !cleaned {
            message.push_str(" An unused credential could not be removed from protected storage.");
        }
        state
            .network_session
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .error = Some(message.clone());
        state.emit_to_all("network_ownership", snapshot(&state));
        transition.armed = false;
        return Err(message);
    }
    state
        .network_session
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .proposed = None;
    state
        .network_session
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .retained_identity = None;
    transition.armed = false;
    if let Some(old) = previous
        .and_then(|s| s.credential_ref)
        .filter(|old| Some(old) != selection.credential_ref.as_ref())
    {
        // A keychain deletion failure never invalidates the committed runtime.
        if !delete_credential(&state, old).await {
            state.network_session.write().unwrap_or_else(|e| e.into_inner()).error = Some("Network changed, but an unused credential could not be removed from protected storage.".into());
        }
    }
    let result = snapshot(&state);
    state.emit_to_all("network_ownership", result.clone());
    state.request_poll_now();
    transition.armed = false;
    Ok(result)
}

/// Deliberately sensitive, explicit export; never included in status or logs.
pub fn export_access(state: &AppState) -> Result<String, String> {
    require_developer_mode(state)?;
    let handle = state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|r| r.handle.clone()))
        .ok_or("Network is not running")?;
    if handle.instance_mode != InstanceMode::Shared {
        return Err("Enable sharing on a Ratspeak-managed stack first.".into());
    }
    let key = handle
        .config
        .rpc_key
        .as_ref()
        .ok_or("The shared service has no control key")?;
    serde_json::to_string_pretty(&json!({"version":1, "endpoint":LocalEndpoint::from_config(&handle.config), "rpc_key":hex::encode(key)})).map_err(|_| "Cannot export access configuration".into())
}

/// Only the documented access object is accepted. It cannot select remote
/// hosts, read files, import interfaces, or execute embedded configuration.
pub fn import_access(input: String) -> Result<Value, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Access {
        version: u32,
        endpoint: LocalEndpoint,
        rpc_key: String,
    }
    let input = Zeroizing::new(input);
    if input.len() > 8192 {
        return Err("Access configuration is too large.".into());
    }
    let access: Access = serde_json::from_str(&input).map_err(|_| "Invalid access configuration. Paste a shared-access JSON object, not a full Reticulum config.".to_string())?;
    if access.version != 1 {
        return Err("Unsupported access configuration version.".into());
    }
    access.endpoint.runtime()?;
    let key = decode_key(access.rpc_key)?;
    Ok(json!({"endpoint":access.endpoint, "rpc_key":hex::encode(&*key)}))
}

pub fn shared_connection_ready(handle: &ReticulumHandle) -> bool {
    handle.instance_mode != InstanceMode::Client
        || handle
            .shared_instance_state()
            .is_none_or(|state| state == SharedInstanceState::Ready)
}

async fn delete_credential(state: &AppState, id: String) -> bool {
    let store = state.network_credentials.clone();
    tokio::task::spawn_blocking(move || store.delete(&id))
        .await
        .is_ok_and(|result| result.is_ok())
}

/// Remove only a deleted, inactive profile's access selection and credential.
/// Retain the reference if the protected store is unavailable, for recovery.
pub async fn forget_deleted_profile(state: &AppState, identity: &str) -> Result<(), String> {
    if helpers::active_identity_id(state) == identity {
        return Err("Cannot remove the active profile's network credentials".into());
    }
    let key = format!("network_ownership.v1.{identity}");
    if let Some(id) = saved(state, &key)?.and_then(|s| s.credential_ref) {
        if !delete_credential(state, id).await {
            return Err("The identity was deleted, but its unused shared-instance credential could not be removed from protected storage.".into());
        }
    }
    state
        .db
        .get()
        .map_err(|_| "Cannot open settings database")?
        .execute("DELETE FROM settings WHERE key = ?1", [&key])
        .map_err(|_| "Cannot remove the deleted profile's network settings")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_secrets::CredentialStore;
    use std::collections::HashMap;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    #[derive(Default)]
    struct TestStore {
        keys: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
        fail_write: AtomicBool,
    }
    impl crate::network_secrets::CredentialStore for TestStore {
        fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, String> {
            self.keys
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or("Test key unavailable".into())
        }
        fn write(&self, id: &str, key: &[u8]) -> Result<(), String> {
            if self.fail_write.load(Ordering::Relaxed) {
                return Err("Protected storage unavailable".into());
            }
            self.keys
                .lock()
                .unwrap()
                .insert(id.into(), Zeroizing::new(key.to_vec()));
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<(), String> {
            self.keys.lock().unwrap().remove(id);
            Ok(())
        }
    }
    async fn fixture() -> (tempfile::TempDir, Arc<AppState>, Arc<TestStore>) {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::DashboardConfig::from_env_and_defaults(dir.path().into());
        let pool = db::init_pool(dir.path()).unwrap();
        db::init_schema(&pool).unwrap();
        let mgr = crate::lxmf::LxmfManager::load_or_create(dir.path(), None, None).unwrap();
        db::save_identity(&pool, &mgr.identity_hash, &mgr.lxmf_hash, "Default", "Test");
        db::set_active_identity(&pool, &mgr.identity_hash).unwrap();
        db::set_setting(&pool, "developer_mode_enabled", "true");
        db::set_setting(&pool, "auto_announce_interval", "0");
        let store = Arc::new(TestStore::default());
        let mut state = AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        );
        state.network_credentials = store.clone();
        let state = Arc::new(state);
        state.set_lxmf(mgr);
        crate::init_rns_lxmf(state.clone(), dir.path().into()).await;
        ready(&state).await.unwrap();
        (dir, state, store)
    }
    fn managed() -> NetworkRequest {
        NetworkRequest {
            mode: Ownership::Managed,
            share: false,
            endpoint: None,
            rpc_key: None,
        }
    }
    async fn owner() -> (tempfile::TempDir, ReticulumHandle, LocalEndpoint) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config"),
            "[reticulum]\nshare_instance = No\nrpc_key = 1234\n[interfaces]\n",
        )
        .unwrap();
        let a = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let b = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = LocalEndpoint::Tcp {
            packet_port: a.local_addr().unwrap().port(),
            control_port: b.local_addr().unwrap().port(),
        };
        drop((a, b));
        let handle = rns_runtime::reticulum::init_with_policy(
            dir.path().to_str(),
            None,
            rns_runtime::lifecycle::ShutdownSignal::new(),
            Arc::new(AtomicBool::new(true)),
            Default::default(),
            Default::default(),
            InstancePolicy::SharedOwnerAt(endpoint.runtime().unwrap()),
        )
        .await
        .unwrap();
        (dir, handle, endpoint)
    }
    fn existing(endpoint: LocalEndpoint) -> NetworkRequest {
        NetworkRequest {
            mode: Ownership::Existing,
            share: false,
            endpoint: Some(endpoint),
            rpc_key: Some("1234".into()),
        }
    }

    #[test]
    fn import_is_bounded_local_only_and_does_not_accept_embedded_config() {
        for value in ["{}", "{\"version\":2}", "[reticulum]\nrpc_key = 1234"] {
            assert!(import_access(value.into()).is_err());
        }
        assert!(import_access("x".repeat(8193)).is_err());
        assert!(import_access(r#"{"version":1,"endpoint":{"carrier":"tcp","host":"example.com","packet_port":1,"control_port":2},"rpc_key":"1234"}"#.into()).is_err());
        assert!(import_access(r#"{"version":1,"endpoint":{"carrier":"tcp","packet_port":1,"control_port":1},"rpc_key":"1234"}"#.into()).is_err());
        let result = import_access(r#"{"version":1,"endpoint":{"carrier":"tcp","packet_port":37428,"control_port":37429},"rpc_key":"1234"}"#.into()).unwrap();
        assert_eq!(result["rpc_key"], "1234");
    }

    #[tokio::test]
    async fn ownership_switch_roundtrip_keeps_identity_config_and_protects_secret() {
        let (_dir, state, store) = fixture().await;
        let (_owner_dir, owner, endpoint) = owner().await;
        let identity = helpers::active_identity_id(&state);
        let path = state.config.identity_rns_config_dir(&identity);
        let original = crate::rns_config::read_config(&path).unwrap();
        let selected = apply(state.clone(), existing(endpoint)).await.unwrap();
        assert_eq!(selected["mode"], "existing");
        assert!(!local_interfaces_allowed(&state));
        assert_eq!(store.keys.lock().unwrap().len(), 1);
        assert!(
            !db::get_setting(&state.db, &profile_key(&state).unwrap())
                .unwrap()
                .contains("rpc_key")
        );
        assert!(!snapshot(&state).to_string().contains("1234"));
        assert_eq!(crate::rns_config::read_config(&path).unwrap(), original);
        db::set_setting(&state.db, "developer_mode_enabled", "false");
        assert!(!local_interfaces_allowed(&state));
        assert!(matches!(
            startup_policy(&state, &path).await.unwrap(),
            InstancePolicy::SharedClient(_)
        ));
        db::set_setting(&state.db, "developer_mode_enabled", "true");
        apply(state.clone(), managed()).await.unwrap();
        assert!(local_interfaces_allowed(&state));
        assert!(store.keys.lock().unwrap().is_empty());
        assert_eq!(helpers::active_identity_id(&state), identity);
        crate::shutdown_rns_lxmf(&state).await.unwrap();
        owner.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn rejected_key_or_secret_storage_failure_leaves_current_runtime_untouched() {
        let (_dir, state, store) = fixture().await;
        let (_owner_dir, owner, endpoint) = owner().await;
        let before = state
            .rns
            .read()
            .unwrap()
            .as_ref()
            .unwrap()
            .handle
            .transport_tx
            .clone();
        let mut invalid = existing(endpoint.clone());
        invalid.rpc_key = Some("9999".into());
        assert!(
            apply(state.clone(), invalid)
                .await
                .unwrap_err()
                .contains("rejected")
        );
        store.fail_write.store(true, Ordering::Relaxed);
        assert!(
            apply(state.clone(), existing(endpoint))
                .await
                .unwrap_err()
                .contains("storage")
        );
        assert!(
            before.same_channel(
                &state
                    .rns
                    .read()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .handle
                    .transport_tx
            )
        );
        assert!(
            saved(&state, &profile_key(&state).unwrap())
                .unwrap()
                .is_none()
        );
        assert!(store.keys.lock().unwrap().is_empty());
        crate::shutdown_rns_lxmf(&state).await.unwrap();
        owner.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn commit_failure_restores_previous_runtime_and_removes_staged_key() {
        let (_dir, state, store) = fixture().await;
        let (_owner_dir, owner, endpoint) = owner().await;
        state.db.get().unwrap().execute_batch("CREATE TRIGGER reject_network_selection BEFORE INSERT ON settings WHEN NEW.key LIKE 'network_ownership.%' BEGIN SELECT RAISE(FAIL, 'test commit failure'); END;").unwrap();
        let result = apply(state.clone(), existing(endpoint)).await.unwrap_err();
        assert!(
            result.contains("previous network selection was restored"),
            "{result}"
        );
        assert!(store.keys.lock().unwrap().is_empty());
        assert_eq!(
            state
                .rns
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .handle
                .instance_mode,
            InstanceMode::Standalone
        );
        assert!(
            state
                .network_session
                .read()
                .unwrap()
                .retained_identity
                .is_none()
        );
        crate::shutdown_rns_lxmf(&state).await.unwrap();
        owner.shutdown_and_wait().await;
    }

    #[tokio::test]
    async fn queued_switch_cannot_apply_to_a_new_identity_generation() {
        let (_dir, state, _) = fixture().await;
        let guard = state.identity_switch_lock.lock().await;
        let requested = state.clone();
        let task = tokio::spawn(async move { apply(requested, managed()).await });
        tokio::task::yield_now().await;
        state.bump_identity_session_generation();
        drop(guard);
        assert!(
            task.await
                .unwrap()
                .unwrap_err()
                .contains("identity changed")
        );
        assert!(
            saved(&state, &profile_key(&state).unwrap())
                .unwrap()
                .is_none()
        );
        crate::shutdown_rns_lxmf(&state).await.unwrap();
    }

    #[tokio::test]
    async fn interrupted_transition_clears_transient_authority_and_stops_traffic() {
        let (_dir, state, _) = fixture().await;
        let identity = state
            .lxmf
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .identity
            .clone();
        state.network_session.write().unwrap().retained_identity = Some((identity, false));
        state.network_session.write().unwrap().proposed = Some((
            profile_key(&state).unwrap(),
            Selection {
                mode: Ownership::Managed,
                share: false,
                endpoint: None,
                credential_ref: None,
            },
            None,
        ));
        drop(TransitionGuard {
            state: state.clone(),
            armed: true,
        });
        assert!(
            state
                .network_session
                .read()
                .unwrap()
                .retained_identity
                .is_none()
        );
        assert!(state.network_session.read().unwrap().proposed.is_none());
        assert_eq!(state.get_startup_stage(), "error");
        assert!(
            state
                .rns
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .handle
                .shutdown
                .is_triggered()
        );
        assert!(
            saved(&state, &profile_key(&state).unwrap())
                .unwrap()
                .is_none()
        );
        crate::shutdown_rns_lxmf(&state).await.unwrap();
    }

    #[tokio::test]
    async fn deleted_profile_cleanup_cannot_delete_active_credentials() {
        let (_dir, state, store) = fixture().await;
        let active = helpers::active_identity_id(&state);
        assert!(forget_deleted_profile(&state, &active).await.is_err());
        store.write("deleted-key", &[1, 2]).unwrap();
        db::set_setting(
            &state.db,
            "network_ownership.v1.deleted-profile",
            &serde_json::to_string(&Selection {
                mode: Ownership::Existing,
                share: false,
                endpoint: Some(LocalEndpoint::Tcp {
                    packet_port: 1,
                    control_port: 2,
                }),
                credential_ref: Some("deleted-key".into()),
            })
            .unwrap(),
        );
        forget_deleted_profile(&state, "deleted-profile")
            .await
            .unwrap();
        assert!(store.keys.lock().unwrap().is_empty());
        assert!(
            saved(&state, "network_ownership.v1.deleted-profile")
                .unwrap()
                .is_none()
        );
        crate::shutdown_rns_lxmf(&state).await.unwrap();
    }
}
