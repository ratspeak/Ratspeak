//! Text received from Android's Sharesheet is data, never a deep-link command.
//! Native persistence owns pending items; JavaScript only chooses a recipient
//! and edits an unsent draft. This module has no transport/send capability.

#[cfg(target_os = "android")]
mod android;
#[cfg(any(target_os = "android", test))]
mod model;

use ratspeak_tauri::state::AppState;
use serde_json::Value;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub(crate) async fn list_text_shares(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    return android::list(state.inner().clone()).await;
    #[cfg(not(target_os = "android"))]
    {
        let _ = state;
        Err("Text sharing is available on Android only.".into())
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextShareEdit {
    pub id: String,
    pub revision: String,
    pub activity_generation: String,
    pub identity_generation: String,
    pub operation: String,
    pub recipient: Option<String>,
    pub text: Option<String>,
}

#[tauri::command]
pub(crate) async fn edit_text_share(
    state: State<'_, Arc<AppState>>,
    args: TextShareEdit,
) -> Result<Value, String> {
    #[cfg(target_os = "android")]
    return android::edit(state.inner().clone(), args).await;
    #[cfg(not(target_os = "android"))]
    {
        let TextShareEdit {
            id,
            revision,
            activity_generation,
            identity_generation,
            operation,
            recipient,
            text,
        } = args;
        let _ = (
            state,
            id,
            revision,
            activity_generation,
            identity_generation,
            operation,
            recipient,
            text,
        );
        Err("Text sharing is available on Android only.".into())
    }
}

#[cfg(target_os = "android")]
pub(crate) fn install(app: &tauri::AppHandle) {
    android::install(app);
}
#[cfg(target_os = "android")]
pub(crate) fn notify_pending() {
    android::notify();
}

#[cfg(target_os = "android")]
pub(crate) fn forget(identity: Option<&str>) -> Result<(), String> {
    android::forget(identity)
}
