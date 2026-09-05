# Dashboard checks

Run the frontend checks from the repository root with Node.js:

```sh
./dashboard/build-css.sh
for test in scripts/dashboard/test_*.js; do node "$test" || exit; done
```

These checks exercise the shipped dashboard controllers and CSS. They stay
outside `dashboard/` because Tauri embeds that directory into every app build.
