"""Private source fixtures: no actual native build, signing or entry execution."""
from contextlib import ExitStack, contextmanager
import hashlib
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import patch

import macos_cpu_component as component
import macos_cpu_pkg_entry as entry

REVISION = "source-package-entry-fixture"
WORKER = f"synthetic scribe-inference-worker@0.1.0#{REVISION}".encode()
DIGEST = hashlib.sha256(WORKER).hexdigest()
ENTRY = f"synthetic entry {DIGEST} {REVISION} {component.CPU_ROOT}".encode()


class Portable(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-entry-source-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.app = self.root / "Scribe.app"
        self.native = self.app / "Contents/MacOS"
        self.resources = self.app / "Contents/Resources"
        self.native.mkdir(parents=True)
        self.resources.mkdir()
        (self.native / "scribe-inference-worker").write_bytes(WORKER)
        (self.native / "Scribe").write_bytes(f"local-transcriber@0.1.0#{REVISION} {DIGEST}".encode())
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.1.0"}))
        (self.resources / "worker-pack-catalog.json").write_bytes(b'{"schema_version":1,"packs":[]}')
        (self.resources / "gpu-pack-release-authority.json").write_bytes(
            (component.REPO / "runtime-manifests/gpu-pack-release-authority-macos-empty.json").read_bytes())
        self.workspace = self.root / "author"
        self.workspace.mkdir()
        self.calls, self.builds = [], []
        self.failure = None

    @contextmanager
    def fake_private(self, parent, prefix):
        with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as private:
            yield Path(private)

    @contextmanager
    def context(self, *, real_workspace=False):
        with ExitStack() as stack:
            stack.enter_context(patch.object(entry.sys, "platform", "darwin"))
            uid = getattr(os, "geteuid", lambda: 501)() or 501
            stack.enter_context(patch.object(entry.os, "geteuid", return_value=uid, create=True))
            stack.enter_context(patch.object(entry.shutil, "which", return_value="/usr/bin/cargo"))
            stack.enter_context(patch.object(component, "capture", side_effect=self.execute))
            if not real_workspace:
                stack.enter_context(patch.object(component, "private_workspace", side_effect=self.fake_private))
            yield

    def execute(self, arguments, **kwargs):
        self.calls.append((list(arguments), kwargs))
        tool = arguments[0]
        if tool == "/usr/bin/cargo":
            self.assertEqual(kwargs["cwd"], component.REPO)
            environment = dict(kwargs["environment"])
            target = arguments[arguments.index("--target") + 1]
            self.builds.append((target, environment, arguments))
            if self.failure in ("build", "uncertain"):
                error = component.CleanupUncertain if self.failure == "uncertain" else component.ComponentError
                raise error("synthetic child failure")
            binary = Path(environment["CARGO_TARGET_DIR"]) / target / "release/scribe-macos-cpu-worker-installer"
            binary.parent.mkdir(parents=True)
            if self.failure == "missing" and len(self.builds) == 2:
                return b""
            if self.failure == "hardlink":
                os.link(environment["SCRIBE_MACOS_CPU_PACKAGE_WORKER"], binary)
            elif self.failure == "symlink":
                binary.symlink_to(environment["SCRIBE_MACOS_CPU_PACKAGE_WORKER"])
            else:
                binary.write_bytes(b"x" * 513 if self.failure == "slice-size" else ENTRY)
            if self.failure == "worker-change":
                Path(environment["SCRIBE_MACOS_CPU_PACKAGE_WORKER"]).write_bytes(b"tampered")
            return b""
        if tool == "/usr/bin/lipo":
            if arguments[1] == "-create":
                if self.failure == "combine":
                    raise component.ComponentError("synthetic combine failure")
                candidate = Path(arguments[3])
                if self.failure == "entry-hardlink":
                    os.link(self.builds[-1][1]["SCRIBE_MACOS_CPU_PACKAGE_WORKER"], candidate)
                else:
                    candidate.write_bytes(b"x" * 257 if self.failure == "entry-size" else ENTRY)
            elif arguments[-1] == "-archs":
                binary = Path(arguments[1])
                if binary.name == "scribe-macos-cpu-worker-installer":
                    target = binary.parent.parent.name
                    return {"wrong": b"arm64e", "fat": b"arm64 x86_64"}.get(
                        self.failure, b"arm64" if target.startswith("aarch64") else b"x86_64")
                return b"arm64 x86_64"
            else:
                self.assertEqual(arguments[2], "-thin")
                Path(arguments[-1]).write_bytes(Path(arguments[1]).read_bytes())
            return b""
        if tool == "/usr/bin/codesign":
            self.assertEqual(arguments[1:3], ["--verify", "--strict"])
            self.assertIn(Path(arguments[-1]).name, ("Scribe", "scribe-inference-worker"))
            if self.failure == "signature":
                raise component.ComponentError("synthetic input signature failure")
            return b""
        if tool == "/usr/bin/otool":
            if arguments[1] == "-l":
                return b"minos 13.0\n"
            library = "@rpath/untrusted.dylib" if self.failure == "entry-dylib" and "postinstall" in arguments[-1] \
                else "/usr/lib/libSystem.B.dylib"
            return f"binary:\n\t{library} (compatibility version 1.0.0)\n".encode()
        self.assertEqual(tool, "/bin/bash")
        self.assertEqual(arguments[1], str(component.REPO / "scripts/verify-macos-release-package.sh"))
        return b""

    def build(self, *, real_workspace=False):
        def publish(candidate, output):
            if self.failure == "collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic publication collision")
            candidate.rename(output)
            if self.failure == "publish-uncertain":
                raise OSError("synthetic post-rename confirmation failure")
        with self.context(real_workspace=real_workspace), \
             patch.object(component, "publish_no_replace", side_effect=publish):
            return entry.build_entry(self.app, self.workspace, DIGEST, REVISION)

    def assert_clean_failure(self, message=None):
        failure = self.assertRaisesRegex(component.ComponentError, message) if message \
            else self.assertRaises((component.ComponentError, OSError))
        with failure:
            self.build()
        self.assertFalse((self.workspace / "postinstall").exists())
        self.assertFalse(list(self.workspace.glob(".scribe-cpu-pkg-entry-*")))
        self.assertEqual((self.native / "scribe-inference-worker").read_bytes(), WORKER)

    def test_entry_bounds_allow_two_complete_payloads_without_changing_writer_bound(self):
        self.assertEqual(component.MAX_WORKER, 512 * 1024 * 1024)
        self.assertEqual(component.MAX_WRITER, 64 * 1024 * 1024)
        self.assertEqual(entry.MAX_SLICE, component.MAX_WORKER + 32 * 1024 * 1024)
        self.assertEqual(entry.MAX_ENTRY, 2 * entry.MAX_SLICE)

    def test_build_binds_both_fresh_slices_to_one_frozen_worker(self):
        output, digest, size = self.build()
        self.assertEqual(output, self.workspace / "postinstall")
        self.assertEqual((digest, size), (hashlib.sha256(ENTRY).hexdigest(), len(ENTRY)))
        self.assertEqual(output.read_bytes(), ENTRY)
        self.assertEqual([target for target, _, _ in self.builds], list(component.TARGETS))
        workers = set()
        for target, environment, arguments in self.builds:
            for required in ("--locked", "--offline", "--release", "--no-default-features"):
                self.assertIn(required, arguments)
            self.assertEqual(arguments[arguments.index("--features") + 1], "installer-package-entry")
            self.assertEqual(arguments[arguments.index("--bin") + 1], "scribe-macos-cpu-worker-installer")
            self.assertEqual(environment["SCRIBE_BUNDLED_WORKER_SHA256"], DIGEST)
            self.assertEqual(environment["SCRIBE_BUILD_REVISION"], REVISION)
            self.assertEqual(environment["MACOSX_DEPLOYMENT_TARGET"], "13.0")
            self.assertIn(target, environment["CARGO_TARGET_DIR"])
            workers.add(environment["SCRIBE_MACOS_CPU_PACKAGE_WORKER"])
        self.assertEqual(len(workers), 1)
        for arguments, _ in self.calls:
            self.assertIn(arguments[0], ("/bin/bash", "/usr/bin/cargo", "/usr/bin/lipo", "/usr/bin/otool", "/usr/bin/codesign"))
            self.assertFalse({"--sign", "--force", "describe", "install", "--nopayload", "--expand"}.intersection(arguments))
        self.assertEqual([args[-1] for args, _ in self.calls if args[0] == "/usr/bin/codesign"],
                         [next(iter(workers)), str(Path(next(iter(workers))).parent / "Scribe")])
        self.assertFalse(list(self.workspace.glob(".scribe-cpu-pkg-entry-*")))

    def test_root_and_foreign_hosts_reject_before_input_access(self):
        for platform, uid in [("win32", 501), ("linux", 501), ("darwin", 0)]:
            with self.subTest(platform=platform), patch.object(entry.sys, "platform", platform), \
                 patch.object(entry.os, "geteuid", return_value=uid, create=True), \
                 patch.object(component, "capture") as capture, self.assertRaises(component.ComponentError):
                entry.build_entry(self.root / "absent", self.root / "absent", DIGEST, REVISION)
            capture.assert_not_called()

    def test_invalid_identity_rejects_before_tools(self):
        for digest, revision in [("invalid", REVISION), (DIGEST, "short")]:
            with self.context(), self.assertRaises(component.ComponentError):
                entry.build_entry(self.app, self.workspace, digest, revision)
        self.assertEqual(self.calls, [])

    def test_existing_output_is_preserved_before_any_tool(self):
        output = self.workspace / "postinstall"
        output.write_bytes(b"retain")
        with self.context(), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        self.assertEqual(output.read_bytes(), b"retain")
        self.assertEqual(self.calls, [])

    def test_publication_collision_or_ambiguous_confirmation_never_removes_final(self):
        for failure, expected in [("collision", b"retain concurrent output"), ("publish-uncertain", ENTRY)]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(dir=self.root) as temporary:
                self.workspace = Path(temporary)
                output = self.workspace / "postinstall"
                self.failure = failure
                with self.assertRaises(OSError):
                    self.build()
                self.assertEqual(output.read_bytes(), expected)
                self.assertFalse(list(self.workspace.glob(".scribe-cpu-pkg-entry-*")))
                self.assertEqual((self.native / "scribe-inference-worker").read_bytes(), WORKER)

    def test_workspace_cannot_mutate_the_app_or_use_a_relative_parent(self):
        for workspace in [self.app, self.native, Path("relative")]:
            with self.subTest(workspace=workspace), self.context(), self.assertRaises(component.ComponentError):
                entry.build_entry(self.app, workspace, DIGEST, REVISION)
        self.assertEqual(self.calls, [])

    def test_copied_worker_must_match_the_frozen_anchor(self):
        (self.native / "scribe-inference-worker").write_bytes(b"forged")
        with self.context(), self.assertRaisesRegex(component.ComponentError, "copied worker"):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        self.assertEqual(self.builds, [])

    def test_worker_snapshot_is_rechecked_after_build(self):
        self.failure = "worker-change"
        self.assert_clean_failure()

    def test_missing_slice_fails_without_publication(self):
        self.failure = "missing"
        self.assert_clean_failure()

    def test_wrong_or_fat_slice_is_not_combined(self):
        for failure in ["wrong", "fat"]:
            self.failure = failure
            self.assert_clean_failure()

    def test_oversized_slice_rejects_before_combining(self):
        self.failure = "slice-size"
        with patch.object(entry, "MAX_SLICE", 512):
            self.assert_clean_failure()

    def test_oversized_universal_entry_rejects_before_publication(self):
        self.failure = "entry-size"
        with patch.object(entry, "MAX_ENTRY", 256):
            self.assert_clean_failure()

    def test_linked_slice_is_not_accepted(self):
        for failure in ["hardlink", "entry-hardlink"]:
            with self.subTest(failure=failure):
                self.calls.clear()
                self.builds.clear()
                self.failure = failure
                self.assert_clean_failure("hard-linked component file")
                combinations = [args for args, _ in self.calls if args[:2] == ["/usr/bin/lipo", "-create"]]
                self.assertEqual(len(combinations), 1 if failure == "entry-hardlink" else 0)

    def test_tool_signature_dependency_and_uncertain_errors_do_not_publish(self):
        with self.context(), patch.object(entry.shutil, "which", return_value=None), \
             self.assertRaisesRegex(component.ComponentError, "toolchain"):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        self.assertEqual(self.builds, [])
        for failure in ["build", "signature", "combine", "entry-dylib", "uncertain"]:
            with self.subTest(failure=failure):
                self.failure = failure
                self.assert_clean_failure()

    def test_nonempty_catalog_or_authority_and_invalid_version_reject(self):
        catalog = self.resources / "worker-pack-catalog.json"
        catalog.write_bytes(b'{"schema_version":1,"packs":[{}]}')
        with self.context(), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        catalog.write_bytes(b'{"schema_version":1,"packs":[]}')
        authority = self.resources / "gpu-pack-release-authority.json"
        saved = authority.read_bytes()
        authority.write_bytes(b"{}")
        with self.context(), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        authority.write_bytes(saved)
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "invalid"}))
        with self.context(), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        self.assertEqual(self.builds, [])

    def test_compiler_loader_and_authority_overrides_do_not_reach_builds(self):
        overrides = {name: "untrusted" for name in ["RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_ENCODED_RUSTFLAGS", "DYLD_INSERT_LIBRARIES", "SCRIBE_GPU_PACK_RELEASE_AUTHORITY",
            "SCRIBE_MACOS_CPU_PACKAGE_WORKER", "SCRIBE_MACOS_SIGNING_IDENTITY", "CARGO_TARGET_DIR"]}
        with patch.dict(os.environ, overrides):
            self.build()
        for _, environment, _ in self.builds:
            for name in ["RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_ENCODED_RUSTFLAGS"]:
                self.assertEqual(environment[name], "")
            for name in ["RUSTFLAGS", "DYLD_INSERT_LIBRARIES", "SCRIBE_GPU_PACK_RELEASE_AUTHORITY", "SCRIBE_MACOS_SIGNING_IDENTITY"]:
                self.assertNotIn(name, environment)
            self.assertNotEqual(environment["SCRIBE_MACOS_CPU_PACKAGE_WORKER"], "untrusted")
            self.assertNotEqual(environment["CARGO_TARGET_DIR"], "untrusted")


class Posix(Portable):
    def test_symlink_slice_rejects_without_publication(self):
        self.failure = "symlink"
        self.assert_clean_failure()

    def test_uncertain_child_cleanup_retains_the_private_build_workspace(self):
        self.failure = "uncertain"
        with self.context(real_workspace=True), self.assertRaises(component.CleanupUncertain):
            entry.build_entry(self.app, self.workspace, DIGEST, REVISION)
        retained = list(self.workspace.glob(".scribe-cpu-pkg-entry-*"))
        self.assertEqual(len(retained), 1)
        self.assertTrue((retained[0] / "scribe-inference-worker").is_file())
        self.assertFalse((self.workspace / "postinstall").exists())
        # All subprocesses were mocked: no live/uncertain process exists here.


def suite():
    result = unittest.TestSuite()
    classes = [Portable] + ([Posix] if os.name == "posix" else [])
    for cls in classes:
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 17 + (2 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"CPU entry source discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
