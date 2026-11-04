// Copyright 2014-2021 The winit contributors
// Copyright 2021-2023 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0

/// A looper result is not proof that its user-event queue is empty. In
/// particular, native FD readiness can consume a simultaneous explicit wake.
/// The event loop calls this after every poll result, before polling again.
pub(super) fn dispatch_pending<T>(
  accepting_events: bool,
  mut next: impl FnMut() -> Option<T>,
  mut dispatch: impl FnMut(T) -> bool,
) {
  if !accepting_events {
    return;
  }
  while let Some(event) = next() {
    if !dispatch(event) {
      break;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::dispatch_pending;
  use std::{cell::RefCell, collections::VecDeque};

  #[test]
  fn queued_restore_dispatches_without_a_distinct_wake_result() {
    // The poll returned an Activity FD, not Poll::Wake. Its queued restoration
    // still has to execute before the event loop goes back to sleep.
    let mut pending = VecDeque::from(["restore"]);
    let mut delivered = Vec::new();
    dispatch_pending(
      true,
      || pending.pop_front(),
      |event| {
        delivered.push(event);
        true
      },
    );
    assert_eq!(delivered, ["restore"]);
    assert!(pending.is_empty());
  }

  #[test]
  fn coalesced_user_events_keep_fifo_order_and_are_not_repeated() {
    let mut pending = VecDeque::from([1, 2, 3]);
    let mut delivered = Vec::new();
    for _ in 0..2 {
      dispatch_pending(
        true,
        || pending.pop_front(),
        |event| {
          delivered.push(event);
          true
        },
      );
    }
    assert_eq!(delivered, [1, 2, 3]);
  }

  #[test]
  fn event_queued_during_dispatch_does_not_need_another_poll_wake() {
    let pending = RefCell::new(VecDeque::from([1]));
    let mut delivered = Vec::new();
    dispatch_pending(
      true,
      || pending.borrow_mut().pop_front(),
      |event| {
        delivered.push(event);
        if event == 1 {
          pending.borrow_mut().push_back(2);
        }
        true
      },
    );
    assert_eq!(delivered, [1, 2]);
  }

  #[test]
  fn accepted_native_exit_does_not_dispatch_a_queued_restore() {
    let mut pending = VecDeque::from(["restore"]);
    dispatch_pending(false, || pending.pop_front(), |_| panic!("already exiting"));
    assert_eq!(pending.len(), 1);
    // A prevented exit keeps normal dispatch enabled.
    dispatch_pending(true, || pending.pop_front(), |_| true);
    assert!(pending.is_empty());
  }

  #[test]
  fn accepted_user_exit_stops_before_later_queued_work() {
    let mut pending = VecDeque::from(["exit", "restore"]);
    let mut delivered = Vec::new();
    dispatch_pending(
      true,
      || pending.pop_front(),
      |event| {
        delivered.push(event);
        false
      },
    );
    assert_eq!(delivered, ["exit"]);
    assert_eq!(pending, ["restore"]);
  }
}
