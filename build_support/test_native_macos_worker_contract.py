"""Exact-discovery source tests; native commands, keys and workers are mocked."""
from contextlib import contextmanager
import hashlib
import io
import os
from pathlib import Path
import signal
import sys
import tempfile
import unittest
from unittest.mock import patch

import native_macos_worker_contract as contract
import macos_cpu_component as component

PASS = b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 123 filtered out; finished in 0.01s\n"
# Independent recipe expectations, not aliases of production constants.
CPU_NAMES = (
    "onnx_worker::tests::macos_native_protected_cpu_worker_authenticates_health_shutdown_and_reap",
    "onnx_worker::tests::macos_native_protected_cpu_vad_authenticates_health_shutdown_and_reap",
)
METAL_NAME = "onnx_worker::tests::macos_native_metal_metadata_helper_launches_authenticated_worker_and_reaps"


class Portable(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="native-contract-source-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.tool = str(self.root / "cargo")

    def test_capture_reuses_output_deadline_environment_and_repository_bounds(self):
        with patch.object(component, "capture", return_value=b"bounded") as capture:
            self.assertEqual(contract._run([self.tool, "test"], 1200, {"FIXED": "yes"}), b"bounded")
        capture.assert_called_once_with([self.tool, "test"], timeout=1200, limit=8 * 1024 * 1024,
                                       error_limit=8 * 1024 * 1024,
                                       environment={"FIXED": "yes"}, cwd=component.REPO)

    def test_tool_lookup_keeps_shim_name_and_rejects_missing_or_relative(self):
        for value in (None, "relative/cargo"):
            with patch.object(contract.shutil, "which", return_value=value), self.assertRaises(component.ComponentError):
                contract._tool("cargo")
        with patch.object(contract.shutil, "which", return_value=self.tool):
            self.assertEqual(contract._tool("cargo"), self.tool)

    def test_unknown_contract_denies_before_tool_lookup(self):
        with patch.object(contract, "_tool") as tool, self.assertRaises(component.ComponentError):
            contract._execute_contract("unreviewed", {}, offline=True)
        tool.assert_not_called()

    def test_exact_success_preserves_cpu_and_metal_commands(self):
        for name in (*CPU_NAMES, METAL_NAME):
            offline = name in CPU_NAMES
            with self.subTest(name=name), patch.object(contract, "_tool", return_value=self.tool), \
                 patch.object(contract, "_run", side_effect=[(name + ": test\n").encode(), PASS]) as run:
                contract._execute_contract(name, {"frozen": "yes"}, offline=offline)
                listing, execution = [call.args for call in run.call_args_list]
                self.assertEqual(listing[1:], (1200, {"frozen": "yes"}))
                self.assertEqual(execution[1:], listing[1:])
                expected = [self.tool, "test", "--locked", *(["--offline"] if offline else []),
                            "--release", "--bin", "local-transcriber", name, "--manifest-path",
                            str(component.REPO / "Cargo.toml"), "--", "--ignored", "--exact"]
                self.assertEqual(listing[0], [*expected, "--list"])
                self.assertEqual(execution[0], [*expected, "--nocapture", "--test-threads=1"])

    def test_discovery_missing_duplicate_unexpected_and_bad_encoding_never_executes(self):
        name = contract.CPU_TESTS[0]
        for listing in (b"", (name + ": test\n").encode() * 2,
                        (name + ": test\nother: test\n").encode(), b"\xff"):
            with self.subTest(listing=listing), patch.object(contract, "_tool", return_value=self.tool), \
                 patch.object(contract, "_run", return_value=listing) as run, self.assertRaises(Exception) as result:
                contract._execute_contract(name, {}, offline=True)
            self.assertNotIsInstance(result.exception, component.CleanupUncertain)
            self.assertEqual(run.call_count, 1)

    def test_actual_execution_failure_or_cancellation_always_retains_without_raw_errors(self):
        name = contract.CPU_TESTS[0]
        for failure in (component.ToolExitError("tool", 17), component.ComponentError("private-deadline"),
                        component.ComponentError("private-output-bound"),
                        component.CleanupUncertain("private-child"), OSError("private-path"),
                        KeyboardInterrupt(), SystemExit(9)):
            with self.subTest(failure=type(failure).__name__), patch.object(contract, "_tool", return_value=self.tool), \
                 patch.object(contract, "_run", side_effect=[(name + ": test\n").encode(), failure]), \
                 self.assertRaises(component.CleanupUncertain) as caught:
                contract._execute_contract(name, {}, offline=True)
            self.assertIsNone(caught.exception.__cause__)
            self.assertTrue(caught.exception.__suppress_context__)
            self.assertNotIn("private-", str(caught.exception))

    def test_success_validation_failure_retains_even_after_cargo_group_exit(self):
        for output in (b"", b"\xff", PASS * 2, PASS.replace(b"1 passed", b"0 passed"),
                       PASS.replace(b"0 ignored", b"1 ignored"), PASS.replace(b"0 failed", b"1 failed")):
            name = contract.METAL_TEST
            with self.subTest(output=output), patch.object(contract, "_tool", return_value=self.tool), \
                 patch.object(contract, "_run", side_effect=[(name + ": test\n").encode(), output]), \
                 self.assertRaises(component.CleanupUncertain):
                contract._execute_contract(name, {}, offline=False)

    def test_listing_uncertainty_stays_uncertain(self):
        with patch.object(contract, "_tool", return_value=self.tool), \
             patch.object(contract, "_run", side_effect=component.CleanupUncertain("group")), \
             self.assertRaises(component.CleanupUncertain):
            contract._execute_contract(contract.METAL_TEST, {}, offline=False)

    def test_platform_and_root_reject_before_environment_or_tools(self):
        for platform, uid in (("win32", 1000), ("darwin", 0)):
            with patch.object(contract.sys, "platform", platform), patch.object(os, "geteuid", return_value=uid, create=True), \
                 patch.object(contract, "_tool") as tool, self.assertRaises(component.ComponentError):
                contract._environment()
            tool.assert_not_called()

    def test_environment_requires_each_input_and_preserves_existing_bindings(self):
        environment = dict(RUNNER_TEMP=str(self.root), SHERPA_ONNX_ARCHIVE_DIR=str(self.root),
                           SCRIBE_BUILD_REVISION="source-test-revision", KEEP="preserved",
                           DOCS_RS="1", SCRIBE_BUILDING_WORKER="1", SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP="excluded")
        with patch.object(contract.sys, "platform", "darwin"), patch.object(os, "geteuid", return_value=1000, create=True):
            for missing in ("RUNNER_TEMP", "SHERPA_ONNX_ARCHIVE_DIR", "SCRIBE_BUILD_REVISION"):
                with patch.dict(os.environ, {k: v for k, v in environment.items() if k != missing}, clear=True), \
                     self.assertRaises(component.ComponentError):
                    contract._environment()
            with patch.dict(os.environ, environment, clear=True):
                root, result = contract._environment()
            self.assertEqual(root, self.root)
            self.assertEqual(result, {k: v for k, v in environment.items() if k not in
                ("DOCS_RS", "SCRIBE_BUILDING_WORKER", "SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP")})

    def test_invalid_modes_deny_before_fixture_or_environment(self):
        for kind, app in (("other", None), ("cpu", None), ("metal", self.root)):
            with patch.object(contract, "_environment") as environment, self.assertRaises(component.ComponentError):
                contract.run_contract(kind, app)
            environment.assert_not_called()

    def test_owner_cleanup_is_part_of_success_and_uncertainty_propagates(self):
        for failure in (None, component.ComponentError("cleanup"), component.CleanupUncertain("group")):
            events = []
            @contextmanager
            def scope(*_args):
                events.append("enter")
                yield self.root
                events.append("cleanup")
                if failure:
                    raise failure
            with self.subTest(failure=failure), patch.object(contract, "_environment", return_value=(self.root, {})), \
                 patch.object(component, "private_workspace", side_effect=scope), \
                 patch.object(contract, "_metal", side_effect=lambda *_args: events.append("contract")):
                if failure:
                    with self.assertRaises(type(failure)):
                        contract.run_contract("metal", None)
                else:
                    contract.run_contract("metal", None)
                self.assertEqual(events, ["enter", "contract", "cleanup"])
            (self.root / "tmp").rmdir()

    def test_cpu_recipe_uses_verified_copy_and_both_roles_without_signing(self):
        app = self.root / "Scribe.app"
        worker = app / component.CPU_APP_INPUTS[0]
        worker.parent.mkdir(parents=True)
        worker.write_bytes(b"fixed-worker")
        with patch.object(contract, "_run") as run, \
             patch.object(component, "copy_new", wraps=component.copy_new) as copied, \
             patch.object(component, "hash_file", wraps=component.hash_file) as hashed, \
             patch.object(contract, "_execute_contract") as execute:
            contract._cpu(app, self.root, {"fixed": "yes"})
        self.assertEqual(copied.call_args.args[2], 512 * 1024 * 1024)
        self.assertTrue(all(call.args[1] == 512 * 1024 * 1024 for call in hashed.call_args_list))
        self.assertEqual(run.call_args_list[0].args,
                         (["/bin/bash", str(component.REPO / "scripts/verify-macos-release-package.sh"),
                           "--app", str(app)], 60, {"fixed": "yes"}))
        self.assertEqual(run.call_args_list[1].args[0][:3], ["/usr/bin/codesign", "--verify", "--strict"])
        expected_environment = {"fixed": "yes", "SCRIBE_BUNDLED_WORKER_SHA256": hashlib.sha256(b"fixed-worker").hexdigest(),
                                "SCRIBE_MACOS_NATIVE_CPU_FIXTURE_ROOT": str(self.root / "image")}
        self.assertEqual([call.args for call in execute.call_args_list],
                         [(name, expected_environment) for name in CPU_NAMES])
        self.assertEqual([call.kwargs for call in execute.call_args_list], [{"offline": True}] * 2)
        self.assertEqual((self.root / "image/scribe-inference-worker").read_bytes(), worker.read_bytes())

    def test_owner_preparation_failure_or_repeated_cancellation_retains_fixture(self):
        for failure in (KeyboardInterrupt(), SystemExit(9), component.ComponentError("preparation"),
                        component.CleanupUncertain("listing")):
            events = []
            @contextmanager
            def scope(*_args):
                try:
                    yield self.root
                except component.CleanupUncertain:
                    events.append("retain")
                    raise
                else:
                    events.append("delete")
            with self.subTest(failure=type(failure).__name__), \
                 patch.object(contract, "_environment", return_value=(self.root, {})), \
                 patch.object(component, "private_workspace", side_effect=scope), \
                 patch.object(contract, "_metal", side_effect=failure), self.assertRaises(component.CleanupUncertain):
                contract.run_contract("metal", None)
            self.assertEqual(events, ["retain"])
            (self.root / "tmp").rmdir()

    def test_cpu_first_role_uncertainty_stops_second_role(self):
        app = self.root / "Scribe.app"
        worker = app / component.CPU_APP_INPUTS[0]
        worker.parent.mkdir(parents=True)
        worker.write_bytes(b"fixed-worker")
        with patch.object(contract, "_run"), \
             patch.object(contract, "_execute_contract", side_effect=component.CleanupUncertain("worker")) as execute, \
             self.assertRaises(component.CleanupUncertain):
            contract._cpu(app, self.root, {})
        self.assertEqual(execute.call_count, 1)

    def test_metal_recipe_preserves_native_commands_deadlines_and_fixture_signing(self):
        for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            with self.subTest(target=target), tempfile.TemporaryDirectory(dir=self.root) as directory:
                root = Path(directory)
                commands = []
                def run(arguments, seconds, environment):
                    commands.append((arguments, seconds, environment))
                    if "-vV" in arguments:
                        return ("host: " + target + "\n").encode()
                    if "build" in arguments:
                        output = root / "worker-target" / target / "release/scribe-inference-worker"
                        output.parent.mkdir(parents=True)
                        output.write_bytes(b"fixture-metal-worker")
                    return b""
                with patch.object(contract, "_tool", side_effect=lambda name: str(self.root / name)), \
                     patch.object(component, "copy_new", wraps=component.copy_new) as copied, \
                     patch.object(component, "hash_file", wraps=component.hash_file) as hashed, \
                     patch.object(contract, "_run", side_effect=run), patch.object(contract, "_execute_contract") as execute:
                    contract._metal(root, {"frozen": "yes", "SCRIBE_BUNDLED_WORKER_SHA256": "excluded"})
                self.assertEqual(copied.call_args.args[2], 2 * 1024 * 1024 * 1024)
                self.assertTrue(all(call.args[1] == 2 * 1024 * 1024 * 1024 for call in hashed.call_args_list))
                cargo = str(self.root / "cargo")
                fixture = root / "macos-metal-metadata-contract"
                pack = root / "fixture-pack"
                worker = pack / "worker/scribe-inference-worker"
                environment = {"frozen": "yes", "SCRIBE_BUNDLED_WORKER_SHA256": "excluded"}
                build_environment = {"frozen": "yes", "MACOSX_DEPLOYMENT_TARGET": "13.0",
                                     "SCRIBE_BUILDING_WORKER": "1", "CARGO_TARGET_DIR": str(root / "worker-target")}
                author = [cargo, "run", "--locked", "--quiet", "--manifest-path",
                          str(component.REPO / "tools/worker-pack-author/Cargo.toml"), "--"]
                expected_commands = [
                    ([str(self.root / "rustc"), "-vV"], 30, environment),
                    (["/usr/bin/xcrun", "--sdk", "macosx", "clang", "-Wall", "-Wextra", "-Werror",
                      "-mmacosx-version-min=13.0", "-I", str(component.REPO / "native"),
                      str(component.REPO / "native/test_scribe_macos_gpu_metadata.m"), "-framework", "Foundation",
                      "-framework", "Metal", "-o", str(fixture)], 120, environment),
                    ([str(fixture)], 30, environment),
                    ([cargo, "build", "--locked", "--release", "--target", target, "--bin", "scribe-inference-worker",
                      "--features", "metal-acceleration", "--manifest-path", str(component.REPO / "Cargo.toml")],
                     1800, build_environment),
                    (["/usr/bin/codesign", "--force", "--sign", "-", str(worker)], 60, environment),
                    (["/usr/bin/codesign", "--verify", "--strict", "--verbose=2", str(worker)], 60, environment),
                    ([*author, "author", "--backend", "metal", "--target-os", "macos", "--target-arch",
                      "aarch64" if target == "aarch64-apple-darwin" else "x86_64", "--pack-id", "metal-native-metadata-helper",
                      "--pack-version", "native-ci-v1", "--pack-root", str(pack), "--provider", "transcribe-cpp-metal",
                      "--security-epoch", "1", "--worker-path", "worker/scribe-inference-worker", "--fixture-signing"],
                     900, environment),
                    ([*author, "verify-fixture", "--pack-root", str(pack)], 300, environment),
                ]
                self.assertEqual(commands, expected_commands)
                self.assertEqual(execute.call_args.args,
                                 (METAL_NAME, {**environment,
                                  "SCRIBE_BUNDLED_WORKER_SHA256": hashlib.sha256(b"fixture-metal-worker").hexdigest(),
                                  "SCRIBE_MACOS_NATIVE_METAL_METADATA_FIXTURE_PACK": str(pack)}))
                self.assertEqual(execute.call_args.kwargs, {"offline": False})

    def test_sigterm_uses_cooperative_cancellation(self):
        with self.assertRaises(KeyboardInterrupt):
            contract._cancel(signal.SIGTERM, None)

    def test_main_redacts_failures_restores_handler_and_never_emits_false_success(self):
        for failure in (OSError("private-sentinel"), component.CleanupUncertain("private-sentinel"), KeyboardInterrupt()):
            output = io.StringIO()
            with patch.object(sys, "argv", ["contract", "metal"]), patch.object(contract, "run_contract", side_effect=failure), \
                 patch.object(signal, "signal", return_value=signal.SIG_DFL) as handler, \
                 patch.object(sys, "stdout", output), patch.object(sys, "stderr", output):
                self.assertEqual(contract.main(), 1)
            self.assertEqual(handler.call_args.args, (signal.SIGTERM, signal.SIG_DFL))
            self.assertNotIn("private-sentinel", output.getvalue())
            self.assertNotIn("_PASS", output.getvalue())

    def test_main_success_is_emitted_only_after_owner_returns(self):
        output = io.StringIO()
        def completed(*_args):
            self.assertEqual(output.getvalue(), "")
        with patch.object(sys, "argv", ["contract", "metal"]), patch.object(contract, "run_contract", side_effect=completed), \
             patch.object(signal, "signal", return_value=signal.SIG_DFL), patch.object(sys, "stdout", output):
            self.assertEqual(contract.main(), 0)
        self.assertEqual(output.getvalue(), "SCRIBE_NATIVE_METAL_CONTRACT_PASS\n")


class Posix(unittest.TestCase):
    def test_real_owned_scope_retains_abnormal_worker_run_and_cleans_success(self):
        with tempfile.TemporaryDirectory(prefix="native-owner-posix-") as directory:
            parent = Path(directory).resolve()
            for output, retained in ((PASS, False), (b"no-proof", True)):
                with patch.object(contract, "_tool", return_value=sys.executable), \
                     patch.object(contract, "_run", side_effect=[(contract.METAL_TEST + ": test\n").encode(), output]):
                    try:
                        with component.private_workspace(parent, "owned-") as root:
                            (root / "sentinel").write_bytes(b"owned")
                            contract._execute_contract(contract.METAL_TEST, {}, offline=False)
                    except component.CleanupUncertain:
                        self.assertTrue(retained)
                self.assertEqual(root.exists(), retained)
                if retained:
                    self.assertEqual((root / "sentinel").read_bytes(), b"owned")


def suite():
    result = unittest.TestSuite(unittest.defaultTestLoader.loadTestsFromTestCase(Portable))
    if os.name == "posix":
        result.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(Posix))
    expected = 19 + (1 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError("native contract source test discovery mismatch")
    return result


if __name__ == "__main__":
    raise SystemExit(not unittest.TextTestRunner(verbosity=2).run(suite()).wasSuccessful())
