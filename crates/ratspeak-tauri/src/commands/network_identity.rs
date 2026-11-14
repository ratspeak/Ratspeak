//! The network identity a private network shares: Reticulum signs this
//! device's discovery announces with it and decrypts the network's.

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::{
    STANDARD as B64, URL_SAFE as B64URL, URL_SAFE_NO_PAD as B64URL_NO_PAD,
};
use ratspeak_runtime::Zeroizing;
use rns_identity::identity::Identity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::State;

use crate::commands::network_settings::{restart, rns_handle};
use crate::commands::shared::{active_rns_config_dir, with_rns_config_lock};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// A Reticulum identity file: the X25519 then Ed25519 private keys.
const IDENTITY_FILE_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkIdentity {
    pub exists: bool,
    /// The identity hash, hex.
    pub hash: Option<String>,
}

fn status(config_dir: &Path) -> NetworkIdentity {
    let identity = crate::rns_config::load_network_identity(config_dir);
    NetworkIdentity {
        exists: identity.is_some(),
        hash: identity.map(|identity| identity.hexhash()),
    }
}

#[derive(Deserialize)]
pub struct ImportNetworkIdentityArgs {
    /// The identity file's bytes.
    #[serde(default, deserialize_with = "secret")]
    pub data_base64: Option<Zeroizing<String>>,
    /// The private key as `rnid -X` prints it: hex, base32, or URL-safe base64.
    #[serde(default, deserialize_with = "secret")]
    pub text: Option<Zeroizing<String>>,
}

fn secret<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Zeroizing<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(|value| value.map(Zeroizing::new))
}

/// The 64 key bytes, from the file or its text form as `rnid -M` reads them.
fn import_key(args: &ImportNetworkIdentityArgs) -> AppResult<Zeroizing<Vec<u8>>> {
    let invalid = || AppError::bad_request("That isn't a Reticulum identity file.");
    let key = match (
        args.data_base64.as_deref().map(String::as_str),
        args.text.as_deref().map(String::as_str),
    ) {
        (Some(data), None) => Zeroizing::new(B64.decode(data.trim()).map_err(|_| invalid())?),
        (None, Some(text)) => {
            let text = Zeroizing::new(text.split_whitespace().collect::<String>());
            let decoded = if text.len() == IDENTITY_FILE_BYTES * 2 {
                hex::decode(text.as_str()).ok()
            } else {
                None
            }
            .or_else(|| crate::commands::identity::base32_decode_text(&text).ok())
            .filter(|key| key.len() == IDENTITY_FILE_BYTES)
            .or_else(|| B64URL.decode(text.as_bytes()).ok())
            .filter(|key| key.len() == IDENTITY_FILE_BYTES)
            .or_else(|| B64URL_NO_PAD.decode(text.as_bytes()).ok());
            Zeroizing::new(decoded.ok_or_else(invalid)?)
        }
        _ => {
            return Err(AppError::bad_request(
                "Choose a network identity file or paste its key.",
            ));
        }
    };
    if key.len() != IDENTITY_FILE_BYTES {
        return Err(invalid());
    }
    Ok(key)
}

/// The identity file and config as they were, for putting a failed change back.
struct Snapshot {
    file: Option<Zeroizing<Vec<u8>>>,
    config: Option<Zeroizing<String>>,
}

impl Snapshot {
    fn take(config_dir: &Path) -> Self {
        Self {
            file: std::fs::read(crate::rns_config::network_identity_path(config_dir))
                .ok()
                .map(Zeroizing::new),
            config: crate::rns_config::read_config(config_dir),
        }
    }

    fn restore(&self, config_dir: &Path) {
        let path = crate::rns_config::network_identity_path(config_dir);
        let _ = match &self.file {
            Some(bytes) => rns_identity::persistence::atomic_write(&path, bytes),
            None => std::fs::remove_file(&path),
        };
        if let Some(config) = &self.config {
            crate::rns_config::write_config(config_dir, config);
        }
    }
}

/// Applies `apply` to the profile's identity file and config, then restarts
/// a running network, which reads the identity only at start.
async fn change(
    state: &Arc<AppState>,
    config_dir: &Path,
    apply: impl FnOnce() -> std::io::Result<bool>,
) -> AppResult<NetworkIdentity> {
    let snapshot = with_rns_config_lock(state, || {
        let snapshot = Snapshot::take(config_dir);
        match apply() {
            Ok(changed) => Ok((snapshot, changed)),
            Err(_) => {
                snapshot.restore(config_dir);
                Err(AppError::internal(
                    "The network identity could not be saved",
                ))
            }
        }
    });
    let (snapshot, changed) = snapshot?;
    if changed
        && rns_handle(state).is_some()
        && let Err(error) = restart(state).await
    {
        // The running network still has the old identity; leave nothing half applied.
        with_rns_config_lock(state, || snapshot.restore(config_dir));
        return Err(error);
    }
    Ok(status(config_dir))
}

async fn install(state: &Arc<AppState>, identity: Identity) -> AppResult<NetworkIdentity> {
    let config_dir = active_rns_config_dir(state);
    let path = crate::rns_config::network_identity_path(&config_dir);
    change(state, &config_dir, || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        identity.to_file(&path).map_err(std::io::Error::other)?;
        crate::rns_config::set_network_identity_key(&config_dir, true)?;
        Ok(true)
    })
    .await
}

#[tauri::command]
pub async fn api_network_identity(state: State<'_, Arc<AppState>>) -> AppResult<NetworkIdentity> {
    Ok(status(&active_rns_config_dir(&state)))
}

#[tauri::command]
pub async fn create_network_identity(
    state: State<'_, Arc<AppState>>,
) -> AppResult<NetworkIdentity> {
    ratspeak_runtime::network_ownership::require_local_interfaces(&state)
        .map_err(AppError::bad_request)?;
    if status(&active_rns_config_dir(&state)).exists {
        return Err(AppError::conflict(
            "This device already has a network identity.",
        ));
    }
    install(&state, Identity::new()).await
}

#[tauri::command]
pub async fn import_network_identity(
    state: State<'_, Arc<AppState>>,
    args: ImportNetworkIdentityArgs,
) -> AppResult<NetworkIdentity> {
    ratspeak_runtime::network_ownership::require_local_interfaces(&state)
        .map_err(AppError::bad_request)?;
    let identity = Identity::from_private_key(&import_key(&args)?)
        .map_err(|_| AppError::bad_request("That isn't a Reticulum identity file."))?;
    let current = status(&active_rns_config_dir(&state));
    if current.hash.as_deref() == Some(identity.hexhash().as_str()) {
        return Ok(current);
    }
    if current.exists {
        return Err(AppError::conflict(
            "Remove this device's network identity before importing another.",
        ));
    }
    install(&state, identity).await
}

/// The identity file for sharing with the network's other devices; `qr_text`
/// is its private key in the hex `rnid -X` prints and `import_network_identity` reads.
#[tauri::command]
pub async fn export_network_identity(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    let identity = crate::rns_config::load_network_identity(&active_rns_config_dir(&state))
        .ok_or_else(|| AppError::not_found("This device has no network identity."))?;
    let key = identity
        .get_private_key()
        .ok_or_else(|| AppError::internal("The network identity has no private key"))?;
    let hash = identity.hexhash();
    let qr_text = Zeroizing::new(hex::encode(key.as_slice()));
    Ok(json!({
        "file_name": format!("network_identity_{}", &hash[..8]),
        "data_base64": B64.encode(key.as_slice()),
        "qr_text": qr_text.as_str(),
        "hash": hash,
    }))
}

/// Refused while a published interface is encrypted for it: Reticulum would
/// stop announcing it, and dropping the encryption would publish it, and any
/// shared IFAC, in the clear.
#[tauri::command]
pub async fn remove_network_identity(
    state: State<'_, Arc<AppState>>,
) -> AppResult<NetworkIdentity> {
    ratspeak_runtime::network_ownership::require_local_interfaces(&state)
        .map_err(AppError::bad_request)?;
    let config_dir = active_rns_config_dir(&state);
    let private = crate::commands::interfaces::private_discovery_interfaces(
        &crate::rns_config::get_all_interfaces(&config_dir),
    );
    if !private.is_empty() {
        return Err(AppError::bad_request(format!(
            "Turn off Private network on {} first.",
            word_list(&private)
        ))
        .with_details(json!({ "interfaces": private })));
    }
    let path = crate::rns_config::network_identity_path(&config_dir);
    change(&state, &config_dir, || {
        let changed = crate::rns_config::set_network_identity_key(&config_dir, false)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(changed),
            Err(error) => Err(error),
        }
    })
    .await
}

fn word_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> ImportNetworkIdentityArgs {
        ImportNetworkIdentityArgs {
            data_base64: None,
            text: Some(Zeroizing::new(value.to_string())),
        }
    }

    fn file(bytes: &[u8]) -> ImportNetworkIdentityArgs {
        ImportNetworkIdentityArgs {
            data_base64: Some(Zeroizing::new(B64.encode(bytes))),
            text: None,
        }
    }

    #[test]
    fn import_reads_the_file_and_each_rnid_text_form() {
        let identity = Identity::new();
        let key = identity.get_private_key().unwrap();
        let hex_text = hex::encode(key.as_slice());
        let base32 = crate::commands::identity::base32_encode_padded(&*key);
        for args in [
            file(&*key),
            text(&hex_text),
            text(&hex_text.to_ascii_uppercase()),
            text(&format!("  {}\n{}  ", &hex_text[..64], &hex_text[64..])),
            text(&base32),
            text(&B64URL.encode(key.as_slice())),
            text(&B64URL_NO_PAD.encode(key.as_slice())),
        ] {
            let imported = Identity::from_private_key(&import_key(&args).unwrap()).unwrap();
            assert_eq!(imported.hash, identity.hash);
        }
    }

    #[test]
    fn import_refuses_anything_but_one_identity() {
        let key = Identity::new().get_private_key().unwrap();
        let refused = |args: ImportNetworkIdentityArgs| import_key(&args).unwrap_err();
        let mut garbage = vec![
            file(&key[..63]),
            file(&[&key[..], &[0u8][..]].concat()),
            file(b"[reticulum]\nnetwork_identity = x\n"),
            file(b""),
        ];
        garbage.push(ImportNetworkIdentityArgs {
            data_base64: Some(Zeroizing::new("not base64 at all!".to_string())),
            text: None,
        });
        for args in garbage {
            let error = refused(args);
            assert_eq!(error.code, "bad_request");
            assert_eq!(error.message, "That isn't a Reticulum identity file.");
        }
        for value in [&hex::encode(&key[..32]), "zz".repeat(64).as_str(), "hello"] {
            assert_eq!(refused(text(value)).code, "bad_request");
        }
        let neither = refused(ImportNetworkIdentityArgs {
            data_base64: None,
            text: None,
        });
        assert_eq!(
            neither.message,
            "Choose a network identity file or paste its key."
        );
        let both = refused(ImportNetworkIdentityArgs {
            text: Some(Zeroizing::new(hex::encode(key.as_slice()))),
            ..file(&*key)
        });
        assert_eq!(both.code, "bad_request");
    }

    #[test]
    fn a_snapshot_puts_the_file_and_config_back() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path();
        crate::rns_config::write_config(config_dir, "[reticulum]\nshare_instance = No\n");
        let snapshot = Snapshot::take(config_dir);
        let path = crate::rns_config::network_identity_path(config_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Identity::new().to_file(&path).unwrap();
        crate::rns_config::set_network_identity_key(config_dir, true).unwrap();
        assert!(status(config_dir).exists);
        snapshot.restore(config_dir);
        assert!(!path.exists());
        assert_eq!(
            crate::rns_config::read_config(config_dir).unwrap().as_str(),
            "[reticulum]\nshare_instance = No\n"
        );
        assert_eq!(
            status(config_dir),
            NetworkIdentity {
                exists: false,
                hash: None
            }
        );

        // A replaced identity comes back byte for byte.
        let kept = Identity::new();
        kept.to_file(&path).unwrap();
        crate::rns_config::set_network_identity_key(config_dir, true).unwrap();
        let snapshot = Snapshot::take(config_dir);
        Identity::new().to_file(&path).unwrap();
        snapshot.restore(config_dir);
        assert_eq!(status(config_dir).hash, Some(kept.hexhash()));
    }

    #[test]
    fn the_refusal_names_each_private_interface() {
        assert_eq!(word_list(&["A".into()]), "A");
        assert_eq!(word_list(&["A".into(), "B".into()]), "A and B");
        assert_eq!(
            word_list(&["A".into(), "B".into(), "C".into()]),
            "A, B, and C"
        );
    }
}
