//! Public contact-card export/import for QR sharing: Ratspeak RSCP cards and the
//! `lxma://` form Columba and other LXMF apps scan.

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

const CONTACT_CARD_PREFIX: &str = "RSCP2:";
const CONTACT_CARD_V1_PREFIX: &str = "RSCP1:";
const LEGACY_CONTACT_PREFIX: &str = "lxma://";
const CONTACT_CARD_FORMAT: &str = "ratspeak.contact.v2";
const LEGACY_CONTACT_FORMAT: &str = "lxma.contact";
const CONTACT_CARD_NAME_BYTES: usize = 40;
const CONTACT_CARD_TOKEN_BYTES: usize = 32;
const CONTACT_CARD_CAPS_MAX: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContactCard {
    pub display_name: String,
    pub lxmf_hash: String,
    pub identity_hash: String,
    pub public_key: [u8; 64],
    /// As the card states it; readers treat anything but `device` as a person.
    pub kind: String,
    pub caps: Vec<String>,
    /// Read from an `lxma://` code: no name, no caps, same key binding.
    pub legacy: bool,
}

impl ContactCard {
    pub fn is_device(&self) -> bool {
        self.kind == db::CONTACT_KIND_DEVICE
    }
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

/// A person's own card; nodes are carded by the web flasher.
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
        "{CONTACT_CARD_PREFIX}{kind}:{name_b64}:{lxmf}:{identity}:{key}:",
        kind = db::CONTACT_KIND_PERSON,
        lxmf = lxmf_hash.to_ascii_lowercase(),
        identity = identity_hash.to_ascii_lowercase(),
        key = key_b64,
    )
}

/// `lxma://<address>:<public key>`, both lowercase hex: the Columba contact
/// format that MeshChatX, Retichat, and rettui also scan. Carries no name.
pub(crate) fn build_legacy_contact_payload(lxmf_hash: &str, public_key: &[u8; 64]) -> String {
    format!(
        "{LEGACY_CONTACT_PREFIX}{}:{}",
        lxmf_hash.to_ascii_lowercase(),
        hex::encode(public_key)
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

fn card_token(value: &str) -> Option<String> {
    let token = value.trim().to_ascii_lowercase();
    (!token.is_empty()
        && token.len() <= CONTACT_CARD_TOKEN_BYTES
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')))
    .then_some(token)
}

pub(crate) fn parse_contact_card_payload(payload: &str) -> Result<ContactCard, String> {
    let trimmed = payload.trim();
    if let Some(raw) = strip_legacy_prefix(trimmed) {
        return parse_legacy_contact_payload(raw);
    }
    let (kind, caps, fields) = if let Some(raw) = trimmed.strip_prefix(CONTACT_CARD_PREFIX) {
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 6 {
            return Err("Ratspeak contact card is incomplete".into());
        }
        let kind = card_token(parts[0]).ok_or("Contact-card kind is invalid")?;
        let mut caps: Vec<String> = Vec::new();
        for cap in parts[5].split(',').filter(|cap| !cap.is_empty()) {
            let cap = card_token(cap).ok_or("Contact-card capabilities are invalid")?;
            if !caps.contains(&cap) {
                caps.push(cap);
            }
        }
        if caps.len() > CONTACT_CARD_CAPS_MAX {
            return Err("Contact card lists too many capabilities".into());
        }
        (kind, caps, [parts[1], parts[2], parts[3], parts[4]])
    } else if let Some(raw) = trimmed.strip_prefix(CONTACT_CARD_V1_PREFIX) {
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() != 4 {
            return Err("Ratspeak contact card is incomplete".into());
        }
        (
            db::CONTACT_KIND_PERSON.to_string(),
            Vec::new(),
            [parts[0], parts[1], parts[2], parts[3]],
        )
    } else {
        return Err("Not a Ratspeak contact card".into());
    };
    let [name_b64, lxmf_hash, identity_hash, key_b64] = fields;

    let name_bytes = B64URL
        .decode(name_b64)
        .map_err(|_| "Contact-card name is invalid".to_string())?;
    let display_name = String::from_utf8(name_bytes)
        .map_err(|_| "Contact-card name is not valid UTF-8".to_string())
        .map(|s| sanitize_text(&s, 64))?;

    let identity_bytes = parse_hex16(&identity_hash.to_ascii_lowercase(), "Identity hash")?;

    let key_bytes = B64URL
        .decode(key_b64)
        .map_err(|_| "Contact-card public key is invalid".to_string())?;
    if key_bytes.len() != 64 {
        return Err("Contact-card public key must be 64 bytes".into());
    }
    let mut public_key = [0u8; 64];
    public_key.copy_from_slice(&key_bytes);

    verified_card(
        display_name,
        lxmf_hash.to_ascii_lowercase(),
        Some(identity_bytes),
        public_key,
        kind,
        caps,
        false,
    )
}

fn strip_legacy_prefix(payload: &str) -> Option<&str> {
    let (head, rest) = payload.split_at_checked(LEGACY_CONTACT_PREFIX.len())?;
    head.eq_ignore_ascii_case(LEGACY_CONTACT_PREFIX)
        .then_some(rest)
}

// Columba's reader: exactly `<address>:<key>`, so a trailing field fails the key check.
fn parse_legacy_contact_payload(raw: &str) -> Result<ContactCard, String> {
    let Some((lxmf_hash, key_hex)) = raw.split_once(':') else {
        return Err("This legacy code has no public key. Add the contact by its address instead.".into());
    };
    if !validate_hex(key_hex, 128, 128) {
        return Err("Legacy code public key must be 64 bytes of hex".into());
    }
    let mut public_key = [0u8; 64];
    public_key.copy_from_slice(
        &hex::decode(key_hex).map_err(|_| "Legacy code public key is not valid hex".to_string())?,
    );
    verified_card(
        String::new(),
        lxmf_hash.to_ascii_lowercase(),
        None,
        public_key,
        db::CONTACT_KIND_PERSON.to_string(),
        Vec::new(),
        true,
    )
}

/// Binds a card to its key: the LXMF address, and the identity hash when the
/// card states one, must both derive from the public key.
fn verified_card(
    display_name: String,
    lxmf_hash: String,
    stated_identity: Option<[u8; 16]>,
    public_key: [u8; 64],
    kind: String,
    caps: Vec<String>,
    legacy: bool,
) -> Result<ContactCard, String> {
    let what = if legacy { "Legacy code" } else { "Contact-card" };
    let lxmf_bytes = parse_hex16(&lxmf_hash, "LXMF address")?;
    let identity = Identity::from_public_key(&public_key)
        .map_err(|_| format!("{what} public key is not a Reticulum identity"))?;
    if stated_identity.is_some_and(|stated| stated != identity.hash) {
        return Err("Contact-card identity hash does not match the public key".into());
    }
    let expected_lxmf =
        Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
    if expected_lxmf != lxmf_bytes {
        return Err(format!("{what} LXMF address does not match the public key"));
    }
    Ok(ContactCard {
        display_name,
        lxmf_hash,
        identity_hash: hex::encode(identity.hash),
        public_key,
        kind,
        caps,
        legacy,
    })
}

fn contact_card_json(card: &ContactCard, payload: Option<&str>) -> Value {
    json!({
        "format": if card.legacy { LEGACY_CONTACT_FORMAT } else { CONTACT_CARD_FORMAT },
        "legacy": card.legacy,
        "payload": payload.unwrap_or(""),
        "display_name": card.display_name,
        "lxmf_hash": card.lxmf_hash,
        "identity_hash": card.identity_hash,
        "public_key": hex::encode(card.public_key),
        "public_key_base64": B64.encode(card.public_key),
        "kind": card.kind,
        "caps": card.caps,
        "is_device": card.is_device(),
    })
}

fn contact_card_public_key(state: &AppState, hash_hex: &str) -> AppResult<[u8; 64]> {
    if let Some(public_key) = state.local_identity_public_key(hash_hex) {
        return Ok(public_key);
    }

    crate::lxmf::contact_card_public_key_from_profile(&state.config.data_dir, hash_hex)
        .ok_or_else(|| AppError::not_found("Identity public key not available"))
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
    let mut json = contact_card_json(&card, Some(&payload));
    json["legacy_payload"] = json!(build_legacy_contact_payload(&lxmf_hash, &public_key));
    Ok(json)
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
    if identity_id.is_empty() {
        return Err(AppError::conflict("no active identity"));
    }
    let dest_hash = card.lxmf_hash.to_ascii_lowercase();
    let display_name = card.display_name.clone();
    let identity_hash = card.identity_hash.clone();
    let public_key_hex = hex::encode(card.public_key);

    let dest_for_db = dest_hash.clone();
    let name_for_db = display_name.clone();
    let id_for_db = identity_id.clone();
    let key_for_db = public_key_hex.clone();
    let kind = card.kind.clone();
    let caps = card.caps.clone();
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
        db::set_contact_kind(&p, &dest_for_db, &id_for_db, &kind, &caps);
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

    let identity_changed = state
        .lxmf
        .lock()
        .ok()
        .and_then(|mut lxmf| {
            lxmf.as_mut().map(|mgr| {
                mgr.update_remote_crypto(&dest_hash, &card.public_key, None)
                    .0
            })
        })
        .unwrap_or(false);
    if identity_changed
        && let Err(error) = ratspeak_runtime::lxmf_persistence::persist_current_delta(
            &state,
            true,
            &[],
            false,
            "contact_import",
        )
        .await
    {
        tracing::warn!(%error, "contact-card identity persistence failed");
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

    fn card_fields(identity: &Identity) -> (String, String, String) {
        let lxmf = hex::encode(Destination::hash_from_name_and_identity(
            LXMF_APP_NAME,
            Some(&identity.hash),
        ));
        (
            lxmf,
            hex::encode(identity.hash),
            B64URL.encode(identity.get_public_key()),
        )
    }

    #[test]
    fn own_card_is_rscp2_person_with_empty_caps() {
        let identity = Identity::new();
        let (lxmf, id, _) = card_fields(&identity);
        let payload = build_contact_card_payload("Ann", &lxmf, &id, &identity.get_public_key());
        assert!(payload.starts_with("RSCP2:person:"));
        assert!(payload.ends_with(':'));
        let parsed = parse_contact_card_payload(&payload).unwrap();
        assert_eq!((parsed.kind.as_str(), parsed.caps.len()), ("person", 0));
        assert!(!parsed.is_device());
    }

    #[test]
    fn rscp1_cards_still_import_as_people() {
        let identity = Identity::new();
        let (lxmf, id, key) = card_fields(&identity);
        let payload = format!("RSCP1:{}:{lxmf}:{id}:{key}", B64URL.encode("Old"));
        let parsed = parse_contact_card_payload(&payload).unwrap();
        assert_eq!(parsed.display_name, "Old");
        assert_eq!(parsed.kind, "person");
        assert!(parsed.caps.is_empty());
        // Five fields were never an RSCP1 card.
        assert!(parse_contact_card_payload(&format!("{payload}:rsnode")).is_err());
    }

    #[test]
    fn rscp2_device_cards_carry_kind_and_caps() {
        let identity = Identity::new();
        let (lxmf, id, key) = card_fields(&identity);
        let name = B64URL.encode("rsNode 1A2B");
        let payload = format!("RSCP2:device:{name}:{lxmf}:{id}:{key}:rsnode,rsnode.update,future");
        let parsed = parse_contact_card_payload(&payload).unwrap();
        assert!(parsed.is_device());
        assert_eq!(parsed.caps, ["rsnode", "rsnode.update", "future"]);
        assert_eq!(parsed.display_name, "rsNode 1A2B");

        let unknown = format!("RSCP2:sensor:{name}:{lxmf}:{id}:{key}:");
        let parsed = parse_contact_card_payload(&unknown).unwrap();
        assert_eq!(parsed.kind, "sensor", "the raw kind is kept");
        assert!(!parsed.is_device(), "and read as a person");

        for bad in [
            format!("RSCP2:device:{name}:{lxmf}:{id}:{key}"),
            format!("RSCP2::{name}:{lxmf}:{id}:{key}:"),
            format!("RSCP2:device:{name}:{lxmf}:{id}:{key}:bad cap"),
        ] {
            assert!(parse_contact_card_payload(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rscp2_checks_hashes_against_the_key_like_rscp1() {
        let identity = Identity::new();
        let other = Identity::new();
        let (lxmf, id, key) = card_fields(&identity);
        let (other_lxmf, other_id, _) = card_fields(&other);
        let name = B64URL.encode("x");
        let err = parse_contact_card_payload(&format!(
            "RSCP2:device:{name}:{other_lxmf}:{id}:{key}:rsnode"
        ))
        .unwrap_err();
        assert!(err.contains("LXMF address"));
        let err = parse_contact_card_payload(&format!(
            "RSCP2:device:{name}:{lxmf}:{other_id}:{key}:rsnode"
        ))
        .unwrap_err();
        assert!(err.contains("identity hash"));
    }

    #[test]
    fn legacy_payload_is_columba_shaped_and_round_trips() {
        let identity = Identity::new();
        let (lxmf, id, key) = card_fields(&identity);
        let payload = build_legacy_contact_payload(&lxmf.to_ascii_uppercase(), &identity.get_public_key());
        assert_eq!(payload, format!("lxma://{lxmf}:{}", hex::encode(identity.get_public_key())));
        assert_eq!(payload.len(), 7 + 32 + 1 + 128);
        let parsed = parse_contact_card_payload(&payload).unwrap();
        assert!(parsed.legacy);
        assert_eq!(parsed.display_name, "");
        assert_eq!(parsed.lxmf_hash, lxmf);
        assert_eq!(parsed.identity_hash, id);
        assert_eq!(parsed.public_key, identity.get_public_key());
        assert_eq!((parsed.kind.as_str(), parsed.caps.len()), ("person", 0));
        assert!(!parsed.is_device());
        assert!(!parse_contact_card_payload(&format!("RSCP1:{}:{lxmf}:{id}:{key}", B64URL.encode("n")))
            .unwrap()
            .legacy);
    }

    #[test]
    fn legacy_payload_reads_any_case_with_surrounding_whitespace() {
        let identity = Identity::new();
        let (lxmf, id, _) = card_fields(&identity);
        let key = hex::encode(identity.get_public_key()).to_ascii_uppercase();
        let payload = format!("  LXMA://{}:{key}\n", lxmf.to_ascii_uppercase());
        let parsed = parse_contact_card_payload(&payload).unwrap();
        assert_eq!((parsed.lxmf_hash, parsed.identity_hash), (lxmf, id));
    }

    #[test]
    fn legacy_payload_is_bound_to_its_key() {
        let identity = Identity::new();
        let other = Identity::new();
        let (lxmf, _, _) = card_fields(&identity);
        let (other_lxmf, _, _) = card_fields(&other);
        let key = hex::encode(identity.get_public_key());
        let err = parse_contact_card_payload(&format!("lxma://{other_lxmf}:{key}")).unwrap_err();
        assert!(err.contains("LXMF address"), "{err}");
        let err = parse_contact_card_payload(&format!("lxma://{lxmf}")).unwrap_err();
        assert!(err.contains("no public key"), "{err}");
        for bad in [
            format!("lxma://{lxmf}:{}", &key[..126]),
            format!("lxma://{lxmf}:{key}:extra"),
            format!("lxma://{lxmf}:{}", key.replace('a', "g")),
            format!("lxma://{}:{key}", &lxmf[..30]),
            "lxma://".to_string(),
            "lxma:/".to_string(),
        ] {
            assert!(parse_contact_card_payload(&bad).is_err(), "{bad}");
        }
    }
}
