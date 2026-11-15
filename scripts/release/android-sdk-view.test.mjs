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
import { createSdkView, installNdk, sdkViewEnvironment, verifySdkView } from "./android-sdk-view.mjs";

const pinned = "27.0.12077973";
const newer = "29.0.13846066";
const script = fileURLToPath(new URL("./android-sdk-view.mjs", import.meta.url));
const host = process.platform === "darwin" ? "darwin-x86_64" : "linux-x86_64";
// android-sdk-view.mjs serves macOS and Linux x86_64 build hosts only; the workflow checks run anywhere.
const hostTest = process.platform === "darwin" || (process.platform === "linux" && process.arch === "x64")
  ? test : test.skip;

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

function fakeSdkmanager(input, operation) {
  const executable = join(input.directory, "sdkmanager");
  const log = join(input.directory, "sdkmanager-attempts.jsonl");
  writeFileSync(executable, `#!${process.execPath}
const { appendFileSync, chmodSync, existsSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } = require("node:fs");
const { join } = require("node:path");
const assert = require("node:assert/strict");
const host = ${JSON.stringify(host)};
const revision = ${JSON.stringify(pinned)};
const log = ${JSON.stringify(log)};
const args = process.argv.slice(2);
assert.deepEqual(args, [${JSON.stringify(`--sdk_root=${input.sdk}`)}, "ndk;" + revision]);
assert.equal(readFileSync(0, "utf8"), "y\\n".repeat(128));
const sdk = args[0].slice("--sdk_root=".length);
const attempt = existsSync(log) ? readFileSync(log, "utf8").trim().split("\\n").length + 1 : 1;
appendFileSync(log, JSON.stringify(args) + "\\n");
const ndk = join(sdk, "ndk", revision);
${addNdk.toString()}
${operation}
`);
  chmodSync(executable, 0o755);
  return { sdkmanager: executable, log };
}

hostTest("installer discards only a corrupt pinned resume operation before a fresh retry", async (t) => {
  const input = fixture(t);
  const ndk = join(input.sdk, "ndk", pinned);
  rmSync(ndk, { recursive: true });
  mkdirSync(join(ndk, ".installer"), { recursive: true });
  writeFileSync(join(ndk, ".installer", "operation"), "corrupt resumable download\n");
  mkdirSync(join(input.sdk, ".temp"));
  writeFileSync(join(input.sdk, ".temp", "other-download"), "unrelated cache\n");
  const otherProperties = readFileSync(join(input.sdk, "ndk", newer, "source.properties"), "utf8");
  const { sdkmanager, log } = fakeSdkmanager(input, `
if (attempt === 1) {
  assert.ok(existsSync(join(ndk, ".installer", "operation")));
  process.exit(1);
}
assert.equal(existsSync(ndk), false, "failed operation must not be resumed");
addNdk(sdk, revision);
`);
  const alias = join(input.directory, "SDK alias");
  symlinkSync(input.sdk, alias, "dir");
  const result = await installNdk({ ...input, sdk: alias, sdkmanager });
  assert.equal(result.attempts, 2);
  assert.equal(readFileSync(log, "utf8").trim().split("\n").length, 2);
  assert.equal(existsSync(join(ndk, ".installer")), false);
  assert.equal(readFileSync(join(input.sdk, "packages.xml"), "utf8"), "installed metadata\n");
  assert.equal(readFileSync(join(input.sdk, ".temp", "other-download"), "utf8"), "unrelated cache\n");
  assert.equal(readFileSync(join(input.sdk, "ndk", newer, "source.properties"), "utf8"), otherProperties);
  assert.doesNotThrow(() => createSdkView(input));
});

hostTest("installer stops after three failed attempts without deleting other revisions", async (t) => {
  const input = fixture(t);
  rmSync(join(input.sdk, "ndk", pinned), { recursive: true });
  const { sdkmanager, log } = fakeSdkmanager(input, `
assert.equal(existsSync(ndk), false);
mkdirSync(join(ndk, ".installer"), { recursive: true });
writeFileSync(join(ndk, ".installer", "operation"), "corrupt download");
process.exit(1);
`);
  await assert.rejects(installNdk({ ...input, sdkmanager }), /after 3 attempts/);
  assert.equal(readFileSync(log, "utf8").trim().split("\n").length, 3);
  assert.equal(existsSync(join(input.sdk, "ndk", newer, "source.properties")), true);
  assert.equal(readFileSync(join(input.sdk, "packages.xml"), "utf8"), "installed metadata\n");
  assert.deepEqual(readdirSync(input.parent), []);
});

hostTest("sdkmanager success with an invalid compiler or revision still fails installation", async (t) => {
  const input = fixture(t);
  rmSync(join(input.sdk, "ndk", pinned), { recursive: true });
  const { sdkmanager, log } = fakeSdkmanager(input, `
assert.equal(existsSync(ndk), false);
mkdirSync(ndk);
writeFileSync(join(ndk, "source.properties"), "Pkg.Revision = 0.0.0\\n");
`);
  await assert.rejects(installNdk({ ...input, sdkmanager }), /after 3 attempts:.*must declare exactly/);
  assert.equal(readFileSync(log, "utf8").trim().split("\n").length, 3);
  assert.deepEqual(readdirSync(input.parent), []);
});

hostTest("installer also rejects a declared pinned revision without its compiler", async (t) => {
  const input = fixture(t);
  rmSync(join(input.sdk, "ndk", pinned), { recursive: true });
  const { sdkmanager, log } = fakeSdkmanager(input, `
assert.equal(existsSync(ndk), false);
mkdirSync(ndk);
writeFileSync(join(ndk, "source.properties"), "Pkg.Revision = " + revision + "\\n");
`);
  await assert.rejects(installNdk({ ...input, sdkmanager }), /after 3 attempts:.*ENOENT/);
  assert.equal(readFileSync(log, "utf8").trim().split("\n").length, 3);
  assert.deepEqual(readdirSync(input.parent), []);
});

hostTest("a valid pinned installation survives a failed sdkmanager attempt", async (t) => {
  const input = fixture(t);
  const sentinel = join(input.sdk, "ndk", pinned, "preserve-valid-package");
  writeFileSync(sentinel, "keep this installation\n");
  const { sdkmanager } = fakeSdkmanager(input, `
assert.equal(readFileSync(join(ndk, "preserve-valid-package"), "utf8"), "keep this installation\\n");
if (attempt === 1) process.exit(1);
`);
  assert.equal((await installNdk({ ...input, sdkmanager })).attempts, 2);
  assert.equal(readFileSync(sentinel, "utf8"), "keep this installation\n");
});

hostTest("installer rejects symlink parents and rechecks child paths after sdkmanager", async (t) => {
  const parentInput = fixture(t);
  const { sdkmanager: parentManager, log: parentLog } = fakeSdkmanager(parentInput, "process.exit(0);");
  rmSync(join(parentInput.sdk, "ndk"), { recursive: true });
  const outside = join(parentInput.directory, "outside ndk");
  mkdirSync(outside);
  writeFileSync(join(outside, "sentinel"), "must not delete\n");
  symlinkSync(outside, join(parentInput.sdk, "ndk"), "dir");
  await assert.rejects(installNdk({ ...parentInput, sdkmanager: parentManager }), /non-symlink NDK directory/);
  assert.equal(existsSync(parentLog), false);
  assert.equal(readFileSync(join(outside, "sentinel"), "utf8"), "must not delete\n");

  const childInput = fixture(t);
  const { sdkmanager: childManager, log: childLog } = fakeSdkmanager(childInput, `
rmSync(ndk, { recursive: true });
symlinkSync(join(sdk, "ndk", ${JSON.stringify(newer)}), ndk, "dir");
process.exit(1);
`);
  await assert.rejects(installNdk({ ...childInput, sdkmanager: childManager }), /pinned NDK must be a non-symlink/);
  assert.equal(readFileSync(childLog, "utf8").trim().split("\n").length, 1);
  assert.equal(existsSync(join(childInput.sdk, "ndk", newer, "source.properties")), true);
});

hostTest("process-start errors and signals are failures even when a valid NDK exists", async (t) => {
  const input = fixture(t);
  await assert.rejects(installNdk({ ...input, sdkmanager: join(input.directory, "missing-sdkmanager") }), /after 3 attempts:.*ENOENT/);
  const { sdkmanager, log } = fakeSdkmanager(input, "process.kill(process.pid, 'SIGTERM');");
  await assert.rejects(installNdk({ ...input, sdkmanager }), /after 3 attempts/);
  assert.equal(readFileSync(log, "utf8").trim().split("\n").length, 3);
  assert.equal(existsSync(join(input.sdk, "ndk", pinned, "source.properties")), true);
});

hostTest("install CLI validates the package with an explicit SDK root before view creation", (t) => {
  const input = fixture(t);
  fakeSdkmanager(input, "assert.ok(existsSync(ndk));");
  const installed = JSON.parse(execFileSync(process.execPath, [
    script, "install", "--sdk", input.sdk, "--ndk", pinned,
  ], { env: { ...process.env, PATH: `${input.directory}:${process.env.PATH}` }, encoding: "utf8" }));
  assert.equal(installed.sourceSdk, input.sdk);
  assert.equal(installed.attempts, 1);
  assert.equal(installed.revision, pinned);
  assert.doesNotThrow(() => createSdkView(input));
});

hostTest("isolated view selects the pinned compiler while preserving newer installed NDKs", (t) => {
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

hostTest("repeated jobs get distinct views without overwriting an existing path", (t) => {
  const input = fixture(t);
  const first = createSdkView(input);
  const second = createSdkView(input);
  assert.notEqual(first.view, second.view);
  assert.doesNotThrow(() => verifySdkView({ view: first.view, revision: pinned }));
  assert.doesNotThrow(() => verifySdkView({ view: second.view, revision: pinned }));
  assert.equal(readdirSync(input.parent).length, 2);
});

hostTest("missing, misleading and ambiguous pinned revisions fail before creating a view", (t) => {
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

hostTest("view cannot be created inside the installed SDK or inject environment lines", (t) => {
  const input = fixture(t);
  assert.throws(() => createSdkView({ ...input, parent: input.sdk }), /outside the installed SDK/);
  assert.throws(() => createSdkView({ ...input, parent: `${input.parent}\nUNRELATED=value` }), /single lines/);
  assert.throws(() => sdkViewEnvironment(`${input.sdk}\nUNRELATED=value`, pinned), /single lines/);
  assert.deepEqual(readdirSync(input.parent), []);
});

hostTest("environment drift and a Tauri log that selected another NDK are rejected", (t) => {
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

hostTest("an extra NDK installed into the disposable view is rejected without deleting it", (t) => {
  const input = fixture(t);
  const { view } = createSdkView(input);
  symlinkSync(join(input.sdk, "ndk", newer), join(view, "ndk", newer), "dir");
  assert.throws(() => verifySdkView({ view, revision: pinned }), /expose only NDK/);
  assert.equal(existsSync(join(view, "ndk", newer)), true);
  assert.equal(existsSync(join(input.sdk, "ndk", newer)), true);
});

hostTest("a compiler redirected outside the pinned NDK is rejected", (t) => {
  const input = fixture(t);
  const compiler = join(input.sdk, "ndk", pinned, "toolchains", "llvm", "prebuilt", host, "bin", "clang");
  rmSync(compiler);
  symlinkSync(join(input.sdk, "ndk", newer, "toolchains", "llvm", "prebuilt", host, "bin", "clang"), compiler);
  assert.throws(() => createSdkView(input), /compiler resolves outside/);
  assert.deepEqual(readdirSync(input.parent), []);
});

hostTest("CLI exports paths with spaces and verifies them in the following job environment", (t) => {
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
  const { toolchains } = JSON.parse(
    readFileSync(join(dirname(script), "../../release/dependency-set.json"), "utf8"));
  assert.ok(androidRows.some((entry) => entry.includes(`toolchain: ${toolchains.rustRelease}\n`)));
  assert.ok(androidRows.some((entry) => entry.includes(`toolchain: ${toolchains.rustMsrv}\n`)));
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
  assert.match(install, /android-sdk-view\.mjs install/);
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

test("both workflows use the shared bounded installer before exposing the SDK view", () => {
  for (const name of ["ci.yml", "release-android.yml"]) {
    const workflow = readFileSync(join(dirname(script), "../../.github/workflows", name), "utf8");
    const install = workflow.indexOf("android-sdk-view.mjs install");
    const create = workflow.indexOf("android-sdk-view.mjs create");
    assert.ok(install > 0 && install < create, `${name}: install must complete before creating a view`);
    const step = workflow.slice(workflow.lastIndexOf("      - ", install), install + 160);
    assert.match(step, /working-directory: Ratspeak/);
    assert.match(step, /PINNED_ANDROID_NDK: \$\{\{ steps\.(?:source|mobile-source)\.outputs\.android_ndk \}\}/);
    assert.match(step, /--sdk "\$ANDROID_HOME" --ndk "\$PINNED_ANDROID_NDK"/);
    assert.doesNotMatch(workflow, /yes\s*\|\s*sdkmanager|sdkmanager_status|rm[^\n]*(?:\.temp|\.android\/cache|packages\.xml)/);
  }
});
