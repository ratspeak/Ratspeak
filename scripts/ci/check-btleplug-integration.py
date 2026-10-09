#!/usr/bin/env python3
"""Keep the packaged Android bridge and its Rust/JNI dependencies in lockstep."""
import argparse
import importlib.util
import hashlib
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]
source = json.loads((ROOT / "third_party/btleplug/source.json").read_text())
version = source["version"]
java = ROOT / source["javaRoot"]
actual = {
    str(p.relative_to(java)): hashlib.sha256(p.read_bytes()).hexdigest()
    for namespace in ("com/nonpolynomial/btleplug", "io/github/gedgygedgy/rust")
    for p in (java / namespace).rglob("*.java")
}
assert actual == source["sha256"], "Bundled Java differs from the reviewed btleplug inventory"
upstream = source["upstreamSha256"]
assert set(upstream) == set(actual), "Upstream and shipped Java inventories differ"
patched_files = set()
for patch in source["patches"]:
    assert hashlib.sha256((ROOT / patch["path"]).read_bytes()).hexdigest() == patch["sha256"], "Reviewed btleplug patch changed"
    patched_files.update(patch["files"])
assert {name for name in actual if actual[name] != upstream[name]} == patched_files, "Undocumented btleplug Java changes"
manifest = tomllib.loads((ROOT / "src-tauri/Cargo.toml").read_text())
android = manifest["target"]['cfg(target_os = "android")']["dependencies"]
assert android["btleplug"] == f"={version}", "Android btleplug dependency/Java version mismatch"
assert android["jni-btleplug"] == {"package": "jni", "version": "0.22"}
assert android["jni"] == "0.19", "Native Android JNI migration requires separate review"
for path in ("Cargo.lock", "src-tauri/Cargo.lock"):
    lock = tomllib.loads((ROOT / path).read_text())
    versions = {p["version"] for p in lock["package"] if p["name"] == "btleplug"}
    assert versions == {version}, f"{path}: expected only btleplug {version}, found {versions}"
proguard = (ROOT / "src-tauri/gen/android/app/proguard-rules.pro").read_text()
for namespace in ("com.nonpolynomial.btleplug.android.impl", "io.github.gedgygedgy.rust"):
    assert f"-keep class {namespace}.** {{ *; }}" in proguard, f"Missing JNI keep rule: {namespace}"
gradle = (ROOT / "src-tauri/gen/android/app/build.gradle.kts").read_text()
assert int(re.search(r"minSdk\s*=\s*(\d+)", gradle).group(1)) >= 24
print(f"btleplug integration: {version}, {len(actual)} Java files, {len(source['patches'])} reviewed patch; JNI versions and shrinker rules aligned")

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--archive", type=Path, help="also verify all bridge method descriptors in the final APK/AAB")
args = parser.parse_args()
if args.archive:
    spec = importlib.util.spec_from_file_location("android_jni", ROOT / "scripts/release/assert-android-jni-boundaries.py")
    boundary = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(boundary)
    contract = json.loads((ROOT / "third_party/btleplug/jni-boundaries.json").read_text())
    assert contract["version"] == version
    boundary.verify_archive(contract, args.archive)
