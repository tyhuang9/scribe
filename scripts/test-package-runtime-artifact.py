#!/usr/bin/env python3

import hashlib
import json
import platform
from pathlib import Path
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
        self.entrypoint = self.runtime / "bin" / "scribe-vosk"
        self.entrypoint.parent.mkdir(parents=True)
        self.entrypoint.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.entrypoint.chmod(0o755)
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
                    "entrypoint": "bin/scribe-vosk",
                    "portable": True,
                }
            ),
            encoding="utf-8",
        )

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
            "bin/scribe-vosk",
            "--release-base-url",
            base_url,
            "--catalog-version",
            "1.0.0",
            "--output-dir",
            str(self.root / "output"),
            "--catalog",
            str(self.root / "catalog.json"),
        ]

    def test_packages_smoke_validated_runtime_with_real_sizes_and_checksum(self):
        result = subprocess.run(self.command(), capture_output=True, text=True, check=False)

        self.assertEqual(result.returncode, 0, result.stderr)
        output = json.loads(result.stdout)
        archive = Path(output["archive"])
        catalog = json.loads((self.root / "catalog.json").read_text(encoding="utf-8"))
        artifact = catalog["artifacts"][0]
        self.assertEqual(artifact["size_bytes"], archive.stat().st_size)
        self.assertEqual(artifact["unpacked_size_bytes"], sum(path.stat().st_size for path in self.runtime.rglob("*") if path.is_file()))
        self.assertEqual(artifact["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest())
        with zipfile.ZipFile(archive) as packaged:
            self.assertEqual(sorted(packaged.namelist()), ["bin/scribe-vosk", "runtime-manifest.json"])

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


if __name__ == "__main__":
    unittest.main()
