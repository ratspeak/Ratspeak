#!/usr/bin/env node
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

function fail(message) {
  throw new Error(message);
}

const [artifactDirectoryArgument, releaseTag, mode] = process.argv.slice(2);
if (!artifactDirectoryArgument || !releaseTag) {
  fail("usage: verify-release-artifacts.mjs ARTIFACT_DIRECTORY RELEASE_TAG [--qualification]");
}
if (mode !== undefined && mode !== "--qualification") fail(`unsupported verification mode: ${mode}`);
const qualification = mode === "--qualification";
if (!/^v[0-9]+\.[0-9]+\.[0-9]+[a-z]?$/.test(releaseTag)) {
  fail(`invalid Ratspeak release tag: ${releaseTag}`);
}

const artifactDirectory = resolve(artifactDirectoryArgument);
const repoRoot = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const dependencySetPath = join(repoRoot, "release/dependency-set.json");
const dependencySetBytes = readFileSync(dependencySetPath);
const dependencySet = JSON.parse(dependencySetBytes.toString("utf8"));
const expectedTag = `v${dependencySet.product.displayVersion}`;
if (releaseTag !== expectedTag) {
  fail(`release tag ${releaseTag} does not match dependency-set display version ${expectedTag}`);
}

const platformFiles = new Map([
  [
    "linux",
    [
      `Ratspeak-${releaseTag}-linux-amd64.deb`,
      `Ratspeak-${releaseTag}-linux-arm64.deb`,
      `Ratspeak-${releaseTag}-linux-x86_64.AppImage`,
      `Ratspeak-${releaseTag}-linux-x86_64.rpm`,
      `Ratspeak-${releaseTag}-linux-amd64-source-bom.json`,
      `Ratspeak-${releaseTag}-linux-arm64-source-bom.json`,
    ],
  ],
  [
    "windows",
    [
      `Ratspeak-${releaseTag}-windows-x64.msi`,
      `Ratspeak-${releaseTag}-windows-x64-setup.exe`,
      `Ratspeak-${releaseTag}-windows-source-bom.json`,
    ],
  ],
  [
    "macos",
    [
      `Ratspeak-${releaseTag}-macos-arm64.dmg`,
      `Ratspeak-${releaseTag}-macos-x64.dmg`,
      `Ratspeak-${releaseTag}-macos-source-bom.json`,
    ],
  ],
  [
    "android",
    [
      `Ratspeak-${releaseTag}-android-arm64.apk`,
      `Ratspeak-${releaseTag}-android-armv7.apk`,
      `Ratspeak-${releaseTag}-android-x86_64.apk`,
      `Ratspeak-${releaseTag}-android-source-bom.json`,
    ],
  ],
]);

const expectedFiles = new Set();
for (const [platform, files] of platformFiles) {
  expectedFiles.add(`checksums-${platform}.txt`);
  for (const file of files) expectedFiles.add(file);
}

const actualFiles = readdirSync(artifactDirectory)
  .filter((name) => statSync(join(artifactDirectory, name)).isFile())
  .sort();
const unexpected = actualFiles.filter((name) => !expectedFiles.has(name));
const missing = [...expectedFiles].filter((name) => !actualFiles.includes(name)).sort();
if (missing.length || unexpected.length) {
  fail(`release artifact set mismatch\nmissing: ${missing.join(", ") || "none"}\nunexpected: ${unexpected.join(", ") || "none"}`);
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

const expectedProductCommit = execFileSync("git", ["rev-parse", "HEAD"], {
  cwd: repoRoot,
  encoding: "utf8",
}).trim();
const expectedDependencySetHash = createHash("sha256").update(dependencySetBytes).digest("hex");
const expectedLockfiles = new Map([
  ["Cargo.lock", sha256(join(repoRoot, "Cargo.lock"))],
  ["src-tauri/Cargo.lock", sha256(join(repoRoot, "src-tauri/Cargo.lock"))],
]);

for (const [platform, expectedPlatformFiles] of platformFiles) {
  const checksumName = `checksums-${platform}.txt`;
  const checksumLines = readFileSync(join(artifactDirectory, checksumName), "utf8")
    .trimEnd()
    .split("\n");
  const recorded = new Map();
  for (const line of checksumLines) {
    const match = /^([0-9a-f]{64})  ([^/\\]+)$/.exec(line);
    if (!match) fail(`${checksumName}: invalid checksum line ${JSON.stringify(line)}`);
    if (recorded.has(match[2])) fail(`${checksumName}: duplicate entry ${match[2]}`);
    recorded.set(match[2], match[1]);
  }
  const recordedNames = [...recorded.keys()].sort();
  const expectedNames = [...expectedPlatformFiles].sort();
  if (JSON.stringify(recordedNames) !== JSON.stringify(expectedNames)) {
    fail(`${checksumName}: entries do not match the exact ${platform} artifact set`);
  }
  for (const [name, expectedHash] of recorded) {
    const actualHash = sha256(join(artifactDirectory, name));
    if (actualHash !== expectedHash) fail(`${name}: SHA-256 mismatch`);
  }
}

const canonicalComponents = new Map(
  dependencySet.components.map((component) => [component.id, component]),
);
const bomFiles = actualFiles.filter((name) => name.endsWith("-source-bom.json"));
if (bomFiles.length !== 5) fail(`expected five source BOMs, found ${bomFiles.length}`);
let productCommit = null;
for (const bomName of bomFiles) {
  const bom = JSON.parse(readFileSync(join(artifactDirectory, bomName), "utf8"));
  if (bom.schemaVersion !== 1) fail(`${bomName}: unsupported schema`);
  const expectedBomRef = qualification ? null : releaseTag;
  if (bom.product?.ref !== expectedBomRef) {
    fail(`${bomName}: source BOM ref is not ${expectedBomRef ?? "untagged"}`);
  }
  if (bom.product?.displayVersion !== dependencySet.product.displayVersion) {
    fail(`${bomName}: display version drift`);
  }
  if (!/^[0-9a-f]{40}$/.test(bom.product?.commit ?? "")) {
    fail(`${bomName}: invalid product commit`);
  }
  if (bom.product.commit !== expectedProductCommit) fail(`${bomName}: product commit is not the release tag commit`);
  productCommit ??= bom.product.commit;
  if (bom.product.commit !== productCommit) fail(`${bomName}: product commit drift`);
  if (bom.dependencySetSha256 !== expectedDependencySetHash) {
    fail(`${bomName}: dependency-set hash drift`);
  }
  for (const [name, hash] of expectedLockfiles) {
    if (bom.lockfiles?.[name] !== hash) fail(`${bomName}: ${name} hash drift`);
  }
  if (JSON.stringify(bom.toolchains?.declared) !== JSON.stringify(dependencySet.toolchains)) {
    fail(`${bomName}: declared toolchain drift`);
  }
  const components = new Map((bom.components ?? []).map((component) => [component.id, component]));
  if (components.size !== canonicalComponents.size) fail(`${bomName}: component count drift`);
  for (const [id, canonical] of canonicalComponents) {
    const component = components.get(id);
    if (!component) fail(`${bomName}: missing ${id}`);
    for (const field of ["name", "repository", "commit", "version", "integrationTag"]) {
      if (component[field] !== canonical[field]) {
        fail(`${bomName}: ${id} ${field} drift`);
      }
    }
  }
}

const verificationKind = qualification ? "pre-tag qualification" : "release";
console.log(`verified ${actualFiles.length} ${verificationKind} files for ${releaseTag} at ${productCommit}`);
