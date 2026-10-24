"""Mocked same-release CPU construction; no native build, signature or execution."""
from dataclasses import replace
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import macos_cpu_component as component
import macos_cpu_pkg_entry as cpu
import macos_full_release as full
import macos_full_release_cpu_entry as entry
import macos_metal_pkg_entry as metal
from test_macos_metal_pkg_entry import CPU, REVISION, SHA, Portable as MetalFixture, digest

IMAGE = f"synthetic CPU entry {SHA} {REVISION} {component.CPU_ROOT}".encode()


class Portable(MetalFixture):
    def setUp(self):
        super().setUp()
        self.mutation = None

    def execute(self, args, **kwargs):
        operation = kwargs["operation"]
        if operation == "metal-content-verification" and self.failure == "cpu-pack-" + args[-1]:
            super().execute(args, **kwargs)
            raise component.ToolExitError(operation, 1)
        if operation in ("entry-build-arm64", "entry-build-x86_64"):
            component.validate_tool_operation(operation)
            self.calls.append((list(args), kwargs))
            self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES))
            env = kwargs["environment"]
            self.assertFalse(any(key.startswith(("SCRIBE_GPU_PACK", "SCRIBE_MACOS_METAL")) for key in env))
            worker = Path(env["SCRIBE_MACOS_CPU_PACKAGE_WORKER"])
            self.assertEqual(worker.read_bytes(), CPU)
            self.assertNotIn(self.app, worker.parents)
            self.assertEqual(args[args.index("--bin") + 1], "scribe-macos-cpu-worker-installer")
            self.builds.append((list(args), dict(env)))
            if self.failure == "cpu-uncertain":
                raise component.CleanupUncertain("synthetic CPU child cleanup uncertainty")
            if self.failure == "cpu-build-failed":
                raise component.ToolExitError(operation, 1)
            target = args[args.index("--target") + 1]
            binary = Path(env["CARGO_TARGET_DIR"]) / target / "release/scribe-macos-cpu-worker-installer"
            binary.parent.mkdir(parents=True)
            if self.failure == "cpu-missing":
                return b""
            binary.write_bytes(IMAGE)
            if self.mutation and operation == "entry-build-x86_64":
                scope, relative, replacement = self.mutation
                root = self.app if scope == "source" else worker.parents[2]
                changed = root / relative
                alternate = self.root / "replacement-input"
                alternate.write_bytes(changed.read_bytes() + (b"" if replacement else b"changed"))
                os.replace(alternate, changed)
            return b""
        if args[:2] == ["/usr/bin/lipo", "-create"]:
            self.calls.append((list(args), kwargs))
            Path(args[3]).write_bytes(IMAGE)
            return b""
        if args[0] == "/usr/bin/lipo" and args[-1] == "-archs" \
                and Path(args[1]).name == "scribe-macos-cpu-worker-installer":
            self.calls.append((list(args), kwargs))
            if self.failure == "cpu-wrong-arch":
                return b"arm64e"
            return b"arm64" if Path(args[1]).parent.parent.name.startswith("aarch64") else b"x86_64"
        return super().execute(args, **kwargs)

    def build(self, **kwargs):
        def publish(source, output):
            if self.failure == "cpu-collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic collision")
            source.rename(output)
            if self.failure == "cpu-publish-uncertain":
                raise OSError("synthetic ambiguous publication")
            if self.failure == "cpu-postpublish-source":
                (self.app / "Contents/Info.plist").write_bytes(b"changed after publication")
        with self.context(**kwargs), patch.object(component, "publish_no_replace", side_effect=publish):
            return entry.build_entry(self.app, self.workspace, self.frozen)

    def test_same_full_release_verifies_both_packs_before_cpu_only_compilation(self):
        self.assertIs(full.FrozenInputs, metal.FrozenInputs)
        output, sha, size = self.build()
        self.assertEqual((sha, size), (digest(IMAGE), len(IMAGE)))
        self.assertEqual(output.read_bytes(), IMAGE)
        self.assertEqual(len(self.builds), 3)  # one native verifier, two CPU entry slices
        self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES))
        self.assertEqual(list(self.workspace.iterdir()), [output])

    def test_existing_cpu_only_constructor_still_rejects_the_full_app(self):
        with self.context(), self.assertRaisesRegex(component.ComponentError, "CPU-only catalog"):
            cpu.build_entry(self.app, self.workspace, SHA, REVISION)
        self.assertEqual(self.builds, [])

    def test_either_pack_failure_prevents_all_cpu_compilation(self):
        for arch in full.ARCHES:
            self.failure = "cpu-pack-" + arch
            self.builds.clear()
            self.verifications.clear()
            self.reject()
            self.assertEqual(len(self.builds), 1)
            self.assertEqual(self.verifications[-1][1], arch)

    def test_every_source_and_private_file_change_or_replacement_rejects(self):
        packs, _ = full.release_inputs(self.app, self.frozen)
        for relative in full.input_files(packs):
            source = self.app / relative
            saved = source.read_bytes()
            for scope in ("source", "private"):
                for replacement in (False, True):
                    with self.subTest(relative=relative, scope=scope, replacement=replacement):
                        self.builds.clear()
                        self.verifications.clear()
                        self.mutation = scope, relative, replacement
                        self.reject()
                        self.assertEqual(len(self.builds), 3)
                        source.write_bytes(saved)

    def test_no_receipt_or_malformed_frozen_identity_reaches_tools(self):
        saved = self.frozen
        for value in (None, True, {}, object(), replace(saved, catalog_sha256="bad")):
            self.frozen = value
            self.reject()
        self.assertEqual(self.calls, [])

    def test_candidate_change_and_same_byte_replacement_at_final_scope_check_reject(self):
        original = full._OwnedRelease.check_unchanged
        for replacement in (False, True):
            self.builds.clear()
            self.verifications.clear()
            reached = []
            def checked(owned):
                original(owned)
                candidate = owned.private / "postinstall"
                if candidate.exists():
                    reached.append(True)
                    alternate = owned.private / "replacement-candidate"
                    alternate.write_bytes(candidate.read_bytes() + (b"" if replacement else b"changed"))
                    os.replace(alternate, candidate)
            with patch.object(full._OwnedRelease, "check_unchanged", autospec=True, side_effect=checked):
                self.reject()
            self.assertEqual(reached, [True])
            self.assertEqual(len(self.builds), 3)
            self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES))

    def test_ambient_metal_and_loader_overrides_do_not_enter_cpu_builds(self):
        with patch.dict(os.environ, {"SCRIBE_MACOS_METAL_PACKAGE_PACK": "/untrusted",
                                     "SCRIBE_GPU_PACK_CATALOG": "/untrusted",
                                     "DYLD_INSERT_LIBRARIES": "/untrusted",
                                     "CUDA_PATH": "/untrusted"}):
            self.build()
        for args, env in self.builds[1:]:
            self.assertNotIn("DYLD_INSERT_LIBRARIES", env)
            self.assertNotIn("CUDA_PATH", env)
            self.assertEqual(args[args.index("--bin") + 1], "scribe-macos-cpu-worker-installer")

    def test_cpu_limits_and_structural_rejections_remain_cpu_specific(self):
        for failure in ("cpu-build-failed", "cpu-missing", "cpu-wrong-arch", "gpu-dependency", "minos"):
            self.failure = failure
            self.builds.clear()
            self.verifications.clear()
            self.reject()
        self.failure = None
        for maximum in ("MAX_SLICE", "MAX_ENTRY"):
            self.builds.clear()
            self.verifications.clear()
            with patch.object(cpu, maximum, 1):
                self.reject()

    def test_existing_output_and_invalid_workspace_reject_before_tools(self):
        output = self.workspace / "postinstall"
        output.write_bytes(b"retain existing")
        with self.assertRaises(component.ComponentError):
            self.build()
        self.assertEqual(output.read_bytes(), b"retain existing")
        output.unlink()
        for workspace in (Path("relative"), self.app, self.app / "Contents"):
            self.workspace = workspace
            with self.assertRaises(component.ComponentError):
                self.build()
        self.assertEqual(self.calls, [])

    def test_collision_and_ambiguous_publication_retain_final(self):
        for failure in ("cpu-collision", "cpu-publish-uncertain"):
            with tempfile.TemporaryDirectory(dir=self.root) as parent:
                self.workspace, self.failure = Path(parent), failure
                self.verifications.clear()
                with self.assertRaises(OSError):
                    self.build()
                self.assertTrue((self.workspace / "postinstall").is_file())

    def test_normal_postpublication_recheck_failure_reports_error_and_retains_final(self):
        self.failure = "cpu-postpublish-source"
        with self.assertRaises(component.ComponentError):
            self.build()
        self.assertEqual((self.workspace / "postinstall").read_bytes(), IMAGE)


class Posix(Portable):
    def test_uncertain_cpu_child_preserves_original_error_and_owned_workspace(self):
        self.failure = "cpu-uncertain"
        with self.assertRaisesRegex(component.CleanupUncertain, "CPU child cleanup uncertainty"):
            self.build(real_workspace=True)
        retained = list(self.workspace.glob(".scribe-full-release-entry-*"))
        self.assertEqual(len(retained), 1)
        self.assertTrue((retained[0] / "app" / full.CATALOG).is_file())
        self.assertFalse((self.workspace / "postinstall").exists())
        # Every child is mocked; fixture cleanup cannot affect live native code.


def suite():
    result = unittest.TestSuite()
    for cls in [Portable] + ([Posix] if os.name == "posix" else []):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 11 + (1 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"Full-release CPU discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
