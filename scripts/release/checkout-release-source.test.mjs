import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, symlinkSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { checkoutReleaseSource } from "./checkout-release-source.mjs";
import { loadDependencySet, withTemporaryDirectory } from "./source-integrity.mjs";

function git(cwd, ...args) {
  return execFileSync("git", ["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", ...args], {
    cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 20_000,
  }).trim();
}

function initialize(directory) {
  mkdirSync(directory, { recursive: true });
  git(directory, "init", "--quiet");
  git(directory, "config", "user.name", "Release source fixture");
  git(directory, "config", "user.email", "release-source@ratspeak.invalid");
}

function commit(directory, message) {
  git(directory, "add", ".");
  git(directory, "commit", "--quiet", "-m", message);
  return git(directory, "rev-parse", "HEAD");
}

function fixture(directory) {
  const set = structuredClone(loadDependencySet());
  set.product.displayVersion = "1.0.99";
  set.product.marketingVersion = "1.0.99";
  const localSources = join(directory, "mirrors");
  mkdirSync(localSources);
  for (const component of set.components) {
    component.integrationTag = "ratspeak-v1.0.99";
    const source = join(localSources, component.name);
    initialize(source);
    writeFileSync(join(source, "Cargo.toml"), `[${component.id === "lrgp" ? "package" : "workspace.package"}]\nversion = "${component.version}"\n`);
    writeFileSync(join(source, ".gitignore"), "target/\n");
    writeFileSync(join(source, "api.txt"), "old standalone API\n");
    commit(source, "standalone release");
    git(source, "tag", "-a", `v${component.version}`, "-m", "standalone");
    writeFileSync(join(source, "api.txt"), "required integration API\n");
    component.commit = commit(source, "integration API");
    git(source, "tag", "-a", component.integrationTag, "-m", "integration");
  }
  const productCheckout = join(localSources, "Ratspeak");
  initialize(productCheckout);
  writeFileSync(join(productCheckout, "predecessor.txt"), "previous product release\n");
  set.product.predecessor.commit = commit(productCheckout, "predecessor release");
  git(productCheckout, "tag", "-a", set.product.predecessor.tag, "-m", "predecessor");
  mkdirSync(join(productCheckout, "release"));
  mkdirSync(join(productCheckout, "src-tauri"));
  writeFileSync(join(productCheckout, "release/dependency-set.json"), JSON.stringify(set));
  writeFileSync(join(productCheckout, "VERSION"), "1.0.99\n");
  writeFileSync(join(productCheckout, "Cargo.lock"), "# fixture workspace lock\n");
  writeFileSync(join(productCheckout, "src-tauri/Cargo.lock"), "# fixture application lock\n");
  commit(productCheckout, "product release");
  git(productCheckout, "tag", "-a", "v1.0.99", "-m", "product");
  const destination = join(directory, "source");
  return { set, productCheckout, options: { tag: "v1.0.99", destination, localSources } };
}

test("reconstructs the exact graph despite stale standalone tags, and reuses pristine checkouts", () => {
  withTemporaryDirectory((directory) => {
    const { set, options } = fixture(directory);
    const result = checkoutReleaseSource(options);
    assert.equal(result.productTagVerified, true);
    assert.equal(result.componentTagsVerified, true);
    assert.equal(git(join(options.destination, "Ratspeak"), "rev-parse", "--is-shallow-repository"), "false");
    assert.equal(git(join(options.destination, "Ratspeak"), "merge-base", "--is-ancestor", set.product.predecessor.commit, "HEAD"), "");
    for (const component of set.components) {
      const source = join(options.destination, component.name);
      assert.equal(git(source, "rev-parse", "HEAD"), component.commit);
      assert.equal(readFileSync(join(source, "api.txt"), "utf8"), "required integration API\n");
      assert.notEqual(git(join(options.localSources, component.name), "rev-parse", `v${component.version}^{commit}`), component.commit);
      git(source, "config", "fixture.preserve", "yes");
    }
    assert.deepEqual(checkoutReleaseSource(options), result);
    for (const component of set.components) {
      assert.equal(git(join(options.destination, component.name), "config", "fixture.preserve"), "yes");
    }
    assert.equal(readdirSync(directory).some((name) => name.startsWith(".ratspeak-release-source-")), false);
  });
});

test("plan verifies product identity and emits commits without creating a destination", () => {
  withTemporaryDirectory((directory) => {
    const { set, options } = fixture(directory);
    const result = checkoutReleaseSource({ ...options, plan: true });
    assert.equal(result.mode, "plan");
    assert.equal(result.productTagVerified, true);
    assert.equal(result.componentTagsVerified, false);
    assert.equal(existsSync(options.destination), false);
    for (const component of set.components) {
      assert.equal(result.repositories.find((item) => item.id === component.id).fetchCommit, component.commit);
    }
  });
});

test("exact product checkout can infer its release, but a later branch tip is refused", () => {
  withTemporaryDirectory((directory) => {
    const { productCheckout, options } = fixture(directory);
    const result = checkoutReleaseSource({ ...options, tag: undefined, productCheckout, plan: true });
    assert.equal(result.releaseTag, "v1.0.99");
    writeFileSync(join(productCheckout, "later.txt"), "unreleased work\n");
    commit(productCheckout, "later work");
    assert.throws(() => checkoutReleaseSource({ ...options, productCheckout }), /must be exactly at v1.0.99/);
    assert.equal(existsSync(options.destination), false);
  });
});

test("unrelated destination contents and symlinks are preserved and refused", () => {
  withTemporaryDirectory((directory) => {
    const { options } = fixture(directory);
    mkdirSync(options.destination);
    writeFileSync(join(options.destination, "personal.txt"), "keep me\n");
    assert.throws(() => checkoutReleaseSource(options), /unrelated entry personal.txt/);
    assert.equal(readFileSync(join(options.destination, "personal.txt"), "utf8"), "keep me\n");
    const alias = join(directory, "alias");
    symlinkSync(options.destination, alias, "dir");
    assert.throws(() => checkoutReleaseSource({ ...options, destination: alias }), /not a symlink or file/);
  });
});

test("modified, untracked, ignored, and wrong-revision sources are never reset", () => {
  for (const kind of ["modified", "untracked", "ignored", "wrong-revision"]) {
    withTemporaryDirectory((directory) => {
      const { options } = fixture(directory);
      checkoutReleaseSource(options);
      const source = join(options.destination, "rsReticulum");
      if (kind === "modified") writeFileSync(join(source, "api.txt"), "my work\n");
      if (kind === "untracked") writeFileSync(join(source, "mine.txt"), "my work\n");
      if (kind === "ignored") {
        mkdirSync(join(source, "target"));
        writeFileSync(join(source, "target/artifact"), "my work\n");
      }
      if (kind === "wrong-revision") {
        git(source, "config", "user.name", "Fixture");
        git(source, "config", "user.email", "fixture@ratspeak.invalid");
        writeFileSync(join(source, "api.txt"), "my committed work\n");
        commit(source, "keep my commit");
      }
      const before = git(source, "status", "--porcelain", "--ignored");
      const head = git(source, "rev-parse", "HEAD");
      assert.throws(() => checkoutReleaseSource(options), /refusing to change it/);
      assert.equal(git(source, "status", "--porcelain", "--ignored"), before);
      assert.equal(git(source, "rev-parse", "HEAD"), head);
    });
  }
});

test("lightweight product tags and moved integration aliases cannot produce sources", () => {
  withTemporaryDirectory((directory) => {
    const { productCheckout, options } = fixture(directory);
    git(productCheckout, "tag", "-d", options.tag);
    git(productCheckout, "tag", options.tag);
    assert.throws(() => checkoutReleaseSource(options), /must be an annotated tag/);
    assert.equal(existsSync(options.destination), false);
  });
  withTemporaryDirectory((directory) => {
    const { set, options } = fixture(directory);
    const component = set.components[0];
    const source = join(options.localSources, component.name);
    git(source, "tag", "-d", component.integrationTag);
    git(source, "tag", "-a", component.integrationTag, `v${component.version}^{commit}`, "-m", "wrong pin");
    assert.throws(() => checkoutReleaseSource(options), /must point at/);
    assert.equal(existsSync(options.destination), false);
  });
});

test("partial exact graph survives a missing source without installing incomplete checkouts", () => {
  withTemporaryDirectory((directory) => {
    const { productCheckout, options } = fixture(directory);
    mkdirSync(options.destination);
    cpSync(productCheckout, join(options.destination, "Ratspeak"), { recursive: true });
    const missing = join(directory, "incomplete-mirrors");
    mkdirSync(missing);
    cpSync(productCheckout, join(missing, "Ratspeak"), { recursive: true });
    assert.throws(() => checkoutReleaseSource({ ...options, localSources: missing }), /git fetch failed/);
    assert.deepEqual(readdirSync(options.destination), ["Ratspeak"]);
    assert.equal(git(join(options.destination, "Ratspeak"), "status", "--porcelain"), "");
    checkoutReleaseSource(options);
    assert.equal(readdirSync(options.destination).length, 5);
  });
});

test("manifest commit and version checks reuse the release integrity contract", () => {
  for (const defect of ["commit", "version", "mapping", "lockfile"]) {
    withTemporaryDirectory((directory) => {
      const { set, productCheckout, options } = fixture(directory);
      if (defect === "commit") set.components[0].commit = "main";
      if (defect === "version") set.components[0].version = "9.9.9";
      if (defect === "mapping") set.components[0].path = "../../elsewhere";
      if (defect === "lockfile") git(productCheckout, "rm", "src-tauri/Cargo.lock");
      writeFileSync(join(productCheckout, "release/dependency-set.json"), JSON.stringify(set));
      commit(productCheckout, "invalid fixture");
      git(productCheckout, "tag", "-d", options.tag);
      git(productCheckout, "tag", "-a", options.tag, "-m", "invalid fixture");
      assert.throws(() => checkoutReleaseSource(options), /40-character commit SHA|component version|reviewed mapping|missing committed/);
      assert.equal(existsSync(options.destination), false);
    });
  }
});

test("archive copies run explicit tag mode; checkout mode reports missing Git metadata", () => {
  withTemporaryDirectory((directory) => {
    const { options } = fixture(directory);
    const archive = join(directory, "archive/scripts/release");
    mkdirSync(archive, { recursive: true });
    for (const name of ["checkout-release-source.mjs", "source-integrity.mjs"]) {
      cpSync(fileURLToPath(new URL(name, import.meta.url)), join(archive, name));
    }
    const command = join(archive, "checkout-release-source.mjs");
    const args = [command, "--tag", options.tag, "--destination", options.destination, "--local-sources", options.localSources, "--plan"];
    const result = spawnSync(process.execPath, args, { cwd: directory, encoding: "utf8", timeout: 20_000 });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(JSON.parse(result.stdout).releaseTag, options.tag);
    const invalid = spawnSync(process.execPath, [command, "--checkout", resolve(archive, "../.."), "--destination", options.destination], {
      cwd: directory, encoding: "utf8", timeout: 20_000,
    });
    assert.equal(invalid.status, 1);
    assert.match(invalid.stderr, /not a git repository/);
  });
});
