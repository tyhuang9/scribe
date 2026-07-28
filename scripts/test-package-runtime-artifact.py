#!/usr/bin/env python3

import hashlib
import json
import os
import platform
from pathlib import Path, PurePath
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile


SCRIPT = Path(__file__).with_name("package-runtime-artifact.py")
NATIVE_OS = {"linux": "linux", "darwin": "macos", "win32": "windows"}[sys.platform]
NATIVE_ARCH = {
    "x86_64": "x86_64",
    "amd64": "x86_64",
    "arm64": "aarch64",
    "aarch64": "aarch64",
}[platform.machine().lower()]


class RuntimeArtifactPackagerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.runtime = self.root / "runtime"
        self.entrypoint_relative = "bin/scribe-vosk.exe" if sys.platform == "win32" else "bin/scribe-vosk"
        self.entrypoint = self.runtime / Path(*PurePath(self.entrypoint_relative).parts)
        self.write_entrypoint(self.entrypoint)
        self.write_manifest("vosk")

    def tearDown(self):
        self.temporary.cleanup()

    def write_manifest(self, runtime_id):
        (self.runtime / "runtime-manifest.json").write_text(
            json.dumps(
                {
                    "manifest_version": 1,
                    "runtime_id": runtime_id,
                    "version": "0.3.45",
                    "platform": f"{NATIVE_OS}-{NATIVE_ARCH}",
                    "device": "cpu",
                    "entrypoint": self.entrypoint_relative,
                    "portable": True,
                }
            ),
            encoding="utf-8",
        )

    def write_entrypoint(self, path):
        path.parent.mkdir(parents=True, exist_ok=True)
        if sys.platform == "win32":
            # The packager executes this renamed native image directly with --help and closed stdin.
            comspec = os.environ.get("COMSPEC")
            if not comspec or not Path(comspec).is_file():
                self.skipTest("COMSPEC is unavailable for the native Windows fixture")
            shutil.copy2(comspec, path)
        else:
            path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            path.chmod(0o755)

    def command(self, base_url="https://downloads.acme.dev/releases/scribe/1.0.0"):
        return [
            sys.executable,
            str(SCRIPT),
            "--runtime-dir",
            str(self.runtime),
            "--runtime-id",
            "vosk",
            "--version",
            "0.3.45",
            "--os",
            NATIVE_OS,
            "--arch",
            NATIVE_ARCH,
            "--device",
            "cpu",
            "--entrypoint",
            self.entrypoint_relative,
            "--release-base-url",
            base_url,
            "--catalog-version",
            "1.0.0",
            "--output-dir",
            str(self.root / "output"),
            "--catalog",
            str(self.root / "catalog.json"),
        ]

    def merge_command(self):
        return [
            sys.executable,
            str(SCRIPT),
            "--merge-catalog-fragments",
            "--catalog-version",
            "1.0.0",
            "--catalog",
            str(self.root / "catalog.json"),
        ]

    def test_packages_smoke_validated_runtime_with_real_sizes_and_checksum(self):
        result = subprocess.run(self.command(), capture_output=True, text=True, check=False)

        self.assertEqual(result.returncode, 0, result.stderr)
        merge = subprocess.run(self.merge_command(), capture_output=True, text=True, check=False)
        self.assertEqual(merge.returncode, 0, merge.stderr)
        output = json.loads(result.stdout)
        archive = Path(output["archive"])
        catalog = json.loads((self.root / "catalog.json").read_text(encoding="utf-8"))
        artifact = catalog["artifacts"][0]
        self.assertEqual(artifact["size_bytes"], archive.stat().st_size)
        self.assertEqual(artifact["unpacked_size_bytes"], sum(path.stat().st_size for path in self.runtime.rglob("*") if path.is_file()))
        self.assertEqual(artifact["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest())
        with zipfile.ZipFile(archive) as packaged:
            self.assertEqual(sorted(packaged.namelist()), [self.entrypoint_relative, "runtime-manifest.json"])

    def test_rejects_placeholder_host_and_manifest_mismatch(self):
        placeholder = subprocess.run(
            self.command("https://artifacts.example.invalid/releases"),
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(placeholder.returncode, 0)
        self.assertIn("real immutable HTTPS", placeholder.stderr)

        self.write_manifest("faster_whisper")
        mismatch = subprocess.run(self.command(), capture_output=True, text=True, check=False)
        self.assertNotEqual(mismatch.returncode, 0)
        self.assertIn("manifest does not match", mismatch.stderr)

    def test_parallel_packagers_emit_fragments_then_merge_deterministically(self):
        second_runtime = self.root / "runtime-sherpa"
        second_relative = (
            "bin/scribe-sherpa-onnx.exe" if sys.platform == "win32" else "bin/scribe-sherpa-onnx"
        )
        self.write_entrypoint(second_runtime / Path(*PurePath(second_relative).parts))
        (second_runtime / "runtime-manifest.json").write_text(
            json.dumps(
                {
                    "manifest_version": 1,
                    "runtime_id": "sherpa_onnx",
                    "version": "0.3.45",
                    "platform": f"{NATIVE_OS}-{NATIVE_ARCH}",
                    "device": "cpu",
                    "entrypoint": second_relative,
                    "portable": True,
                }
            ),
            encoding="utf-8",
        )
        second_command = self.command()
        second_command[second_command.index("--runtime-dir") + 1] = str(second_runtime)
        second_command[second_command.index("--runtime-id") + 1] = "sherpa_onnx"
        second_command[second_command.index("--entrypoint") + 1] = second_relative

        first = subprocess.Popen(self.command(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        second = subprocess.Popen(second_command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        first_stdout, first_stderr = first.communicate()
        second_stdout, second_stderr = second.communicate()
        self.assertEqual(first.returncode, 0, first_stderr)
        self.assertEqual(second.returncode, 0, second_stderr)
        self.assertTrue(first_stdout)
        self.assertTrue(second_stdout)

        merge = subprocess.run(self.merge_command(), capture_output=True, text=True, check=False)
        self.assertEqual(merge.returncode, 0, merge.stderr)
        catalog = json.loads((self.root / "catalog.json").read_text(encoding="utf-8"))
        self.assertEqual(
            [artifact["runtime_id"] for artifact in catalog["artifacts"]],
            ["sherpa_onnx", "vosk"],
        )

    def test_rejects_nonportable_member_paths_and_reserved_host_variants(self):
        unsafe = self.runtime / "bin" / "NUL.txt"
        unsafe.write_text("reserved", encoding="utf-8")
        invalid_path = subprocess.run(self.command(), capture_output=True, text=True, check=False)
        self.assertNotEqual(invalid_path.returncode, 0)
        self.assertIn("unsafe portable path", invalid_path.stderr)
        unsafe.unlink()

        for host in [
            "https://cdn.example.com/releases",
            "https://runtime.dev.localhost/releases",
            "https://127.1.2.3/releases",
            "https://[::1]/releases",
        ]:
            result = subprocess.run(
                self.command(host), capture_output=True, text=True, check=False
            )
            self.assertNotEqual(result.returncode, 0, host)
            self.assertIn("real immutable HTTPS", result.stderr)


if __name__ == "__main__":
    unittest.main()
