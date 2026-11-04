#!/usr/bin/env python3
"""Tests of package.py's Windows checks, on fake PE images: `python3 packaging/release/test_package.py`.

Any system with Python 3.11 or later; no Windows tool needed.
"""

import contextlib
import io
import struct
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.dont_write_bytecode = True  # no __pycache__ in the repository
sys.path.insert(0, str(Path(__file__).resolve().parent))
import package  # noqa: E402

SECTION_RVA = 0x1000
SECTION_RAW = 0x200


def fake_pe(imports: list[str], delay: list[str] | None = None, pe32: bool = False) -> bytes:
    """A minimal PE32+ (or PE32) image whose one section holds an import table naming
    `imports` and a delay-load import table naming `delay` (each DLL with no function:
    its thunk lists are one shared null entry). llvm-readobj --coff-imports reads it."""
    delay = delay or []
    names = b""
    name_rvas = {}
    tables = 20 * (len(imports) + 1) + (32 * (len(delay) + 1) if delay else 0)
    empty = SECTION_RVA + tables  # an empty thunk list, then the module handle slot
    for dll in [*imports, *delay]:
        name_rvas[dll] = empty + 16 + len(names)
        names += dll.encode() + b"\0"
    section = b"".join(struct.pack("<5I", empty, 0, 0, name_rvas[d], empty) for d in imports) + bytes(20)
    delay_rva = SECTION_RVA + len(section)
    if delay:
        section += b"".join(struct.pack("<8I", 1, name_rvas[d], empty + 8, empty, empty, 0, 0, 0) for d in delay)
        section += bytes(32)
    section += bytes(16) + names

    optional_size = 224 if pe32 else 240
    optional = bytearray(optional_size)
    struct.pack_into("<H", optional, 0, 0x10B if pe32 else 0x20B)
    directories = 96 if pe32 else 112
    struct.pack_into("<I", optional, directories - 4, 16)
    struct.pack_into("<II", optional, directories + 8 * 1, SECTION_RVA, 20 * (len(imports) + 1))
    if delay:
        struct.pack_into("<II", optional, directories + 8 * 13, delay_rva, 32 * (len(delay) + 1))
    header = bytearray(0x40)
    header[:2] = b"MZ"
    struct.pack_into("<I", header, 0x3C, 0x40)
    header += b"PE\0\0" + struct.pack("<HHIIIHH", 0x14C if pe32 else 0x8664, 1, 0, 0, 0, optional_size, 0x22)
    header += optional
    header += struct.pack("<8s4I16x", b".idata", len(section), SECTION_RVA, len(section), SECTION_RAW)
    assert len(header) <= SECTION_RAW
    return bytes(header) + bytes(SECTION_RAW - len(header)) + section


CLEAN = ["KERNEL32.dll", "api-ms-win-crt-runtime-l1-1-0.dll", "ntdll.dll"]


class PeImports(unittest.TestCase):
    def test_reads_both_tables(self):
        image = fake_pe(CLEAN + ["VCRUNTIME140.dll"], delay=["msvcp140.dll", "user32.dll"])
        self.assertEqual(package.pe_imports(image), CLEAN + ["VCRUNTIME140.dll", "msvcp140.dll", "user32.dll"])

    def test_pe32(self):
        self.assertEqual(package.pe_imports(fake_pe(["kernel32.dll"], pe32=True)), ["kernel32.dll"])

    def test_no_imports(self):
        self.assertEqual(package.pe_imports(fake_pe([])), [])

    def test_not_a_pe(self):
        for data in [b"", b"#!/bin/sh\n", b"MZ" + bytes(100), fake_pe(CLEAN)[: SECTION_RAW + 30]]:
            with self.subTest(data=data[:8]), self.assertRaises(ValueError):
                package.pe_imports(data)


class VcRedist(unittest.TestCase):
    def test_names(self):
        refused = [
            "VCRUNTIME140.dll",
            "vcruntime140_1.dll",
            "VCRUNTIME140D.dll",
            "MSVCP140.dll",
            "msvcp140_atomic_wait.dll",
            "concrt140.dll",
            "vccorlib140.dll",
            "VCOMP140.DLL",
            "mfc140u.dll",
            "mfcm140.dll",
            "ucrtbased.dll",
        ]
        accepted = [
            "msvcrt.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-heap-l1-1-0.dll",
            "KERNEL32.dll",
            "vulkan-1.dll",
            "avutil-60.dll",
            "libcef.dll",
        ]
        for dll in refused:
            self.assertTrue(package.VC_REDIST_DLL.fullmatch(dll), dll)
        for dll in accepted:
            self.assertFalse(package.VC_REDIST_DLL.fullmatch(dll), dll)


class CheckWindowsZip(unittest.TestCase):
    """check_windows_zip on a zip without FFmpeg and CEF (the programs and the bundle files)."""

    def check(self, programs: dict[str, bytes]) -> tuple[int, str]:
        top = "lumenwall-0.2.0-x86_64-pc-windows-msvc"
        files = {
            **programs,
            "LICENSE-MIT": b"MIT",
            "LICENSE-APACHE": b"Apache",
            "THIRD-PARTY-NOTICES.html": b"<html></html>",
            "README.txt": b"Lumenwall",
            "lumenwall.bundle": b"x86_64-pc-windows-msvc 0.2.0\n",
        }
        with tempfile.TemporaryDirectory() as tmp:
            archive = Path(tmp) / f"{top}.zip"
            with zipfile.ZipFile(archive, "w") as z:
                for name, data in files.items():
                    z.writestr(f"{top}/{name}", data)
            err = io.StringIO()
            with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
                try:
                    package.check_windows_zip(archive, ffmpeg=False, web=False)
                except SystemExit as e:
                    return e.code, err.getvalue()
            return 0, err.getvalue()

    def test_static_crt_accepted(self):
        code, err = self.check({"lumenwall.exe": fake_pe(CLEAN), "lumenwall-ui.exe": fake_pe(CLEAN, delay=["dwmapi.dll"])})
        self.assertEqual((code, err), (0, ""))

    def test_vc_redist_refused(self):
        code, err = self.check(
            {
                "lumenwall.exe": fake_pe(CLEAN + ["VCRUNTIME140.dll"]),
                "lumenwall-ui.exe": fake_pe(CLEAN, delay=["MSVCP140.dll"]),
            }
        )
        self.assertEqual(code, 1)
        self.assertIn("lumenwall.exe imports VCRUNTIME140.dll", err)
        self.assertIn("lumenwall-ui.exe imports MSVCP140.dll", err)
        self.assertIn("crt-static", err)

    def test_not_a_pe_refused(self):
        code, err = self.check({"lumenwall.exe": b"#!/bin/sh\n", "lumenwall-ui.exe": fake_pe(CLEAN)})
        self.assertEqual(code, 1)
        self.assertIn("lumenwall.exe: not a PE image", err)


if __name__ == "__main__":
    unittest.main()
