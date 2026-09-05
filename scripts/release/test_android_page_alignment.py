"""Synthetic, dependency-free regression tests for final Android packages."""

import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import tempfile
import unittest
import warnings
import zipfile

SCRIPT = Path(__file__).with_name("assert-android-page-alignment.py")
SPEC = importlib.util.spec_from_file_location("android_page_alignment", SCRIPT)
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)
PAGE = 16384


def elf(bits=64, machine=None, alignment=PAGE, second_alignment=None, relro=True):
    machine = machine if machine is not None else (183 if bits == 64 else 40)
    is64 = bits == 64
    header_size, phsize = (64, 56) if is64 else (52, 32)
    count = 3 if relro else 2
    ident = b"\x7fELF" + bytes([2 if is64 else 1, 1, 1]) + b"\0" * 9
    header = struct.pack("<HHIQQQIHHHHHH" if is64 else "<HHIIIIIHHHHHH",
                         3, machine, 1, 0, header_size, 0, 0,
                         header_size, phsize, count, 0, 0, 0)

    def ph(kind, flags, offset, size, align):
        if is64:
            return struct.pack("<IIQQQQQQ", kind, flags, offset, offset, offset, size, size, align)
        return struct.pack("<IIIIIIII", kind, offset, offset, offset, size, size, flags, align)

    headers = ph(1, 5, 0, PAGE, alignment) + ph(1, 6, PAGE, PAGE, second_alignment or alignment)
    if relro:
        headers += ph(gate.PT_GNU_RELRO, 4, PAGE, PAGE, 1)
    return (ident + header + headers).ljust(2 * PAGE, b"\0")


def field(number, value, message=False):
    assert number < 16
    if message:
        assert len(value) < 128
        return bytes([(number << 3) | 2, len(value)]) + value
    return bytes([number << 3, value])


def bundle_config(alignment=2, enabled=1):
    native = field(1, enabled) + field(2, alignment)
    return field(2, field(2, native, True), True)


class AndroidPageAlignmentTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="android page alignment ")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def archive(self, entries, *, bundle=False, align=True, compressed=False, config=None):
        artifact = self.root / ("fixture with spaces.aab" if bundle else "fixture with spaces.apk")
        with zipfile.ZipFile(artifact, "w") as archive:
            for name, contents in entries:
                info = zipfile.ZipInfo(name)
                info.compress_type = zipfile.ZIP_DEFLATED if compressed else zipfile.ZIP_STORED
                if align and not bundle and not compressed:
                    padding = -(archive.fp.tell() + 30 + len(name.encode("utf-8"))) % PAGE
                    if padding < 4:
                        padding += PAGE
                    info.extra = struct.pack("<HH", 0xCAFE, padding - 4) + b"\0" * (padding - 4)
                archive.writestr(info, contents)
            if bundle and config is not None:
                archive.writestr("BundleConfig.pb", config)
        return artifact

    def test_all_64_bit_libraries_and_32_bit_relro_are_checked_read_only(self):
        artifact = self.archive([
            ("lib/arm64-v8a/libratspeak.so", elf()),
            ("lib/arm64-v8a/libdependency.so", elf()),
            ("lib/x86_64/libratspeak.so", elf(machine=62)),
            ("lib/armeabi-v7a/libratspeak.so", elf(32, alignment=4096)),
        ])
        before = hashlib.sha256(artifact.read_bytes()).digest()
        result = gate.check_archive(artifact)
        self.assertEqual(result["libraries64Bit"], 3)
        self.assertEqual(len(result["libraries"]), 4)
        self.assertEqual(hashlib.sha256(artifact.read_bytes()).digest(), before)

    def test_rejects_4k_and_bad_later_load_not_just_first_segment(self):
        for contents in (elf(alignment=4096), elf(second_alignment=4096), elf(second_alignment=8192)):
            with self.subTest():
                with self.assertRaisesRegex(gate.AlignmentError, "below 16384"):
                    gate.check_elf(io.BytesIO(contents), len(contents))

    def test_rejects_bad_packaged_dependency_and_wrong_abi(self):
        artifact = self.archive([("lib/arm64-v8a/libmain.so", elf()),
                                 ("lib/arm64-v8a/libdependency.so", elf(alignment=4096))])
        with self.assertRaisesRegex(gate.AlignmentError, "libdependency"):
            gate.check_archive(artifact)
        artifact = self.archive([("lib/arm64-v8a/libwrong.so", elf(32))])
        with self.assertRaisesRegex(gate.AlignmentError, "disagrees with ABI"):
            gate.check_archive(artifact)

    def test_relro_required_for_32_and_64_bit(self):
        for bits in (32, 64):
            contents = elf(bits, relro=False)
            with self.subTest(bits=bits), self.assertRaisesRegex(gate.AlignmentError, "GNU_RELRO"):
                gate.check_elf(io.BytesIO(contents), len(contents))

    def test_relro_must_be_backed_by_writable_load(self):
        contents = bytearray(elf())
        struct.pack_into("<I", contents, 64 + 56 + 4, 4)
        with self.assertRaisesRegex(gate.AlignmentError, "writable LOAD"):
            gate.check_elf(io.BytesIO(contents), len(contents))

    def test_zip_stored_alignment_is_independent_of_elf_alignment(self):
        artifact = self.archive([("lib/arm64-v8a/libok.so", elf())], align=False)
        with self.assertRaisesRegex(gate.AlignmentError, "ZIP-aligned"):
            gate.check_archive(artifact)
        artifact = self.archive([("lib/arm64-v8a/libok.so", elf())], compressed=True)
        self.assertEqual(gate.check_archive(artifact)["libraries64Bit"], 1)

    def test_aab_checks_requested_apk_alignment_not_aab_zip_offsets(self):
        for alignment, label in ((2, "PAGE_ALIGNMENT_16K"), (3, "PAGE_ALIGNMENT_64K")):
            artifact = self.archive([("base/lib/arm64-v8a/libok.so", elf())],
                                    bundle=True, config=bundle_config(alignment))
            self.assertEqual(gate.check_archive(artifact)["bundleApkAlignment"], label)

    def test_aab_rejects_missing_4k_disabled_or_ambiguous_policy(self):
        for config in (None, b"", bundle_config(1), bundle_config(0), bundle_config(2, 0),
                       bundle_config(2) + bundle_config(1), b"\x12\xff", b"\x17"):
            with self.subTest(config=config):
                artifact = self.archive([("base/lib/x86_64/libok.so", elf(machine=62))],
                                        bundle=True, config=config)
                with self.assertRaises(gate.AlignmentError):
                    gate.check_archive(artifact)

    def test_bundle_proto_skips_bounded_unknown_fields(self):
        config = field(1, b"version", True) + bundle_config() + field(4, 1)
        self.assertEqual(gate.check_bundle_config(config), "PAGE_ALIGNMENT_16K")
        for invalid in (b"\x00", b"\x12" + b"\xff" * 11,
                        b"\x09\x00", b"\x12\x03\x12\x04\x00"):
            with self.subTest(), self.assertRaises(gate.AlignmentError):
                gate.check_bundle_config(invalid)

    def test_truncated_oversized_and_malformed_elf_fail_closed(self):
        for contents in (b"", b"not ELF", elf()[:63], elf()[:100]):
            with self.subTest(), self.assertRaises(gate.AlignmentError):
                gate.check_elf(io.BytesIO(contents), len(contents))
        contents = bytearray(elf())
        struct.pack_into("<Q", contents, 32, 2**40)
        with self.assertRaisesRegex(gate.AlignmentError, "bounds"):
            gate.check_elf(io.BytesIO(contents), len(contents))
        with self.assertRaisesRegex(gate.AlignmentError, "bounds"):
            gate.check_elf(io.BytesIO(elf()), gate.MAX_LIBRARY_BYTES + 1)

    def test_load_alignment_power_and_congruence(self):
        for offset, value, message in ((112, 24576, "power of two"), (72, 1, "congruent")):
            contents = bytearray(elf())
            struct.pack_into("<Q", contents, offset, value)
            with self.subTest(), self.assertRaisesRegex(gate.AlignmentError, message):
                gate.check_elf(io.BytesIO(contents), len(contents))

    def test_empty_duplicate_and_non_elf_archives_do_not_false_pass(self):
        with self.assertRaisesRegex(gate.AlignmentError, "no native"):
            gate.check_archive(self.archive([]))
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            artifact = self.archive([("lib/arm64-v8a/libok.so", elf())] * 2)
        with self.assertRaisesRegex(gate.AlignmentError, "duplicate"):
            gate.check_archive(artifact)
        with self.assertRaisesRegex(gate.AlignmentError, "not ELF"):
            gate.check_archive(self.archive([("lib/arm64-v8a/libbad.so", b"not a shared object")]))

    def test_workflows_gate_actual_apk_and_aab_before_staging(self):
        root = SCRIPT.parents[2]
        release = (root / ".github/workflows/release-android.yml").read_text()
        ci = (root / ".github/workflows/ci.yml").read_text()
        self.assertIn('python3 scripts/release/assert-android-page-alignment.py "$artifact"', release)
        self.assertIn("-name '*.apk' -o -name '*.aab'", release)
        self.assertIn('python3 ../scripts/release/assert-android-page-alignment.py "$apk"', ci)
        self.assertIn("python3 -m unittest discover -s scripts/release -p 'test_android_page_alignment.py'", ci)


if __name__ == "__main__":
    unittest.main()
