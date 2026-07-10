//! Public contact-card export/import for QR sharing.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::shared::format_contacts_list;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::state::AppState;

use ratspeak_core::LXMF_DELIVERY_APP_NAME as LXMF_APP_NAME;

const CONTACT_CARD_PREFIX: &str = "RSCP1:";
const CONTACT_CARD_FORMAT: &str = "ratspeak.contact.v1";
const CONTACT_CARD_NAME_BYTES: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactCard {
    pub display_name: String,
    pub lxmf_hash: String,
    pub identity_hash: String,
    pub public_key: [u8; 64],
}

fn trim_utf8_bytes(input: &str, max_bytes: usize) -> String {
    let trimmed = input.trim();
    if trimmed.len() <= max_bytes {
        return trimmed.to_string();
    }
    let mut end = 0usize;
    for (idx, ch) in trimmed.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    trimmed[..end].trim().to_string()
}

pub(crate) fn build_contact_card_payload(
    display_name: &str,
    lxmf_hash: &str,
    identity_hash: &str,
    public_key: &[u8; 64],
) -> String {
    let name = trim_utf8_bytes(display_name, CONTACT_CARD_NAME_BYTES);
    let name_b64 = B64URL.encode(name.as_bytes());
    let key_b64 = B64URL.encode(public_key);
    format!(
        "{CONTACT_CARD_PREFIX}{name_b64}:{lxmf}:{identity}:{key}",
        lxmf = lxmf_hash.to_ascii_lowercase(),
        identity = identity_hash.to_ascii_lowercase(),
        key = key_b64,
    )
}

fn parse_hex16(value: &str, label: &str) -> Result<[u8; 16], String> {
    if !validate_hex(value, 32, 32) {
        return Err(format!("{label} must be a 16-byte hex value"));
    }
    let bytes = hex::decode(value).map_err(|_| format!("{label} is not valid hex"))?;
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub(crate) fn parse_contact_card_payload(payload: &str) -> Result<ContactCard, String> {
    let trimmed = payload.trim();
    let raw = trimmed
        .strip_prefix(CONTACT_CARD_PREFIX)
        .ok_or_else(|| "Not a Ratspeak contact card".to_string())?;
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 4 {
        return Err("Ratspeak contact card is incomplete".into());
    }

    let name_bytes = B64URL
        .decode(parts[0])
        .map_err(|_| "Contact-card name is invalid".to_string())?;
    let display_name = String::from_utf8(name_bytes)
        .map_err(|_| "Contact-card name is not valid UTF-8".to_string())
        .map(|s| sanitize_text(&s, 64))?;

    let lxmf_hash = parts[1].to_ascii_lowercase();
    let identity_hash = parts[2].to_ascii_lowercase();
    let lxmf_bytes = parse_hex16(&lxmf_hash, "LXMF address")?;
    let identity_bytes = parse_hex16(&identity_hash, "Identity hash")?;

    let key_bytes = B64URL
        .decode(parts[3])
        .map_err(|_| "Contact-card public key is invalid".to_string())?;
    if key_bytes.len() != 64 {
        return Err("Contact-card public key must be 64 bytes".into());
    }
    let mut public_key = [0u8; 64];
    public_key.copy_from_slice(&key_bytes);

    let identity = Identity::from_public_key(&public_key)
        .map_err(|_| "Contact-card public key is not a Reticulum identity".to_string())?;
    if identity.hash != identity_bytes {
        return Err("Contact-card identity hash does not match the public key".into());
    }

    let expected_lxmf =
        Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    if expected_lxmf != lxmf_bytes {
        return Err("Contact-card LXMF address does not match the public key".into());
    }

    Ok(ContactCard {
        display_name,
        lxmf_hash,
        identity_hash,
        public_key,
    })
}

fn contact_card_json(card: &ContactCard, payload: Option<&str>) -> Value {
    json!({
        "format": CONTACT_CARD_FORMAT,
        "payload": payload.unwrap_or(""),
        "display_name": card.display_name,
        "lxmf_hash": card.lxmf_hash,
        "identity_hash": card.identity_hash,
        "public_key": hex::encode(card.public_key),
        "public_key_base64": B64.encode(card.public_key),
    })
}

fn contact_card_public_key(state: &AppState, hash_hex: &str) -> AppResult<[u8; 64]> {
    if let Ok(lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_ref()
        && let Some(public_key) = mgr.contact_card_public_key(hash_hex)
    {
        return Ok(public_key);
    }

    let key_bytes = super::identity::export_identity_key_bytes(state, hash_hex)?;
    let identity =
        Identity::from_private_key(&key_bytes).map_err(|_| AppError::bad_request("Invalid key"))?;
    Ok(identity.get_public_key())
}

#[tauri::command]
pub async fn api_contact_card(
    state: State<'_, Arc<AppState>>,
    hash_hex: Option<String>,
) -> AppResult<Value> {
    let requested = hash_hex
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| active_identity_id(&state));
    if !validate_hex(&requested, 16, 128) {
        return Err(AppError::bad_request("Invalid identity hash"));
    }

    let public_key = contact_card_public_key(&state, &requested)?;
    let identity = Identity::from_public_key(&public_key)
        .map_err(|_| AppError::bad_request("Invalid public key"))?;
    let identity_hash = hex::encode(identity.hash);
    if identity_hash != requested {
        return Err(AppError::conflict("Identity public key hash mismatch"));
    }
    let lxmf_hash = hex::encode(Destination::hash_from_name_and_identity(
        LXMF_APP_NAME,
        Some(&identity.hash),
    ));

    let row_hash = requested.clone();
    let row = db::spawn_db(state.db.clone(), move |p| db::get_identity(&p, &row_hash))
        .await
        .map_err(|_| AppError::internal("identity metadata db task panicked"))?
        .unwrap_or_default();
    let display_name = row
        .get("display_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| row.get("nickname").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    let payload =
        build_contact_card_payload(&display_name, &lxmf_hash, &identity_hash, &public_key);
    let card = parse_contact_card_payload(&payload).map_err(AppError::internal)?;
    Ok(contact_card_json(&card, Some(&payload)))
}

#[tauri::command]
pub async fn api_preview_contact_card(payload: String) -> AppResult<Value> {
    let card = parse_contact_card_payload(&payload).map_err(AppError::bad_request)?;
    Ok(contact_card_json(&card, Some(payload.trim())))
}

#[tauri::command]
pub async fn import_contact_card(
    state: State<'_, Arc<AppState>>,
    payload: String,
) -> AppResult<Value> {
    let card = parse_contact_card_payload(&payload).map_err(AppError::bad_request)?;
    let identity_id = active_identity_id(&state);
    let dest_hash = card.lxmf_hash.clone();
    let display_name = card.display_name.clone();
    let identity_hash = card.identity_hash.clone();
    let public_key_hex = hex::encode(card.public_key);

    let dest_for_db = dest_hash.clone();
    let name_for_db = display_name.clone();
    let id_for_db = identity_id.clone();
    let key_for_db = public_key_hex.clone();
    let contacts_list = db::spawn_db(state.db.clone(), move |p| {
        let conn = match p.get() {
            Ok(c) => c,
            Err(_) => return Vec::<Value>::new(),
        };
        db::save_contact_with_identity_pubkey(
            &p,
            &dest_for_db,
            if name_for_db.is_empty() {
                None
            } else {
                Some(name_for_db.as_str())
            },
            Some(&key_for_db),
            "trusted",
            &id_for_db,
        );
        db::touch_identity_activity_for_service(
            &p,
            &[(
                dest_for_db.clone(),
                now_ts(),
                Some(name_for_db.clone()),
                None,
            )],
            Some(&identity_hash),
            db::PEER_SERVICE_LXMF_DELIVERY,
        );
        let contacts = db::get_all_contacts_conn(&conn, &id_for_db);
        format_contacts_list(&contacts)
    })
    .await
    .map_err(|_| AppError::internal("contact-card import db task panicked"))?;

    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.update_remote_crypto(&dest_hash, &card.public_key, None);
        mgr.save_crypto_state();
    }

    state.emit_to_all("contacts_update", json!(contacts_list));
    state.emit_to_all(
        "contact_added",
        json!({
            "hash": dest_hash,
            "display_name": if display_name.is_empty() {
                dest_hash[..12.min(dest_hash.len())].to_string()
            } else {
                display_name.clone()
            },
        }),
    );
    super::contacts::emit_peer_delta_for(&state, &dest_hash).await;
    Ok(contact_card_json(&card, Some(payload.trim())))
}

fn now_ts() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_card_payload_round_trips_and_validates_identity() {
        let identity = Identity::new();
        let identity_hash = hex::encode(identity.hash);
        let lxmf_hash = hex::encode(Destination::hash_from_name_and_identity(
            LXMF_APP_NAME,
            Some(&identity.hash),
        ));
        let public_key = identity.get_public_key();

        let payload =
            build_contact_card_payload("Alice Example", &lxmf_hash, &identity_hash, &public_key);
        let parsed = parse_contact_card_payload(&payload).unwrap();

        assert_eq!(parsed.display_name, "Alice Example");
        assert_eq!(parsed.lxmf_hash, lxmf_hash);
        assert_eq!(parsed.identity_hash, identity_hash);
        assert_eq!(parsed.public_key, public_key);
    }

    #[test]
    fn contact_card_payload_rejects_lxmf_mismatch() {
        let identity = Identity::new();
        let other = Identity::new();
        let public_key = identity.get_public_key();
        let payload = build_contact_card_payload(
            "Alice",
            &hex::encode(Destination::hash_from_name_and_identity(
                LXMF_APP_NAME,
                Some(&other.hash),
            )),
            &hex::encode(identity.hash),
            &public_key,
        );

        let err = parse_contact_card_payload(&payload).unwrap_err();
        assert!(err.contains("LXMF address"));
    }

    #[test]
    fn contact_card_payload_trims_long_names_to_fit_qr_budget() {
        let identity = Identity::new();
        let identity_hash = hex::encode(identity.hash);
        let lxmf_hash = hex::encode(Destination::hash_from_name_and_identity(
            LXMF_APP_NAME,
            Some(&identity.hash),
        ));
        let public_key = identity.get_public_key();
        let payload = build_contact_card_payload(
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
            &lxmf_hash,
            &identity_hash,
            &public_key,
        );

        assert!(payload.len() <= 230);
    }
}
