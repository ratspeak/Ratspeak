import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { chmodSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  generateBom,
  githubOutputs,
  loadDependencySet,
  validateDependencySet,
  verifyComponentIntegrationTag,
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
  for (const component of letteredDisplay.components) {
    component.integrationTag = `ratspeak-v${letteredDisplay.product.displayVersion}`;
  }
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

  const futureRelease = structuredClone(set);
  futureRelease.product.displayVersion = "1.0.29";
  futureRelease.product.marketingVersion = "1.0.29";
  for (const component of futureRelease.components) {
    component.integrationTag = null;
  }
  assert.throws(
    () => validateDependencySet(futureRelease),
    /integrationTag must be ratspeak-v1.0.29/,
  );
  for (const component of futureRelease.components) {
    component.integrationTag = "ratspeak-v1.0.29";
  }
  assert.doesNotThrow(() => validateDependencySet(futureRelease));
  futureRelease.components[0].integrationTag = "ratspeak-v1.0.29-wrong";
  assert.throws(
    () => validateDependencySet(futureRelease),
    /integrationTag must be exactly ratspeak-v1.0.29/,
  );
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
});

test("integration aliases must be annotated tags at the exact component commit", () => {
  withTemporaryDirectory((directory) => {
    execFileSync("git", ["init", "--quiet"], { cwd: directory });
    execFileSync("git", ["config", "user.name", "Ratspeak release test"], { cwd: directory });
    execFileSync("git", ["config", "user.email", "release-test@ratspeak.invalid"], {
      cwd: directory,
    });
    writeFileSync(join(directory, "source.txt"), "first\n");
    execFileSync("git", ["add", "source.txt"], { cwd: directory });
    execFileSync("git", ["commit", "--quiet", "-m", "first"], { cwd: directory });
    const first = execFileSync("git", ["rev-parse", "HEAD"], {
      cwd: directory,
      encoding: "utf8",
    }).trim();
    const component = {
      name: "fixture",
      commit: first,
      integrationTag: "ratspeak-v1.0.29",
    };

    assert.throws(
      () => verifyComponentIntegrationTag(component, directory),
      /does not resolve/,
    );

    execFileSync("git", ["tag", "ratspeak-v1.0.29"], { cwd: directory });
    assert.throws(
      () => verifyComponentIntegrationTag(component, directory),
      /integration tag type/,
    );

    execFileSync("git", ["tag", "-d", "ratspeak-v1.0.29"], { cwd: directory });
    execFileSync("git", ["tag", "-a", "ratspeak-v1.0.29", "-m", "compatibility"], {
      cwd: directory,
    });
    assert.doesNotThrow(() => verifyComponentIntegrationTag(component, directory));

    writeFileSync(join(directory, "source.txt"), "second\n");
    execFileSync("git", ["add", "source.txt"], { cwd: directory });
    execFileSync("git", ["commit", "--quiet", "-m", "second"], { cwd: directory });
    component.commit = execFileSync("git", ["rev-parse", "HEAD"], {
      cwd: directory,
      encoding: "utf8",
    }).trim();
    assert.throws(
      () => verifyComponentIntegrationTag(component, directory),
      /integration tag: expected/,
    );
  });
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

test("final release artifact gate requires the exact complete checksummed source graph", () => {
  const set = loadDependencySet();
  const releaseTag = `v${set.product.displayVersion}`;
  const platformFiles = new Map([
    ["linux", [
      `Ratspeak-${releaseTag}-linux-amd64.deb`,
      `Ratspeak-${releaseTag}-linux-arm64.deb`,
      `Ratspeak-${releaseTag}-linux-x86_64.AppImage`,
      `Ratspeak-${releaseTag}-linux-x86_64.rpm`,
      `Ratspeak-${releaseTag}-linux-amd64-source-bom.json`,
      `Ratspeak-${releaseTag}-linux-arm64-source-bom.json`,
    ]],
    ["windows", [
      `Ratspeak-${releaseTag}-windows-x64.msi`,
      `Ratspeak-${releaseTag}-windows-x64-setup.exe`,
      `Ratspeak-${releaseTag}-windows-source-bom.json`,
    ]],
    ["macos", [
      `Ratspeak-${releaseTag}-macos-arm64.dmg`,
      `Ratspeak-${releaseTag}-macos-x64.dmg`,
      `Ratspeak-${releaseTag}-macos-source-bom.json`,
    ]],
    ["android", [
      `Ratspeak-${releaseTag}-android-arm64.apk`,
      `Ratspeak-${releaseTag}-android-armv7.apk`,
      `Ratspeak-${releaseTag}-android-x86_64.apk`,
      `Ratspeak-${releaseTag}-android-source-bom.json`,
    ]],
  ]);
  const bom = generateBom(set, null, { requireClean: false });
  const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

  withTemporaryDirectory((directory) => {
    const writeFixture = (bomRef, mutateBom = () => {}) => {
      const fixtureBom = structuredClone(bom);
      fixtureBom.product.ref = bomRef;
      mutateBom(fixtureBom);
      const bomBytes = `${JSON.stringify(fixtureBom, null, 2)}\n`;
      for (const [platform, files] of platformFiles) {
        const checksums = [];
        for (const name of files) {
          const bytes = name.endsWith("-source-bom.json") ? bomBytes : `fixture:${name}\n`;
          writeFileSync(join(directory, name), bytes);
          checksums.push(`${sha256(bytes)}  ${name}`);
        }
        writeFileSync(join(directory, `checksums-${platform}.txt`), `${checksums.join("\n")}\n`);
      }
    };

    const script = join(dirname(fileURLToPath(import.meta.url)), "verify-release-artifacts.mjs");
    writeFixture(null);
    assert.doesNotThrow(() =>
      execFileSync(process.execPath, [script, directory, releaseTag, "--qualification"], {
        stdio: "pipe",
      }),
    );
    assert.throws(
      () => execFileSync(process.execPath, [script, directory, releaseTag], { stdio: "pipe" }),
      /Command failed/,
    );

    writeFixture(releaseTag);
    assert.doesNotThrow(() =>
      execFileSync(process.execPath, [script, directory, releaseTag], { stdio: "pipe" }),
    );

    writeFixture(releaseTag, (fixtureBom) => {
      fixtureBom.components[0].integrationTag = "ratspeak-v0.0.0";
    });
    assert.throws(
      () => execFileSync(process.execPath, [script, directory, releaseTag], { stdio: "pipe" }),
      /Command failed/,
    );

    writeFixture(releaseTag);
    const corrupted = `Ratspeak-${releaseTag}-android-arm64.apk`;
    writeFileSync(join(directory, corrupted), "corrupted\n");
    assert.throws(
      () => execFileSync(process.execPath, [script, directory, releaseTag], { stdio: "pipe" }),
      /Command failed/,
    );
  });
});

test("macOS notarization timeout preserves IDs and resumes without resubmitting", () => {
  const scriptsDirectory = dirname(fileURLToPath(import.meta.url));
  const notarizer = join(scriptsDirectory, "notarize-macos-dmgs.sh");

  withTemporaryDirectory((directory) => {
    const binDirectory = join(directory, "bin");
    const stateDirectory = join(directory, "state");
    const invocationLog = join(directory, "xcrun.log");
    const arm64Dmg = join(directory, "Ratspeak-v1.0.28-macos-arm64.dmg");
    const x64Dmg = join(directory, "Ratspeak-v1.0.28-macos-x64.dmg");
    mkdirSync(binDirectory);
    writeFileSync(arm64Dmg, "signed arm64 fixture\n");
    writeFileSync(x64Dmg, "signed x64 fixture\n");

    const xcrun = `#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$*" >> "$MOCK_XCRUN_LOG"
if [[ "$1" == "notarytool" && "$2" == "submit" ]]; then
  if [[ "$3" == *arm64* ]]; then
    printf '{"id":"11111111-1111-1111-1111-111111111111"}\\n'
  else
    printf '{"id":"22222222-2222-2222-2222-222222222222"}\\n'
  fi
elif [[ "$1" == "notarytool" && "$2" == "wait" ]]; then
  if [[ "$MOCK_NOTARY_WAIT" == "timeout" ]]; then
    exit 1
  fi
  printf '{"status":"Accepted"}\\n'
elif [[ "$1" == "notarytool" && "$2" == "info" ]]; then
  printf '{"status":"In Progress"}\\n'
elif [[ "$1" == "notarytool" && "$2" == "log" ]]; then
  printf '{"status":"Accepted"}\\n' > "$4"
elif [[ "$1" != "stapler" ]]; then
  exit 2
fi
`;
    const plutil = `#!/usr/bin/env bash
set -euo pipefail
node -e 'const fs = require("fs"); const value = JSON.parse(fs.readFileSync(process.argv[2], "utf8"))[process.argv[1]]; if (!value) process.exit(1); process.stdout.write(String(value));' "$2" "$6"
`;
    const xcrunPath = join(binDirectory, "xcrun");
    const plutilPath = join(binDirectory, "plutil");
    writeFileSync(xcrunPath, xcrun);
    writeFileSync(plutilPath, plutil);
    chmodSync(xcrunPath, 0o755);
    chmodSync(plutilPath, 0o755);

    const environment = {
      ...process.env,
      APPLE_ID: "release@example.invalid",
      APPLE_PASSWORD: "fixture-password",
      APPLE_TEAM_ID: "FIXTURETEAM",
      MOCK_NOTARY_WAIT: "timeout",
      MOCK_XCRUN_LOG: invocationLog,
      NOTARY_WAIT_TIMEOUT: "1s",
      PATH: `${binDirectory}:${process.env.PATH}`,
    };
    assert.throws(() =>
      execFileSync("bash", [notarizer, "submit", stateDirectory, arm64Dmg, x64Dmg], {
        env: environment,
        stdio: "pipe",
      }),
    );

    const submissions = readFileSync(join(stateDirectory, "submissions.tsv"), "utf8")
      .trimEnd()
      .split("\n");
    assert.deepEqual(submissions, [
      "Ratspeak-v1.0.28-macos-arm64.dmg\t11111111-1111-1111-1111-111111111111",
      "Ratspeak-v1.0.28-macos-x64.dmg\t22222222-2222-2222-2222-222222222222",
    ]);
    assert.equal(
      readFileSync(invocationLog, "utf8").split("\n").filter((line) => line.startsWith("notarytool submit ")).length,
      2,
    );

    writeFileSync(invocationLog, "");
    execFileSync("bash", [notarizer, "resume", stateDirectory, arm64Dmg, x64Dmg], {
      env: { ...environment, MOCK_NOTARY_WAIT: "accepted" },
      stdio: "pipe",
    });
    const resumedInvocations = readFileSync(invocationLog, "utf8");
    assert.doesNotMatch(resumedInvocations, /^notarytool submit /m);
    assert.equal(
      resumedInvocations.split("\n").filter((line) => line.startsWith("notarytool wait ")).length,
      2,
    );
    assert.equal(
      resumedInvocations.split("\n").filter((line) => line.startsWith("stapler ")).length,
      4,
    );
  });
});
