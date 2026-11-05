//! LXMF paper messages: signed, recipient-encrypted `lxm://` URIs that travel
//! outside the network (QR code, link, print) and are filed by the recipient.
//!
//! Python reference: `LXMessage.as_uri` and `LXMRouter.ingest_lxm_uri`.

use std::sync::Arc;

use lxmf_core::constants::{DESTINATION_LENGTH, PAPER_MDU};
use lxmf_core::message_api::{DeliveryMethod, LxMessage, LxMessageDraft, MessageError};
use serde::Serialize;
use serde_json::Value;

use crate::lxmf::LxmfManager;
use crate::state::AppState;
use crate::{InboundLxmfSource, InboundOutcome, db};

/// Raw input bound; a full QR is 2,953 characters.
pub const PAPER_URI_INPUT_LIMIT: usize = 16 * 1024;
const URI_PREFIX: &str = "lxm://";
/// Identity encryption adds an ephemeral key, IV and HMAC around PKCS#7 blocks.
const TOKEN_FIXED_OVERHEAD: usize = 32 + 16 + 32;
const AES_BLOCK: usize = 16;
/// Largest plaintext (the message after its destination hash) that still fits
/// `PAPER_MDU` once encrypted.
const PAPER_MAX_PLAINTEXT: usize =
    (PAPER_MDU - DESTINATION_LENGTH - TOKEN_FIXED_OVERHEAD) / AES_BLOCK * AES_BLOCK - 1;
/// Bound before drafting; far above anything that fits on paper.
const PAPER_CONTENT_LIMIT: usize = 64 * 1024;
/// Source hash, signature and msgpack framing around title and content.
const PAPER_ENVELOPE_ESTIMATE: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperError {
    Empty,
    InvalidDestination,
    TooLong { overage_chars: usize },
    UnknownIdentity,
    Invalid,
    NotForYou,
    DecryptFailed,
    NotFound,
    SignatureInvalid,
    Blocked,
    Unavailable,
    Storage,
}

impl PaperError {
    /// Stable snake_case code the frontend matches on.
    pub fn code(self) -> &'static str {
        match self {
            Self::Empty => "paper_empty",
            Self::InvalidDestination => "invalid_destination",
            Self::TooLong { .. } => "paper_too_long",
            Self::UnknownIdentity => "paper_unknown_identity",
            Self::Invalid => "paper_invalid",
            Self::NotForYou => "paper_not_for_you",
            Self::DecryptFailed => "paper_decrypt_failed",
            Self::NotFound => "paper_not_found",
            Self::SignatureInvalid => "paper_signature_invalid",
            Self::Blocked => "paper_blocked",
            Self::Unavailable => "lxmf_not_initialized",
            Self::Storage => "database_unavailable",
        }
    }

    pub fn message(self) -> String {
        match self {
            Self::Empty => "Paper messages need text.".into(),
            Self::InvalidDestination => "Invalid identity hash".into(),
            Self::TooLong { overage_chars } => {
                format!("Too long for a paper message. Remove about {overage_chars} characters.")
            }
            Self::UnknownIdentity => {
                "The recipient's identity isn't known yet, so a paper message can't be created."
                    .into()
            }
            Self::Invalid => "That isn't a valid paper message.".into(),
            Self::NotForYou => "This paper message isn't addressed to you.".into(),
            Self::DecryptFailed => {
                "Couldn't decrypt this paper message. It may be incomplete or damaged.".into()
            }
            Self::NotFound => "No paper link is stored for this message.".into(),
            Self::SignatureInvalid => "This paper message's signature doesn't match.".into(),
            Self::Blocked => "You blocked this sender.".into(),
            Self::Unavailable => "LXMF not initialized".into(),
            Self::Storage => "Database temporarily unavailable".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaperSignature {
    Verified,
    /// The sender's identity is not known, so the signature cannot be checked.
    Unknown,
    Invalid,
}

/// Canonical `lxm://` form: case-insensitive scheme, surrounding whitespace,
/// `/`, line breaks and padding removed, as upstream ingestion tolerates.
pub fn normalize_paper_uri(input: &str) -> Option<String> {
    if input.len() > PAPER_URI_INPUT_LIMIT {
        return None;
    }
    let trimmed = input.trim();
    let scheme = trimmed.get(..URI_PREFIX.len())?;
    if !scheme.eq_ignore_ascii_case(URI_PREFIX) {
        return None;
    }
    let data: String = trimmed[URI_PREFIX.len()..]
        .chars()
        .filter(|c| *c != '/' && !c.is_ascii_whitespace())
        .collect();
    let data = data.trim_end_matches('=');
    (!data.is_empty()).then(|| format!("{URI_PREFIX}{data}"))
}

/// Decode a paper URI addressed to `local_destination` with `decrypt`.
fn decode_paper_uri_with(
    uri: &str,
    local_destination: [u8; 16],
    decrypt: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) -> Result<LxMessage, PaperError> {
    let uri = normalize_paper_uri(uri).ok_or(PaperError::Invalid)?;
    let (destination, _) = LxMessage::decode_paper_uri(&uri).map_err(|_| PaperError::Invalid)?;
    if destination != local_destination {
        return Err(PaperError::NotForYou);
    }
    let mut decrypted = true;
    LxMessage::from_paper_uri(&uri, |ciphertext| {
        decrypt(ciphertext).ok_or_else(|| {
            decrypted = false;
            MessageError::InvalidUri("undecryptable paper message".into())
        })
    })
    .map_err(|_| {
        if decrypted {
            PaperError::Invalid
        } else {
            PaperError::DecryptFailed
        }
    })
}

pub struct DecodedPaperMessage {
    pub message: LxMessage,
    pub signature: PaperSignature,
}

/// Decode and authenticate a paper URI for `manager`'s delivery destination.
pub fn decode_paper_message(
    manager: &LxmfManager,
    uri: &str,
) -> Result<DecodedPaperMessage, PaperError> {
    let message = decode_paper_uri_with(uri, manager.lxmf_dest_hash, |ciphertext| {
        manager.decrypt_inbound(ciphertext)
    })?;
    let signature = match manager.inbound_source_public_key(&message.source_hash()) {
        None => PaperSignature::Unknown,
        Some(key) if message.verify(&key).is_ok() => PaperSignature::Verified,
        Some(_) => PaperSignature::Invalid,
    };
    Ok(DecodedPaperMessage { message, signature })
}

/// Approximate characters to drop from the end of `content` to shed `bytes`.
fn overage_chars(content: &str, bytes: usize) -> usize {
    let mut removed = 0usize;
    let mut chars = 0usize;
    for c in content.chars().rev() {
        if removed >= bytes {
            break;
        }
        removed += c.len_utf8();
        chars += 1;
    }
    (chars + bytes.saturating_sub(removed)).max(1)
}

pub struct BuiltPaperMessage {
    pub message_id: String,
    pub paper_uri: String,
    pub timestamp: f64,
}

/// Sign and encrypt a paper message for `dest_hash_hex`. No ticket or stamp:
/// receivers skip stamp enforcement for paper, as upstream senders do.
pub fn build_paper_message(
    manager: &LxmfManager,
    dest_hash_hex: &str,
    content: &str,
    title: &str,
) -> Result<BuiltPaperMessage, PaperError> {
    if content.trim().is_empty() {
        return Err(PaperError::Empty);
    }
    let destination: [u8; 16] = hex::decode(dest_hash_hex)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(PaperError::InvalidDestination)?;
    let too_long = |plaintext: usize| PaperError::TooLong {
        overage_chars: overage_chars(content, plaintext.saturating_sub(PAPER_MAX_PLAINTEXT)),
    };
    let estimate = content.len() + title.len() + PAPER_ENVELOPE_ESTIMATE;
    if content.len() + title.len() > PAPER_CONTENT_LIMIT {
        return Err(too_long(estimate));
    }
    let draft = LxMessageDraft::new(
        destination,
        manager.lxmf_dest_hash,
        title,
        content,
        DeliveryMethod::Paper,
    )
    .map_err(|_| too_long(estimate))?;
    let plaintext = draft
        .checked_wire_len()
        .map_err(|_| PaperError::Invalid)?
        .saturating_sub(DESTINATION_LENGTH);
    if plaintext > PAPER_MAX_PLAINTEXT {
        return Err(too_long(plaintext));
    }
    if !manager.is_destination_known(dest_hash_hex) {
        return Err(PaperError::UnknownIdentity);
    }
    let identity = &manager.identity;
    let signed = draft
        .sign_with(|bytes| identity.sign(bytes).ok_or("identity signing unavailable"))
        .map_err(|_| PaperError::Unavailable)?;
    let stamp = signed
        .generate_recipient_stamp()
        .map_err(|_| PaperError::Unavailable)?;
    let message = signed
        .finalize(stamp)
        .map_err(|_| PaperError::Unavailable)?;

    let mut encrypted_len = None;
    let paper_uri = message
        .to_paper_uri(|plaintext| {
            let ciphertext = manager
                .encrypt_for_destination(dest_hash_hex, plaintext)
                .ok_or_else(|| MessageError::PackFailed("recipient key unavailable".into()))?;
            encrypted_len = Some(ciphertext.len());
            Ok(ciphertext)
        })
        .map_err(|_| match encrypted_len {
            None => PaperError::UnknownIdentity,
            Some(len) => PaperError::TooLong {
                overage_chars: overage_chars(
                    content,
                    (DESTINATION_LENGTH + len).saturating_sub(PAPER_MDU),
                ),
            },
        })?;
    Ok(BuiltPaperMessage {
        message_id: hex::encode(message.message_id()),
        paper_uri,
        timestamp: message.timestamp(),
    })
}

pub struct CreatedPaperMessage {
    /// The stored row, in conversation-history shape.
    pub message: Value,
    pub paper_uri: String,
}

/// Create, store and announce an outbound paper message. It never enters the
/// router, so no send, retry or timeout path can pick it up.
pub async fn create_paper_message(
    state: &Arc<AppState>,
    dest_hash_hex: &str,
    content: &str,
    title: &str,
) -> Result<CreatedPaperMessage, PaperError> {
    let (built, identity_id, source) = {
        let lxmf = state.lxmf.lock().map_err(|_| PaperError::Unavailable)?;
        let manager = lxmf.as_ref().ok_or(PaperError::Unavailable)?;
        (
            build_paper_message(manager, dest_hash_hex, content, title)?,
            manager.identity_hash.clone(),
            manager.lxmf_hash.clone(),
        )
    };
    let BuiltPaperMessage {
        message_id,
        paper_uri,
        timestamp,
    } = built;
    let row = {
        let message_id = message_id.clone();
        let identity_id = identity_id.clone();
        let destination = dest_hash_hex.to_string();
        let content = content.to_string();
        let title = title.to_string();
        let paper_uri = paper_uri.clone();
        db::spawn_db(state.db.clone(), move |p| {
            let display_timestamp =
                db::next_conversation_observed_timestamp(&p, &destination, &identity_id, timestamp);
            db::try_save_paper_message(
                &p,
                &message_id,
                &source,
                &destination,
                &content,
                &title,
                display_timestamp,
                &identity_id,
                &paper_uri,
            )
            .ok()?;
            db::unhide_conversation(&p, &destination, &identity_id);
            db::get_message_for_identity(&p, &message_id, &identity_id)
        })
        .await
        .map_err(|_| PaperError::Storage)?
        .ok_or(PaperError::Storage)?
    };
    if crate::with_receiver(state, &identity_id, |_| ()).is_some() {
        state.emit_to_all("lxmf_message", row.clone());
        crate::messaging::broadcast_conversations(Arc::clone(state));
    }
    Ok(CreatedPaperMessage {
        message: row,
        paper_uri,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PaperPreview {
    /// `ok`, or `duplicate` when this identity already holds the message.
    pub status: &'static str,
    pub message_id: String,
    pub source_hash: String,
    pub display_name: Option<String>,
    pub timestamp: f64,
    pub title: String,
    pub content: String,
    pub signature: PaperSignature,
    pub blocked: bool,
}

struct DecodedForActive {
    decoded: DecodedPaperMessage,
    identity_id: String,
}

fn decode_for_active(state: &AppState, uri: &str) -> Result<DecodedForActive, PaperError> {
    let lxmf = state.lxmf.lock().map_err(|_| PaperError::Unavailable)?;
    let manager = lxmf.as_ref().ok_or(PaperError::Unavailable)?;
    Ok(DecodedForActive {
        decoded: decode_paper_message(manager, uri)?,
        identity_id: manager.identity_hash.clone(),
    })
}

/// Hex source destination of a paper URI for the active identity, so callers
/// can learn the sender's key before previewing.
pub fn paper_source_hash(state: &AppState, uri: &str) -> Result<String, PaperError> {
    decode_for_active(state, uri).map(|decoded| hex::encode(decoded.decoded.message.source_hash()))
}

/// Decode a paper URI without storing anything or emitting events.
pub async fn preview_paper_message(
    state: &AppState,
    uri: &str,
) -> Result<PaperPreview, PaperError> {
    let DecodedForActive {
        decoded: DecodedPaperMessage { message, signature },
        identity_id,
    } = decode_for_active(state, uri)?;
    let message_id = hex::encode(message.message_id());
    let source_hash = hex::encode(message.source_hash());
    let (duplicate, blocked, display_name) = {
        let message_id = message_id.clone();
        let source_hash = source_hash.clone();
        db::spawn_db(state.db.clone(), move |p| {
            (
                db::message_exists_for_identity(&p, &message_id, &identity_id),
                db::is_blocked(&p, &source_hash, &identity_id),
                crate::contact_name_from_db(&p, &source_hash, &identity_id),
            )
        })
        .await
        .map_err(|_| PaperError::Storage)?
    };
    Ok(PaperPreview {
        status: if duplicate { "duplicate" } else { "ok" },
        message_id,
        source_hash,
        display_name,
        timestamp: message.timestamp(),
        title: message.title_text().into_owned(),
        content: message.content_text().into_owned(),
        signature,
        blocked,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PaperAdded {
    /// `added`, or `duplicate` when the message was already filed.
    pub status: &'static str,
    pub message_id: String,
    pub source_hash: String,
    /// The stored row's timestamp, for scrolling to it.
    pub timestamp: f64,
}

async fn stored_timestamp(state: &AppState, message_id: &str, identity_id: &str) -> Option<f64> {
    let message_id = message_id.to_string();
    let identity_id = identity_id.to_string();
    db::spawn_db(state.db.clone(), move |p| {
        db::get_message_for_identity(&p, &message_id, &identity_id)
            .and_then(|row| row.get("timestamp").and_then(Value::as_f64))
    })
    .await
    .ok()
    .flatten()
}

/// File a paper message through the inbound pipeline as `Paper`: no stamp
/// requirement, no delivery proof, no last-heard touch.
pub async fn add_paper_message(state: &Arc<AppState>, uri: &str) -> Result<PaperAdded, PaperError> {
    let activity_origin = state.activity_request_fence();
    let session = state
        .session_shutdown
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let DecodedForActive {
        decoded: DecodedPaperMessage { message, signature },
        identity_id,
    } = decode_for_active(state, uri)?;
    if signature == PaperSignature::Invalid {
        return Err(PaperError::SignatureInvalid);
    }
    let message_id = hex::encode(message.message_id());
    let source_hash = hex::encode(message.source_hash());
    let duplicate = |timestamp: f64| PaperAdded {
        status: "duplicate",
        message_id: message_id.clone(),
        source_hash: source_hash.clone(),
        timestamp,
    };
    let (existing, blocked) = {
        let message_id = message_id.clone();
        let source_hash = source_hash.clone();
        let identity_id = identity_id.clone();
        db::spawn_db(state.db.clone(), move |p| {
            (
                db::get_message_for_identity(&p, &message_id, &identity_id)
                    .and_then(|row| row.get("timestamp").and_then(Value::as_f64)),
                db::is_blocked(&p, &source_hash, &identity_id),
            )
        })
        .await
        .map_err(|_| PaperError::Storage)?
    };
    if let Some(timestamp) = existing {
        return Ok(duplicate(timestamp));
    }
    if blocked {
        return Err(PaperError::Blocked);
    }

    let sent_at = message.timestamp();
    let wire = message.wire_bytes().to_vec();
    let outcome = crate::process_inbound_lxmf(
        state,
        message,
        &wire,
        InboundLxmfSource::Paper,
        activity_origin,
        session,
    )
    .await;
    match outcome {
        InboundOutcome::Stored { timestamp } => Ok(PaperAdded {
            status: "added",
            message_id: message_id.clone(),
            source_hash: source_hash.clone(),
            timestamp,
        }),
        InboundOutcome::Consumed => Ok(PaperAdded {
            status: "added",
            message_id: message_id.clone(),
            source_hash: source_hash.clone(),
            timestamp: sent_at,
        }),
        InboundOutcome::Duplicate => Ok(duplicate(
            stored_timestamp(state, &message_id, &identity_id)
                .await
                .unwrap_or(sent_at),
        )),
        InboundOutcome::Blocked | InboundOutcome::Blackholed => Err(PaperError::Blocked),
        InboundOutcome::SignatureInvalid => Err(PaperError::SignatureInvalid),
        InboundOutcome::NotForUs => Err(PaperError::NotForYou),
        InboundOutcome::StampRejected => Err(PaperError::Invalid),
        InboundOutcome::SaveFailed => Err(PaperError::Storage),
    }
}

#[cfg(test)]
mod tests;
