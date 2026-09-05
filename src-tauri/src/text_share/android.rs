use super::{
    model::{self, Inbox},
    TextShareEdit,
};
use jni::objects::{JClass, JString, JValue};
use ratspeak_tauri::{helpers::active_identity_snapshot, state::AppState};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Emitter;

const CLASS: &str = "org/ratspeak/android/RatspeakTextShares";
static SERIAL: Mutex<()> = Mutex::new(());
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

pub(super) fn install(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
}
pub(super) fn notify() {
    if let Some(app) = APP.get() {
        let _ = app.emit("text_shares_available", ());
    }
}

fn read() -> Result<Inbox, String> {
    let raw = crate::mobile_native::with_android_class(CLASS, |env, class| {
        let value = env
            .call_static_method(class, "read", "()Ljava/lang/String;", &[])?
            .l()?;
        let result = env.get_string(JString::from(value))?.into();
        Ok::<String, jni::errors::Error>(result)
    })
    .ok_or("Shared drafts storage is unavailable. Retry after unlocking the device.")?;
    Inbox::decode(&raw)
}

fn write(raw: &str) -> Result<(), String> {
    crate::mobile_native::with_android_class(CLASS, |env, class| {
        let raw = env.new_string(raw)?;
        env.call_static_method(
            class,
            "write",
            "(Ljava/lang/String;)V",
            &[JValue::Object(raw.into())],
        )?;
        Ok(())
    })
    .ok_or_else(|| "Could not save the shared draft. Check device storage and retry.".into())
}

fn transaction<T>(edit: impl FnOnce(&mut Inbox) -> Result<T, String>) -> Result<T, String> {
    let _serial = SERIAL
        .lock()
        .map_err(|_| "Shared drafts storage is busy. Restart Ratspeak.")?;
    let mut inbox = read()?;
    model::transaction(&mut inbox, edit, write)
}

fn identity(state: &AppState) -> Result<String, String> {
    if state.get_startup_stage() != "ready" {
        return Err("Wait for identity setup to finish before sharing.".into());
    }
    if state.hw_locked_hash().is_some() {
        return Err("Unlock your identity before sharing.".into());
    }
    active_identity_snapshot(state)
        .map(|(id, _)| id)
        .filter(|id| model::valid_hash(id))
        .ok_or_else(|| "Finish identity setup before sharing.".into())
}

fn owner(expected: Option<&str>) -> Result<String, String> {
    crate::android_lifecycle::with_presentation_owner(expected, |generation| generation.to_string())
        .ok_or_else(|| "Open Ratspeak before using a shared draft.".into())
}

pub(super) async fn list(state: Arc<AppState>) -> Result<Value, String> {
    let _identity = state.identity_switch_lock.lock().await;
    let activity = owner(None)?;
    let identity = identity(&state)?;
    let generation = state.current_identity_session_generation().to_string();
    let rows = tokio::task::spawn_blocking(move || {
        let _serial = SERIAL
            .lock()
            .map_err(|_| "Shared drafts storage is unavailable.")?;
        let inbox = read()?;
        Ok::<_, String>(
            inbox
                .items
                .into_iter()
                .filter(|i| i.identity.as_deref().is_none_or(|id| id == identity))
                .collect::<Vec<_>>(),
        )
    })
    .await
    .map_err(|_| "Shared drafts could not be loaded.")??;
    owner(Some(&activity))?;
    Ok(json!({"items": rows, "activity_generation": activity, "identity_generation": generation}))
}

pub(super) async fn edit(state: Arc<AppState>, args: TextShareEdit) -> Result<Value, String> {
    let _identity = state.identity_switch_lock.lock().await;
    let identity = identity(&state)?;
    if state.current_identity_session_generation().to_string() != args.identity_generation {
        return Err("The identity changed. Open the shared draft again.".into());
    }
    // Admit an explicit operation under current native authority. Disk/JNI work
    // happens outside the lifecycle mutex, while identity replacement remains
    // serialized. Later page retirement does not undo an admitted user action;
    // exact item revisions prevent it consuming a newer draft.
    owner(Some(&args.activity_generation))?;
    let item =
        tokio::task::spawn_blocking(move || transaction(|inbox| inbox.edit(&identity, &args)))
            .await
            .map_err(|_| "Shared draft operation failed.")??;
    notify();
    Ok(json!({"item": item}))
}

pub(super) fn forget(identity: Option<&str>) -> Result<(), String> {
    if let Some(identity) = identity {
        transaction(|inbox| {
            let keep: Vec<_> = inbox
                .items
                .iter()
                .filter_map(|i| i.identity.clone())
                .filter(|id| id != identity)
                .collect();
            inbox.prune(&keep);
            Ok(())
        })?;
    } else {
        // Reset is also the recovery path for corrupt/unreadable old data;
        // never require decrypting it before replacing it with an empty inbox.
        let _serial = SERIAL
            .lock()
            .map_err(|_| "Shared drafts cleanup unavailable.")?;
        let encoded = serde_json::to_string(&Inbox::default())
            .map_err(|_| "Shared drafts cleanup failed.")?;
        write(&encoded)?;
    }
    notify();
    Ok(())
}

#[no_mangle]
pub extern "system" fn Java_org_ratspeak_android_RatspeakTextShares_accept(
    env: jni::JNIEnv,
    _class: JClass,
    id: JString,
    text: JString,
    subject: JString,
) -> jni::sys::jstring {
    let result = (|| {
        let id: String = env
            .get_string(id)
            .map_err(|_| "Invalid share identifier.")?
            .into();
        let text: String = env
            .get_string(text)
            .map_err(|_| "Invalid shared text.")?
            .into();
        let subject: String = env
            .get_string(subject)
            .map_err(|_| "Invalid shared title.")?
            .into();
        transaction(|inbox| inbox.accept(&id, &text, &subject))?;
        Ok::<_, String>(())
    })();
    if result.is_ok() {
        notify();
    }
    env.new_string(result.err().unwrap_or_default())
        .map(|v| v.into_inner())
        .unwrap_or(std::ptr::null_mut())
}
