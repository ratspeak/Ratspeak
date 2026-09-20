# Dashboard checks

Run the frontend checks from the repository root with Node.js:

```sh
./dashboard/build-css.sh
for test in scripts/dashboard/test_*.js; do node "$test" || exit; done
```

These checks exercise the shipped dashboard controllers and CSS. They stay
outside `dashboard/` because Tauri embeds that directory into every app build.

`test_native_theme.js` exercises the production appearance controller against a
deterministic native-window/media-query model: System releases the override,
explicit modes stay explicit, rapid updates serialize, and failed saves can
return to System even when media events coalesce. It also protects the existing
Android and GTK paths. This does not replace a Windows/WebView2 OS-theme check.
