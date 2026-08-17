#!/usr/bin/env node

import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = dirname(fileURLToPath(import.meta.url));
export const repoRoot = resolve(scriptDir, "../..");
export const dependencySetPath = join(repoRoot, "release/dependency-set.json");

function fail(message) {
  throw new Error(message);
}

function readUtf8(path) {
  return readFileSync(path, "utf8");
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function git(args, cwd = repoRoot) {
  return execFileSync("git", args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  }).trim();
}

function observedCommand(command, args, { stderr = false } = {}) {
  const result = spawnSync(command, args, { encoding: "utf8" });
  if (result.error || result.status !== 0) return null;
  return (stderr ? result.stderr : result.stdout).trim() || null;
}

function observedToolchains() {
  const ndkSource = process.env.ANDROID_NDK_HOME
    ? join(process.env.ANDROID_NDK_HOME, "source.properties")
    : null;
  const ndkRevision = ndkSource && existsSync(ndkSource)
    ? readUtf8(ndkSource).match(/^Pkg\.Revision\s*=\s*(\S+)\s*$/m)?.[1] ?? null
    : null;
  return {
    rustc: observedCommand("rustc", ["--version"]),
    cargo: observedCommand("cargo", ["--version"]),
    node: observedCommand("node", ["--version"]),
    tauriCli: observedCommand("cargo", ["tauri", "--version"]),
    java: observedCommand("java", ["-version"], { stderr: true })?.split("\n")[0] ?? null,
    xcode: observedCommand("xcodebuild", ["-version"])?.split("\n")[0] ?? null,
    iosSdk: observedCommand("xcrun", ["--sdk", "iphoneos", "--show-sdk-version"]),
    androidNdk: ndkRevision,
  };
}

function requireString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${label} must be a non-empty string`);
  }
  return value;
}

function requireVersion(value, label) {
  requireString(value, label);
  if (!/^\d+\.\d+\.\d+$/.test(value)) {
    fail(`${label} must be a numeric three-part version, found ${value}`);
  }
}

function requireAppleVersion(value, label) {
  requireString(value, label);
  if (!/^\d+\.\d+(\.\d+)?$/.test(value)) {
    fail(`${label} must be a numeric two- or three-part version, found ${value}`);
  }
}

function requireCommit(value, label) {
  requireString(value, label);
  if (!/^[0-9a-f]{40}$/.test(value)) {
    fail(`${label} must be a lowercase 40-character commit SHA, found ${value}`);
  }
}

function compareNumericBuildVersions(left, right) {
  const leftParts = left.split(".").map(Number);
  const rightParts = right.split(".").map(Number);
  const length = Math.max(leftParts.length, rightParts.length);
  for (let index = 0; index < length; index += 1) {
    const difference = (leftParts[index] ?? 0) - (rightParts[index] ?? 0);
    if (difference !== 0) return Math.sign(difference);
  }
  return 0;
}

function parseDisplayVersion(value, label) {
  requireString(value, label);
  const match = value.match(/^(\d+\.\d+\.\d+)([a-z]?)$/);
  if (!match) fail(`${label} must be a numeric version with an optional lowercase prerelease letter`);
  return { marketing: match[1], suffix: match[2] };
}

function compareNumericVersions(left, right) {
  return compareNumericBuildVersions(left, right);
}

function manifestSection(source, section) {
  const marker = `[${section}]`;
  const start = source.indexOf(marker);
  if (start < 0) fail(`missing ${marker}`);
  const tail = source.slice(start + marker.length);
  const next = tail.search(/^\s*\[/m);
  return next < 0 ? tail : tail.slice(0, next);
}

function manifestValue(source, section, key) {
  const body = manifestSection(source, section);
  const match = body.match(new RegExp(`^\\s*${key.replace("-", "\\-")}\\s*=\\s*"([^"]+)"`, "m"));
  if (!match) fail(`missing ${key} in [${section}]`);
  return match[1];
}

function componentManifestVersion(componentRoot) {
  const source = readUtf8(join(componentRoot, "Cargo.toml"));
  if (source.includes("[workspace.package]")) {
    return manifestValue(source, "workspace.package", "version");
  }
  return manifestValue(source, "package", "version");
}

export function loadDependencySet() {
  const parsed = JSON.parse(readUtf8(dependencySetPath));
  validateDependencySet(parsed);
  return parsed;
}

export function validateDependencySet(set) {
  if (set.schemaVersion !== 1) fail(`unsupported dependency-set schema ${set.schemaVersion}`);
  if (!set.product || !set.toolchains || !Array.isArray(set.components)) {
    fail("dependency set must contain product, toolchains and components");
  }

  const currentDisplay = parseDisplayVersion(set.product.displayVersion, "product.displayVersion");
  requireVersion(set.product.marketingVersion, "product.marketingVersion");
  if (currentDisplay.marketing !== set.product.marketingVersion) {
    fail("displayVersion must be the exact marketingVersion with at most one lowercase prerelease letter");
  }
  const expectedAndroidTargets = ["aarch64", "armv7", "x86_64"];
  if (
    !Array.isArray(set.product.androidArtifactTargets) ||
    JSON.stringify(set.product.androidArtifactTargets) !== JSON.stringify(expectedAndroidTargets)
  ) {
    fail(`androidArtifactTargets must be exactly ${expectedAndroidTargets.join(", ")}; i686 is unsupported`);
  }
  const builds = set.product.platformBuilds;
  if (!Number.isSafeInteger(builds?.androidVersionCode) || builds.androidVersionCode < 1) {
    fail("androidVersionCode must be a positive integer");
  }
  requireString(builds.iosBundleVersion, "iosBundleVersion");
  if (!/^\d+(\.\d+){0,2}$/.test(builds.iosBundleVersion)) {
    fail("iosBundleVersion must satisfy Apple's numeric bundle-version form");
  }
  if (`${builds.androidVersionCode}` !== builds.iosBundleVersion) {
    fail("Android and iOS candidates must share one monotonic build sequence");
  }

  const predecessor = set.product.predecessor;
  const predecessorDisplay = parseDisplayVersion(
    predecessor?.displayVersion,
    "product.predecessor.displayVersion",
  );
  requireString(predecessor?.tag, "product.predecessor.tag");
  requireCommit(predecessor?.commit, "product.predecessor.commit");
  if (!Number.isSafeInteger(predecessor?.androidVersionCode) || predecessor.androidVersionCode < 1) {
    fail("product.predecessor.androidVersionCode must be a positive integer");
  }
  requireString(predecessor?.iosBundleVersion, "product.predecessor.iosBundleVersion");
  if (!/^\d+(\.\d+){0,2}$/.test(predecessor.iosBundleVersion)) {
    fail("product.predecessor.iosBundleVersion must satisfy Apple's numeric bundle-version form");
  }
  if (predecessor.tag !== `v${predecessor.displayVersion}`) {
    fail("product.predecessor.tag must identify predecessor.displayVersion");
  }
  const marketingOrder = compareNumericVersions(currentDisplay.marketing, predecessorDisplay.marketing);
  if (marketingOrder < 0) {
    fail("displayVersion marketing version must not precede the recorded predecessor");
  }
  if (marketingOrder === 0) {
    const promotesPrerelease = predecessorDisplay.suffix !== "" && currentDisplay.suffix === "";
    const advancesPrerelease =
      predecessorDisplay.suffix !== "" &&
      currentDisplay.suffix !== "" &&
      currentDisplay.suffix.charCodeAt(0) > predecessorDisplay.suffix.charCodeAt(0);
    if (!promotesPrerelease && !advancesPrerelease) {
      fail("displayVersion must advance its prerelease letter or promote the predecessor to stable");
    }
  }
  if (builds.androidVersionCode <= predecessor.androidVersionCode) {
    fail("androidVersionCode must exceed the recorded predecessor");
  }
  if (compareNumericBuildVersions(builds.iosBundleVersion, predecessor.iosBundleVersion) <= 0) {
    fail("iosBundleVersion must exceed the recorded predecessor");
  }

  requireVersion(set.toolchains.rustRelease, "toolchains.rustRelease");
  requireVersion(set.toolchains.rustMsrv, "toolchains.rustMsrv");
  requireVersion(set.toolchains.node, "toolchains.node");
  requireVersion(set.toolchains.tauriCli, "toolchains.tauriCli");
  requireString(set.toolchains.java, "toolchains.java");
  if (!/^\d+$/.test(set.toolchains.java)) fail("toolchains.java must be a numeric major version");
  requireString(set.toolchains.androidNdk, "toolchains.androidNdk");
  if (!/^\d+\.\d+\.\d+$/.test(set.toolchains.androidNdk)) {
    fail("toolchains.androidNdk must be a numeric three-part package revision");
  }
  requireAppleVersion(set.toolchains.xcode, "toolchains.xcode");
  requireAppleVersion(set.toolchains.iosSdk, "toolchains.iosSdk");

  const expectedComponents = new Map([
    ["rsreticulum", ["rsReticulum", "https://github.com/ratspeak/rsReticulum.git", "../rsReticulum"]],
    ["rslxmf", ["rsLXMF", "https://github.com/ratspeak/rsLXMF.git", "../rsLXMF"]],
    ["rslxst", ["rsLXST", "https://github.com/ratspeak/rsLXST.git", "../rsLXST"]],
    ["lrgp", ["lrgp-rs", "https://github.com/ratspeak/lrgp-rs.git", "../lrgp-rs"]],
  ]);
  const expectedIds = [...expectedComponents.keys()];
  const ids = set.components.map((component) => component.id);
  if (new Set(ids).size !== ids.length || expectedIds.some((id) => !ids.includes(id))) {
    fail(`components must contain each unique required id: ${expectedIds.join(", ")}`);
  }
  for (const component of set.components) {
    requireString(component.id, "component.id");
    requireString(component.name, `${component.id}.name`);
    requireString(component.repository, `${component.id}.repository`);
    requireString(component.path, `${component.id}.path`);
    const expected = expectedComponents.get(component.id);
    if (
      !expected ||
      component.name !== expected[0] ||
      component.repository !== expected[1] ||
      component.path !== expected[2]
    ) {
      fail(`${component.id}: component name, repository and path must match the reviewed mapping`);
    }
    requireCommit(component.commit, `${component.id}.commit`);
    requireVersion(component.version, `${component.id}.version`);
    if (component.integrationTag !== null) {
      requireString(component.integrationTag, `${component.id}.integrationTag`);
    }
  }
}

function expectEqual(actual, expected, label) {
  if (`${actual}` !== `${expected}`) {
    fail(`${label}: expected ${expected}, found ${actual}`);
  }
}

function expectContains(source, fragment, label) {
  if (!source.includes(fragment)) fail(`${label}: missing ${fragment}`);
}

export function verifyProductSurfaces(set) {
  const marketing = set.product.marketingVersion;
  const display = set.product.displayVersion;
  const androidBuild = set.product.platformBuilds.androidVersionCode;
  const iosBuild = set.product.platformBuilds.iosBundleVersion;

  let existingDisplayTag = null;
  try {
    existingDisplayTag = git(["rev-parse", `v${display}^{commit}`]);
  } catch {
    // The candidate display tag is expected to be absent before its release.
  }
  if (existingDisplayTag !== null) {
    try {
      git(["merge-base", "--is-ancestor", existingDisplayTag, "HEAD"]);
    } catch {
      fail(`existing display tag v${display} is not an ancestor of HEAD`);
    }
  }

  const predecessor = set.product.predecessor;
  expectEqual(git(["cat-file", "-t", predecessor.tag]), "tag", "predecessor tag type");
  expectEqual(
    git(["rev-parse", `${predecessor.tag}^{commit}`]),
    predecessor.commit,
    "predecessor tag commit",
  );
  const predecessorTauri = JSON.parse(
    git(["show", `${predecessor.tag}:src-tauri/tauri.conf.json`]),
  );
  expectEqual(
    predecessorTauri.bundle?.android?.versionCode,
    predecessor.androidVersionCode,
    "predecessor Android versionCode",
  );
  expectContains(
    git(["show", `${predecessor.tag}:src-tauri/gen/apple/project.yml`]),
    `CFBundleVersion: "${predecessor.iosBundleVersion}"`,
    "predecessor iOS build number",
  );

  expectContains(readUtf8(join(repoRoot, "CHANGELOG.md")), "## [Unreleased]", "changelog policy");
  expectContains(
    readUtf8(join(repoRoot, "RELEASING.md")),
    "release/dependency-set.json",
    "source qualification policy",
  );
  for (const lockfile of ["Cargo.lock", "src-tauri/Cargo.lock"]) {
    if (!existsSync(join(repoRoot, lockfile))) fail(`missing committed release lockfile ${lockfile}`);
  }

  expectEqual(readUtf8(join(repoRoot, "VERSION")).trim(), display, "VERSION");
  expectEqual(
    manifestValue(readUtf8(join(repoRoot, "Cargo.toml")), "workspace.package", "version"),
    marketing,
    "workspace Cargo version",
  );
  expectEqual(
    manifestValue(readUtf8(join(repoRoot, "src-tauri/Cargo.toml")), "package", "version"),
    marketing,
    "Tauri Cargo version",
  );

  const tauri = JSON.parse(readUtf8(join(repoRoot, "src-tauri/tauri.conf.json")));
  expectEqual(tauri.version, marketing, "Tauri marketing version");
  expectEqual(tauri.bundle?.android?.versionCode, androidBuild, "Android versionCode");
  expectEqual(tauri.bundle?.iOS?.bundleVersion, iosBuild, "Tauri iOS bundle version");
  expectEqual(
    tauri.bundle?.resources?.["../third_party/opus-rs-0.1.29-COPYING"],
    "third-party/opus-rs-0.1.29-COPYING.txt",
    "Tauri bundled opus-rs notice path",
  );
  expectEqual(
    sha256(join(repoRoot, "third_party/opus-rs-0.1.29-COPYING")),
    "67c6f0a4bac3019fb08948838d7203bf661a629416f69057081c6f39db5e96a5",
    "preserved opus-rs 0.1.29 notice",
  );

  const project = readUtf8(join(repoRoot, "src-tauri/gen/apple/project.yml"));
  expectContains(project, `CFBundleShortVersionString: ${marketing}`, "iOS project marketing version");
  expectContains(project, `CFBundleVersion: "${iosBuild}"`, "iOS project build number");
  const plist = readUtf8(join(repoRoot, "src-tauri/gen/apple/ratspeak_iOS/Info.plist"));
  expectContains(plist, `<string>${marketing}</string>`, "iOS Info.plist marketing version");
  expectContains(plist, `<string>${iosBuild}</string>`, "iOS Info.plist build number");

  const rootManifest = readUtf8(join(repoRoot, "Cargo.toml"));
  const workspacePackage = manifestSection(rootManifest, "workspace.package");
  expectContains(workspacePackage, "rust-version = \"1.87\"", "workspace MSRV");
  expectContains(workspacePackage, "publish = false", "workspace publish policy");

  for (const manifest of [
    "crates/ratspeak-core/Cargo.toml",
    "crates/ratspeak-db/Cargo.toml",
    "crates/ratspeak-runtime/Cargo.toml",
    "crates/ratspeak-tauri/Cargo.toml",
  ]) {
    const packageSource = readUtf8(join(repoRoot, manifest));
    const packageSection = manifestSection(packageSource, "package");
    expectContains(packageSection, "rust-version.workspace = true", `${manifest} MSRV inheritance`);
    expectContains(packageSection, "publish.workspace = true", `${manifest} publish inheritance`);
  }

  const standaloneManifest = readUtf8(join(repoRoot, "src-tauri/Cargo.toml"));
  const standalonePackage = manifestSection(standaloneManifest, "package");
  expectContains(standalonePackage, "rust-version = \"1.87\"", "standalone Tauri MSRV");
  expectContains(standalonePackage, "publish = false", "standalone Tauri publish policy");

  const expectedRequirements = new Map([
    ["rsreticulum", ["rns-crypto", "rns-wire", "rns-identity", "rns-transport", "rns-link", "rns-protocol", "rns-interface", "rns-runtime", "rns-ratkey"]],
    ["rslxmf", ["lxmf-core"]],
    ["rslxst", ["lxst-core", "lxst-rns", "lxst-telephony"]],
    ["lrgp", ["lrgp"]],
  ]);
  for (const component of set.components) {
    for (const dependency of expectedRequirements.get(component.id) ?? []) {
      const pattern = new RegExp(`^${dependency}\\s*=\\s*\\{[^\\n]*version\\s*=\\s*"${component.version.replaceAll(".", "\\.")}"[^\\n]*\\}`, "m");
      if (!pattern.test(rootManifest)) {
        fail(`Cargo.toml: ${dependency} must declare compatible version ${component.version}`);
      }
    }
  }

  for (const [dependency, version] of [
    ["ratspeak-tauri", marketing],
    ["rns-interface", set.components.find((component) => component.id === "rsreticulum").version],
  ]) {
    const pattern = new RegExp(`^${dependency}\\s*=\\s*\\{[^\\n]*version\\s*=\\s*"${version.replaceAll(".", "\\.")}"[^\\n]*\\}`, "m");
    if (!pattern.test(standaloneManifest)) {
      fail(`src-tauri/Cargo.toml: ${dependency} must declare compatible version ${version}`);
    }
  }

  const workflowDirectory = join(repoRoot, ".github/workflows");
  for (const workflowName of readdirSync(workflowDirectory).filter((name) => name.endsWith(".yml"))) {
    const workflow = readUtf8(join(workflowDirectory, workflowName));
    verifyImmutableActions(workflowName, workflow);
  }

  const ciWorkflow = readUtf8(join(workflowDirectory, "ci.yml"));
  expectContains(ciWorkflow, `node-version: ${set.toolchains.node}`, "CI Node toolchain");
  expectContains(
    ciWorkflow,
    "source-integrity.mjs github-outputs",
    "CI mobile reviewed toolchain load",
  );
  expectContains(
    ciWorkflow,
    'cargo install tauri-cli --version "${{ steps.mobile-source.outputs.tauri_cli }}" --locked',
    "CI minified Android Tauri CLI",
  );

  for (const workflowName of [
    "release-android.yml",
    "release-desktop.yml",
    "release-ios.yml",
    "release-macos.yml",
    "release-windows.yml",
  ]) {
    const workflow = readUtf8(join(workflowDirectory, workflowName));
    expectContains(
      workflow,
      `node-version: ${set.toolchains.node}`,
      `${workflowName} Node toolchain`,
    );
    expectContains(workflow, "source-integrity.mjs github-outputs", `${workflowName} reviewed source load`);
    expectContains(
      workflow,
      "source-integrity.mjs verify-release-source",
      `${workflowName} reviewed source verification`,
    );
    expectContains(workflow, "-- --locked", `${workflowName} locked Tauri build`);
    if (workflowName !== "release-ios.yml") {
      expectContains(
        workflow,
        "source-integrity.mjs verify-release-ref",
        `${workflowName} publishing ref verification`,
      );
      expectContains(
        workflow,
        "PUBLISH_GITHUB_RELEASE",
        `${workflowName} publishing BOM binding`,
      );
    }
  }

  const androidWorkflow = readUtf8(join(workflowDirectory, "release-android.yml"));
  expectContains(
    androidWorkflow,
    `--target ${set.product.androidArtifactTargets.join(" ")}`,
    "Android artifact target matrix",
  );
  if (androidWorkflow.includes("i686")) {
    fail("release-android.yml must not claim unsupported Android i686 artifacts");
  }
}

export function verifyImmutableActions(workflowName, workflow) {
  for (const match of workflow.matchAll(/^\s*(?:-\s+)?uses:\s+([^\s#]+).*$/gm)) {
    const action = match[1];
    if (action.startsWith("./")) continue;
    const separator = action.lastIndexOf("@");
    if (separator < 0 || !/^[0-9a-f]{40}$/.test(action.slice(separator + 1))) {
      fail(`${workflowName}: external action must use a full immutable commit SHA (${action})`);
    }
  }
}

export function verifyLocalComponents(set) {
  for (const component of set.components) {
    const componentRoot = resolve(repoRoot, component.path);
    if (!existsSync(join(componentRoot, ".git"))) {
      fail(`${component.name}: missing Git checkout at ${componentRoot}`);
    }
    expectEqual(git(["rev-parse", "HEAD"], componentRoot), component.commit, `${component.name} HEAD`);
    expectEqual(componentManifestVersion(componentRoot), component.version, `${component.name} Cargo version`);
    if (component.integrationTag !== null) {
      let tagType;
      try {
        tagType = git(["cat-file", "-t", component.integrationTag], componentRoot);
      } catch {
        fail(`${component.name} integration tag type: ${component.integrationTag} does not resolve`);
      }
      expectEqual(
        tagType,
        "tag",
        `${component.name} integration tag type`,
      );
      const tagCommit = git(["rev-parse", `${component.integrationTag}^{commit}`], componentRoot);
      expectEqual(tagCommit, component.commit, `${component.name} integration tag`);
    }
  }
}

export function verifyCleanWorktrees(set, { includeUntracked = false } = {}) {
  const repositories = [
    ["Ratspeak", repoRoot],
    ...set.components.map((component) => [component.name, resolve(repoRoot, component.path)]),
  ];
  for (const [name, root] of repositories) {
    const status = git(
      ["status", "--porcelain=v1", `--untracked-files=${includeUntracked ? "all" : "no"}`],
      root,
    );
    if (status !== "") {
      fail(`${name}: source worktree is not clean:\n${status}`);
    }
  }
}

export function githubOutputs(set) {
  const lines = set.components.map((component) => `${component.id}_ref=${component.commit}`);
  lines.push(`rust_toolchain=${set.toolchains.rustRelease}`);
  lines.push(`rust_msrv=${set.toolchains.rustMsrv}`);
  lines.push(`node_version=${set.toolchains.node}`);
  lines.push(`tauri_cli=${set.toolchains.tauriCli}`);
  lines.push(`android_ndk=${set.toolchains.androidNdk}`);
  lines.push(`java=${set.toolchains.java}`);
  lines.push(`xcode=${set.toolchains.xcode}`);
  lines.push(`ios_sdk=${set.toolchains.iosSdk}`);
  lines.push(`display_version=${set.product.displayVersion}`);
  lines.push(`marketing_version=${set.product.marketingVersion}`);
  lines.push(`android_version_code=${set.product.platformBuilds.androidVersionCode}`);
  lines.push(`ios_bundle_version=${set.product.platformBuilds.iosBundleVersion}`);
  return `${lines.join("\n")}\n`;
}

export function verifyReleaseRef(set, releaseRef) {
  verifyProductSurfaces(set);
  requireString(releaseRef, "Ratspeak release ref");
  expectEqual(releaseRef, `v${set.product.displayVersion}`, "Ratspeak release ref");
  expectEqual(git(["cat-file", "-t", releaseRef]), "tag", `Ratspeak release ref ${releaseRef} type`);
  expectEqual(
    git(["rev-parse", `${releaseRef}^{commit}`]),
    git(["rev-parse", "HEAD"]),
    `Ratspeak release ref ${releaseRef}`,
  );
}

export function generateBom(set, releaseRef = null, { requireClean = true } = {}) {
  verifyProductSurfaces(set);
  verifyLocalComponents(set);
  if (requireClean) verifyCleanWorktrees(set);
  const productCommit = git(["rev-parse", "HEAD"]);
  if (releaseRef !== null) {
    verifyReleaseRef(set, releaseRef);
  }
  const exactTag = (() => {
    const candidate = `v${set.product.displayVersion}`;
    try {
      return git(["rev-parse", `${candidate}^{commit}`]) === productCommit ? candidate : null;
    } catch {
      return null;
    }
  })();
  return {
    schemaVersion: 1,
    product: {
      name: "Ratspeak",
      repository: "https://github.com/ratspeak/Ratspeak.git",
      commit: productCommit,
      ref: releaseRef ?? exactTag,
      displayVersion: set.product.displayVersion,
      marketingVersion: set.product.marketingVersion,
      androidArtifactTargets: set.product.androidArtifactTargets,
      platformBuilds: set.product.platformBuilds,
    },
    components: set.components.map((component) => ({
      id: component.id,
      name: component.name,
      repository: component.repository,
      commit: component.commit,
      version: component.version,
      integrationTag: component.integrationTag,
    })),
    lockfiles: {
      "Cargo.lock": sha256(join(repoRoot, "Cargo.lock")),
      "src-tauri/Cargo.lock": sha256(join(repoRoot, "src-tauri/Cargo.lock")),
    },
    toolchains: {
      declared: set.toolchains,
      observed: observedToolchains(),
    },
    dependencySetSha256: sha256(dependencySetPath),
  };
}

function usage() {
  console.error("Usage: source-integrity.mjs <verify|verify-local|verify-release-source|verify-release-ref|verify-tags|github-outputs|bom> [options]");
}

function main(argv) {
  const [command, ...args] = argv;
  const set = loadDependencySet();
  if (command === "verify") {
    verifyProductSurfaces(set);
    console.log("source integrity: product surfaces aligned");
    return;
  }
  if (command === "verify-local") {
    verifyProductSurfaces(set);
    verifyLocalComponents(set);
    console.log("source integrity: exact local dependency set verified");
    return;
  }
  if (command === "verify-release-source") {
    verifyProductSurfaces(set);
    verifyLocalComponents(set);
    verifyCleanWorktrees(set, { includeUntracked: true });
    console.log("source integrity: clean exact release source verified");
    return;
  }
  if (command === "verify-release-ref") {
    if (args.length !== 1) fail("verify-release-ref requires exactly one ref");
    verifyReleaseRef(set, args[0]);
    console.log(`source integrity: annotated release ref ${args[0]} verified`);
    return;
  }
  if (command === "verify-tags") {
    verifyProductSurfaces(set);
    verifyLocalComponents(set);
    console.log("source integrity: exact local dependency tags verified");
    return;
  }
  if (command === "github-outputs") {
    process.stdout.write(githubOutputs(set));
    return;
  }
  if (command === "bom") {
    const outputIndex = args.indexOf("--output");
    if (outputIndex < 0 || !args[outputIndex + 1]) fail("bom requires --output <path>");
    const refIndex = args.indexOf("--release-ref");
    const releaseRef = refIndex >= 0 ? args[refIndex + 1] : null;
    const output = resolve(process.cwd(), args[outputIndex + 1]);
    const bom = generateBom(set, releaseRef);
    writeFileSync(output, `${JSON.stringify(bom, null, 2)}\n`);
    console.log(`source integrity: wrote ${relative(repoRoot, output)}`);
    return;
  }
  usage();
  process.exitCode = 2;
}

const isMain = process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
if (isMain) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`source integrity: ${error.message}`);
    process.exitCode = 1;
  }
}

// Keep the temporary-directory imports exercised in Node's permission-aware
// packagers; tests use this helper without adding a third-party dependency.
export function withTemporaryDirectory(callback) {
  const directory = mkdtempSync(join(tmpdir(), "ratspeak-source-integrity-"));
  try {
    return callback(directory);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}
