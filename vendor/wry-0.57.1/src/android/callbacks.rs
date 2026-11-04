// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Each Java WebView receives a fresh opaque key, never the reusable window label.
//! Snapshot handler Arcs before publishing that key so a callback racing retirement
//! can finish against its original handler but cannot enter a replacement's handler.

use super::{
  ASSET_LOADER_DOMAIN, IPC, ON_LOAD_HANDLER, PERMISSION_HANDLER, REQUEST_HANDLER,
  TITLE_CHANGE_HANDLER, URL_LOADING_OVERRIDE, UnsafeIpc, UnsafeOnPageLoadHandler,
  UnsafePermissionHandler, UnsafeRequestHandler, UnsafeTitleHandler, UnsafeUrlLoadingOverride,
  handler_key::HandlerKey, main_pipe::ActivityId,
};
use std::sync::{Arc, LazyLock, Mutex};

#[path = "callback_registry.rs"]
mod callback_registry;
use callback_registry::CallbackRegistry;

pub(super) struct NativeCallbacks {
  pub logical_id: String,
  pub request: Option<Arc<Mutex<UnsafeRequestHandler>>>,
  pub ipc: Option<Arc<Mutex<UnsafeIpc>>>,
  pub title: Option<Arc<Mutex<UnsafeTitleHandler>>>,
  pub navigation: Option<Arc<Mutex<UnsafeUrlLoadingOverride>>>,
  pub load: Option<Arc<Mutex<UnsafeOnPageLoadHandler>>>,
  pub permission: Option<Arc<Mutex<UnsafePermissionHandler>>>,
  pub asset_loader_domain: Option<String>,
}

static CALLBACKS: LazyLock<Mutex<CallbackRegistry<NativeCallbacks>>> =
  LazyLock::new(|| Mutex::new(CallbackRegistry::new()));

// Called only by Android's UI-thread creation path, after Rust handler publication.
pub(super) fn register(activity_id: ActivityId, key: &HandlerKey) -> Option<String> {
  if key.activity_id != activity_id {
    return None;
  }
  // Separate statements release each global map lock before acquiring another.
  // This handler is always published, even without custom protocols. Missing
  // ownership is a failed construction, never a ready-but-blank Java view.
  let request = REQUEST_HANDLER.lock().unwrap().get(key).cloned()?;
  let ipc = IPC.lock().unwrap().get(key).cloned();
  let title = TITLE_CHANGE_HANDLER.lock().unwrap().get(key).cloned();
  let navigation = URL_LOADING_OVERRIDE.lock().unwrap().get(key).cloned();
  let load = ON_LOAD_HANDLER.lock().unwrap().get(key).cloned();
  let permission = PERMISSION_HANDLER.lock().unwrap().get(key).cloned();
  let asset_loader_domain = ASSET_LOADER_DOMAIN.lock().unwrap().get(key).cloned();
  let callbacks = NativeCallbacks {
    logical_id: key.label.clone(),
    request: Some(request),
    ipc,
    title,
    navigation,
    load,
    permission,
    asset_loader_domain,
  };
  Some(CALLBACKS.lock().unwrap().register(activity_id, callbacks))
}

pub(super) fn retire(activity_id: ActivityId) {
  CALLBACKS.lock().unwrap().retire(activity_id);
}

pub(super) fn resolve(key: &str) -> Option<Arc<NativeCallbacks>> {
  CALLBACKS.lock().unwrap().resolve(key)
}

pub(super) fn is_current(key: &str, snapshot: &Arc<NativeCallbacks>) -> bool {
  CALLBACKS.lock().unwrap().is_current(key, snapshot)
}
