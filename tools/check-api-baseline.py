#!/usr/bin/env python3
"""Validate or refresh the reviewed public-API compatibility snapshots."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONFIG_PATH = ROOT / "api-stability.json"
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
HASH_PATTERN = re.compile(r"^[0-9a-f]{64}$")
TIERS = {
    "candidate-stable",
    "provisional",
    "experimental",
    "application-internal",
    "tool-internal",
}


def fail(message: str) -> None:
    print(f"api baseline: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(args: list[str], *, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        args,
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        if result.stdout:
            print(result.stdout, file=sys.stderr, end="")
        if result.stderr:
            print(result.stderr, file=sys.stderr, end="")
        fail(f"command failed ({result.returncode}): {' '.join(args)}")
    return result.stdout


def load_config() -> dict[str, object]:
    try:
        config = json.loads(CONFIG_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read {CONFIG_PATH.name}: {error}")
    if config.get("schemaVersion") != 1:
        fail("unsupported api-stability schema")
    return config


def public_packages(manifest_path: str) -> set[str]:
    metadata = json.loads(
        run(
            [
                "cargo",
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
                manifest_path,
            ]
        )
    )
    return {
        str(package["name"])
        for package in metadata["packages"]
        if any("lib" in target["kind"] for target in package["targets"])
    }


def validate_metadata(
    config: dict[str, object], *, updating: bool = False
) -> list[dict[str, object]]:
    tool = config.get("tool")
    if not isinstance(tool, dict):
        fail("tool contract is missing")
    if tool.get("name") != "cargo-public-api" or tool.get("version") != "0.52.0":
        fail("tool must be cargo-public-api 0.52.0")
    if tool.get("rustdocToolchain") != "nightly-2026-08-01":
        fail("rustdoc toolchain must be nightly-2026-08-01")
    if tool.get("target") != "aarch64-apple-darwin":
        fail("API target must be aarch64-apple-darwin")
    if tool.get("arguments") != ["--all-features", "-sss"]:
        fail("API arguments must remain all-features and simplified")

    baseline = config.get("baseline")
    if not isinstance(baseline, dict):
        fail("baseline identity is missing")
    commit = baseline.get("commit")
    if not isinstance(commit, str) or not SHA_PATTERN.fullmatch(commit):
        fail("baseline commit must be a full Git commit")
    commit_check = subprocess.run(
        ["git", "cat-file", "-e", f"{commit}^{{commit}}"], cwd=ROOT
    )
    if commit_check.returncode != 0:
        fail(f"baseline commit {commit} is unavailable in this checkout")
    ancestor_check = subprocess.run(
        ["git", "merge-base", "--is-ancestor", commit, "HEAD"], cwd=ROOT
    )
    if ancestor_check.returncode != 0:
        fail(f"baseline commit {commit} is not an ancestor of HEAD")

    packages = config.get("packages")
    if not isinstance(packages, list) or not packages:
        fail("package ledger is empty")

    manifests: dict[str, set[str]] = {}
    seen_names: set[tuple[str, str]] = set()
    for package in packages:
        if not isinstance(package, dict):
            fail("package ledger entry is not an object")
        name = package.get("name")
        manifest = package.get("manifestPath")
        snapshot = package.get("snapshot")
        if not isinstance(name, str) or not isinstance(manifest, str):
            fail("package name or manifest path is invalid")
        if (manifest, name) in seen_names:
            fail(f"duplicate package ledger entry {manifest}:{name}")
        seen_names.add((manifest, name))
        if package.get("tier") not in TIERS:
            fail(f"{name} has an unknown stability tier")
        if package.get("compatibility") != "reviewed-snapshot":
            fail(f"{name} must use the reviewed-snapshot compatibility policy")
        if not isinstance(snapshot, str) or not snapshot.startswith("api-baseline/"):
            fail(f"{name} has an invalid snapshot path")
        snapshot_path = ROOT / snapshot
        if not snapshot_path.is_file() and not updating:
            fail(f"{name} snapshot is missing: {snapshot}")
        if not updating:
            snapshot_bytes = snapshot_path.read_bytes()
            expected_hash = package.get("sha256")
            actual_hash = hashlib.sha256(snapshot_bytes).hexdigest()
            if not isinstance(expected_hash, str) or not HASH_PATTERN.fullmatch(expected_hash):
                fail(f"{name} has an invalid snapshot hash")
            if actual_hash != expected_hash:
                fail(f"{name} snapshot hash is {actual_hash}, expected {expected_hash}")
            item_count = len(snapshot_bytes.decode("utf-8").splitlines())
            if package.get("itemCount") != item_count:
                fail(
                    f"{name} snapshot has {item_count} items, "
                    f"expected {package.get('itemCount')}"
                )
        manifests.setdefault(manifest, public_packages(manifest))

    for manifest, actual_names in manifests.items():
        recorded_names = {
            str(package["name"])
            for package in packages
            if package["manifestPath"] == manifest
        }
        if recorded_names != actual_names:
            fail(
                f"library inventory drift for {manifest}: "
                f"recorded={sorted(recorded_names)}, actual={sorted(actual_names)}"
            )
    return packages


def tool_path_and_environment(config: dict[str, object]) -> tuple[str, dict[str, str]]:
    tool_config = config["tool"]
    assert isinstance(tool_config, dict)
    executable = os.environ.get("CARGO_PUBLIC_API") or shutil.which("cargo-public-api")
    if not executable:
        fail("cargo-public-api is unavailable; install exact version 0.52.0")
    version = run([executable, "public-api", "--version"]).strip()
    if version != "cargo-public-api 0.52.0":
        fail(f"unexpected cargo-public-api version: {version!r}")
    environment = os.environ.copy()
    environment["RUSTUP_TOOLCHAIN"] = str(tool_config["rustdocToolchain"])
    return executable, environment


def render_snapshot(
    executable: str,
    environment: dict[str, str],
    config: dict[str, object],
    package: dict[str, object],
) -> bytes:
    tool = config["tool"]
    assert isinstance(tool, dict)
    arguments = tool["arguments"]
    assert isinstance(arguments, list)
    output = run(
        [
            executable,
            "public-api",
            "--manifest-path",
            str(package["manifestPath"]),
            "--package",
            str(package["name"]),
            *[str(argument) for argument in arguments],
            "--target",
            str(tool["target"]),
            "--color",
            "never",
        ],
        env=environment,
    )
    return (output.rstrip() + "\n").encode("utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata-only", action="store_true")
    parser.add_argument("--update", action="store_true")
    args = parser.parse_args()
    if args.metadata_only and args.update:
        fail("--metadata-only and --update are mutually exclusive")

    config = load_config()
    packages = validate_metadata(config, updating=args.update)
    if args.metadata_only:
        print("api baseline metadata: ok")
        return

    executable, environment = tool_path_and_environment(config)
    changed = False
    for package in packages:
        rendered = render_snapshot(executable, environment, config, package)
        snapshot_path = ROOT / str(package["snapshot"])
        if args.update:
            if not snapshot_path.is_file() or snapshot_path.read_bytes() != rendered:
                snapshot_path.write_bytes(rendered)
                changed = True
            package["itemCount"] = len(rendered.decode("utf-8").splitlines())
            package["sha256"] = hashlib.sha256(rendered).hexdigest()
        elif snapshot_path.read_bytes() != rendered:
            fail(
                f"{package['name']} public API differs from {package['snapshot']}; "
                "review the diff before running --update"
            )

    if args.update:
        updated = json.dumps(config, indent=2) + "\n"
        if CONFIG_PATH.read_text(encoding="utf-8") != updated:
            CONFIG_PATH.write_text(updated, encoding="utf-8")
            changed = True
        print("api baseline snapshots updated" if changed else "api baseline unchanged")
    else:
        print("api baseline: ok")


if __name__ == "__main__":
    main()
