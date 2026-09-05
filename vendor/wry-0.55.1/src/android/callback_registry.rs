// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

//! Native-view callback authority, independent of Android/JNI for regression tests.

use std::{collections::HashMap, sync::Arc};

pub(super) struct CallbackRegistry<T> {
  serial: u64,
  activities: HashMap<i32, String>,
  callbacks: HashMap<String, Arc<T>>,
}

impl<T> CallbackRegistry<T> {
  pub(super) fn new() -> Self {
    Self {
      serial: 0,
      activities: HashMap::new(),
      callbacks: HashMap::new(),
    }
  }

  pub(super) fn register(&mut self, activity_id: i32, callbacks: T) -> String {
    self.retire(activity_id);
    // Never wrap into an identity used by an old Java object.
    self.serial = self
      .serial
      .checked_add(1)
      .expect("native callback identities exhausted");
    let key = format!("wry-native-{}", self.serial);
    self.activities.insert(activity_id, key.clone());
    self.callbacks.insert(key.clone(), Arc::new(callbacks));
    key
  }

  pub(super) fn resolve(&self, key: &str) -> Option<Arc<T>> {
    self.callbacks.get(key).cloned()
  }

  pub(super) fn is_current(&self, key: &str, snapshot: &Arc<T>) -> bool {
    self
      .callbacks
      .get(key)
      .is_some_and(|current| Arc::ptr_eq(current, snapshot))
  }

  pub(super) fn retire(&mut self, activity_id: i32) {
    if let Some(key) = self.activities.remove(&activity_id) {
      self.callbacks.remove(&key);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn retired_view_cannot_resolve_replacement_with_same_logical_label() {
    let mut registry = CallbackRegistry::new();
    let old = registry.register(7, ("main", "old-handler"));
    registry.retire(7);
    let new = registry.register(7, ("main", "new-handler"));
    assert_ne!(old, new);
    assert!(registry.resolve(&old).is_none());
    assert_eq!(*registry.resolve(&new).unwrap(), ("main", "new-handler"));
    assert!(registry.resolve("main").is_none());
  }

  #[test]
  fn configuration_replaces_native_key_without_replacing_logical_handler() {
    let handler = Arc::new(());
    let mut registry = CallbackRegistry::new();
    let old = registry.register(7, handler.clone());
    let new = registry.register(7, handler.clone());
    assert!(registry.resolve(&old).is_none());
    assert!(Arc::ptr_eq(
      registry.resolve(&new).unwrap().as_ref(),
      &handler
    ));
  }

  #[test]
  fn in_flight_snapshot_never_relooks_up_a_replacement_handler() {
    let mut registry = CallbackRegistry::new();
    let old = registry.register(7, "old-handler");
    let in_flight = registry.resolve(&old).unwrap();
    let new = registry.register(7, "new-handler");
    assert_eq!(*in_flight, "old-handler");
    assert_eq!(*registry.resolve(&new).unwrap(), "new-handler");
    assert!(registry.resolve(&old).is_none());
  }

  #[test]
  fn retiring_one_activity_does_not_revoke_another() {
    let mut registry = CallbackRegistry::new();
    let first = registry.register(7, "first");
    let second = registry.register(8, "second");
    registry.retire(7);
    registry.retire(7);
    assert!(registry.resolve(&first).is_none());
    assert_eq!(*registry.resolve(&second).unwrap(), "second");
    assert_eq!(registry.activities.len(), 1);
    assert_eq!(registry.callbacks.len(), 1);
  }

  #[test]
  fn callback_waiting_for_handler_lock_is_rechecked_after_retirement() {
    use std::{
      sync::{mpsc, Mutex},
      thread,
      time::Duration,
    };
    let registry = Arc::new(Mutex::new(CallbackRegistry::new()));
    let key = registry.lock().unwrap().register(7, Mutex::new(0));
    let handler = registry.lock().unwrap().resolve(&key).unwrap();
    let busy = handler.lock().unwrap();
    let (resolved, wait_for_resolved) = mpsc::channel();
    let worker_registry = registry.clone();
    let worker = thread::spawn(move || {
      let snapshot = worker_registry.lock().unwrap().resolve(&key).unwrap();
      resolved.send(()).unwrap();
      let mut handler = snapshot.lock().unwrap();
      if worker_registry.lock().unwrap().is_current(&key, &snapshot) {
        *handler += 1;
      }
    });
    wait_for_resolved
      .recv_timeout(Duration::from_secs(5))
      .unwrap();
    // Teardown never waits for the blocked handler or its response.
    registry.lock().unwrap().retire(7);
    drop(busy);
    worker.join().unwrap();
    assert_eq!(*handler.lock().unwrap(), 0);
  }
}
