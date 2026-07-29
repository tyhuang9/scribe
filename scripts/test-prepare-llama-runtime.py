#!/usr/bin/env python3

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).with_name("prepare-llama-runtime.py")
SPEC = importlib.util.spec_from_file_location("prepare_llama_runtime", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class PrepareLlamaRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.original_entries = MODULE.UPSTREAM_ENTRIES
        self.original_unpacked = MODULE.UPSTREAM_UNPACKED_SIZE

    def tearDown(self):
        MODULE.UPSTREAM_ENTRIES = self.original_entries
        MODULE.UPSTREAM_UNPACKED_SIZE = self.original_unpacked

    def archive(self, root: Path, entries: list[tuple[str, bytes]]) -> Path:
        path = root / "llama.zip"
        with zipfile.ZipFile(path, "w") as archive:
            for name, contents in entries:
                archive.writestr(name, contents)
        MODULE.UPSTREAM_ENTRIES = len(entries)
        MODULE.UPSTREAM_UNPACKED_SIZE = sum(len(contents) for _, contents in entries)
        return path

    def test_selects_only_server_and_required_dll_payload(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive_path = self.archive(
                Path(temporary),
                [
                    ("llama-server.exe", b"MZserver"),
                    ("ggml.dll", b"MZdll"),
                    ("llama-cli.exe", b"MZcli"),
                ],
            )
            with zipfile.ZipFile(archive_path) as archive:
                selected = MODULE.selected_payload(archive)
            self.assertEqual(
                [info.filename for info in selected],
                ["llama-server.exe", "ggml.dll"],
            )

    def test_rejects_traversal_duplicates_and_missing_runtime_files(self):
        cases = [
            [("../llama-server.exe", b"MZ"), ("ggml.dll", b"MZ")],
            [("llama-server.exe", b"MZ"), ("LLAMA-SERVER.EXE", b"MZ"), ("ggml.dll", b"MZ")],
            [("llama-server.exe", b"MZ"), ("readme.txt", b"text")],
        ]
        for entries in cases:
            with self.subTest(entries=entries), tempfile.TemporaryDirectory() as temporary:
                archive_path = self.archive(Path(temporary), entries)
                with zipfile.ZipFile(archive_path) as archive, self.assertRaises(ValueError):
                    MODULE.selected_payload(archive)

    def test_file_verification_checks_both_size_and_sha(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "input"
            path.write_bytes(b"verified")
            digest = hashlib.sha256(b"verified").hexdigest()
            MODULE.verify_file(path, 8, digest, "fixture")
            with self.assertRaisesRegex(ValueError, "pinned upstream bytes"):
                MODULE.verify_file(path, 9, digest, "fixture")


if __name__ == "__main__":
    unittest.main()
