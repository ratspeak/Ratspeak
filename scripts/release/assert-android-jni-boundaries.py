#!/usr/bin/env python3
"""Verify Ratspeak's name-based Rust/Kotlin boundary in source and final DEX."""

from __future__ import annotations

import argparse
import json
import re
import struct
import sys
import zipfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = Path(__file__).with_name("android-jni-boundaries.json")
PROGUARD_RULES = REPO_ROOT / "src-tauri/gen/android/app/proguard-rules.pro"
KOTLIN_ROOT = REPO_ROOT / "src-tauri/gen/android/app/src/main/java/org/ratspeak/android"
CUSTOM_CLASS_RE = re.compile(r'org\.ratspeak\.android\.Ratspeak[A-Za-z0-9_$]+')


class BoundaryError(RuntimeError):
    pass


def u16(data: bytes, offset: int) -> int:
    return struct.unpack_from("<H", data, offset)[0]


def u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def uleb128(data: bytes, offset: int) -> tuple[int, int]:
    value = 0
    shift = 0
    for _ in range(5):
        byte = data[offset]
        offset += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, offset
        shift += 7
    raise BoundaryError("invalid DEX ULEB128 value")


def dex_string(data: bytes, offset: int) -> str:
    _, offset = uleb128(data, offset)
    end = data.index(0, offset)
    # JVM identifiers and descriptors in this contract are ASCII. Replacement
    # decoding safely skips unrelated modified-UTF-8 strings in the same DEX.
    return data[offset:end].decode("utf-8", errors="replace")


def defined_methods(data: bytes, label: str) -> set[tuple[str, str, str]]:
    if len(data) < 0x70 or not data.startswith(b"dex\n"):
        raise BoundaryError(f"{label} is not a standard DEX file")

    string_count, string_offset = u32(data, 0x38), u32(data, 0x3C)
    type_count, type_offset = u32(data, 0x40), u32(data, 0x44)
    proto_count, proto_offset = u32(data, 0x48), u32(data, 0x4C)
    method_count, method_offset = u32(data, 0x58), u32(data, 0x5C)
    class_count, class_offset = u32(data, 0x60), u32(data, 0x64)

    strings = [dex_string(data, u32(data, string_offset + index * 4)) for index in range(string_count)]
    types = [strings[u32(data, type_offset + index * 4)] for index in range(type_count)]

    protos: list[str] = []
    for index in range(proto_count):
        item = proto_offset + index * 12
        return_type = types[u32(data, item + 4)]
        parameters_offset = u32(data, item + 8)
        parameters: list[str] = []
        if parameters_offset:
            parameter_count = u32(data, parameters_offset)
            parameters = [
                types[u16(data, parameters_offset + 4 + parameter * 2)]
                for parameter in range(parameter_count)
            ]
        protos.append(f"({''.join(parameters)}){return_type}")

    method_ids: list[tuple[str, str, str]] = []
    for index in range(method_count):
        item = method_offset + index * 8
        method_ids.append(
            (
                types[u16(data, item)],
                strings[u32(data, item + 4)],
                protos[u16(data, item + 2)],
            )
        )

    defined_indices: set[int] = set()
    for index in range(class_count):
        class_data_offset = u32(data, class_offset + index * 32 + 24)
        if not class_data_offset:
            continue
        offset = class_data_offset
        static_fields, offset = uleb128(data, offset)
        instance_fields, offset = uleb128(data, offset)
        direct_methods, offset = uleb128(data, offset)
        virtual_methods, offset = uleb128(data, offset)
        for _ in range(static_fields + instance_fields):
            _, offset = uleb128(data, offset)
            _, offset = uleb128(data, offset)
        for count in (direct_methods, virtual_methods):
            method_index = 0
            for _ in range(count):
                difference, offset = uleb128(data, offset)
                method_index += difference
                _, offset = uleb128(data, offset)
                _, offset = uleb128(data, offset)
                defined_indices.add(method_index)

    try:
        return {method_ids[index] for index in defined_indices}
    except IndexError as error:
        raise BoundaryError(f"{label} has an invalid method index") from error


def load_manifest(path: Path) -> dict:
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise BoundaryError(f"cannot read boundary manifest {path}: {error}") from error
    if manifest.get("schemaVersion") != 1:
        raise BoundaryError("Android JNI boundary manifest must use schemaVersion 1")
    classes = manifest.get("classes")
    if not isinstance(classes, list) or not classes:
        raise BoundaryError("Android JNI boundary manifest has no classes")
    seen_classes: set[str] = set()
    for entry in classes:
        name = entry.get("name")
        methods = entry.get("methods")
        if not isinstance(name, str) or not CUSTOM_CLASS_RE.fullmatch(name):
            raise BoundaryError(f"invalid Android JNI boundary class: {name!r}")
        if name in seen_classes:
            raise BoundaryError(f"duplicate Android JNI boundary class: {name}")
        seen_classes.add(name)
        if not isinstance(methods, list) or not methods:
            raise BoundaryError(f"Android JNI boundary class has no methods: {name}")
        normalized = [tuple(method) for method in methods]
        if any(len(method) != 2 for method in normalized) or len(set(normalized)) != len(normalized):
            raise BoundaryError(f"invalid or duplicate methods for Android JNI boundary class: {name}")
    return manifest


def verify_source(manifest: dict) -> None:
    rules = PROGUARD_RULES.read_text(encoding="utf-8")
    declared = {entry["name"] for entry in manifest["classes"]}
    failures: list[str] = []

    for class_name in sorted(declared):
        keep = re.compile(rf"^-keep class {re.escape(class_name)} \{{ \*; \}}$", re.MULTILINE)
        if not keep.search(rules):
            failures.append(f"missing exact R8 keep rule for {class_name}")
        kotlin_file = KOTLIN_ROOT / f"{class_name.rsplit('.', 1)[-1]}.kt"
        if not kotlin_file.is_file():
            failures.append(f"missing Kotlin source for {class_name}: {kotlin_file}")

    referenced: set[str] = set()
    for source_name in manifest.get("rustSources", []):
        source_path = (REPO_ROOT / source_name).resolve()
        if not source_path.is_file():
            failures.append(f"missing Rust boundary source: {source_path}")
            continue
        referenced.update(CUSTOM_CLASS_RE.findall(source_path.read_text(encoding="utf-8")))
    for class_name in sorted(referenced - declared):
        failures.append(f"Rust references unregistered Android JNI class {class_name}")

    if failures:
        raise BoundaryError("Android JNI source contract failed:\n- " + "\n- ".join(failures))
    print(f"Android JNI source contract: {len(declared)} classes are R8-pinned")


def verify_archive(manifest: dict, archive: Path) -> None:
    if not archive.is_file():
        raise BoundaryError(f"Android artifact does not exist: {archive}")
    methods: set[tuple[str, str, str]] = set()
    try:
        with zipfile.ZipFile(archive) as package:
            dex_names = sorted(name for name in package.namelist() if name.endswith(".dex"))
            if not dex_names:
                raise BoundaryError(f"Android artifact contains no DEX files: {archive}")
            for dex_name in dex_names:
                methods.update(defined_methods(package.read(dex_name), f"{archive}:{dex_name}"))
    except zipfile.BadZipFile as error:
        raise BoundaryError(f"Android artifact is not a valid ZIP archive: {archive}") from error

    missing: list[str] = []
    checked = 0
    for entry in manifest["classes"]:
        descriptor = f"L{entry['name'].replace('.', '/')};"
        for method_name, method_descriptor in entry["methods"]:
            checked += 1
            if (descriptor, method_name, method_descriptor) not in methods:
                missing.append(f"{entry['name']}.{method_name}{method_descriptor}")
    if missing:
        raise BoundaryError(
            f"Android artifact is missing {len(missing)} required JNI boundary method(s):\n- "
            + "\n- ".join(missing)
        )
    print(f"Android JNI artifact contract: {checked} methods preserved in {archive}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("source", help="verify manifest, Kotlin sources, Rust references, and R8 rules")
    archive_parser = subparsers.add_parser("archive", help="verify required classes and methods in an APK or AAB")
    archive_parser.add_argument("artifact", type=Path)
    args = parser.parse_args()

    try:
        manifest = load_manifest(args.manifest)
        if args.command == "source":
            verify_source(manifest)
        else:
            verify_archive(manifest, args.artifact)
    except (BoundaryError, OSError, ValueError, IndexError, struct.error) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
