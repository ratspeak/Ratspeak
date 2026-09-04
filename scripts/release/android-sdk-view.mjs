#!/usr/bin/env node

// Tauri CLI 2.11.1 selects the last entry in ANDROID_HOME/ndk, even when
// NDK_HOME is already set. Keep the installed SDK intact and expose only the
// reviewed NDK for the entire build job, including later Gradle/JNI consumers.
import { execFileSync } from "node:child_process";
import {
  appendFileSync, copyFileSync, lstatSync, mkdirSync, mkdtempSync, readFileSync,
  readdirSync, realpathSync, rmSync, statSync, symlinkSync, writeFileSync,
} from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const receiptName = ".ratspeak-android-sdk-view.json";
const environmentNames = ["ANDROID_HOME", "ANDROID_SDK_ROOT", "ANDROID_NDK_HOME", "NDK_HOME"];

function requireRevision(revision) {
  if (!/^\d+\.\d+\.\d+$/.test(revision ?? "")) {
    throw new Error("The pinned NDK must be a numeric three-part revision");
  }
}

function requireSingleLine(value) {
  if (typeof value !== "string" || value.length === 0 || /[\r\n\0]/.test(value)) {
    throw new Error("SDK paths and environment values must be non-empty single lines");
  }
  return value;
}

function isWithin(parent, child) {
  const path = relative(parent, child);
  return path === "" || (!isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`));
}

function hostTag() {
  if (process.platform === "darwin") return "darwin-x86_64";
  if (process.platform === "linux" && process.arch === "x64") return "linux-x86_64";
  throw new Error("Android release SDK views support macOS and Linux x86_64 build hosts");
}

function inspectNdk(ndkPath, revision) {
  requireRevision(revision);
  const ndk = realpathSync(ndkPath);
  if (!statSync(ndk).isDirectory()) throw new Error("Pinned NDK is not a directory");
  const properties = readFileSync(join(ndk, "source.properties"), "utf8");
  const revisions = [...properties.matchAll(/^Pkg\.Revision\s*=\s*(\S+)\s*$/gm)];
  if (revisions.length !== 1 || revisions[0][1] !== revision) {
    throw new Error(`Pinned NDK source.properties must declare exactly ${revision}`);
  }
  const compiler = join(ndkPath, "toolchains", "llvm", "prebuilt", hostTag(), "bin", "clang");
  const compilerRealPath = realpathSync(compiler);
  if (!isWithin(ndk, compilerRealPath)) {
    throw new Error("Pinned NDK compiler resolves outside its NDK directory");
  }
  const compilerVersion = execFileSync(compiler, ["--version"], {
    encoding: "utf8", timeout: 15_000, maxBuffer: 64 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  }).trim().split(/\r?\n/)[0];
  if (!/clang version\s+\S+/.test(compilerVersion)) {
    throw new Error("Pinned NDK compiler did not report a Clang version");
  }
  return { ndk, revision, compiler, compilerRealPath, compilerVersion };
}

export function sdkViewEnvironment(view, revision) {
  requireRevision(revision);
  requireSingleLine(view);
  const ndk = join(view, "ndk", revision);
  return {
    ANDROID_HOME: view,
    ANDROID_SDK_ROOT: view,
    ANDROID_NDK_HOME: ndk,
    NDK_HOME: ndk,
    RATSPEAK_ANDROID_SDK_VIEW: view,
  };
}

export function createSdkView({ sdk, revision, parent }) {
  requireRevision(revision);
  const sourceSdk = realpathSync(requireSingleLine(sdk));
  const viewParent = realpathSync(requireSingleLine(parent));
  if (isWithin(sourceSdk, viewParent)) {
    throw new Error("The disposable SDK view must be outside the installed SDK");
  }
  const sourceNdk = inspectNdk(join(sourceSdk, "ndk", revision), revision);
  const view = mkdtempSync(join(viewParent, "ratspeak-android-sdk-"));
  try {
    // SDK components are shared, but metadata files and temporary directories
    // belong to the view. In particular, never expose the entire ndk directory
    // or the legacy ndk-bundle alias to tools selecting their own NDK.
    for (const entry of readdirSync(sourceSdk)) {
      if (entry.startsWith(".") || entry === "ndk" || entry === "ndk-bundle") continue;
      const source = join(sourceSdk, entry);
      const destination = join(view, entry);
      if (statSync(source).isDirectory()) symlinkSync(source, destination, "dir");
      else copyFileSync(source, destination);
    }
    mkdirSync(join(view, "ndk"));
    symlinkSync(sourceNdk.ndk, join(view, "ndk", revision), "dir");
    writeFileSync(join(view, receiptName), `${JSON.stringify({
      schemaVersion: 1, sourceSdk, revision, sourceNdk: sourceNdk.ndk,
    }, null, 2)}\n`, { flag: "wx" });
    return verifySdkView({ view, revision });
  } catch (error) {
    // This exact path was exclusively created by mkdtemp above. Never delete
    // the installed SDK or a caller-selected existing directory.
    rmSync(view, { recursive: true, force: true });
    throw error;
  }
}

export function verifySdkView({ view, revision, environment, buildLog }) {
  requireRevision(revision);
  const viewPath = realpathSync(requireSingleLine(view));
  const receipt = JSON.parse(readFileSync(join(viewPath, receiptName), "utf8"));
  if (receipt.schemaVersion !== 1 || receipt.revision !== revision) {
    throw new Error("SDK view receipt does not match the pinned NDK revision");
  }
  const ndkDirectory = join(viewPath, "ndk");
  if (lstatSync(ndkDirectory).isSymbolicLink()) {
    throw new Error("SDK view must own its NDK directory");
  }
  const entries = readdirSync(ndkDirectory);
  if (entries.length !== 1 || entries[0] !== revision) {
    throw new Error(`SDK view must expose only NDK ${revision}`);
  }
  const observed = inspectNdk(join(ndkDirectory, revision), revision);
  if (observed.ndk !== receipt.sourceNdk ||
      observed.ndk !== realpathSync(join(receipt.sourceSdk, "ndk", revision))) {
    throw new Error("SDK view no longer resolves to its reviewed installed NDK");
  }
  if (environment) {
    const expected = sdkViewEnvironment(viewPath, revision);
    for (const name of environmentNames) {
      if (!environment[name] || resolve(environment[name]) !== expected[name]) {
        throw new Error(`${name} must point to the pinned SDK view`);
      }
    }
  }
  let tauriSelections;
  if (buildLog !== undefined) {
    // cargo tauri emits ANSI styling before this line on interactive hosts.
    const plainLog = buildLog.replace(/\u001b\[[0-?]*[ -/]*[@-~]/g, "");
    tauriSelections = [...plainLog.matchAll(/Using installed NDK:\s*([^\r\n]+)/g)]
      .map((match) => match[1].trim());
    if (tauriSelections.length === 0) {
      throw new Error("Build log does not prove which NDK Tauri selected");
    }
    if (tauriSelections.some((selection) => selection !== join(ndkDirectory, revision))) {
      throw new Error("Tauri build selected an NDK outside the pinned SDK view");
    }
  }
  return { view: viewPath, ...observed, ...(tauriSelections ? { tauriSelections } : {}) };
}

function argumentsMap(args) {
  const options = {};
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index];
    const value = args[index + 1];
    if (!key?.startsWith("--") || !value || value.startsWith("--") || key in options) {
      throw new Error("Expected unique --option value pairs");
    }
    options[key] = value;
  }
  return options;
}

function main() {
  const [command, ...args] = process.argv.slice(2);
  const options = argumentsMap(args);
  const allowed = command === "create"
    ? ["--sdk", "--ndk", "--parent", "--github-env"]
    : command === "verify" ? ["--view", "--ndk", "--build-log"] : [];
  if (allowed.length === 0 || Object.keys(options).some((key) => !allowed.includes(key))) {
    throw new Error("Usage: android-sdk-view.mjs create --sdk SDK --ndk REVISION --parent DIR [--github-env FILE], or verify --view VIEW --ndk REVISION [--build-log FILE]");
  }
  const result = command === "create"
    ? createSdkView({ sdk: options["--sdk"], revision: options["--ndk"], parent: options["--parent"] })
    : verifySdkView({
      view: options["--view"], revision: options["--ndk"], environment: process.env,
      buildLog: options["--build-log"] ? readFileSync(options["--build-log"], "utf8") : undefined,
    });
  if (options["--github-env"]) {
    const lines = Object.entries(sdkViewEnvironment(result.view, result.revision))
      .map(([name, value]) => `${name}=${requireSingleLine(value)}\n`).join("");
    appendFileSync(options["--github-env"], lines);
  }
  console.log(JSON.stringify(result, null, 2));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
