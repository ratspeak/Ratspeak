// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

/// Rust handler ownership is separate from its reusable public WebView label.
/// Configuration recreation keeps this key; a new logical window never does.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct HandlerKey {
  pub activity_id: i32,
  pub generation: u64,
  pub label: String,
}

impl HandlerKey {
  pub fn new(activity_id: i32, generation: u64, label: String) -> Self {
    Self {
      activity_id,
      generation,
      label,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::HandlerKey;
  use std::collections::HashMap;

  #[test]
  fn late_old_cleanup_cannot_delete_already_published_replacement_handlers() {
    let old = HandlerKey::new(1, 1, "main".into());
    let new = HandlerKey::new(2, 2, "main".into());
    let mut handlers = HashMap::from([(old.clone(), "old"), (new.clone(), "new")]);
    handlers.remove(&old);
    assert_eq!(handlers.get(&new), Some(&"new"));
  }

  #[test]
  fn reused_activity_id_and_label_do_not_reuse_handler_ownership() {
    let old = HandlerKey::new(1, 1, "main".into());
    let new = HandlerKey::new(1, 2, "main".into());
    let mut handlers = HashMap::from([(new.clone(), "new")]);
    handlers.remove(&old);
    assert_eq!(handlers.get(&new), Some(&"new"));
    assert_eq!(new, new.clone()); // Physical configuration changes keep the key.
  }
}
