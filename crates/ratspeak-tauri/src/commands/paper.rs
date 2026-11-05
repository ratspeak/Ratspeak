//! LXMF paper messages: create, preview, and file `lxm://` URIs.
//!
//! Arguments are snake_case at the top level. Errors carry stable `paper_*`
//! codes; `paper_too_long` adds `details.overage_chars`.

use std::sync::Arc;

use serde_json::{Value, json};
use tauri::State;

use crate::db;
use crate::error::{AppError, AppResult};
use crate::helpers::{active_identity_id, sanitize_text, validate_hex};
use crate::state::AppState;
use ratspeak_runtime::paper::{self, PaperError};

pub use ratspeak_runtime::paper::normalize_paper_uri;

fn paper_error(error: PaperError) -> AppError {
    let app_error = AppError::new(error.code(), error.message());
    match error {
        PaperError::TooLong { overage_chars } => {
            app_error.with_details(json!({ "overage_chars": overage_chars }))
        }
        _ => app_error,
    }
}

/// Learn the sender's key from a saved contact card so its signature can be
/// checked; the URI itself is never trusted for that key.
async fn hydrate_paper_sender(state: &AppState, uri: &str) {
    if let Ok(source) = paper::paper_source_hash(state, uri) {
        crate::commands::shared::hydrate_contact_identity_for_send(state, &source).await;
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn create_paper_message(
    state: State<'_, Arc<AppState>>,
    dest_hash: String,
    content: String,
    title: Option<String>,
) -> AppResult<Value> {
    let dest_hash = sanitize_text(&dest_hash, 128).to_ascii_lowercase();
    if !validate_hex(&dest_hash, 32, 32) {
        return Err(paper_error(PaperError::InvalidDestination));
    }
    let content = content.trim();
    if content.is_empty() {
        return Err(paper_error(PaperError::Empty));
    }
    let title = sanitize_text(title.as_deref().unwrap_or(""), 256);
    crate::commands::shared::hydrate_contact_identity_for_send(&state, &dest_hash).await;
    let created = paper::create_paper_message(&state, &dest_hash, content, &title)
        .await
        .map_err(paper_error)?;
    let uri_chars = created.paper_uri.chars().count();
    Ok(json!({
        "message": created.message,
        "paper_uri": created.paper_uri,
        "uri_chars": uri_chars,
    }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn get_paper_uri(
    state: State<'_, Arc<AppState>>,
    message_id: String,
) -> AppResult<Value> {
    let message_id = sanitize_text(&message_id, 128).to_ascii_lowercase();
    if !validate_hex(&message_id, 64, 64) {
        return Err(paper_error(PaperError::NotFound));
    }
    let identity_id = active_identity_id(&state);
    let paper_uri = db::spawn_db(state.db.clone(), move |p| {
        db::get_paper_uri(&p, &message_id, &identity_id)
    })
    .await
    .map_err(|_| paper_error(PaperError::Storage))?
    .ok_or_else(|| paper_error(PaperError::NotFound))?;
    Ok(json!({ "paper_uri": paper_uri }))
}

#[tauri::command(rename_all = "snake_case")]
pub async fn preview_paper_message(
    state: State<'_, Arc<AppState>>,
    uri: String,
) -> AppResult<Value> {
    hydrate_paper_sender(&state, &uri).await;
    let preview = paper::preview_paper_message(&state, &uri)
        .await
        .map_err(paper_error)?;
    Ok(serde_json::to_value(preview)?)
}

#[tauri::command(rename_all = "snake_case")]
pub async fn add_paper_message(state: State<'_, Arc<AppState>>, uri: String) -> AppResult<Value> {
    hydrate_paper_sender(&state, &uri).await;
    let added = paper::add_paper_message(&state, &uri)
        .await
        .map_err(paper_error)?;
    Ok(serde_json::to_value(added)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_long_errors_carry_the_overage_for_the_frontend() {
        let error = paper_error(PaperError::TooLong { overage_chars: 42 });
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "paper_too_long");
        assert_eq!(value["details"]["overage_chars"], 42);
        let plain = serde_json::to_value(paper_error(PaperError::NotForYou)).unwrap();
        assert_eq!(plain["code"], "paper_not_for_you");
        assert!(plain.get("details").is_none());
    }
}
