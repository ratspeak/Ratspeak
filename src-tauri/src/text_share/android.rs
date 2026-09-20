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
    prune_images(&inbox)?;
    let result = model::transaction(&mut inbox, edit, write);
    // On failed acceptance this removes the uncommitted photo; on discard it
    // removes only this inbox's retired copies. Never remove a live item's data.
    let cleanup = prune_images(&inbox);
    if result.is_ok() {
        cleanup?;
    }
    result
}

fn prune_images(inbox: &Inbox) -> Result<(), String> {
    let ids: Vec<_> = inbox
        .items
        .iter()
        .filter(|i| i.image.is_some())
        .map(|i| &i.id)
        .collect();
    let ids = serde_json::to_string(&ids).map_err(|_| "Shared photo cleanup failed.")?;
    crate::mobile_native::with_android_class(CLASS, |env, class| {
        let ids = env.new_string(ids)?;
        env.call_static_method(
            class,
            "pruneImages",
            "(Ljava/lang/String;)V",
            &[JValue::Object(ids.into())],
        )?;
        Ok(())
    })
    .ok_or_else(|| "Shared photo cleanup failed. Check device storage and retry.".into())
}

fn stage_image(
    state: &Arc<AppState>,
    identity: &str,
    args: &TextShareEdit,
) -> Result<Value, String> {
    let _serial = SERIAL
        .lock()
        .map_err(|_| "Shared drafts storage is busy.")?;
    let inbox = read()?;
    let item = inbox
        .items
        .iter()
        .find(|i| {
            i.id == args.id
                && i.revision == args.revision
                && i.identity.as_deref() == Some(identity)
                && i.recipient.is_some()
        })
        .ok_or("This shared draft changed. Open it again.")?;
    let image = item
        .image
        .as_ref()
        .ok_or("This shared item has no photo.")?;
    let token = state
        .begin_attachment_staging(image.name.clone(), image.mime.clone(), image.size, true)
        .map_err(|_| "Could not prepare this photo. Finish any other attachment and retry.")?;
    let result = (|| {
        let mut offset = 0;
        for index in 0..image.size.div_ceil(model::IMAGE_CHUNK_BYTES) {
            let mut bytes = crate::mobile_native::with_android_class(CLASS, |env, class| {
                let id = env.new_string(&item.id)?;
                let value = env
                    .call_static_method(
                        class,
                        "readImageChunk",
                        "(Ljava/lang/String;IJ)[B",
                        &[
                            JValue::Object(id.into()),
                            JValue::Int(index as i32),
                            JValue::Long(image.size as i64),
                        ],
                    )?
                    .l()?;
                let result = env.convert_byte_array(value.into_inner());
                env.delete_local_ref(value)?;
                result
            })
            .ok_or("The saved photo could not be read. Discard it and share it again.")?;
            if bytes.len() != model::IMAGE_CHUNK_BYTES.min(image.size - offset) {
                bytes.fill(0);
                return Err("The saved photo is incomplete. Share it again.");
            }
            let appended = state.append_attachment_staging(&token, offset, &bytes);
            offset += bytes.len();
            bytes.fill(0);
            appended.map_err(|_| "Could not stage the shared photo.")?;
        }
        Ok(
            json!({"item": item, "stage": {"token": token, "name": image.name, "mime": image.mime, "size": image.size}}),
        )
    })();
    if result.is_err() {
        state.cancel_attachment_staging(&token);
    }
    result.map_err(String::from)
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
    if args.operation == "stage_image" {
        let activity = args.activity_generation.clone();
        let staged_state = state.clone();
        let result =
            tokio::task::spawn_blocking(move || stage_image(&staged_state, &identity, &args))
                .await
                .map_err(|_| "Shared photo preparation failed.")??;
        if let Err(error) = owner(Some(&activity)) {
            if let Some(token) = result["stage"]["token"].as_str() {
                state.cancel_attachment_staging(token);
            }
            return Err(error);
        }
        return Ok(result);
    }
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
        prune_images(&Inbox::default())?;
    }
    notify();
    Ok(())
}

#[no_mangle]
pub extern "system" fn Java_org_ratspeak_android_RatspeakTextShares_acceptImage(
    env: jni::JNIEnv,
    _class: JClass,
    id: JString,
    text: JString,
    subject: JString,
    uri: JString,
    mime: JString,
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
        let uri: String = env
            .get_string(uri)
            .map_err(|_| "Invalid shared photo.")?
            .into();
        let mime: String = env
            .get_string(mime)
            .map_err(|_| "Invalid shared photo type.")?
            .into();
        if !model::valid_hash(&id) {
            return Err("Invalid share identifier.".into());
        }
        transaction(|inbox| {
            // Deduplicate saved-state delivery before reopening a possibly
            // expired URI, and reject a full queue before copying any bytes.
            if inbox.known(&id) {
                return Ok(false);
            }
            if inbox.items.len() >= model::MAX_ITEMS {
                return Err(
                    "Eight shared drafts are pending. Use or discard one, then share again.".into(),
                );
            }
            let raw = crate::mobile_native::with_android_class(CLASS, |env, class| {
                let id = env.new_string(&id)?;
                let uri = env.new_string(&uri)?;
                let mime = env.new_string(&mime)?;
                let result = env.call_static_method(class, "copyImage", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                    &[JValue::Object(id.into()), JValue::Object(uri.into()), JValue::Object(mime.into())])?.l()?;
                Ok::<String, jni::errors::Error>(env.get_string(JString::from(result))?.into())
            }).ok_or("Could not save the photo. Share one accessible photo up to 128 MB and try again.")?;
            let image = serde_json::from_str(&raw).map_err(|_| "Invalid shared photo metadata.")?;
            inbox.accept_image(&id, &text, &subject, image)
        })?;
        Ok::<_, String>(())
    })();
    if result.is_ok() {
        notify();
    }
    env.new_string(result.err().unwrap_or_default())
        .map(|v| v.into_inner())
        .unwrap_or(std::ptr::null_mut())
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
