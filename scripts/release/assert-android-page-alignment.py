#!/usr/bin/env python3
"""Read-only, dependency-free ELF/ZIP 16 KiB gate for final APKs and AABs.

Checks every packaged .so (including dependencies), not just the Rust library.
No extraction, external executable, signing key, or device is needed. This does
not replace runtime testing on an actual 16 KiB Android system.

Policy: https://developer.android.com/guide/practices/page-sizes
BundleConfig wire fields (also checked against AGP's bundletool 1.18.1 JAR):
https://github.com/google/bundletool/blob/1.18.1/src/main/proto/config.proto
BundleConfig.optimizations(2).uncompress_native_libraries(2).alignment(2).
"""

import argparse
import json
from pathlib import Path
import struct
import sys
import zipfile

PAGE_SIZE = 16384
MAX_HEADER_BYTES = 1024 * 1024
MAX_LIBRARY_BYTES = 2 * 1024 * 1024 * 1024
MAX_ARCHIVE_BYTES = 8 * 1024 * 1024 * 1024
MAX_ENTRIES = 20000
PT_LOAD = 1
PT_GNU_RELRO = 0x6474E552
ABI_FORMATS = {
    "arm64-v8a": (2, 183), "x86_64": (2, 62),
    "armeabi-v7a": (1, 40), "x86": (1, 3),
}


class AlignmentError(ValueError):
    """Artifact does not meet the release alignment contract."""


def require(condition, reason):
    if not condition:
        raise AlignmentError(reason)


def exact_read(stream, size):
    data = stream.read(size)
    require(len(data) == size, "truncated ELF/ZIP header")
    return data


def check_elf(stream, size, abi=None):
    require(0 < size <= MAX_LIBRARY_BYTES, "native library exceeds inspection bounds")
    ident = exact_read(stream, 16)
    require(ident[:4] == b"\x7fELF", "native .so is not ELF")
    elf_class = ident[4]
    require(elf_class in (1, 2) and ident[5:7] == b"\x01\x01",
            "unsupported ELF class, byte order, or version")
    is_64 = elf_class == 2
    header_format = "<HHIQQQIHHHHHH" if is_64 else "<HHIIIIIHHHHHH"
    header = struct.unpack(header_format, exact_read(stream, struct.calcsize(header_format)))
    elf_type, machine, version, _, phoff, _, _, ehsize, phentsize, phnum, _, _, _ = header
    require(elf_type == 3 and version == 1, "native library must be an ELF shared object")
    require((elf_class, machine) in ABI_FORMATS.values(), "unsupported Android ELF machine")
    if abi is not None:
        require(ABI_FORMATS.get(abi) == (elf_class, machine), "ELF machine/class disagrees with ABI directory")
    expected_header = 64 if is_64 else 52
    expected_ph = 56 if is_64 else 32
    require(ehsize == expected_header and phentsize == expected_ph,
            "invalid ELF header sizes")
    require(0 < phnum <= 4096 and phoff >= ehsize,
            "invalid or unsupported ELF program-header count/offset")
    require(phoff + phnum * phentsize <= min(size, MAX_HEADER_BYTES),
            "ELF program headers exceed inspection bounds")
    stream.seek(phoff)
    loads, relros = [], []
    address_limit = 1 << (64 if is_64 else 32)
    for _ in range(phnum):
        raw = exact_read(stream, phentsize)
        if is_64:
            kind, flags, offset, vaddr, _, filesz, memsz, align = struct.unpack("<IIQQQQQQ", raw)
        else:
            kind, offset, vaddr, _, filesz, memsz, flags, align = struct.unpack("<IIIIIIII", raw)
        require(offset + filesz <= size and vaddr + memsz <= address_limit,
                "ELF segment exceeds file/address bounds")
        if kind == PT_LOAD:
            require(filesz <= memsz, "ELF LOAD file size exceeds memory size")
            require(align > 0 and align & (align - 1) == 0,
                    "ELF LOAD alignment is not a power of two")
            require(offset % align == vaddr % align, "ELF LOAD offsets are not congruent")
            if is_64:
                require(align >= PAGE_SIZE, "64-bit ELF LOAD alignment is below 16384")
            loads.append((vaddr, memsz, flags, align))
        elif kind == PT_GNU_RELRO:
            require(memsz > 0, "empty GNU_RELRO segment")
            relros.append((vaddr, memsz))
    require(loads, "ELF has no LOAD segment")
    require(relros, "ELF has no GNU_RELRO security segment")
    for start, length in relros:
        require(any(flags & 2 and base <= start and start + length <= base + count
                    for base, count, flags, _ in loads),
                "GNU_RELRO is not contained in a writable LOAD")
    return {"bits": 64 if is_64 else 32, "loadSegments": len(loads),
            "minimumLoadAlignment": min(segment[3] for segment in loads), "relro": True}


def varint(data, offset):
    value = 0
    for shift in range(0, 70, 7):
        require(offset < len(data), "truncated BundleConfig varint")
        byte = data[offset]
        offset += 1
        require(shift != 63 or byte <= 1, "oversized BundleConfig varint")
        value |= (byte & 127) << shift
        if byte < 128:
            return value, offset
    raise AlignmentError("oversized BundleConfig varint")


def proto_field(data, wanted, expected_wire):
    """Bounded protobuf scan; reject ambiguous duplicate policy fields."""
    require(len(data) <= MAX_HEADER_BYTES, "BundleConfig exceeds inspection bounds")
    offset, found = 0, []
    while offset < len(data):
        key, offset = varint(data, offset)
        number, wire = key >> 3, key & 7
        require(0 < number < 1 << 29, "invalid BundleConfig field number")
        if wire == 0:
            value, offset = varint(data, offset)
        elif wire in (1, 5):
            length = 8 if wire == 1 else 4
            require(offset + length <= len(data), "truncated BundleConfig fixed field")
            value, offset = data[offset:offset + length], offset + length
        elif wire == 2:
            length, offset = varint(data, offset)
            require(offset + length <= len(data), "truncated BundleConfig message")
            value, offset = data[offset:offset + length], offset + length
        else:
            raise AlignmentError("unsupported BundleConfig wire type")
        if number == wanted:
            require(wire == expected_wire, "wrong BundleConfig policy wire type")
            found.append(value)
    require(len(found) == 1, "missing or duplicate BundleConfig alignment policy")
    return found[0]


def check_bundle_config(data):
    optimization = proto_field(data, 2, 2)
    native = proto_field(optimization, 2, 2)
    require(proto_field(native, 1, 0) == 1, "AAB must explicitly enable uncompressed native libraries")
    alignment = proto_field(native, 2, 0)
    require(alignment in (2, 3), "AAB does not request at least 16 KiB APK page alignment")
    return "PAGE_ALIGNMENT_16K" if alignment == 2 else "PAGE_ALIGNMENT_64K"


def check_zip_alignment(raw, info):
    raw.seek(info.header_offset)
    header = exact_read(raw, 30)
    signature, _, flags, compression, _, _, _, _, _, name_size, extra_size = struct.unpack("<IHHHHHIIIHH", header)
    require(signature == 0x04034B50 and flags == info.flag_bits and compression == info.compress_type,
            "ZIP local header disagrees with central directory")
    encoded_name = exact_read(raw, name_size)
    encoding = "utf-8" if flags & 0x800 else "cp437"
    require(encoded_name.decode(encoding) == info.filename, "ZIP local filename disagrees with directory")
    offset = info.header_offset + 30 + name_size + extra_size
    require(offset % PAGE_SIZE == 0, "stored native library is not 16384-byte ZIP-aligned")


def check_archive(artifact):
    artifact = Path(artifact)
    require(artifact.suffix.lower() in (".apk", ".aab"), "expected an APK or AAB artifact")
    require(0 < artifact.stat().st_size <= MAX_ARCHIVE_BYTES, "artifact exceeds inspection bounds")
    is_bundle = artifact.suffix.lower() == ".aab"
    results = []
    with zipfile.ZipFile(artifact) as archive, artifact.open("rb") as raw:
        entries = archive.infolist()
        require(len(entries) <= MAX_ENTRIES, "ZIP entry count exceeds inspection bounds")
        require(len({entry.filename for entry in entries}) == len(entries), "duplicate ZIP entries")
        for entry in entries:
            if not entry.filename.endswith(".so"):
                continue
            parts = entry.filename.split("/")
            require(not entry.is_dir() and not entry.flag_bits & 1,
                    "native library is a directory or encrypted ZIP entry")
            require(entry.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED),
                    "unsupported native library ZIP compression")
            abi = None
            if "lib" in parts:
                position = parts.index("lib")
                require(position + 2 == len(parts) - 1, "invalid native library path")
                abi = parts[position + 1]
            try:
                with archive.open(entry) as stream:
                    result = check_elf(stream, entry.file_size, abi)
                if not is_bundle and entry.compress_type == zipfile.ZIP_STORED:
                    check_zip_alignment(raw, entry)
            except AlignmentError as error:
                raise AlignmentError(f"{json.dumps(entry.filename)}: {error}") from None
            results.append({"library": entry.filename, **result,
                            "zipStored": entry.compress_type == zipfile.ZIP_STORED})
        require(results, "artifact contains no native libraries")
        alignment = None
        if is_bundle:
            require("BundleConfig.pb" in archive.namelist(), "AAB is missing BundleConfig.pb")
            config = archive.getinfo("BundleConfig.pb")
            require(config.file_size <= MAX_HEADER_BYTES, "BundleConfig exceeds inspection bounds")
            alignment = check_bundle_config(archive.read(config))
    return {"artifact": artifact.name, "pageSize": PAGE_SIZE,
            "libraries64Bit": sum(result["bits"] == 64 for result in results),
            "libraries": results, "bundleApkAlignment": alignment}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path, help="final .apk or .aab to inspect without modifying it")
    args = parser.parse_args()
    try:
        result = check_archive(args.artifact)
    except (AlignmentError, OSError, zipfile.BadZipFile, UnicodeError, RuntimeError) as error:
        print(f"Android page-alignment validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
