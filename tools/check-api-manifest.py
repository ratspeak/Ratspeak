#!/usr/bin/env python3
"""Verify the reviewed Cargo manifest and feature contract for library packages."""

from __future__ import annotations

import argparse
import difflib
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LEDGER_PATH = ROOT / "api" / "stability.json"
CONTRACT_PATH = ROOT / "api" / "snapshots" / "manifest-contract.json"


def fail(message: str) -> None:
    print(f"api manifest contract: {message}", file=sys.stderr)
    raise SystemExit(1)


def metadata(manifest_path: str) -> dict[str, object]:
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
            manifest_path,
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        if result.stderr:
            print(result.stderr, file=sys.stderr, end="")
        fail(f"cargo metadata failed for {manifest_path}")
    return json.loads(result.stdout)


def dependency_contract(dependency: dict[str, object]) -> dict[str, object]:
    source = dependency.get("source")
    return {
        "name": dependency["name"],
        "rename": dependency.get("rename"),
        "kind": dependency.get("kind"),
        "target": dependency.get("target"),
        "requirement": dependency["req"],
        "optional": dependency["optional"],
        "defaultFeatures": dependency["uses_default_features"],
        "features": sorted(dependency["features"]),
        "sourceKind": "registry" if source else "path",
    }


def package_contract(package: dict[str, object]) -> dict[str, object]:
    targets = []
    for target in package["targets"]:
        if "lib" not in target["kind"]:
            continue
        targets.append(
            {
                "name": target["name"],
                "kind": sorted(target["kind"]),
                "crateTypes": sorted(target["crate_types"]),
                "requiredFeatures": sorted(target.get("required-features", [])),
            }
        )
    dependencies = [
        dependency_contract(dependency)
        for dependency in package["dependencies"]
        if dependency.get("kind") != "dev"
    ]
    dependencies.sort(
        key=lambda item: (
            str(item["kind"]),
            str(item["target"]),
            str(item["rename"]),
            str(item["name"]),
        )
    )
    return {
        "name": package["name"],
        "version": package["version"],
        "rustVersion": package.get("rust_version"),
        "edition": package["edition"],
        "features": {
            name: sorted(values)
            for name, values in sorted(package["features"].items())
        },
        "targets": sorted(targets, key=lambda item: str(item["name"])),
        "dependencies": dependencies,
    }


def render_contract() -> str:
    ledger = json.loads(LEDGER_PATH.read_text(encoding="utf-8"))
    manifests = sorted({entry["manifestPath"] for entry in ledger["packages"]})
    recorded_names = {entry["name"] for entry in ledger["packages"]}
    packages: list[dict[str, object]] = []
    for manifest in manifests:
        for package in metadata(manifest)["packages"]:
            if package["name"] in recorded_names:
                packages.append(package_contract(package))
    actual_names = {entry["name"] for entry in packages}
    if actual_names != recorded_names:
        fail(
            f"package inventory differs: expected={sorted(recorded_names)}, "
            f"actual={sorted(actual_names)}"
        )
    contract = {
        "schemaVersion": 1,
        "scope": "library Cargo manifests, public features, targets, and non-dev dependencies",
        "packages": sorted(packages, key=lambda item: str(item["name"])),
    }
    return json.dumps(contract, indent=2) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--print",
        action="store_true",
        dest="print_contract",
        help="print the current canonical contract for reviewed application",
    )
    parser.add_argument(
        "--update",
        action="store_true",
        help="write the current reviewed manifest contract",
    )
    args = parser.parse_args()
    if args.print_contract and args.update:
        fail("--print and --update are mutually exclusive")
    rendered = render_contract()
    if args.print_contract:
        print(rendered, end="")
        return
    if args.update:
        CONTRACT_PATH.write_text(rendered, encoding="utf-8")
        print("api manifest contract updated")
        return
    if not CONTRACT_PATH.is_file():
        fail(f"missing {CONTRACT_PATH.relative_to(ROOT)}")
    expected = CONTRACT_PATH.read_text(encoding="utf-8")
    if expected != rendered:
        print(
            "".join(
                difflib.unified_diff(
                    expected.splitlines(keepends=True),
                    rendered.splitlines(keepends=True),
                    fromfile=str(CONTRACT_PATH.relative_to(ROOT)),
                    tofile="current Cargo metadata",
                )
            ),
            file=sys.stderr,
            end="",
        )
        fail("manifest, feature, target, or dependency contract drift")
    print("api manifest contract: ok")


if __name__ == "__main__":
    main()
