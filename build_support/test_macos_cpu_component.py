"""Source-only component checks; never invoke the installer command or /Library."""
from contextlib import ExitStack, contextmanager
import hashlib
import ctypes
import errno
import json
import os
from pathlib import Path
import plistlib
import shutil
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

import macos_cpu_component as component
import macos_cpu_pkg_entry as entry

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

    def test_marker_diagnostic_labels_are_closed_before_reading(self):
        for role, architecture in [("private-sentinel", "arm64"), ([], "arm64"),
                                   ("writer", "private-sentinel"), ("writer", None)]:
            with self.subTest(role=role, architecture=architecture), \
                 patch.object(component, "open_regular") as opened, \
                 self.assertRaises(component.ComponentError) as result:
                component.has_markers(self.root / "private-sentinel", [], 1024,
                                      role=role, architecture=architecture)
            opened.assert_not_called()
            self.assertNotIn("private-sentinel", str(result.exception))
        with patch.object(component, "capture") as execute, self.assertRaises(component.ComponentError):
            component._inspect_universal_slices(self.root / "unused", [], self.root, 1024, role="private-sentinel")
        execute.assert_not_called()

    def test_each_missing_slice_marker_rejects_with_redacted_context(self):
        binary = self.root / "private-path-sentinel"
        markers = [b"private-build-sentinel", b"private-digest-sentinel"]
        binary.write_bytes(b" ".join(markers))  # Presence in the fat image is insufficient.
        for architecture in ("arm64", "x86_64"):
            for missing_index in (0, 1):
                def execute(arguments, **_kwargs):
                    if arguments[0] == "/usr/bin/lipo":
                        content = [marker for index, marker in enumerate(markers)
                                   if arguments[3] != architecture or index != missing_index]
                        Path(arguments[-1]).write_bytes(b" ".join(content))
                        return b""
                    if arguments[1] == "-l":
                        return b"minos 13.0\n"
                    return b"fixture:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0)\n"
                with self.subTest(architecture=architecture, missing_index=missing_index), \
                     patch.object(component, "capture", side_effect=execute), \
                     self.assertRaises(component.ComponentError) as result:
                    component._inspect_universal_slices(binary, markers, self.root, 1024, role="writer")
                error = result.exception
                self.assertEqual(str(error), "binary slice does not contain the frozen identities "
                                 f"(role=writer; architecture={architecture}; marker-indices={missing_index})")
                self.assertNotIn("sentinel", str(error) + repr(error) + repr(error.args) + repr(error.__dict__))
                self.assertFalse((self.root / (binary.name + "-arm64")).exists())
                self.assertFalse((self.root / (binary.name + "-x86_64")).exists())

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

    def test_tool_exit_diagnostic_contains_only_closed_operation_and_numeric_status(self):
        for status in [1, 101, -9, 3221225477]:
            error = component.ToolExitError("writer-build-arm64", status)
            self.assertIsInstance(error, component.ComponentError)
            self.assertEqual(error.__dict__, {"operation": "writer-build-arm64", "status": status})
            self.assertIn("writer-build-arm64", str(error))
            self.assertIn(str(status), str(error))
            if os.name == "posix" and status == -9:
                self.assertIn("signal 9", str(error))
        for status in [0, True, "secret", 1 << 32, -(1 << 31) - 1]:
            with self.subTest(status=status), self.assertRaises(component.ComponentError):
                component.ToolExitError("writer-description", status)

    def test_unknown_operation_rejects_before_spawning_or_rendering_input(self):
        for operation in ["private-path-or-secret", "", None, [], 5]:
            with self.subTest(operation=operation), patch.object(component.subprocess, "Popen") as spawn, \
                 self.assertRaisesRegex(component.ComponentError, "unknown bounded tool operation"):
                component.capture([sys.executable, "private-argument"], operation=operation)
            spawn.assert_not_called()
        with self.assertRaisesRegex(component.ComponentError, "unknown bounded tool operation"):
            component.ToolExitError("private-path-or-secret", 1)

    def test_signature_property_stream_is_fixed_command_only_before_spawn(self):
        valid = ["/usr/bin/codesign", "--display", "--verbose=4", "--arch", "arm64", "/owned/postinstall"]
        cases = [(valid, "tool", True), (valid, "final-entry-properties", False),
                 (valid, "final-entry-properties", "private-value")]
        for position, value in [(0, "/private/tool"), (1, "--sign"), (2, "--verbose=6"),
                                (3, "--all-architectures"), (4, "all"), (5, "relative"), (5, None)]:
            changed = valid.copy()
            changed[position] = value
            cases.append((changed, "final-entry-properties", True))
        for arguments, operation, flag in cases:
            with patch.object(component.subprocess, "Popen") as spawn, self.assertRaises(component.ComponentError) as caught:
                component.capture(arguments, operation=operation, signature_properties=flag)
            spawn.assert_not_called()
            self.assertNotIn("private", str(caught.exception))

    def test_slice_inspection_retains_thin_after_uncertain_child_cleanup(self):
        binary = self.root / "binary"
        binary.write_bytes(b"fixture")
        thin = self.root / "binary-arm64"
        def capture(arguments, **_kwargs):
            if arguments[0] == "/usr/bin/lipo":
                Path(arguments[-1]).write_bytes(b"fixture")
                return b""
            raise component.CleanupUncertain("synthetic otool cleanup uncertainty")
        with patch.object(component, "capture", side_effect=capture), \
             self.assertRaises(component.CleanupUncertain):
            component._inspect_universal_slices(binary, [], self.root, 1024)
        self.assertEqual(thin.read_bytes(), b"fixture")
        # The child is mocked; the test-owned directory can safely be removed.

    def test_signed_wrapper_keeps_signature_verification_before_slices(self):
        binary = self.root / "binary"
        binary.write_bytes(b"fixture")
        calls = []
        def capture(arguments, **_kwargs):
            calls.append(arguments)
            return b"arm64 x86_64" if arguments[0] == "/usr/bin/lipo" else b""
        def inspect(*_args, **kwargs):
            self.assertEqual(calls[-1], ["/usr/bin/codesign", "--verify", "--strict", str(binary)])
            self.assertEqual(kwargs, {"role": "artifact"})
        with patch.object(component, "capture", side_effect=capture), \
             patch.object(component, "_inspect_universal_slices", side_effect=inspect) as slices:
            component.inspect_universal(binary, [], self.root, 1024)
        slices.assert_called_once_with(binary, [], self.root, 1024, role="artifact")
        def reject(arguments, **_kwargs):
            if arguments[0] == "/usr/bin/codesign":
                raise component.ComponentError("invalid signature")
            return b"arm64 x86_64"
        with patch.object(component, "capture", side_effect=reject), \
             patch.object(component, "_inspect_universal_slices") as slices, \
             self.assertRaises(component.ComponentError):
            component.inspect_universal(binary, [], self.root, 1024)
        slices.assert_not_called()

    def test_structure_wrapper_does_not_verify_signatures_or_execute_an_image(self):
        binary = self.root / "postinstall"
        binary.write_bytes(b"fixture")
        with patch.object(component, "capture", return_value=b"arm64 x86_64") as capture, \
             patch.object(component, "_inspect_universal_slices") as slices:
            component.inspect_universal_structure(binary, [], self.root, 1024)
        capture.assert_called_once_with(["/usr/bin/lipo", str(binary), "-archs"], operation="artifact-architectures")
        slices.assert_called_once_with(binary, [], self.root, 1024, role="structure")

    def test_structure_wrapper_preserves_size_and_exact_architecture_checks(self):
        binary = self.root / "binary"
        binary.write_bytes(b"fixture")
        with patch.object(component, "capture") as capture, self.assertRaises(component.ComponentError):
            component.inspect_universal_structure(binary, [], self.root, 1)
        capture.assert_not_called()
        for architectures in [b"arm64", b"x86_64", b"arm64 x86_64 arm64e"]:
            with patch.object(component, "capture", return_value=architectures), \
                 patch.object(component, "_inspect_universal_slices") as slices, \
                 self.assertRaises(component.ComponentError):
                component.inspect_universal_structure(binary, [], self.root, 1024)
            slices.assert_not_called()

    def app_snapshot_fixture(self, name="app-fixture"):
        root = self.root / name
        root.mkdir()
        app = root / "Scribe.app"
        for relative in component.CPU_APP_INPUTS:
            path = app / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(WORKER_BYTES if relative == component.CPU_APP_INPUTS[0] else b"context fixture")
        workspace = root / "private"
        workspace.mkdir()
        return app, workspace / "worker", workspace / "Scribe", workspace

    def test_desktop_copy_keeps_signature_verification_in_bundle_context(self):
        app, worker, desktop, workspace = self.app_snapshot_fixture()
        calls = []
        with patch.object(component, "verify_cpu_app", return_value="0.1.0") as verify, \
             patch.object(component, "capture", side_effect=lambda args, **kwargs: calls.append((args, kwargs))) as capture, \
             patch.object(component, "inspect_universal") as signed, \
             patch.object(component, "inspect_universal_structure", side_effect=lambda *_args: self.assertEqual(len(calls), 1)) as structure:
            size = component.snapshot_cpu_app_inputs(app, worker, desktop, workspace, DIGEST, REVISION)
        self.assertEqual(size, len(WORKER_BYTES))
        verify.assert_called_once_with(app)
        capture.assert_called_once_with(["/usr/bin/codesign", "--verify", "--strict",
                                         str(app / component.CPU_APP_INPUTS[1])],
                                        operation="desktop-bundle-signature-verification")
        signed.assert_called_once_with(worker, [f"scribe-inference-worker@0.1.0#{REVISION}".encode()],
                                       workspace, component.MAX_WORKER,
                                       signature_operation="copied-worker-signature-verification")
        structure.assert_called_once_with(desktop, [f"local-transcriber@0.1.0#{REVISION}".encode(), DIGEST.encode()],
                                          workspace, component.MAX_WORKER)
        self.assertEqual(worker.read_bytes(), WORKER_BYTES)
        self.assertEqual(desktop.read_bytes(), (app / component.CPU_APP_INPUTS[1]).read_bytes())

    def test_app_or_worker_signature_failure_cannot_reach_desktop_inspection(self):
        for failure in ("app", "main", "worker"):
            app, worker, desktop, workspace = self.app_snapshot_fixture(failure)
            def verify(_app):
                if failure == "app":
                    raise component.ComponentError("invalid app signature")
                return "0.1.0"
            def capture(*_args, **_kwargs):
                if failure == "main":
                    raise component.ToolExitError("desktop-bundle-signature-verification", 1)
            def signed(*_args, **_kwargs):
                if failure == "worker":
                    raise component.ToolExitError("copied-worker-signature-verification", 1)
            with patch.object(component, "verify_cpu_app", side_effect=verify), \
                 patch.object(component, "capture", side_effect=capture), \
                 patch.object(component, "inspect_universal", side_effect=signed), \
                 patch.object(component, "inspect_universal_structure") as structure, \
                 self.assertRaises(component.ComponentError):
                component.snapshot_cpu_app_inputs(app, worker, desktop, workspace, DIGEST, REVISION)
            structure.assert_not_called()

    def test_every_source_context_change_or_same_bytes_replacement_rejects(self):
        for index, relative in enumerate(component.CPU_APP_INPUTS):
            for replace in (False, True):
                app, worker, desktop, workspace = self.app_snapshot_fixture(f"change-{index}-{replace}")
                def verify(_app):
                    path = app / relative
                    saved = path.read_bytes()
                    if replace:
                        path.rename(path.with_name(path.name + ".old"))
                    path.write_bytes(saved if replace else saved + b"changed")
                    return "0.1.0"
                with self.subTest(relative=relative, replace=replace), \
                     patch.object(component, "verify_cpu_app", side_effect=verify), \
                     patch.object(component, "capture"), \
                     patch.object(component, "inspect_universal") as signed, \
                     self.assertRaisesRegex(component.ComponentError, "inputs changed"):
                    component.snapshot_cpu_app_inputs(app, worker, desktop, workspace, DIGEST, REVISION)
                signed.assert_not_called()

    def test_copied_byte_mismatch_and_changes_during_inspection_reject(self):
        original_copy = component.copy_new
        for index, failure in enumerate(("wrong-copy", "worker", "desktop", *component.CPU_APP_INPUTS)):
            app, worker, desktop, workspace = self.app_snapshot_fixture(f"copy-{index}")
            def copy(source, output, maximum):
                result = original_copy(source, output, maximum)
                return ("0" * 64, result[1]) if failure == "wrong-copy" and output == desktop else result
            def inspect(*_args):
                if failure != "wrong-copy":
                    path = worker if failure == "worker" else desktop if failure == "desktop" else app / failure
                    path.write_bytes(path.read_bytes() + b"changed")
            with self.subTest(failure=failure), patch.object(component, "copy_new", side_effect=copy), \
                 patch.object(component, "verify_cpu_app", return_value="0.1.0") as verify, \
                 patch.object(component, "capture"), patch.object(component, "inspect_universal"), \
                 patch.object(component, "inspect_universal_structure", side_effect=inspect), \
                 self.assertRaisesRegex(component.ComponentError, "copy differs|inspection changed"):
                component.snapshot_cpu_app_inputs(app, worker, desktop, workspace, DIGEST, REVISION)
            if failure == "wrong-copy":
                verify.assert_not_called()

    def test_signature_role_is_closed_before_any_tool(self):
        with patch.object(component, "capture") as capture, self.assertRaises(component.ComponentError):
            component.inspect_universal(self.root / "absent", [], self.root, 1024,
                                        signature_operation="private/arbitrary/path")
        capture.assert_not_called()

    def test_both_callers_stop_before_build_or_publication_after_admission_rejection(self):
        @contextmanager
        def private(parent, prefix):
            with tempfile.TemporaryDirectory(dir=parent, prefix=prefix) as directory:
                yield Path(directory)
        for caller in ("component", "entry"):
            for failure in ("app", "main", "worker", "copy", "context"):
                app, _worker, _desktop, workspace = self.app_snapshot_fixture(f"{caller}-{failure}")
                output = app.parent / "published" if caller == "component" else workspace / "postinstall"
                with self.subTest(caller=caller, failure=failure), ExitStack() as stack:
                    stack.enter_context(patch.object(component.sys, "platform", "darwin"))
                    stack.enter_context(patch.object(component.os, "geteuid", return_value=501, create=True))
                    stack.enter_context(patch.object(component.os, "waitid", create=True))
                    stack.enter_context(patch.object(component, "private_workspace", side_effect=private))
                    admission = stack.enter_context(patch.object(component, "snapshot_cpu_app_inputs",
                        side_effect=component.ComponentError(f"synthetic {failure} admission rejection")))
                    environment = stack.enter_context(patch.object(component, "build_environment"))
                    capture = stack.enter_context(patch.object(component, "capture"))
                    publication = stack.enter_context(patch.object(component, "publish_no_replace"))
                    with self.assertRaisesRegex(component.ComponentError, f"synthetic {failure} admission rejection"):
                        if caller == "component":
                            component.assemble(app, output, DIGEST, REVISION)
                        else:
                            entry.build_entry(app, workspace, DIGEST, REVISION)
                    admission.assert_called_once()
                    environment.assert_not_called()
                    capture.assert_not_called()
                    publication.assert_not_called()
                    self.assertFalse(output.exists())


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

    def test_signature_property_stream_keeps_both_bounds_and_returns_only_stderr(self):
        original = component.subprocess.Popen
        status = 0
        def synthetic(_arguments, **kwargs):
            return original([sys.executable, "-I", "-S", "-c",
                             "import sys; print('stdout-must-not-be-returned'); print('bounded-properties', file=sys.stderr); "
                             f"sys.exit({status})"],
                            **kwargs)
        arguments = ["/usr/bin/codesign", "--display", "--verbose=4", "--arch", "x86_64", "/owned/postinstall"]
        with patch.object(component.subprocess, "Popen", side_effect=synthetic):
            self.assertEqual(component.capture(arguments, operation="final-entry-properties", signature_properties=True),
                             b"bounded-properties\n")
            for limit, error_limit in [(4, 128), (128, 4)]:
                with self.assertRaisesRegex(component.ComponentError, "output bound"):
                    component.capture(arguments, operation="final-entry-properties", signature_properties=True,
                                      limit=limit, error_limit=error_limit)
            status = 17
            with self.assertRaises(component.ToolExitError) as caught:
                component.capture(arguments, operation="final-entry-properties", signature_properties=True)
            self.assertEqual((caught.exception.operation, caught.exception.status), ("final-entry-properties", 17))
            self.assertNotIn("bounded-properties", str(caught.exception))
            self.assertNotIn("stdout-must-not-be-returned", str(caught.exception))

    def test_capture_nonzero(self):
        with self.assertRaises(component.ComponentError):
            component.capture([sys.executable, "-I", "-S", "-c", "raise SystemExit(7)"])

    def test_tool_exit_diagnostic_redacts_real_child_arguments_environment_and_streams(self):
        sentinel = "private-path-audio-transcript-secret-sentinel"
        command = [sys.executable, "-I", "-S", "-c",
                   f"import sys; print('{sentinel}'); print('{sentinel}', file=sys.stderr); sys.exit(17)"]
        with self.assertRaises(component.ToolExitError) as result:
            component.capture(command, environment={**component.probe_environment(), "PRIVATE": sentinel},
                              operation="application-verification")
        error = result.exception
        self.assertEqual((error.operation, error.status), ("application-verification", 17))
        rendered = str(error) + repr(error) + repr(error.args) + repr(error.__dict__)
        self.assertNotIn(sentinel, rendered)
        self.assertNotIn(sys.executable, rendered)
        self.assertIsNone(error.__cause__)
        # An uncertain descendant outcome wins even when the leader exits nonzero.
        with patch.object(os, "killpg"), self.assertRaises(component.CleanupUncertain):
            component.capture([sys.executable, "-I", "-S", "-c", "raise SystemExit(17)"],
                              operation="application-verification")

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
        def inspect(path, markers, workspace, maximum, *, signature_operation):
            self.assertEqual(signature_operation, "copied-worker-signature-verification"
                             if path.name == "scribe-inference-worker" else "writer-signature-verification")
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
        (app / "Contents/_CodeSignature").mkdir()
        (app / "Contents/_CodeSignature/CodeResources").write_bytes(b"synthetic signature context")
        output = self.root / "published"
        builds, probes = [], []
        def execute(arguments, **kwargs):
            self.assertIn(kwargs["operation"], component.TOOL_OPERATIONS)
            self.assertNotEqual(kwargs["operation"], "tool")
            if arguments[0] == "/usr/bin/cargo":
                self.assertIn("--locked", arguments)
                self.assertIn("--offline", arguments)
                self.assertEqual(kwargs["cwd"], component.REPO)
                environment = kwargs["environment"]
                self.assertEqual(environment["SCRIBE_BUNDLED_WORKER_SHA256"], DIGEST)
                self.assertEqual(environment["SCRIBE_BUILD_REVISION"], REVISION)
                target = arguments[arguments.index("--target") + 1]
                self.assertEqual(kwargs["operation"], "writer-build-arm64" if target.startswith("aarch64")
                                 else "writer-build-x86_64")
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
             patch.object(component, "inspect_universal_structure"), \
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
                         patch.object(component, "inspect_universal_structure"), \
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
    def test_optimized_rust_provenance_survives_both_slices_without_execution(self):
        # Include the production definitions, not a C lookalike or runtime use of
        # the values. Full LTO, symbol stripping and linker GC must retain bytes.
        compiler = shutil.which("rustc")
        self.assertIsNotNone(compiler, "reviewed Rust compiler is required")
        self.assertTrue(Path(compiler).is_absolute())
        with component.private_workspace(self.root, "native-retained-provenance-") as root:
            source = root / "fixture.rs"
            source.write_text(
                '#[allow(dead_code)]\n#[path = ' + json.dumps(str(component.REPO / "src/worker_identity.rs")) +
                '] mod worker_identity;\n#[allow(dead_code)]\n#[path = ' +
                json.dumps(str(component.REPO / "src/macos_cpu_identity.rs")) +
                '] mod cpu_identity;\nfn main() {}\n', encoding="utf-8")
            previous = []
            for variant, digit in enumerate(("a", "b")):
                digest, revision = digit * 64, f"native-retained-revision-{variant}"
                markers = [f"local-transcriber@0.1.0#{revision}".encode(),
                           f"scribe-inference-worker@0.1.0#{revision}".encode(),
                           digest.encode(), revision.encode(), component.CPU_ROOT.encode()]
                environment = component.build_environment(digest, revision)
                environment["CARGO_PKG_VERSION"] = "0.1.0"
                slices = []
                for target in component.TARGETS:
                    output = root / f"fixture-{variant}-{target}"
                    component.capture([compiler, "--edition=2024", "--target", target,
                                       "-C", "opt-level=3", "-C", "lto=fat", "-C", "codegen-units=1",
                                       "-C", "strip=symbols", "-C", "link-arg=-Wl,-dead_strip",
                                       str(source), "-o", str(output)], cwd=component.REPO,
                                      environment=environment, timeout=60, limit=65536, error_limit=65536)
                    component.has_markers(output, markers, component.MAX_WRITER)
                    image = output.read_bytes()
                    for stale in previous:
                        self.assertNotIn(stale, image)
                    slices.append(str(output))
                universal = root / f"universal-{variant}"
                component.capture(["/usr/bin/lipo", "-create", *slices, "-output", str(universal)])
                component.inspect_universal_structure(universal, markers, root, component.MAX_WRITER)
                previous = markers[:4]  # The protected root intentionally stays constant.

    def test_actual_bundle_context_preserves_signed_input_binding_without_execution(self):
        # Fresh inert code, never launched. Match release's worker/main/outer-app
        # signing order, using ad hoc signatures only (no key or identity).
        with component.private_workspace(self.root, "native-signature-context-") as root:
            app = root / "Fixture.app"
            native, resources = app / "Contents/MacOS", app / "Contents/Resources"
            native.mkdir(parents=True, mode=0o700)
            resources.mkdir(mode=0o700)
            worker, desktop = native / "scribe-inference-worker", native / "Scribe"
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({
                "CFBundleIdentifier": "com.scribe.source-test.signature-context",
                "CFBundleExecutable": "Scribe", "CFBundlePackageType": "APPL",
                "CFBundleName": "SourceFixture", "CFBundleShortVersionString": "0.1.0",
                "LSMinimumSystemVersion": "13.0"}))
            (resources / "worker-pack-catalog.json").write_bytes(b'{"schema_version":1,"packs":[]}')
            (resources / "gpu-pack-release-authority.json").write_bytes((component.REPO /
                "runtime-manifests/gpu-pack-release-authority-macos-empty.json").read_bytes())
            def compile_image(output, marker):
                source = root / (output.name + ".c")
                source.write_text('const char marker[] = ' + json.dumps(marker) + ';\n'
                                  'int main(void) { return marker[0] == 0; }\n', encoding="ascii")
                component.capture(["/usr/bin/clang", "-arch", "arm64", "-arch", "x86_64",
                                   "-mmacosx-version-min=13.0", str(source), "-o", str(output)],
                                  timeout=30, limit=65536, error_limit=65536)
                component.capture(["/usr/bin/codesign", "--force", "--sign", "-", "--options", "runtime", str(output)])
            compile_image(worker, f"scribe-inference-worker@0.1.0#{REVISION}")
            digest = component.hash_file(worker, component.MAX_WORKER)[0]
            compile_image(desktop, f"local-transcriber@0.1.0#{REVISION} {digest}")
            component.capture(["/usr/bin/codesign", "--force", "--sign", "-", "--options", "runtime", str(app)])
            before = component.cpu_app_input_snapshot(app)
            self.assertEqual(before[component.CPU_APP_INPUTS[0]][0], digest)
            for path in (app, worker, desktop):
                component.capture(["/usr/bin/codesign", "--verify", "--strict", str(path)])
            loose_worker, loose_main = root / "loose-worker", root / "loose-main"
            component.copy_new(worker, loose_worker, component.MAX_WORKER)
            component.copy_new(desktop, loose_main, component.MAX_WORKER)
            component.capture(["/usr/bin/codesign", "--verify", "--strict", str(loose_worker)],
                              operation="copied-worker-signature-verification")
            loose_main_verified = True
            try:
                component.capture(["/usr/bin/codesign", "--verify", "--strict", str(loose_main)])
            except component.ToolExitError:
                loose_main_verified = False
            # The fixture isolates signature context, not Scribe's full resource
            # policy. That production verifier is never replaced by the helper.
            def verify_context(candidate):
                self.assertEqual(candidate, app)
                component.capture(["/usr/bin/codesign", "--verify", "--strict", str(candidate)])
                return "0.1.0"
            workspace = root / "private"
            workspace.mkdir(mode=0o700)
            with patch.object(component, "verify_cpu_app", side_effect=verify_context):
                component.snapshot_cpu_app_inputs(app, workspace / "worker", workspace / "Scribe",
                                                  workspace, digest, REVISION)
            self.assertEqual(component.cpu_app_input_snapshot(app), before)
            print("SCRIBE_NATIVE_CPU_SIGNATURE_CONTEXT=" + json.dumps({
                "bundle_verified": True, "loose_worker_verified": True,
                "loose_main_verified": loose_main_verified, "bound_snapshot_verified": True,
                "executed": False}, sort_keys=True))

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
    expected = 36 + (19 if os.name == "posix" else 0) + (6 if sys.platform == "darwin" else 0)
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
