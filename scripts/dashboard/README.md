# Dashboard checks

Run the frontend checks from the repository root with Node.js:

```sh
./dashboard/build-css.sh
for test in scripts/dashboard/test_*.js; do node "$test" || exit; done
```

These checks exercise the shipped dashboard controllers and CSS. They stay
outside `dashboard/` because Tauri embeds that directory into every app build.

`test_attachment_staging.js` exercises the production picker and uploader with
real Blob slices and Base64, including full 256 KiB chunks, partial tails,
acknowledgements, failures and image preparation routing. Pair it with
`cargo test -p ratspeak-tauri --test attachment_staging --locked`, which runs
serialized Tauri commands against real private staging files and image codecs.
`test_attachment_replacement.js` covers ordered cleanup, rapid replacement,
late replies, chooser retirement, native shares, and detached send ownership.
`test_attachment_download_queue.js` runs the download and image hydration
controllers together, covering queued large images, byte fidelity, small-file
budgets, failure/retry, single-flight requests, and stale cache generations.
These host checks do not replace native picker and peer-to-peer delivery checks.

`test_native_theme.js` exercises the production appearance controller against a
deterministic native-window/media-query model: System releases the override,
explicit modes stay explicit, rapid updates serialize, and failed saves can
return to System even when media events coalesce. It also protects the existing
Android and GTK paths. This does not replace a Windows/WebView2 OS-theme check.
