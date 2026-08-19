import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

import {
  generateBom,
  githubOutputs,
  loadDependencySet,
  validateDependencySet,
  verifyImmutableActions,
  verifyLocalComponents,
  verifyProductSurfaces,
  verifyReleaseRef,
  withTemporaryDirectory,
} from "./source-integrity.mjs";

test("dependency-set schema rejects source and platform identity drift", () => {
  const set = loadDependencySet();
  const invalidCommit = structuredClone(set);
  invalidCommit.components[0].commit = "main";
  assert.throws(() => validateDependencySet(invalidCommit), /40-character commit SHA/);

  const splitBuildSequence = structuredClone(set);
  splitBuildSequence.product.platformBuilds.iosBundleVersion = String(
    splitBuildSequence.product.platformBuilds.androidVersionCode + 1,
  );
  assert.throws(
    () => validateDependencySet(splitBuildSequence),
    /share one monotonic build sequence/,
  );

  const mismatchedDisplay = structuredClone(set);
  mismatchedDisplay.product.displayVersion = "1.0.260a";
  assert.throws(() => validateDependencySet(mismatchedDisplay), /exact marketingVersion/);

  const stablePromotion = structuredClone(set);
  stablePromotion.product.predecessor.displayVersion = `${stablePromotion.product.marketingVersion}a`;
  stablePromotion.product.predecessor.tag = `v${stablePromotion.product.marketingVersion}a`;
  stablePromotion.product.displayVersion = stablePromotion.product.marketingVersion;
  assert.doesNotThrow(() => validateDependencySet(stablePromotion));

  const letteredDisplay = structuredClone(set);
  letteredDisplay.product.displayVersion = `${letteredDisplay.product.marketingVersion}a`;
  assert.doesNotThrow(() => validateDependencySet(letteredDisplay));

  const reusedBuild = structuredClone(set);
  reusedBuild.product.platformBuilds.androidVersionCode = 1000040;
  reusedBuild.product.platformBuilds.iosBundleVersion = "1000040";
  assert.throws(() => validateDependencySet(reusedBuild), /must exceed the recorded predecessor/);

  const remappedRepository = structuredClone(set);
  remappedRepository.components[0].repository = "https://example.invalid/rsReticulum.git";
  assert.throws(() => validateDependencySet(remappedRepository), /reviewed mapping/);

  const unsupportedAndroidAbi = structuredClone(set);
  unsupportedAndroidAbi.product.androidArtifactTargets.push("i686");
  assert.throws(() => validateDependencySet(unsupportedAndroidAbi), /i686 is unsupported/);
});

test("immutable-action guard scans named and anonymous workflow steps", () => {
  const sha = "fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09";
  assert.doesNotThrow(() =>
    verifyImmutableActions("valid.yml", `steps:\n  - uses: actions/checkout@${sha}\n  - name: Local\n    uses: ./actions/local\n`),
  );
  assert.throws(
    () => verifyImmutableActions("mutable.yml", "steps:\n  - uses: actions/checkout@v5\n"),
    /full immutable commit SHA/,
  );
  assert.throws(
    () => verifyImmutableActions("named.yml", "steps:\n  - name: Mutable\n    uses: actions/checkout@v5\n"),
    /full immutable commit SHA/,
  );
});

test("dependency set aligns product and exact local component sources", () => {
  const set = loadDependencySet();
  verifyProductSurfaces(set);
  verifyLocalComponents(set);

  const invalidIntegrationTag = structuredClone(set);
  invalidIntegrationTag.components[0].integrationTag = "main";
  assert.throws(() => verifyLocalComponents(invalidIntegrationTag), /integration tag type/);
});

test("GitHub outputs contain exact commits instead of integration tags", () => {
  const set = loadDependencySet();
  const output = githubOutputs(set);
  for (const component of set.components) {
    assert.match(output, new RegExp(`^${component.id}_ref=${component.commit}$`, "m"));
    assert.doesNotMatch(output, new RegExp(`${component.integrationTag}$`, "m"));
  }
  assert.match(output, /^rust_toolchain=\d+\.\d+\.\d+$/m);
  assert.match(output, /^node_version=\d+\.\d+\.\d+$/m);
  assert.match(output, /^tauri_cli=\d+\.\d+\.\d+$/m);
});

test("publishing requires the exact annotated display-version tag", () => {
  const set = loadDependencySet();
  assert.throws(() => verifyReleaseRef(set, "main"), /Ratspeak release ref/);
  assert.throws(() => verifyReleaseRef(set, "v0.0.0"), /Ratspeak release ref/);
});

test("source BOM is deterministic and records both lockfile hashes", () => {
  const set = loadDependencySet();
  assert.throws(
    () => generateBom(set, "v0.0.0", { requireClean: false }),
    /Ratspeak release ref/,
  );
  const first = generateBom(set, null, { requireClean: false });
  const second = generateBom(set, null, { requireClean: false });
  assert.deepEqual(first, second);
  assert.ok(
    first.product.ref === null || first.product.ref === `v${set.product.displayVersion}`,
    `unexpected exact product tag ${first.product.ref}`,
  );
  assert.match(first.lockfiles["Cargo.lock"], /^[0-9a-f]{64}$/);
  assert.match(first.lockfiles["src-tauri/Cargo.lock"], /^[0-9a-f]{64}$/);
  assert.deepEqual(first.product.androidArtifactTargets, ["aarch64", "armv7", "x86_64"]);
  assert.deepEqual(first.toolchains.declared, set.toolchains);
  assert.match(first.toolchains.observed.rustc, /^rustc \d+\.\d+\.\d+/);
  assert.match(first.toolchains.observed.cargo, /^cargo \d+\.\d+\.\d+/);
  assert.match(first.toolchains.observed.node, /^v\d+\.\d+\.\d+/);
  assert.ok(
    first.toolchains.observed.tauriCli === null ||
      /^tauri-cli \d+\.\d+\.\d+/.test(first.toolchains.observed.tauriCli),
    `unexpected observed Tauri CLI ${first.toolchains.observed.tauriCli}`,
  );

  withTemporaryDirectory((directory) => {
    const path = join(directory, "bom.json");
    const serialized = `${JSON.stringify(first, null, 2)}\n`;
    writeFileSync(path, serialized);
    assert.equal(readFileSync(path, "utf8"), serialized);
  });
});
