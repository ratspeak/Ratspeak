#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import {
  existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  realpathSync, renameSync, rmSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { validateDependencySet } from "./source-integrity.mjs";

const productRepository = "https://github.com/ratspeak/Ratspeak.git";
const scriptRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const commandTimeout = 120_000;

function git(cwd, args) {
  const env = { ...process.env, GIT_TERMINAL_PROMPT: "0", GIT_LFS_SKIP_SMUDGE: "1" };
  // A parent Git command must not redirect operations into its own worktree.
  for (const key of ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"]) delete env[key];
  const result = spawnSync("git", args, {
    cwd, env, encoding: "utf8", timeout: commandTimeout, maxBuffer: 1024 * 1024,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error || result.status !== 0) {
    const detail = (result.error?.message || result.stderr || `exit ${result.status}`).trim();
    throw new Error(`git ${args[0]} failed in ${cwd}: ${detail.slice(-2000)}`);
  }
  return result.stdout.trim();
}

function parseSet(directory) {
  const set = JSON.parse(readFileSync(join(directory, "release/dependency-set.json"), "utf8"));
  validateDependencySet(set);
  return set;
}

function annotatedCommit(directory, tag, expected) {
  const ref = `refs/tags/${tag}`;
  if (git(directory, ["cat-file", "-t", ref]) !== "tag") {
    throw new Error(`${directory}: ${tag} must be an annotated tag`);
  }
  const commit = git(directory, ["rev-parse", "--verify", `${ref}^{commit}`]);
  if (!/^[0-9a-f]{40}$/.test(commit) || (expected && commit !== expected)) {
    throw new Error(`${directory}: ${tag} must point at ${expected}, found ${commit}`);
  }
  return commit;
}

function pristineCheckout(directory) {
  if (!lstatSync(directory).isDirectory()) throw new Error(`${directory}: expected a directory, not a symlink or file`);
  const root = git(directory, ["rev-parse", "--show-toplevel"]);
  if (realpathSync(root) !== realpathSync(directory)) {
    throw new Error(`${directory}: must be its own Git checkout`);
  }
  if (git(directory, ["status", "--porcelain=v1", "--untracked-files=all", "--ignored=matching"])) {
    throw new Error(`${directory}: contains modified, untracked, or ignored files; refusing to change it`);
  }
}

function componentVersion(directory) {
  const source = readFileSync(join(directory, "Cargo.toml"), "utf8");
  const section = source.includes("[workspace.package]") ? "workspace.package" : "package";
  const body = source.split(`[${section}]`)[1]?.split(/^\s*\[/m)[0];
  const version = body?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error(`${directory}: missing version in [${section}]`);
  return version;
}

function verifyCheckout(directory, entry) {
  pristineCheckout(directory);
  if (entry.id === "ratspeak" && git(directory, ["rev-parse", "--is-shallow-repository"]) === "true") {
    throw new Error(`${directory}: full product history is required for API-baseline release verification; use a fresh destination and a complete source mirror`);
  }
  const actual = git(directory, ["rev-parse", "--verify", "HEAD"]);
  if (actual !== entry.fetchCommit) {
    throw new Error(`${directory}: expected commit ${entry.fetchCommit}, found ${actual}; refusing to change it`);
  }
  annotatedCommit(directory, entry.tag, entry.fetchCommit);
  if (entry.predecessorTag) annotatedCommit(directory, entry.predecessorTag, entry.predecessorCommit);
  if (entry.componentVersion && componentVersion(directory) !== entry.componentVersion) {
    throw new Error(`${directory}: component version does not match dependency-set.json`);
  }
}

function initialize(directory, repository) {
  mkdirSync(directory);
  git(directory, ["init", "--quiet"]);
  git(directory, ["remote", "add", "origin", repository]);
}

function fetchTag(directory, tag, shallow = true) {
  git(directory, ["fetch", "--quiet", "--no-tags", ...(shallow ? ["--depth=1"] : []), "origin",
    `refs/tags/${tag}:refs/tags/${tag}`]);
}

function checkout(directory, commit) {
  git(directory, ["-c", "advice.detachedHead=false", "checkout", "--quiet", "--detach", commit]);
}

function destinationContents(destination, entries) {
  if (!existsSync(destination)) return new Set();
  if (!lstatSync(destination).isDirectory()) throw new Error(`${destination}: expected a directory, not a symlink or file`);
  const present = new Set(readdirSync(destination));
  const allowed = new Set(entries.map((entry) => entry.directory));
  for (const name of present) {
    if (!allowed.has(name)) throw new Error(`${destination}: unrelated entry ${name}; refusing to change it`);
    verifyCheckout(join(destination, name), entries.find((entry) => entry.directory === name));
  }
  return present;
}

/** Reconstruct an immutable released graph; never reset or clean existing sources. */
export function checkoutReleaseSource({ destination, tag, productCheckout, localSources, plan = false }) {
  if (!destination) throw new Error("--destination is required (use a dedicated source directory)");
  destination = resolve(destination);
  if (localSources) localSources = resolve(localSources);
  if (productCheckout) productCheckout = resolve(productCheckout);
  if (!tag) {
    productCheckout ??= scriptRoot;
    pristineCheckout(productCheckout);
    tag = `v${parseSet(productCheckout).product.displayVersion}`;
  }
  if (!/^v\d+\.\d+\.\d+[a-z]?$/.test(tag)) throw new Error("--tag must be an exact Ratspeak tag such as v1.0.31");
  if (productCheckout) {
    pristineCheckout(productCheckout);
    const expected = annotatedCommit(productCheckout, tag);
    if (git(productCheckout, ["rev-parse", "HEAD"]) !== expected) {
      throw new Error(`--checkout must be exactly at ${tag}; local edits and branch tips are not release sources`);
    }
  }
  const parent = dirname(destination);
  if (!existsSync(parent) || !lstatSync(parent).isDirectory()) {
    throw new Error(`destination parent must already be a directory: ${parent}`);
  }
  // Stage beside the destination so verified checkouts can be renamed on the same filesystem.
  const temporary = mkdtempSync(join(parent, ".ratspeak-release-source-"));
  try {
    const productDirectory = join(temporary, "Ratspeak");
    const productFetch = productCheckout ?? (localSources ? join(localSources, "Ratspeak") : productRepository);
    initialize(productDirectory, productFetch);
    // Product API-baseline checks require ancestry, not only release-tip objects.
    fetchTag(productDirectory, tag, false);
    const productCommit = annotatedCommit(productDirectory, tag);
    checkout(productDirectory, productCommit);
    const set = parseSet(productDirectory);
    if (tag !== `v${set.product.displayVersion}`) {
      throw new Error(`${tag}: dependency-set product version is ${set.product.displayVersion}`);
    }
    if (readFileSync(join(productDirectory, "VERSION"), "utf8").trim() !== set.product.displayVersion) {
      throw new Error(`${tag}: VERSION does not match dependency-set.json`);
    }
    for (const lockfile of ["Cargo.lock", "src-tauri/Cargo.lock"]) {
      if (!existsSync(join(productDirectory, lockfile))) throw new Error(`${tag}: missing committed ${lockfile}`);
    }
    // Retain the release predecessor required by the existing product-surface verifier.
    fetchTag(productDirectory, set.product.predecessor.tag, false);
    annotatedCommit(productDirectory, set.product.predecessor.tag, set.product.predecessor.commit);
    const entries = [{
      id: "ratspeak", directory: "Ratspeak", repository: productRepository,
      fetchRepository: productFetch, fetchCommit: productCommit, tag,
      predecessorTag: set.product.predecessor.tag, predecessorCommit: set.product.predecessor.commit,
    }, ...set.components.map((component) => {
      if (!component.integrationTag) throw new Error("source checkout requires coordinated integration tags (v1.0.29 or later)");
      return {
        id: component.id, directory: component.name, repository: component.repository,
        fetchRepository: localSources ? join(localSources, component.name) : component.repository,
        fetchCommit: component.commit, tag: component.integrationTag, componentVersion: component.version,
      };
    })];
    let present = destinationContents(destination, entries);
    const result = {
      schemaVersion: 1, releaseTag: tag, destination, mode: plan ? "plan" : "checkout",
      productTagVerified: true, componentTagsVerified: false,
      repositories: entries,
    };
    if (plan) return result;
    verifyCheckout(productDirectory, entries[0]);
    for (const entry of entries.slice(1)) {
      if (present.has(entry.directory)) continue;
      const directory = join(temporary, entry.directory);
      initialize(directory, entry.fetchRepository);
      // The SHA is the fetch input. The readable tag must independently agree.
      git(directory, ["fetch", "--quiet", "--no-tags", "--depth=1", "origin", entry.fetchCommit]);
      fetchTag(directory, entry.tag);
      annotatedCommit(directory, entry.tag, entry.fetchCommit);
      checkout(directory, entry.fetchCommit);
      verifyCheckout(directory, entry);
    }
    // Recheck immediately before installation; existing checkouts are never fetched or rewritten.
    present = destinationContents(destination, entries);
    if (!existsSync(destination)) mkdirSync(destination);
    for (const entry of entries) {
      if (!present.has(entry.directory)) renameSync(join(temporary, entry.directory), join(destination, entry.directory));
    }
    result.componentTagsVerified = true;
    return result;
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

const usage = `Usage: node scripts/release/checkout-release-source.mjs --tag vX.Y.Z --destination DIR [--plan]
       node scripts/release/checkout-release-source.mjs --checkout DIR --destination DIR [--plan]

--tag TAG           Fetch an exact annotated product release tag from published sources.
--checkout DIR      Use a clean product checkout already at its exact annotated release tag.
--destination DIR   Dedicated parent of Ratspeak, rsReticulum, rsLXMF, rsLXST, and lrgp-rs.
--plan              Print pins; fetch product and predecessor tags, leaving destination unchanged.
--local-sources DIR Fetch from local Git mirrors named like the five repositories (offline verification).

Requires Node.js and Git. Never builds, installs, resets, cleans, or pushes.
Existing destination entries must be clean, exact, verified release checkouts.
Downloaded archives can run --tag mode; --checkout requires Git metadata.
`;

function main(args) {
  const options = {};
  const flags = { "--tag": "tag", "--checkout": "productCheckout", "--destination": "destination", "--local-sources": "localSources" };
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--help") { process.stdout.write(usage); return; }
    if (arg === "--plan") { options.plan = true; continue; }
    const key = flags[arg];
    if (!key || !args[index + 1] || args[index + 1].startsWith("--") || options[key]) {
      throw new Error(`invalid or duplicate argument ${arg}\n${usage}`);
    }
    options[key] = args[++index];
  }
  process.stdout.write(`${JSON.stringify(checkoutReleaseSource(options), null, 2)}\n`);
}

if (process.argv[1] && realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(`release source: ${error.message}`); process.exitCode = 1; }
}
