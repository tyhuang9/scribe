"""Source-only component checks; never invoke the installer command or /Library."""
import hashlib
import ctypes
import errno
import json
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

import macos_cpu_component as component

REVISION = "source-component-fixture"
WORKER_BYTES = b"bounded CPU worker fixture"
DIGEST = hashlib.sha256(WORKER_BYTES).hexdigest()


class Portable(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-component-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def fixture(self):
        (self.root / "payload").mkdir()
        (self.root / "tools").mkdir()
        (self.root / component.WORKER).write_bytes(WORKER_BYTES)
        writer = self.root / component.WRITER
        writer.write_bytes(b"writer fixture")
        writer_digest, size = component.hash_file(writer, component.MAX_WRITER)
        self.record = component.record_for(DIGEST, REVISION, len(WORKER_BYTES), writer_digest, size)
        self.write_record()

    def write_record(self):
        (self.root / "component.json").write_text(json.dumps(self.record), encoding="utf-8")

    def test_valid_identity(self):
        component.validate_identity(DIGEST, REVISION)

    def test_invalid_identity(self):
        for digest, revision in [(DIGEST.upper(), REVISION), ("x", REVISION),
                                 (DIGEST, "short"), (DIGEST, "bad revision!"), (DIGEST, "x" * 97)]:
            with self.subTest(digest=digest, revision=revision), self.assertRaises(component.ComponentError):
                component.validate_identity(digest, revision)

    def test_strict_metadata(self):
        for data in [b"", b"[]", b"{", b'"x"', b'{"x":1,"x":2}',
                     b'{"nested":{"x":1,"x":2}}', b'{"x":NaN}', b"\xff", b" " * 8193]:
            with self.subTest(data=data[:24]), self.assertRaises(component.ComponentError):
                component.strict_json(data)

    def test_exact_description(self):
        data = component.expected_description(DIGEST, REVISION)
        component.check_description(json.dumps(data).encode(), DIGEST, REVISION)
        for key, value in [("schemaVersion", True), ("workerSha256", "0" * 64),
                           ("buildRevision", "other-revision"), ("installsApplication", True),
                           ("extra", None)]:
            with self.subTest(key=key), self.assertRaises(component.ComponentError):
                component.check_description(json.dumps({**data, key: value}).encode(), DIGEST, REVISION)

    def test_exact_inventory(self):
        self.fixture()
        self.assertEqual(component.verify_files(self.root, DIGEST, REVISION), self.record)

    def test_record_is_not_authority(self):
        self.fixture()
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, "0" * 64, REVISION)
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, "other-revision")

    def test_extra_file(self):
        self.fixture()
        (self.root / "unexpected").write_bytes(b"extra")
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_root_resource_metadata(self):
        self.fixture()
        with patch.object(component.os, "listxattr", return_value=["com.apple.FinderInfo"], create=True), \
             self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_acl_rights_and_error_policy(self):
        for tag, rights, accepted in [(1, 1 << 1, True), (2, 1 << 6, True),
                                      (1, 1 << 6, False), (1, 1 << 12, False),
                                      (99, 0, False), (2, 1 << 63, False)]:
            library = Mock()
            library.acl_get_fd_np.return_value = 123
            library.acl_valid.return_value = 0
            library.acl_free.return_value = 0
            def entry(_acl, index, result):
                if index == 0:
                    result._obj.value = 456
                    return 0
                ctypes.set_errno(errno.EINVAL)
                return -1
            def set_tag(_entry, result):
                result._obj.value = tag
                return 0
            def set_rights(_entry, result):
                result._obj.value = rights
                return 0
            library.acl_get_entry.side_effect = entry
            library.acl_get_tag_type.side_effect = set_tag
            library.acl_get_permset_mask_np.side_effect = set_rights
            with self.subTest(tag=tag, rights=rights), patch.object(component.ctypes, "CDLL", return_value=library):
                if accepted:
                    component.check_directory_acl(17)
                else:
                    with self.assertRaises(component.ComponentError):
                        component.check_directory_acl(17)
                library.acl_free.assert_called_once_with(123)
        for error, accepted in [(errno.ENOENT, True), (errno.EACCES, False)]:
            library = Mock()
            library.acl_get_fd_np.return_value = None
            ctypes.set_errno(error)
            with self.subTest(error=error), patch.object(component.ctypes, "CDLL", return_value=library):
                if accepted:
                    component.check_directory_acl(17)
                else:
                    with self.assertRaises(component.ComponentError):
                        component.check_directory_acl(17)

    def test_case_alias(self):
        self.fixture()
        original = self.root / "component.json"
        intermediate = self.root / "rename-test"
        original.rename(intermediate)
        intermediate.rename(self.root / "Component.json")
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_missing_file(self):
        self.fixture()
        (self.root / component.WRITER).unlink()
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_tampered_worker(self):
        self.fixture()
        (self.root / component.WORKER).write_bytes(b"tampered")
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_tampered_writer(self):
        self.fixture()
        (self.root / component.WRITER).write_bytes(b"tampered")
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_typed_record(self):
        self.fixture()
        self.record["schema_version"] = True
        self.write_record()
        with self.assertRaises(component.ComponentError):
            component.verify_files(self.root, DIGEST, REVISION)

    def test_hardlink(self):
        self.fixture()
        os.link(self.root / component.WORKER, self.root / "alias")
        with self.assertRaises(component.ComponentError):
            component.hash_file(self.root / component.WORKER, component.MAX_WORKER)

    def test_bounded_read(self):
        source = self.root / "metadata"
        source.write_bytes(b"x" * 20)
        with self.assertRaises(component.ComponentError):
            component.read_bounded(source, 10)
        self.assertEqual(component.read_bounded(source, 20), b"x" * 20)

    def test_copy_is_exclusive_and_independently_hashed(self):
        source, target = self.root / "source", self.root / "target"
        source.write_bytes(WORKER_BYTES)
        self.assertEqual(component.copy_new(source, target, 1024), (DIGEST, len(WORKER_BYTES)))
        with self.assertRaises(FileExistsError):
            component.copy_new(source, target, 1024)
        self.assertEqual(target.read_bytes(), WORKER_BYTES)

    def test_marker_boundary(self):
        binary = self.root / "binary"
        binary.write_bytes(b"x" * 65534 + b"frozen-identity")
        component.has_markers(binary, [b"frozen-identity"], component.MAX_WORKER)
        with self.assertRaises(component.ComponentError):
            component.has_markers(binary, [b"missing"], component.MAX_WORKER)

    def test_sterile_probe_environment(self):
        with patch.dict(os.environ, {"DYLD_INSERT_LIBRARIES": "untrusted", "SECRET": "never"}):
            self.assertEqual(set(component.probe_environment()), {"PATH", "LANG", "LC_ALL"})

    def test_build_override_isolation(self):
        with patch.dict(os.environ, {"RUSTC": "bad", "RUSTFLAGS": "bad", "SECRET": "bad",
                                     "DYLD_INSERT_LIBRARIES": "bad", "CARGO_BUILD_RUSTC": "bad",
                                     "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER": "bad",
                                     "SCRIBE_BUNDLED_WORKER_SHA256": "bad"}):
            environment = component.build_environment(DIGEST, REVISION)
        for key in ("RUSTC", "RUSTFLAGS", "SECRET", "DYLD_INSERT_LIBRARIES", "CARGO_BUILD_RUSTC",
                    "CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER"):
            self.assertNotIn(key, environment)
        self.assertEqual(environment["SCRIBE_BUNDLED_WORKER_SHA256"], DIGEST)
        self.assertEqual(environment["RUSTC_WRAPPER"], "")
        self.assertEqual(environment["CARGO_ENCODED_RUSTFLAGS"], "")

    def test_app_ancestor_rejection(self):
        app = self.root / "Scribe.app"
        child = app / "Contents"
        child.mkdir(parents=True)
        with self.assertRaises(component.ComponentError):
            component.require_outside_app(child, app)
        component.require_outside_app(self.root, app)


class Posix(Portable):
    # Do not inherit/re-run Portable tests: runner loads only names defined here.
    def test_symlink_file(self):
        target = self.root / "target"
        target.write_bytes(b"data")
        alias = self.root / "alias"
        alias.symlink_to(target)
        with self.assertRaises(component.ComponentError):
            component.hash_file(alias, 1024)

    def test_private_workspace_cleanup(self):
        with component.private_workspace(self.root, "owned-") as workspace:
            (workspace / "fixture").write_bytes(b"private")
        self.assertFalse(workspace.exists())

    def test_uncertain_workspace_is_retained(self):
        with self.assertRaises(component.CleanupUncertain):
            with component.private_workspace(self.root, "owned-") as workspace:
                raise component.CleanupUncertain("fixture")
        self.assertTrue(workspace.is_dir())

    def test_replaced_workspace_not_deleted(self):
        with self.assertRaises(component.ComponentError):
            with component.private_workspace(self.root, "owned-") as workspace:
                workspace.rename(self.root / "retained-original")
                workspace.mkdir()
                (workspace / "sentinel").write_bytes(b"retain")
        self.assertEqual((workspace / "sentinel").read_bytes(), b"retain")

    def test_capture_success_and_empty_stdin(self):
        data = component.capture([sys.executable, "-I", "-S", "-c",
                                  "import sys; assert sys.stdin.read() == ''; print('bounded')"])
        self.assertEqual(data, b"bounded\n")

    def test_capture_nonzero(self):
        with self.assertRaises(component.ComponentError):
            component.capture([sys.executable, "-I", "-S", "-c", "raise SystemExit(7)"])

    def test_capture_timeout_and_reap(self):
        with self.assertRaisesRegex(component.ComponentError, "deadline"):
            component.capture([sys.executable, "-I", "-S", "-c", "while True: pass"], timeout=0.2)

    def test_capture_flood_bounds(self):
        for descriptor in (1, 2):
            with self.subTest(descriptor=descriptor), self.assertRaisesRegex(component.ComponentError, "output bound"):
                component.capture([sys.executable, "-I", "-S", "-c",
                                   f"import os; os.write({descriptor}, b'x' * 4096)"], limit=32, error_limit=32)

    def test_capture_requires_absolute_path(self):
        with self.assertRaises(component.ComponentError):
            component.capture(["python3", "-V"])

    def test_lingering_group_fails_closed_without_signal(self):
        with patch.object(os, "killpg") as kill_group, self.assertRaises(component.CleanupUncertain):
            component.capture([sys.executable, "-I", "-S", "-c", "pass"])
        self.assertTrue(kill_group.call_args_list)
        self.assertTrue(all(call.args[1] == 0 for call in kill_group.call_args_list))

    def test_inspector_uses_private_snapshot_and_never_writer(self):
        self.fixture()
        seen = []
        def inspect(path, markers, workspace, maximum):
            self.assertNotEqual(path.parent.parent, self.root)
            self.assertEqual(path.parent.parent.name, "snapshot")
            seen.append(path.name)
        with patch.object(component.sys, "platform", "darwin"), \
             patch.object(component, "inspect_universal", side_effect=inspect), \
             patch.object(component, "capture") as execute:
            component.inspect_component(self.root, DIGEST, REVISION)
        execute.assert_not_called()
        self.assertEqual(seen, ["scribe-inference-worker", "scribe-macos-cpu-worker-installer"])

    def test_wrong_architecture(self):
        binary = self.root / "binary"
        binary.write_bytes(b"fixture")
        for arches in [b"arm64", b"x86_64", b"arm64 x86_64 arm64e"]:
            with self.subTest(arches=arches), patch.object(component, "capture", return_value=arches), \
                 self.assertRaisesRegex(component.ComponentError, "exactly ARM64 and x64"):
                component.inspect_universal(binary, [], self.root, 1024)

    def test_binary_rejection_predicates(self):
        binary = self.root / "binary"
        binary.write_bytes(b"fixture")
        for attack in ("valid", "minos", "dylib", "metal", "cuda", "vulkan", "identity", "signature"):
            def execute(arguments, **_kwargs):
                if arguments[0] == "/usr/bin/lipo" and arguments[-1] == "-archs":
                    return b"arm64 x86_64"
                if arguments[0] == "/usr/bin/codesign":
                    if attack == "signature":
                        raise component.ComponentError("signature fixture")
                    return b""
                if arguments[0] == "/usr/bin/lipo":
                    marker = b"missing" if attack == "identity" and arguments[3] == "x86_64" else b"identity"
                    Path(arguments[-1]).write_bytes(marker)
                    return b""
                if arguments[1] == "-l":
                    return b"minos 12.0\n" if attack == "minos" else b"minos 13.0\n"
                library = {"dylib": "@rpath/untrusted.dylib", "metal": "/System/Library/Frameworks/Metal.framework/Metal",
                           "cuda": "/usr/lib/libcuda.dylib", "vulkan": "/usr/lib/libvulkan.dylib"}.get(
                               attack, "/usr/lib/libSystem.B.dylib")
                return f"binary:\n\t{library} (compatibility version 1.0.0)\n".encode()
            with self.subTest(attack=attack), patch.object(component, "capture", side_effect=execute):
                if attack == "valid":
                    component.inspect_universal(binary, [b"identity"], self.root, 1024)
                else:
                    with self.assertRaises(component.ComponentError):
                        component.inspect_universal(binary, [b"identity"], self.root, 1024)
            self.assertFalse((self.root / "binary-arm64").exists())
            self.assertFalse((self.root / "binary-x86_64").exists())

    def assembly_fixture(self, failure=None):
        app = self.root / "Scribe.app"
        native, resources = app / "Contents/MacOS", app / "Contents/Resources"
        native.mkdir(parents=True)
        resources.mkdir()
        (native / "scribe-inference-worker").write_bytes(WORKER_BYTES)
        (native / "Scribe").write_bytes(b"desktop fixture")
        (resources / "worker-pack-catalog.json").write_bytes(b'{"schema_version":1,"packs":[]}')
        (resources / "gpu-pack-release-authority.json").write_bytes((component.REPO /
            "runtime-manifests/gpu-pack-release-authority-macos-empty.json").read_bytes())
        (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleShortVersionString": "0.1.0"}))
        output = self.root / "published"
        builds, probes = [], []
        def execute(arguments, **kwargs):
            if arguments[0] == "/usr/bin/cargo":
                self.assertIn("--locked", arguments)
                self.assertIn("--offline", arguments)
                self.assertEqual(kwargs["cwd"], component.REPO)
                environment = kwargs["environment"]
                self.assertEqual(environment["SCRIBE_BUNDLED_WORKER_SHA256"], DIGEST)
                self.assertEqual(environment["SCRIBE_BUILD_REVISION"], REVISION)
                target = arguments[arguments.index("--target") + 1]
                builds.append(target)
                if failure == "build" and len(builds) == 2:
                    raise component.ComponentError("build fixture")
                writer = Path(environment["CARGO_TARGET_DIR"]) / target / "release/scribe-macos-cpu-worker-installer"
                writer.parent.mkdir(parents=True)
                writer.write_bytes(b"fresh writer")
            elif arguments[0] == "/usr/bin/lipo":
                Path(arguments[3]).write_bytes(b"fresh universal writer")
            elif arguments[-1] == "describe":
                self.assertEqual(Path(arguments[0]).name, "scribe-macos-cpu-worker-installer")
                self.assertIn(".scribe-cpu-component-build-", arguments[0])
                probes.append(arguments)
                return json.dumps(component.expected_description(
                    "0" * 64 if failure == "description" else DIGEST, REVISION)).encode()
            else:
                self.assertIn(arguments[0], ("/bin/bash", "/usr/bin/codesign"))
            return b""
        return app, output, execute, builds, probes

    def test_source_assembly_binds_fresh_builds_and_probe(self):
        app, output, execute, builds, probes = self.assembly_fixture()
        with patch.object(component.sys, "platform", "darwin"), \
             patch.object(component.os, "geteuid", return_value=os.geteuid() or 501), \
             patch.object(component.shutil, "which", return_value="/usr/bin/cargo"), \
             patch.object(component, "capture", side_effect=execute), \
             patch.object(component, "inspect_universal"), \
             patch.object(component, "publish_no_replace", side_effect=lambda a, b: a.rename(b)):
            component.assemble(app, output, DIGEST, REVISION)
        self.assertEqual(builds, list(component.TARGETS))
        self.assertEqual(len(probes), 1)
        component.verify_files(output, DIGEST, REVISION)
        self.assertEqual((app / "Contents/MacOS/scribe-inference-worker").read_bytes(), WORKER_BYTES)
        self.assertFalse(list(self.root.glob(".scribe-cpu-component-build-*")))

    def test_source_assembly_failure_preserves_app_without_publication(self):
        for failure in ("build", "description"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(dir=self.root) as temporary:
                saved_root, self.root = self.root, Path(temporary)
                try:
                    app, output, execute, builds, probes = self.assembly_fixture(failure)
                    with patch.object(component.sys, "platform", "darwin"), \
                         patch.object(component.os, "geteuid", return_value=os.geteuid() or 501), \
                         patch.object(component.shutil, "which", return_value="/usr/bin/cargo"), \
                         patch.object(component, "capture", side_effect=execute), \
                         patch.object(component, "inspect_universal"), \
                         patch.object(component, "publish_no_replace") as publish, \
                         self.assertRaises(component.ComponentError):
                        component.assemble(app, output, DIGEST, REVISION)
                    publish.assert_not_called()
                    self.assertFalse(output.exists())
                    self.assertEqual((app / "Contents/MacOS/scribe-inference-worker").read_bytes(), WORKER_BYTES)
                    self.assertFalse(list(self.root.glob(".scribe-cpu-component-build-*")))
                finally:
                    self.root = saved_root

    def test_existing_output_is_not_replaced_or_executed(self):
        app = self.root / "Scribe.app"
        app.mkdir()
        output = self.root / "component"
        output.mkdir()
        (output / "sentinel").write_bytes(b"retain")
        with patch.object(component.sys, "platform", "darwin"), \
             patch.object(component.os, "geteuid", return_value=501), \
             patch.object(component, "capture") as execute, self.assertRaises(component.ComponentError):
            component.assemble(app, output, DIGEST, REVISION)
        execute.assert_not_called()
        self.assertEqual((output / "sentinel").read_bytes(), b"retain")

    def test_interrupted_probe_reaps(self):
        with patch.object(component.selectors.DefaultSelector, "select", side_effect=KeyboardInterrupt), \
             self.assertRaises(KeyboardInterrupt):
            component.capture([sys.executable, "-I", "-S", "-c", "while True: pass"])


class NativeMac(Portable):
    def test_mutation_acl_parent_is_rejected(self):
        component.capture(["/bin/chmod", "+a", "everyone allow delete_child,add_subdirectory", str(self.root)])
        try:
            with self.assertRaises(component.ComponentError):
                with component.private_workspace(self.root, "must-not-create-"):
                    self.fail("ACL widened parent was accepted")
        finally:
            component.capture(["/bin/chmod", "-N", str(self.root)])

    def test_writable_ancestor_is_rejected(self):
        ancestor = self.root / "shared"
        child = ancestor / "private"
        child.mkdir(parents=True, mode=0o700)
        ancestor.chmod(0o777)
        try:
            with self.assertRaises(component.ComponentError):
                with component.private_workspace(child, "must-not-create-"):
                    self.fail("mutable ancestor was accepted")
        finally:
            ancestor.chmod(0o700)

    def test_exclusive_publication(self):
        stage, target = self.root / "stage", self.root / "target"
        stage.mkdir()
        (stage / "sentinel").write_bytes(b"first")
        component.publish_no_replace(stage, target)
        stage.mkdir()
        with self.assertRaises(OSError):
            component.publish_no_replace(stage, target)
        self.assertEqual((target / "sentinel").read_bytes(), b"first")

    def test_case_alias_app_ancestor(self):
        app = self.root / "Scribe.app"
        app.mkdir()
        alias = self.root / "scribe.app"
        if not alias.exists():
            alias.symlink_to(app, target_is_directory=True)
        with self.assertRaises(component.ComponentError):
            component.require_outside_app(alias, app)


def suite():
    result = unittest.TestSuite()
    classes = [Portable]
    if os.name == "posix":
        classes.append(Posix)
    if sys.platform == "darwin":
        classes.append(NativeMac)
    expected = 21 + (17 if os.name == "posix" else 0) + (4 if sys.platform == "darwin" else 0)
    for cls in classes:
        for name in sorted(cls.__dict__):
            if name.startswith("test_"):
                result.addTest(cls(name))
    if result.countTestCases() != expected:
        raise RuntimeError(f"component test discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
