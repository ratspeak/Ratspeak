import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  realpathSync, rmSync, symlinkSync, writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createSdkView, sdkViewEnvironment, verifySdkView } from "./android-sdk-view.mjs";

const pinned = "27.0.12077973";
const newer = "29.0.13846066";
const script = fileURLToPath(new URL("./android-sdk-view.mjs", import.meta.url));
const host = process.platform === "darwin" ? "darwin-x86_64" : "linux-x86_64";

function addNdk(sdk, revision) {
  const ndk = join(sdk, "ndk", revision);
  const bin = join(ndk, "toolchains", "llvm", "prebuilt", host, "bin");
  mkdirSync(bin, { recursive: true });
  writeFileSync(join(ndk, "source.properties"), `Pkg.Desc = Android NDK\nPkg.Revision = ${revision}\n`);
  const compiler = join(bin, "clang");
  writeFileSync(compiler, `#!/bin/sh\nprintf '%s\\n' 'Android ${revision} clang version 18.0.1'\n`);
  chmodSync(compiler, 0o755);
  return ndk;
}

function fixture(t) {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), "ratspeak-sdk-view-test-")));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const sdk = join(directory, "installed SDK with spaces");
  const parent = join(directory, "job temporary files");
  mkdirSync(parent);
  mkdirSync(join(sdk, "platform-tools"), { recursive: true });
  writeFileSync(join(sdk, "platform-tools", "adb"), "installed SDK component\n");
  writeFileSync(join(sdk, "packages.xml"), "installed metadata\n");
  addNdk(sdk, pinned);
  addNdk(sdk, newer);
  symlinkSync(join(sdk, "ndk", newer), join(sdk, "ndk-bundle"), "dir");
  return { directory, sdk, parent, revision: pinned };
}

test("isolated view selects the pinned compiler while preserving newer installed NDKs", (t) => {
  const input = fixture(t);
  const observed = createSdkView(input);
  assert.deepEqual(readdirSync(join(observed.view, "ndk")), [pinned]);
  assert.deepEqual(readdirSync(join(input.sdk, "ndk")).sort(), [pinned, newer]);
  assert.equal(existsSync(join(observed.view, "ndk-bundle")), false);
  assert.equal(realpathSync(join(observed.view, "platform-tools")), join(input.sdk, "platform-tools"));
  assert.equal(observed.ndk, join(input.sdk, "ndk", pinned));
  assert.match(observed.compilerVersion, /27\.0\.12077973 clang version/);
  writeFileSync(join(observed.view, "packages.xml"), "view-only metadata\n");
  assert.equal(readFileSync(join(input.sdk, "packages.xml"), "utf8"), "installed metadata\n");
  assert.doesNotThrow(() => verifySdkView({
    view: observed.view, revision: pinned,
    environment: sdkViewEnvironment(observed.view, pinned),
    buildLog: `\u001b[32mInfo\u001b[0m Using installed NDK: ${join(observed.view, "ndk", pinned)}\n`,
  }));
});

test("repeated jobs get distinct views without overwriting an existing path", (t) => {
  const input = fixture(t);
  const first = createSdkView(input);
  const second = createSdkView(input);
  assert.notEqual(first.view, second.view);
  assert.doesNotThrow(() => verifySdkView({ view: first.view, revision: pinned }));
  assert.doesNotThrow(() => verifySdkView({ view: second.view, revision: pinned }));
  assert.equal(readdirSync(input.parent).length, 2);
});

test("missing, misleading and ambiguous pinned revisions fail before creating a view", (t) => {
  const input = fixture(t);
  assert.throws(() => createSdkView({ ...input, revision: "28.0.0" }), /ENOENT/);
  assert.throws(() => createSdkView({ ...input, revision: "../ndk-bundle" }), /numeric three-part/);
  const properties = join(input.sdk, "ndk", pinned, "source.properties");
  writeFileSync(properties, `Pkg.Revision = ${newer}\n`);
  assert.throws(() => createSdkView(input), /must declare exactly/);
  writeFileSync(properties, `Pkg.Revision = ${pinned}\nPkg.Revision = ${pinned}\n`);
  assert.throws(() => createSdkView(input), /must declare exactly/);
  assert.deepEqual(readdirSync(input.parent), []);
});

test("view cannot be created inside the installed SDK or inject environment lines", (t) => {
  const input = fixture(t);
  assert.throws(() => createSdkView({ ...input, parent: input.sdk }), /outside the installed SDK/);
  assert.throws(() => createSdkView({ ...input, parent: `${input.parent}\nUNRELATED=value` }), /single lines/);
  assert.throws(() => sdkViewEnvironment(`${input.sdk}\nUNRELATED=value`, pinned), /single lines/);
  assert.deepEqual(readdirSync(input.parent), []);
});

test("environment drift and a Tauri log that selected another NDK are rejected", (t) => {
  const input = fixture(t);
  const { view } = createSdkView(input);
  const environment = sdkViewEnvironment(view, pinned);
  for (const name of ["ANDROID_HOME", "ANDROID_SDK_ROOT", "ANDROID_NDK_HOME", "NDK_HOME"]) {
    assert.throws(() => verifySdkView({
      view, revision: pinned, environment: { ...environment, [name]: input.sdk },
    }), new RegExp(`${name} must point`));
  }
  assert.throws(() => verifySdkView({ view, revision: pinned, buildLog: "Build succeeded\n" }), /does not prove/);
  assert.throws(() => verifySdkView({
    view, revision: pinned,
    buildLog: `Using installed NDK: ${join(view, "ndk", pinned)}\nUsing installed NDK: ${join(input.sdk, "ndk", newer)}\n`,
  }), /outside the pinned SDK view/);
});

test("an extra NDK installed into the disposable view is rejected without deleting it", (t) => {
  const input = fixture(t);
  const { view } = createSdkView(input);
  symlinkSync(join(input.sdk, "ndk", newer), join(view, "ndk", newer), "dir");
  assert.throws(() => verifySdkView({ view, revision: pinned }), /expose only NDK/);
  assert.equal(existsSync(join(view, "ndk", newer)), true);
  assert.equal(existsSync(join(input.sdk, "ndk", newer)), true);
});

test("a compiler redirected outside the pinned NDK is rejected", (t) => {
  const input = fixture(t);
  const compiler = join(input.sdk, "ndk", pinned, "toolchains", "llvm", "prebuilt", host, "bin", "clang");
  rmSync(compiler);
  symlinkSync(join(input.sdk, "ndk", newer, "toolchains", "llvm", "prebuilt", host, "bin", "clang"), compiler);
  assert.throws(() => createSdkView(input), /compiler resolves outside/);
  assert.deepEqual(readdirSync(input.parent), []);
});

test("CLI exports paths with spaces and verifies them in the following job environment", (t) => {
  const input = fixture(t);
  const environmentFile = join(input.directory, "github env");
  writeFileSync(environmentFile, "EXISTING=value\n");
  const result = JSON.parse(execFileSync(process.execPath, [
    script, "create", "--sdk", input.sdk, "--ndk", pinned,
    "--parent", input.parent, "--github-env", environmentFile,
  ], { encoding: "utf8" }));
  const env = { ...process.env };
  for (const line of readFileSync(environmentFile, "utf8").trim().split("\n")) {
    const equals = line.indexOf("=");
    env[line.slice(0, equals)] = line.slice(equals + 1);
  }
  assert.equal(env.EXISTING, "value");
  assert.equal(env.ANDROID_HOME, result.view);
  const buildLog = join(input.directory, "build output.log");
  writeFileSync(buildLog, `Info Using installed NDK: ${env.NDK_HOME}\n`);
  const verified = JSON.parse(execFileSync(process.execPath, [
    script, "verify", "--view", result.view, "--ndk", pinned, "--build-log", buildLog,
  ], { env, encoding: "utf8" }));
  assert.deepEqual(verified.tauriSelections, [env.NDK_HOME]);
  const invalid = spawnSync(process.execPath, [
    script, "verify", "--view", result.view, "--ndk", pinned, "--ndk", newer,
  ], { env, encoding: "utf8" });
  assert.notEqual(invalid.status, 0);
  assert.match(invalid.stderr, /unique --option/);
});

test("release workflow verifies actual Tauri selection for APK and AAB and retains the view for lint", () => {
  const workflow = readFileSync(join(dirname(script), "../../.github/workflows/release-android.yml"), "utf8");
  const create = workflow.indexOf("- name: Isolate pinned Android NDK");
  const apk = workflow.indexOf("- name: Build signed Android APKs");
  const lint = workflow.indexOf("- name: Lint Android release variant");
  const aab = workflow.indexOf("- name: Build signed Android AAB");
  assert.ok(create > 0 && create < apk && apk < lint && lint < aab);
  for (const [start, end, kind] of [[apk, lint, "apk"], [aab, workflow.indexOf("- name: Validate Android JNI"), "aab"]]) {
    const step = workflow.slice(start, end);
    assert.match(step, /shell: bash/); // GitHub uses bash -e -o pipefail.
    assert.match(step, new RegExp(`tee "\\$RUNNER_TEMP/ratspeak-android-${kind}-build\\.log"`));
    assert.match(step, new RegExp(`--build-log "\\$RUNNER_TEMP/ratspeak-android-${kind}-build\\.log"`));
  }
  assert.match(workflow.slice(lint, aab), /android-sdk-view\.mjs verify/);
  assert.doesNotMatch(workflow, /rm\s+[^\n]*(?:RATSPEAK_ANDROID_SDK_VIEW|ANDROID_HOME|ANDROID_SDK_ROOT)/);
});

test("Android CI and signed packaging do not request the removed legacy SDK tools package", () => {
  for (const name of ["ci.yml", "release-android.yml"]) {
    const workflow = readFileSync(join(dirname(script), "../../.github/workflows", name), "utf8");
    const setupSteps = workflow.split(/(?=^      - )/m)
      .filter((entry) => /uses: android-actions\/setup-android@/.test(entry));
    assert.equal(setupSteps.length, 1, `${name}: expected one SDK setup step`);
    assert.match(setupSteps[0], /^        with:\n(?:          #[^\n]*\n)*          packages: platform-tools$/m);
  }
});

test("both Android CI toolchains use the reviewed SDK view while iOS skips Android setup", () => {
  const workflow = readFileSync(join(dirname(script), "../../.github/workflows/ci.yml"), "utf8");
  const mobile = workflow.slice(workflow.indexOf("\n  mobile-rust-lint:"));
  const [matrix, stepsText] = mobile.split("    steps:\n");
  const steps = stepsText.split(/(?=^      - )/m);
  const step = (selector) => {
    const matches = steps.filter((entry) => selector.test(entry.split("\n", 1)[0]));
    assert.equal(matches.length, 1, `expected one mobile step for ${selector}`);
    return matches[0];
  };
  const androidRows = matrix.split(/(?=^          - os:)/m)
    .filter((entry) => /target: aarch64-linux-android/.test(entry));
  assert.equal(androidRows.length, 2);
  assert.ok(androidRows.some((entry) => /toolchain: stable/.test(entry)));
  assert.ok(androidRows.some((entry) => /toolchain: 1\.87\.0/.test(entry)));
  for (const selector of [
    /uses: actions\/setup-node@/,
    /name: Load reviewed mobile toolchains/,
    /uses: actions\/setup-java@/,
    /uses: android-actions\/setup-android@/,
    /name: Install and isolate reviewed Android NDK/,
    /name: Clippy Android/,
  ]) {
    assert.match(step(selector), /^        if: matrix\.target == 'aarch64-linux-android'$/m);
  }
  const install = step(/name: Install and isolate reviewed Android NDK/);
  assert.match(install, /PINNED_ANDROID_NDK: \$\{\{ steps\.mobile-source\.outputs\.android_ndk \}\}/);
  assert.match(install, /sdkmanager "ndk;\$PINNED_ANDROID_NDK"/);
  assert.match(install, /android-sdk-view\.mjs create/);
  assert.match(install, /--github-env "\$GITHUB_ENV"/);
  const clippy = step(/name: Clippy Android/);
  assert.match(clippy, /android-sdk-view\.mjs verify/);
  for (const tool of ["AR", "CC", "CXX"]) {
    assert.match(clippy, new RegExp(`export ${tool}_aarch64_linux_android="\\$ANDROID_NDK_HOME/`));
  }
  const build = step(/name: Build minified Android APK and run release lint/);
  // Explicit bash enables pipefail: a failed cargo build cannot be hidden by
  // a successful tee and stale APK files on a cached runner.
  assert.match(build, /^        shell: bash$/m);
  assert.match(build, /tee "\$RUNNER_TEMP\/ratspeak-android-ci-build\.log"/);
  assert.match(build, /--build-log "\$RUNNER_TEMP\/ratspeak-android-ci-build\.log"/);
  assert.ok(build.indexOf("--build-log") < build.indexOf(":app:lintArm64Release"));
  assert.doesNotMatch(mobile, /setup-ndk|r27d|steps\.setup-ndk/);
  const ios = step(/name: Clippy iOS simulator/);
  assert.match(ios, /if: matrix\.target == 'aarch64-apple-ios-sim'/);
  assert.doesNotMatch(ios, /ANDROID|NDK|android-sdk-view/);
});
