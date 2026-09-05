// Copyright 2020-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

/// Physical Activity creation state. Unlike the logical window lifetime, this
/// epoch changes during configuration recreation and cannot replay old work.
#[derive(Clone)]
pub(crate) struct NativeCreation {
  pub epoch: u64,
  started: bool,
  ready: bool,
  retired: bool,
}

impl NativeCreation {
  pub fn new(epoch: u64) -> Self {
    Self {
      epoch,
      started: false,
      ready: false,
      retired: false,
    }
  }

  pub fn begin(&mut self, epoch: u64) -> bool {
    if self.epoch != epoch || self.started || self.retired {
      return false;
    }
    self.started = true;
    true
  }

  pub fn complete(&mut self, epoch: u64) -> bool {
    if self.epoch != epoch || !self.started || self.ready || self.retired {
      return false;
    }
    self.ready = true;
    true
  }

  pub fn retire(&mut self) {
    self.retired = true;
    self.ready = false;
  }
}

#[cfg(test)]
mod tests {
  use super::NativeCreation;

  #[test]
  fn readiness_requires_complete_creation_once() {
    let mut state = NativeCreation::new(1);
    assert!(!state.complete(1));
    assert!(state.begin(1));
    assert!(!state.begin(1));
    assert!(state.complete(1));
    assert!(!state.complete(1));
  }

  #[test]
  fn inline_recreation_rejects_old_and_duplicate_queued_creation() {
    let mut state = NativeCreation::new(2);
    assert!(!state.begin(1));
    assert!(state.begin(2));
    assert!(state.complete(2));
    assert!(!state.begin(1));
    assert!(!state.begin(2));
  }

  #[test]
  fn retired_and_failed_attempts_require_a_new_physical_activity() {
    let mut state = NativeCreation::new(1);
    assert!(state.begin(1));
    state.retire();
    assert!(!state.complete(1));
    assert!(!state.begin(1));
    let mut replacement = NativeCreation::new(2);
    assert!(replacement.begin(2));
    assert!(replacement.complete(2));
  }
}
