"""Portable assertions for the native driver; every build/tool is mocked here."""
from contextlib import ExitStack, contextmanager, redirect_stderr, redirect_stdout
import hashlib
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import macos_cpu_component as component
import native_macos_cpu_pkg_entry as native

REVISION = "source-native-entry-fixture"
WORKER = b"synthetic frozen worker"
DIGEST = hashlib.sha256(WORKER).hexdigest()
IMAGE = b"synthetic universal package entry"


class Portable(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-native-entry-source-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.app = self.root / "Scribe.app"
        for name in native.INPUTS:
            path = self.app / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(WORKER if name == native.INPUTS[0] else b"synthetic input")
        self.failure, self.builds, self.workspace, self.verifications = None, 0, None, 0

    @contextmanager
    def fake_workspace(self, parent, prefix):
        self.assertEqual(prefix, "scribe-native-cpu-entry-")
        with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as directory:
            self.workspace = Path(directory)
            yield self.workspace

    def build(self, app, workspace, digest, revision):
        self.assertEqual((app, digest, revision), (self.app, DIGEST, REVISION))
        self.builds += 1
        self.workspace = workspace
        output = workspace / "postinstall"
        if self.builds == 2:
            if self.failure == "uncertain-second":
                raise component.CleanupUncertain("synthetic repeat cleanup uncertainty")
            if self.failure == "second-success":
                return output, hashlib.sha256(IMAGE).hexdigest(), len(IMAGE)
            if self.failure == "replace":
                output.write_bytes(b"replacement")
            raise component.ComponentError("wrong repeat reason" if self.failure == "repeat-reason"
                                           else "CPU entry output already exists")
        if self.failure in ("build", "uncertain"):
            error = component.CleanupUncertain if self.failure == "uncertain" else component.ComponentError
            raise error("synthetic construction failure")
        output.write_bytes(IMAGE)
        if self.failure == "empty":
            output.write_bytes(b"")
        if self.failure == "linked":
            os.link(output, self.root / "alias")
        if self.failure == "directory":
            output.unlink()
            output.mkdir()
        if self.failure == "extra":
            (workspace / "leftover").mkdir()
        if self.failure in native.INPUTS:
            (app / self.failure).write_bytes(b"tampered input")
        return (workspace / "wrong" if self.failure == "path" else output,
                "0" * 64 if self.failure == "digest" else hashlib.sha256(IMAGE).hexdigest(),
                len(IMAGE) + (1 if self.failure == "size" else 0))

    @contextmanager
    def context(self, *, real_workspace=False):
        def verify(app):
            self.assertEqual(app, self.app)
            self.verifications += 1
            if self.failure == "app-verification" and self.verifications == 2:
                raise component.ComponentError("synthetic final app verification failure")
            return "0.1.0"
        with ExitStack() as stack:
            stack.enter_context(patch.object(native.sys, "platform", "darwin"))
            uid = getattr(os, "geteuid", lambda: 501)() or 501
            stack.enter_context(patch.object(native.os, "geteuid", return_value=uid, create=True))
            stack.enter_context(patch.object(native.tempfile, "gettempdir", return_value=str(self.root)))
            stack.enter_context(patch.object(component, "verify_cpu_app", side_effect=verify))
            stack.enter_context(patch.object(native.entry, "build_entry", side_effect=self.build))
            stack.enter_context(patch.object(native.os, "access", return_value=self.failure != "not-executable"))
            capture = stack.enter_context(patch.object(component, "capture", return_value=
                b"arm64" if self.failure == "architecture" else b"arm64 x86_64"))
            if not real_workspace:
                stack.enter_context(patch.object(component, "private_workspace", side_effect=self.fake_workspace))
            yield capture

    def run_failure(self, failure, message):
        self.failure, self.builds, self.verifications = failure, 0, 0
        with self.context(), self.assertRaisesRegex(component.ComponentError, message):
            native.run_contract(self.app, DIGEST, REVISION)
        self.assertFalse(list(self.root.glob("scribe-native-cpu-entry-*")))

    def test_real_driver_contract_calls_only_builder_and_read_only_lipo(self):
        with self.context() as capture:
            report = native.run_contract(self.app, DIGEST, REVISION)
        self.assertEqual(self.builds, 2)
        self.assertEqual(self.verifications, 2)
        capture.assert_called_once_with(["/usr/bin/lipo", str(self.workspace / "postinstall"), "-archs"])
        self.assertEqual(report, {"constructed": True, "executed": False, "architectures": ["arm64", "x86_64"],
                                  "entry_size_bytes": len(IMAGE), "worker_size_bytes": len(WORKER)})
        self.assertFalse(self.workspace.exists())

    def test_root_and_foreign_hosts_reject_before_input_access(self):
        for platform, uid in [("win32", 501), ("linux", 501), ("darwin", 0)]:
            with self.subTest(platform=platform), patch.object(native.sys, "platform", platform), \
                 patch.object(native.os, "geteuid", return_value=uid, create=True), \
                 patch.object(component, "verify_cpu_app") as verify, \
                 self.assertRaises(component.ComponentError):
                native.run_contract(self.root / "absent", DIGEST, REVISION)
            verify.assert_not_called()

    def test_invalid_frozen_identity_rejects_before_build(self):
        with self.context(), self.assertRaisesRegex(component.ComponentError, "frozen"):
            native.run_contract(self.app, "invalid", REVISION)
        self.assertEqual(self.builds, 0)

    def test_missing_or_mismatched_worker_cannot_build(self):
        worker = self.app / native.INPUTS[0]
        worker.write_bytes(b"wrong")
        with self.context(), self.assertRaisesRegex(component.ComponentError, "frozen anchor"):
            native.run_contract(self.app, DIGEST, REVISION)
        worker.unlink()
        with self.context(), self.assertRaises(OSError):
            native.run_contract(self.app, DIGEST, REVISION)
        self.assertEqual(self.builds, 0)

    def test_wrong_output_path_is_rejected(self):
        self.run_failure("path", "wrong path")

    def test_unexpected_construction_output_is_rejected(self):
        self.run_failure("extra", "unexpected output")

    def test_output_must_be_nonempty_executable_and_exactly_hashed(self):
        for failure, message in [("empty", "size"), ("not-executable", "not executable"),
                                 ("linked", "hard-linked"), ("directory", "unexpected node type"),
                                 ("digest", "identity mismatch"), ("size", "identity mismatch")]:
            with self.subTest(failure=failure):
                self.run_failure(failure, message)

    def test_both_output_architectures_are_required(self):
        self.run_failure("architecture", "exactly universal")

    def test_repeat_cannot_replace_or_reject_for_an_unrelated_reason(self):
        for failure, message in [("replace", "changed the entry"), ("second-success", "replaced"),
                                 ("repeat-reason", "wrong reason")]:
            with self.subTest(failure=failure):
                self.run_failure(failure, message)

    def test_worker_desktop_signature_and_metadata_must_remain_unchanged(self):
        for name in native.INPUTS:
            saved = (self.app / name).read_bytes()
            with self.subTest(name=name):
                self.run_failure(name, "changed application inputs")
            (self.app / name).write_bytes(saved)

    def test_builder_or_final_signature_failure_cleans_only_the_owned_workspace(self):
        sentinel = self.root / "retain"
        sentinel.write_bytes(b"unrelated")
        for failure, message in [("build", "synthetic construction failure"),
                                 ("app-verification", "final app verification")]:
            self.run_failure(failure, message)
        self.assertEqual(sentinel.read_bytes(), b"unrelated")

    def test_cli_requires_exact_inputs_and_prints_only_the_path_free_report(self):
        report = {"constructed": True, "executed": False}
        args = ["native-test", "--app", str(self.app), "--worker-sha256", DIGEST, "--build-revision", REVISION]
        output = io.StringIO()
        with patch.object(sys, "argv", args), patch.object(native, "run_contract", return_value=report) as run, \
             redirect_stdout(output):
            self.assertEqual(native.main(), 0)
        run.assert_called_once_with(self.app, DIGEST, REVISION)
        self.assertIn('SCRIBE_NATIVE_CPU_ENTRY_CONSTRUCTION {"constructed": true, "executed": false}', output.getvalue())
        self.assertNotIn(str(self.root), output.getvalue())
        for invalid in [args[:1], args + ["--destination", "/unexpected"]]:
            with patch.object(sys, "argv", invalid), patch.object(native, "run_contract") as run, \
                 redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                native.main()
            run.assert_not_called()

    def test_cli_errors_fail_without_publishing_paths_or_report(self):
        args = ["native-test", "--app", str(self.app), "--worker-sha256", DIGEST, "--build-revision", REVISION]
        for error in [component.ComponentError("categorical failure"), OSError(str(self.root))]:
            output, errors = io.StringIO(), io.StringIO()
            with patch.object(sys, "argv", args), patch.object(native, "run_contract", side_effect=error), \
                 redirect_stdout(output), redirect_stderr(errors):
                self.assertEqual(native.main(), 1)
            self.assertEqual(output.getvalue(), "")
            self.assertNotIn(str(self.root), errors.getvalue())


class Posix(Portable):
    def test_uncertain_child_cleanup_retains_the_entire_native_fixture(self):
        for failure in ["uncertain", "uncertain-second"]:
            self.failure, self.builds, self.verifications = failure, 0, 0
            with self.subTest(failure=failure), self.context(real_workspace=True), \
                 self.assertRaises(component.CleanupUncertain):
                native.run_contract(self.app, DIGEST, REVISION)
            self.assertTrue(self.workspace.is_dir())
            self.assertEqual((self.workspace / "postinstall").exists(), failure == "uncertain-second")
        # Build and child are mocked; no uncertain process actually exists.


def suite():
    result = unittest.TestSuite()
    for cls in [Portable] + ([Posix] if os.name == "posix" else []):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 13 + (1 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"native driver test discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    result = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if result.wasSuccessful() else 1)
