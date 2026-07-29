#!/usr/bin/env python3

import hashlib
import importlib.util
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
SPEC = importlib.util.spec_from_file_location("package_runtime_artifact", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)
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

    def write_manifest(self, runtime_id, version="0.3.45"):
        manifest = {
            "manifest_version": 1,
            "runtime_id": runtime_id,
            "version": version,
            "platform": f"{NATIVE_OS}-{NATIVE_ARCH}",
            "device": "cpu",
            "entrypoint": self.entrypoint_relative,
            "portable": True,
        }
        if runtime_id == "voice_intent_llama_cpp":
            manifest.update(
                {
                    "upstream_repository": "ggml-org/llama.cpp",
                    "upstream_revision": "aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3",
                    "upstream_asset": "llama-b9637-bin-win-cpu-x64.zip",
                    "upstream_sha256": "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e",
                    "upstream_size_bytes": 16_906_751,
                    "license": "MIT",
                    "license_sha256": "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d",
                }
            )
        (self.runtime / "runtime-manifest.json").write_text(
            json.dumps(manifest),
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

    def write_voice_attestation(self):
        files = []
        for path in sorted(candidate for candidate in self.runtime.rglob("*") if candidate.is_file()):
            digest, size = MODULE.hash_and_size(path)
            files.append(
                {
                    "path": path.relative_to(self.runtime).as_posix(),
                    "size_bytes": size,
                    "sha256": digest,
                }
            )
        attestation = {
            "attestation_version": 1,
            "runtime_id": "voice_intent_llama_cpp",
            "version": "b9637",
            "platform": "windows-x86_64",
            "device": "cpu",
            "entrypoint": self.entrypoint_relative,
            **MODULE.VOICE_RUNTIME_PROVENANCE,
            "files": files,
        }
        (self.runtime / MODULE.VOICE_RUNTIME_ATTESTATION).write_text(
            json.dumps(attestation), encoding="utf-8"
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

    def test_packages_voice_intent_llama_as_a_cpu_only_auxiliary_runtime(self):
        self.write_manifest("voice_intent_llama_cpp", "b9637")
        self.write_voice_attestation()
        command = self.command()
        command[command.index("--runtime-id") + 1] = "voice_intent_llama_cpp"
        command[command.index("--version") + 1] = "b9637"

        result = subprocess.run(command, capture_output=True, text=True, check=False)

        if (NATIVE_OS, NATIVE_ARCH) != ("windows", "x86_64"):
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Windows x86_64 CPU-only", result.stderr)
            return

        self.assertEqual(result.returncode, 0, result.stderr)
        fragment = json.loads(Path(json.loads(result.stdout)["fragment"]).read_text(encoding="utf-8"))
        self.assertEqual(fragment["runtime_id"], "voice_intent_llama_cpp")
        self.assertEqual(fragment["device"], "cpu")
        self.assertEqual(
            fragment["upstream_revision"],
            "aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3",
        )
        self.assertEqual(
            fragment["upstream_sha256"],
            "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e",
        )
        self.assertEqual(
            fragment["license_sha256"],
            "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d",
        )

        gpu = command.copy()
        gpu[gpu.index("--device") + 1] = "gpu"
        rejected = subprocess.run(gpu, capture_output=True, text=True, check=False)
        self.assertNotEqual(rejected.returncode, 0)
        self.assertIn("does not support GPU", rejected.stderr)

    def test_voice_runtime_rejects_missing_or_mismatched_preparation_attestation(self):
        self.write_manifest("voice_intent_llama_cpp", "b9637")
        entrypoint = MODULE.normalized_entrypoint(self.entrypoint_relative)
        files = MODULE.runtime_files(self.runtime)
        with self.assertRaisesRegex(ValueError, "attestation is required"):
            MODULE.verify_voice_runtime_attestation(self.runtime, files, entrypoint)

        self.write_voice_attestation()
        attestation_path = self.runtime / MODULE.VOICE_RUNTIME_ATTESTATION
        attestation = json.loads(attestation_path.read_text(encoding="utf-8"))
        attestation["upstream_revision"] = "a" * 40
        attestation_path.write_text(json.dumps(attestation), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "unapproved identity"):
            MODULE.verify_voice_runtime_attestation(
                self.runtime, MODULE.runtime_files(self.runtime), entrypoint
            )
        attestation["upstream_revision"] = MODULE.VOICE_RUNTIME_PROVENANCE[
            "upstream_revision"
        ]
        attestation_path.write_text(json.dumps(attestation), encoding="utf-8")
        files = MODULE.runtime_files(self.runtime)
        payload, attested_files = MODULE.verify_voice_runtime_attestation(
            self.runtime, files, entrypoint
        )
        self.assertNotIn(self.runtime / MODULE.VOICE_RUNTIME_ATTESTATION, payload)
        self.assertEqual(
            set(attested_files),
            {path.relative_to(self.runtime).as_posix() for path in payload},
        )

        with self.entrypoint.open("ab") as target:
            target.write(b"tampered")
        with self.assertRaisesRegex(ValueError, "changed during packaging"):
            MODULE.write_archive(
                self.runtime,
                payload,
                self.root / "tampered.zip",
                attested_files,
            )
        with self.assertRaisesRegex(ValueError, "digest mismatch"):
            MODULE.verify_voice_runtime_attestation(
                self.runtime, MODULE.runtime_files(self.runtime), entrypoint
            )

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
