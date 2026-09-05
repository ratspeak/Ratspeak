import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const android = path.join(root, "vendor/wry-0.55.1/src/android");
const generated = path.join(root, "src-tauri/gen/android/app/src/main/java/org/ratspeak/android/generated");

test("native callback authority passes portable Rust retirement and replacement regressions", () => {
  const directory = mkdtempSync(path.join(tmpdir(), "ratspeak-native-callbacks-"));
  try {
    const source = path.join(directory, "callback-tests.rs");
    const binary = path.join(directory, process.platform === "win32" ? "callback-tests.exe" : "callback-tests");
    writeFileSync(source, `#[path = ${JSON.stringify(path.join(android, "callback_registry.rs"))}]\nmod callback_registry;\n#[path = ${JSON.stringify(path.join(android, "native_creation.rs"))}]\nmod native_creation;\n#[path = ${JSON.stringify(path.join(android, "handler_key.rs"))}]\nmod handler_key;\n#[path = ${JSON.stringify(path.join(root, "vendor/tao-0.35.3/src/platform_impl/android/event_queue.rs"))}]\nmod event_queue;\n`);
    const compile = spawnSync("rustc", ["--edition=2021", "--test", source, "-o", binary], {
      encoding: "utf8", timeout: 30_000, maxBuffer: 1024 * 1024,
    });
    assert.equal(compile.error, undefined, compile.error?.message);
    assert.equal(compile.status, 0, compile.stderr);
    const result = spawnSync(binary, [], { encoding: "utf8", timeout: 10_000, maxBuffer: 1024 * 1024 });
    assert.equal(result.error, undefined, result.error?.message);
    assert.equal(result.status, 0, result.stdout + result.stderr);
    assert.match(result.stdout, /15 passed; 0 failed/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("Android runtime drains coalesced wakes after every native poll result", () => {
  const source = readFileSync(path.join(root, "vendor/tao-0.35.3/src/platform_impl/android/mod.rs"), "utf8");
  assert.doesNotMatch(source, /\.poll_all(?:_timeout)?\(/);
  assert.match(source, /Poll::Callback => Some\(EventSource::User\)/);
  assert.match(source, /dispatch_pending\(\s*!matches!\(control_flow, ControlFlow::ExitWithCode\(_\)\)/);
  assert.match(source, /Some\(EventSource::User\) \| None => \{\}\s*\}\s*\/\/[^]*?event_queue::dispatch_pending\(/);
  const drain = source.indexOf("event_queue::dispatch_pending(");
  assert.ok(drain > source.indexOf("match self.first_event.take()"));
  assert.ok(drain < source.indexOf("event::Event::MainEventsCleared"));
});

test("completed readiness follows publication and physical creation is fenced", () => {
  const pipe = readFileSync(path.join(android, "main_pipe.rs"), "utf8");
  const activity = readFileSync(path.join(android, "kotlin/WryActivity.kt"), "utf8");
  assert.ok(pipe.indexOf('"setContentView"') < pipe.indexOf('"onWebViewReady"'));
  assert.ok(pipe.indexOf("creation.complete(native_generation)") < pipe.indexOf('"onWebViewReady"'));
  assert.match(pipe, /creation\s*\.begin\(native_generation\)/);
  assert.match(activity, /open fun onWebViewReady\(webView: WebView\)/);
  assert.ok(activity.indexOf("Rust.onWebviewDestroy") < activity.indexOf("mWebView.destroy()"));
  assert.ok(activity.indexOf("Rust.onWebviewDestroy(this") < activity.indexOf("Rust.onActivityDestroy(this"));
  const navigation = pipe.indexOf('phase = "navigation"');
  assert.ok(navigation > pipe.indexOf('"setWebViewClient"'));
  assert.ok(navigation > pipe.indexOf('"addJavascriptInterface"'));
  assert.ok(navigation < pipe.indexOf('"onWebViewReady"'));
});

test("every Java callback uses the native-view key in templates and generated sources", () => {
  const callback = /Rust\.(?:handleRequest|withAssetLoader|assetLoaderDomain|shouldOverride|onEval|onPageLoading|onPageLoaded|ipc|handleReceivedTitle)\([^\n]+/g;
  let count = 0;
  for (const filename of ["RustWebView.kt", "RustWebViewClient.kt", "RustWebChromeClient.kt", "Ipc.kt"]) {
    const template = readFileSync(path.join(android, "kotlin", filename), "utf8");
    const actual = readFileSync(path.join(generated, filename), "utf8");
    const expectedCalls = [...template.matchAll(callback)].map(match => match[0]);
    const generatedCalls = [...actual.matchAll(callback)].map(match => match[0]);
    assert.deepEqual(generatedCalls, expectedCalls, `${filename}: generated JNI callback arguments drifted`);
    for (const call of expectedCalls) {
      assert.match(call, /callbackKey/, `${filename}: callback routed by a reusable logical label`);
      assert.doesNotMatch(call, /(?:webView|view|this)\.id\b/);
    }
    count += expectedCalls.length;
  }
  assert.equal(count, 11);
  const webview = readFileSync(path.join(android, "kotlin/RustWebView.kt"), "utf8");
  assert.match(webview, /val id: String, val callbackKey: String/);
});

test("JNI dispatch resolves immutable snapshots without re-reading logical handler maps", () => {
  const binding = readFileSync(path.join(android, "binding.rs"), "utf8");
  const snapshots = readFileSync(path.join(android, "callbacks.rs"), "utf8");
  assert.doesNotMatch(binding, /\b(?:REQUEST_HANDLER|IPC|TITLE_CHANGE_HANDLER|URL_LOADING_OVERRIDE|ON_LOAD_HANDLER|WITH_ASSET_LOADER|ASSET_LOADER_DOMAIN)\b/);
  assert.match(binding, /&callbacks\.logical_id/); // Tauri protocol handlers still receive their public label.
  for (const field of ["request", "ipc", "title", "navigation", "load", "with_asset_loader", "asset_loader_domain"]) {
    assert.ok(new RegExp(`callbacks\\s*\\.${field}\\b`).test(binding), `missing immutable ${field} snapshot dispatch`);
  }
  assert.match(binding, /retire_activity_view\(activity_id\)/);
  assert.match(snapshots, /key: &HandlerKey\) -> Option<String>/);
  assert.match(snapshots, /key\.activity_id != activity_id/);
  assert.match(snapshots, /REQUEST_HANDLER\.lock\(\)\.unwrap\(\)\.get\(key\)\.cloned\(\)\?/);
  assert.match(snapshots, /logical_id: key\.label\.clone\(\)/);
  assert.doesNotMatch(snapshots, /\.get\(logical_id\)/);
});
