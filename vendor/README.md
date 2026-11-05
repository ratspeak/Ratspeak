# Maintained lifecycle patches

These are exact crates.io source archives plus narrowly maintained Android
Activity/WebView lifetime fixes and one iOS scene fix. Both Cargo roots select
the same local sources. Desktop behavior is unchanged. Original licenses and VCS provenance remain
in each crate; Ratspeak's license does not replace their upstream licenses.

| Package | Version | Original archive SHA-256 |
| --- | --- | --- |
| tao | 0.37.2 | d5777b06fe6435126ae8d816eba269e050e1e10d302cf5461e9c49eccabc3548 |
| wry | 0.57.1 | 759c50ddad0ef9913bb5496cb9d509a50433a3b78cb0238d61105526589cf0e9 |
| tauri-runtime-wry | 2.12.3 | e1aa9d8b7a120f879f44fdc1053f7b7dbf416bf0bbc6a72b714dec46b782b52f |

Archives: `https://static.crates.io/crates/<package>/<package>-<version>.crate`.
Reviewable changes relative to those archives are in `patches/`. To reproduce,
verify the archive SHA-256, extract the three archives into an empty directory,
then apply each patch with `patch -p1 < <patch-file>` from that directory. Compare
the reconstructed crate directories against these vendored copies. The Cargo
registry's generated `.cargo-ok` marker is not source and is not included.

## Why this patch exists

Ratspeak retains its real Rust session when the last Android UI task is removed;
reopening must create only the replacement UI. The pinned upstream backend
otherwise exits that process or cannot safely reconstruct the window:

- Tao captures an exact requested Activity and retains a valid JVM raw-window
  handle through native construction and teardown.
- Tao uses `ALooper_pollOnce` in every control-flow mode and stops dispatching
  queued runtime work once an exit is accepted. Upstream already drains the queue
  after every poll result (the NDK `android/looper.h` contract prohibits relying
  on `pollAll` wakes), but still polls with `pollAll` and keeps dispatching after
  an accepted exit.
- Wry fences every queued message by Activity lifetime, retires handler maps
  synchronously on non-configuration destruction, and never waits for a protocol
  response while holding those global maps. Per-handler mutexes serialize every
  callback except protocol requests, which run concurrently as upstream does.
  Stale/missing targets and canceled replies do not panic.
- Configuration recreation preserves the logical window/lifetime and processes
  its saved WebView creation inline on Android's main thread, avoiding a send to
  that same thread's full bounded queue. A separate physical Activity epoch
  rejects an older queued creation and prevents duplicate creation. Queue
  capacity is unchanged.
- Each Java WebView receives an opaque callback key bound to immutable handler
  snapshots. IPC, protocol, load, title, navigation, eval, permission, and asset
  callbacks cannot use a replacement view's handlers through a reused logical
  label; its per-view WebChromeClient carries the same key. A retired view's
  permission requests are denied. Retirement does not wait for an in-flight
  protocol handler; a queued handler rechecks its authority after acquiring its
  serialization lock, and a protocol request rechecks before it starts.
- Rust handler maps are keyed by Activity ID, logical generation, and label.
  Late teardown cannot delete a replacement's handlers through the reused
  `main` label. Wry retirement completes before Tao wakes window-destruction
  handling, and initial navigation starts only after client/IPC installation.
  Missing mandatory request-handler state fails creation instead of marking a
  blank view ready or falling back to network access for local assets.
- Detached WebViews are removed and destroyed on Android's UI thread after
  callback retirement, including during configuration recreation. Pending eval
  callbacks are released when their native view retires.
- Copy JNI handles carry a monotonically allocated lifetime ID. (Upstream wry
  0.57.1 now reads the Activity ID only when the intent or saved state holds it.)
- A failed Java operation reports a content-free error and leaves the native
  queue available for a subsequent UI recreation.
- tauri-runtime-wry publishes the native window mapping only after both the
  window and its WebView were constructed. (Upstream 2.12 now waits for and
  returns the native construction result itself.)

On iOS, Tao reports the URLs and universal links in a scene's connection
options as `Opened`. Under the scene lifecycle a cold launch delivers its link
only there; upstream handles only `scene:openURLContexts:` and
`scene:continueUserActivity:`, which cover a running app. UIKit returns nil for
those option sets on a plain launch, so they are read as optional.

The application schedules creation on the Rust runtime event thread inside
`tao::platform::android::prelude::with_activity(captured_target, ...)`. Capture
that target on Android's main thread with `capture_activity(id)` before scheduling
creation: an integer ID alone can be reused by a later Activity. The app must not call
the builder from Android's main thread. Java WebView creation remains
asynchronous: native construction success is not proof of rendering. The existing
early `onWebViewCreate` setup hook is preserved; a separate `onWebViewReady` hook
runs only after client/IPC setup, content attachment, the native creation hook,
and exact-epoch proxy publication succeed. Ratspeak records only this completed
readiness and offers bounded failure/retry feedback without restarting its
protocol runtime.

Tao retains one original WindowManager reference until its logical Window is
dropped. That may retain its original Activity across configuration recreation;
there is no accumulating collection of old Activity references. Revisit this
tradeoff when upstream introduces an owned, refreshable raw-handle abstraction.

## Maintenance and release qualification

Treat upgrades as a backend compatibility change, not an automatic dependency
refresh. Rebase/remove each patch only after checking upstream lifecycle behavior
and rerunning retained-process, rapid destroy/reopen, configuration, notification
tap, explicit-stop/process-death, native failure, and foreground-state tests.
Both Cargo lockfiles must continue to identify these exact path packages.

Public source archives and deterministic source-set archives must include this
directory, its original licenses, and the patch files; there is no registry
checksum on a Cargo path package, so the source-set hash covers the patched
bytes. Never require users to modify their global Cargo registry cache.
