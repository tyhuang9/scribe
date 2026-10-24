"""Mock-only full-release CPU finalization/PKG; no keys, signing or execution."""
from contextlib import ExitStack, contextmanager
from dataclasses import replace
import json
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch

import macos_cpu_component as component
import macos_cpu_pkg as codec
import macos_cpu_pkg_entry as cpu
import macos_cpu_pkg_final as signing
import macos_full_release as full
import macos_full_release_cpu_entry as entry
import macos_full_release_cpu_pkg_final as author
from test_macos_cpu_pkg import VERSION, encode, package_info
from test_macos_cpu_pkg_final import PROPERTIES, SELECTOR, TEAM
from test_macos_full_release_cpu_entry import IMAGE, Portable, digest

SIGNED = IMAGE + b" synthetic final signature; never execute"


class Owner(Portable):
    def setUp(self):
        super().setUp()
        self.policy = self.root / "policy.json"
        self.policy.write_text(json.dumps({"schema_version": 1, "mode": "developer_id",
                                          "publisher_team_id": TEAM}), encoding="utf-8")
        self.operations, self.packages = [], []
        self.source_mutation = None

    @contextmanager
    def context(self, **kwargs):
        with ExitStack() as stack:
            stack.enter_context(super().context(**kwargs))
            stack.enter_context(patch.object(signing, "POLICY_PATH", self.policy))
            yield

    def change_source(self, phase):
        if self.source_mutation and self.source_mutation[0] == phase:
            _, relative, replacement = self.source_mutation
            source = self.app / relative
            alternate = self.root / "alternate-input"
            alternate.write_bytes(source.read_bytes() + (b"" if replacement else b"changed"))
            os.replace(alternate, source)

    def execute(self, args, **kwargs):
        operation = kwargs["operation"]
        if not operation.startswith("final-entry-") and operation != "cpu-component-pkg-build":
            result = super().execute(args, **kwargs)
            if args[:2] == ["/usr/bin/lipo", "-create"]:
                Path(args[3]).chmod(0o700)
            return result
        component.validate_tool_operation(operation)
        self.operations.append((operation, list(args), kwargs))
        path = Path(args[-1])
        if operation == "final-entry-signature":
            role = "application" if path == self.app else "cpu"
            self.assertEqual(args[:5], ["/usr/bin/codesign", "--verify", "--strict", "--all-architectures",
                                        "--test-requirement"])
            self.assertEqual(args[5], signing._requirement(TEAM, role))
            if self.failure == "app-signature" or (role == "cpu" and self.failure == "entry-signature"):
                raise component.ToolExitError(operation, 1)
            if role == "application":
                self.change_source("application")
        elif operation == "final-entry-signing":
            self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES))
            self.assertEqual(args[:-1], ["/usr/bin/codesign", "--force", "--sign", SELECTOR, "--options", "runtime",
                                         "--timestamp", "--identifier", signing.CODE_IDENTIFIERS["cpu"]])
            self.assertEqual(path.read_bytes(), IMAGE)
            path.write_bytes(SIGNED)
            self.change_source("signing")
            if self.failure == "signing-uncertain":
                raise component.CleanupUncertain("synthetic signing cleanup uncertainty")
            if self.failure == "sign-failure":
                raise component.ToolExitError(operation, 1)
            if self.failure == "signed-extra-operation":
                (path.parent / "preinstall").write_bytes(b"never execute")
        elif operation == "final-entry-properties":
            self.assertTrue(kwargs["signature_properties"])
            self.assertEqual(args[1:4], ["--display", "--verbose=4", "--arch"])
            self.assertIn(args[4], signing.ARCHITECTURES)
            if args[4] == "x86_64":
                if self.failure == "foreign-runtime":
                    return PROPERTIES.replace(b"0x10000(runtime)", b"0x0(none)")
                if self.failure == "foreign-timestamp":
                    return PROPERTIES.replace(b"Timestamp=", b"Signed Time=")
                if self.failure in ("entry-change", "entry-replace"):
                    alternate = path.parent / "alternate"
                    alternate.write_bytes(path.read_bytes() + (b"" if self.failure.endswith("replace") else b"changed"))
                    os.replace(alternate, path)
            return PROPERTIES
        elif operation == "final-entry-entitlements":
            self.assertEqual(args[1:3], ["--display", "--arch"])
            self.assertEqual(args[4:-1], ["--entitlements", "-", "--xml"])
            return plistlib.dumps({"com.apple.security.get-task-allow": False}) \
                if args[3] == "x86_64" and self.failure == "foreign-entitlements" else b""
        elif operation == "cpu-component-pkg-build":
            scripts = Path(args[args.index("--scripts") + 1])
            self.assertEqual(args[:-1], ["/usr/bin/pkgbuild", "--nopayload", "--scripts", str(scripts),
                                         "--identifier", codec.IDENTIFIER, "--version", VERSION, "--install-location", "/"])
            self.assertEqual(set(kwargs["environment"]), {"PATH", "LC_ALL", "LANG", "TMPDIR"})
            self.assertEqual(kwargs["timeout"], 120)
            body = (scripts / "postinstall").read_bytes()
            self.assertEqual(body, SIGNED)
            if self.failure == "pkg-uncertain":
                raise component.CleanupUncertain("synthetic packaging cleanup uncertainty")
            if self.failure == "pkg-failed":
                raise component.ToolExitError(operation, 1)
            if self.failure == "pkg-missing":
                return b""
            info = package_info().replace(codec.IDENTIFIER.encode(), codec._METAL_IDENTIFIER.encode()) \
                if self.failure == "pkg-metal-role" else package_info(empty_controls=True)
            if self.failure == "pkg-version":
                info = info.replace(b'version="0.1.0"', b'version="9.9.9"')
            path.write_bytes(b"invalid" if self.failure == "pkg-malformed" else
                             encode(IMAGE if self.failure == "pkg-presign" else body, info=info))
            self.packages.append(path)
            self.change_source("packaging")
            if self.failure == "pkg-extra-source":
                (self.app / "Contents/Resources/unexpected").write_bytes(b"not in frozen inventory")
            if self.failure in ("pkg-entry-change", "pkg-entry-replace"):
                retained = scripts / "postinstall"
                alternate = scripts / "alternate"
                alternate.write_bytes(retained.read_bytes() + (b"" if self.failure.endswith("replace") else b"changed"))
                os.replace(alternate, retained)
            if self.failure == "pkg-extra-operation":
                (scripts / "preinstall").write_bytes(b"never execute")
        else:
            self.fail("unexpected finalization operation")
        return b""

    def build(self, *, selector=SELECTOR, **kwargs):
        def publish(source, output):
            if output == self.workspace / "cpu-worker.pkg" and self.failure == "pkg-collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic collision")
            source.rename(output)
            if output == self.workspace / "cpu-worker.pkg" and self.failure == "pkg-publish-uncertain":
                raise OSError("synthetic post-rename uncertainty")
        with self.context(**kwargs), patch.object(component, "publish_no_replace", side_effect=publish):
            return author.build_component(self.app, self.workspace, self.frozen, selector)

    def reject(self):
        with self.assertRaises((component.ComponentError, OSError)):
            self.build()
        self.assertEqual(list(self.workspace.iterdir()), [])

    def test_default_policy_denies_before_inputs_tools_or_workspace(self):
        with patch.object(component, "capture") as tool, patch.object(entry, "build_entry") as builder, \
             patch.object(full, "release_inputs") as inputs, patch.object(component, "private_workspace") as workspace, \
             self.assertRaisesRegex(component.ComponentError, "publisher is not provisioned"):
            author.build_component(None, None, None, None)
        for operation in (tool, builder, inputs, workspace):
            operation.assert_not_called()

    def test_owns_full_construction_and_packages_only_postsign_cpu_entry(self):
        with patch.object(component, "verify_cpu_app") as cpu_admission, \
             patch.object(codec, "_package_constructed_entry") as old_tail:
            output, sha, size = self.build()
        cpu_admission.assert_not_called()
        old_tail.assert_not_called()
        codec._inspect_fresh(output, digest(SIGNED), len(SIGNED), VERSION)
        self.assertEqual((sha, size), (digest(output.read_bytes()), output.stat().st_size))
        self.assertEqual(len(self.builds), 3)
        self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES))
        self.assertEqual([op for op, _, _ in self.operations], ["final-entry-signature", "final-entry-signing",
                         "final-entry-signature", "final-entry-properties", "final-entry-entitlements",
                         "final-entry-properties", "final-entry-entitlements", "cpu-component-pkg-build"])
        self.assertEqual(list(self.workspace.iterdir()), [output])
        for args, _ in self.calls:
            self.assertNotIn(Path(args[0]).name, ("postinstall", "productbuild", "installer",
                                                "scribe-inference-worker", "scribe-macos-cpu-worker-installer"))

    def test_invalid_identity_and_selector_precede_tools(self):
        saved = self.frozen
        for value in (None, {}, True, replace(saved, revision="short"), replace(saved, catalog_sha256="bad")):
            self.frozen = value
            self.reject()
        self.frozen = saved
        for selector in (None, "-", "name", "A" * 40, SELECTOR + "\n"):
            with self.assertRaises(component.ComponentError):
                self.build(selector=selector)
        self.assertEqual(self.operations + self.calls, [])

    def test_legacy_cpu_only_finalizer_still_rejects_full_release(self):
        with self.context(), self.assertRaisesRegex(component.ComponentError, "CPU-only catalog"):
            signing.build_component(self.app, self.workspace, self.frozen.cpu_worker_sha256, self.frozen.revision, SELECTOR)
        self.assertEqual(self.operations + self.builds, [])
        self.assertEqual([kwargs["operation"] for _, kwargs in self.calls], ["application-verification"])

    def test_each_frozen_digest_mismatch_prevents_signing(self):
        saved = self.frozen
        for field in ("cpu_worker_sha256", "catalog_sha256", "authority_sha256", "aarch64_manifest_sha256",
                      "x86_64_manifest_sha256"):
            self.frozen = replace(saved, **{field: "0" * 64})
            self.operations.clear()
            self.reject()
            self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_app_pack_and_cpu_build_failures_cannot_sign_or_package(self):
        for failure in ("app-signature", "application", "cpu-pack-aarch64", "cpu-pack-x86_64", "descriptor",
                        "cpu-build-failed", "cpu-missing", "cpu-wrong-arch"):
            self.failure = failure
            self.operations.clear()
            self.verifications.clear()
            self.reject()
            self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_constructor_path_hash_size_links_and_extra_inventory_reject_before_signing(self):
        original = entry.build_entry
        for failure in ("path", "hash", "size", "link", "inventory"):
            def constructor(*args):
                path, sha, size = original(*args)
                if failure == "link":
                    os.link(path, self.root / "linked-entry")
                if failure == "inventory":
                    (path.parent / "preinstall").write_bytes(b"never execute")
                return (path.parent / "wrong" if failure == "path" else path,
                        "0" * 64 if failure == "hash" else sha, size + (failure == "size"))
            self.operations.clear()
            self.verifications.clear()
            with patch.object(entry, "build_entry", side_effect=constructor):
                self.reject()
            self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

        policy_check = signing._unchanged_policy
        for replacement in (False, True):
            self.operations.clear()
            self.builds.clear()
            self.verifications.clear()
            reached = []
            def checked(expected):
                policy_check(expected)  # The real policy check must pass first.
                paths = list(self.workspace.glob(".scribe-full-cpu-final-*/scripts/postinstall"))
                self.assertEqual(len(paths), 1)
                reached.append(True)
                path = paths[0]
                alternate = self.root / "presign-replacement"
                alternate.write_bytes(path.read_bytes() + (b"" if replacement else b"changed"))
                os.replace(alternate, path)
            with patch.object(signing, "_unchanged_policy", side_effect=checked), \
                 self.assertRaisesRegex(component.ComponentError, "CPU entry changed before signing"):
                self.build()
            self.assertEqual(reached, [True])
            self.assertEqual(len(self.builds), 3)
            self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES))
            self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])
            self.assertNotIn("cpu-component-pkg-build", [op for op, _, _ in self.operations])
            self.assertEqual(list(self.workspace.iterdir()), [])

    def test_every_full_source_file_content_and_same_byte_replacement_remain_bound(self):
        packs, _ = full.release_inputs(self.app, self.frozen)
        for relative in full.input_files(packs):
            source = self.app / relative
            saved = source.read_bytes()
            for phase in ("application", "signing", "packaging"):
                for replacement in (False, True):
                    with self.subTest(relative=relative, phase=phase, replacement=replacement):
                        self.source_mutation = phase, relative, replacement
                        self.operations.clear()
                        self.verifications.clear()
                        self.reject()
                        self.assertIn("final-entry-signature" if phase == "application" else
                                      "final-entry-signing" if phase == "signing" else "cpu-component-pkg-build",
                                      [op for op, _, _ in self.operations])
                        source.write_bytes(saved)

    def test_same_baseline_rejects_input_replacement_when_constructor_returns(self):
        original = entry.build_entry
        for replacement in (False, True):
            source = self.app / "Contents/Info.plist"
            saved = source.read_bytes()
            def constructed(*args):
                result = original(*args)
                self.source_mutation = "construction", "Contents/Info.plist", replacement
                self.change_source("construction")
                return result
            self.operations.clear()
            self.verifications.clear()
            with patch.object(entry, "build_entry", side_effect=constructed):
                self.reject()
            self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])
            source.write_bytes(saved)

    def test_policy_change_and_same_byte_replacement_reject_at_each_phase(self):
        saved = self.policy.read_bytes()
        for phase in ("entry-combine", "final-entry-signing", "cpu-component-pkg-build"):
            for replacement in (False, True):
                self.policy.write_bytes(saved)
                self.verifications.clear()
                def capture(args, **kwargs):
                    result = self.execute(args, **kwargs)
                    if kwargs["operation"] == phase:
                        alternate = self.root / "alternate-policy"
                        alternate.write_bytes(saved if replacement else b"changed policy")
                        os.replace(alternate, self.policy)
                    return result
                with self.context(), patch.object(component, "capture", side_effect=capture), \
                     patch.object(component, "publish_no_replace", side_effect=lambda source, output: source.rename(output)), \
                     self.assertRaises(component.ComponentError):
                    author.build_component(self.app, self.workspace, self.frozen, SELECTOR)
                self.assertEqual(list(self.workspace.iterdir()), [])

    def test_signature_properties_entitlements_entry_changes_and_failure_reject(self):
        for failure in ("entry-signature", "foreign-runtime", "foreign-timestamp", "foreign-entitlements",
                        "entry-change", "entry-replace", "signed-extra-operation", "sign-failure"):
            self.failure = failure
            self.operations.clear()
            self.verifications.clear()
            self.reject()
            self.assertNotIn("cpu-component-pkg-build", [op for op, _, _ in self.operations])

    def test_cpu_signed_size_bound_and_exact_structure_markers(self):
        self.operations.clear()
        with patch.object(cpu, "MAX_ENTRY", len(IMAGE)), \
             self.assertRaisesRegex(component.ComponentError, "component file size is outside its bound"):
            self.build()
        self.assertEqual([op for op, _, _ in self.operations], ["final-entry-signature", "final-entry-signing"])
        self.assertFalse((self.workspace / "cpu-worker.pkg").exists())
        self.verifications.clear()
        with patch.object(component, "inspect_universal_structure", wraps=component.inspect_universal_structure) as inspect:
            self.build()
        self.assertEqual(inspect.call_args.args[1], [self.frozen.cpu_worker_sha256.encode(), self.frozen.revision.encode(),
                                                   component.CPU_ROOT.encode()])
        self.assertEqual(inspect.call_args.args[-1], cpu.MAX_ENTRY)

    def test_missing_failed_malformed_cross_role_and_presign_packages_reject(self):
        for failure in ("pkg-missing", "pkg-failed", "pkg-malformed", "pkg-metal-role", "pkg-version", "pkg-presign"):
            self.failure = failure
            self.operations.clear()
            self.verifications.clear()
            self.reject()
            self.assertIn("cpu-component-pkg-build", [op for op, _, _ in self.operations])

    def test_retained_postsign_entry_and_operation_inventory_remain_bound(self):
        for failure in ("pkg-entry-change", "pkg-entry-replace", "pkg-extra-operation", "pkg-extra-source"):
            self.failure = failure
            self.verifications.clear()
            self.reject()

    def test_inspected_package_bytes_and_node_identity_remain_bound_until_publication(self):
        original = codec._inspect_fresh
        for replacement in (False, True):
            def inspect(package, *args):
                original(package, *args)
                alternate = package.with_suffix(".alternate")
                alternate.write_bytes(package.read_bytes() + (b"" if replacement else b"changed"))
                os.replace(alternate, package)
            self.verifications.clear()
            with patch.object(codec, "_inspect_fresh", side_effect=inspect):
                self.reject()
        policy_check = signing._unchanged_policy
        for replacement in (False, True):
            self.packages.clear()
            self.verifications.clear()
            def checked(expected):
                policy_check(expected)
                if self.packages:
                    path = self.packages[-1]
                    alternate = path.with_suffix(".alternate")
                    alternate.write_bytes(path.read_bytes() + (b"" if replacement else b"changed"))
                    os.replace(alternate, path)
            with patch.object(signing, "_unchanged_policy", side_effect=checked):
                self.reject()

    def test_existing_output_and_unsafe_workspace_precede_tools(self):
        output = self.workspace / "cpu-worker.pkg"
        output.write_bytes(b"retain existing")
        with self.assertRaises(component.ComponentError):
            self.build()
        self.assertEqual(output.read_bytes(), b"retain existing")
        for workspace in (Path("relative"), self.app, self.app / "Contents"):
            self.workspace = workspace
            with self.assertRaises(component.ComponentError):
                self.build()
        self.assertEqual(self.operations + self.calls, [])

    def test_publication_collision_and_uncertainty_preserve_final(self):
        for failure in ("pkg-collision", "pkg-publish-uncertain"):
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace, self.failure = Path(directory), failure
                self.verifications.clear()
                with self.assertRaises(OSError):
                    self.build()
                self.assertTrue((self.workspace / "cpu-worker.pkg").is_file())
                if failure == "pkg-collision":
                    self.assertEqual((self.workspace / "cpu-worker.pkg").read_bytes(), b"retain concurrent output")


class Posix(Owner):
    def test_uncertain_signer_and_packager_retain_owned_workspace_and_original_error(self):
        for phase in ("signing", "pkg"):
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace = Path(directory)
                self.failure = phase + "-uncertain"
                self.verifications.clear()
                with self.assertRaisesRegex(component.CleanupUncertain, "synthetic .* cleanup uncertainty"):
                    self.build(real_workspace=True)
                retained = list(self.workspace.glob(".scribe-full-cpu-final-*"))
                self.assertEqual(len(retained), 1)
                self.assertTrue((retained[0] / "scripts/postinstall").is_file())
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())
                # Every native child is mocked; no live process exists here.


def suite():
    result = unittest.TestSuite()
    for cls in [Owner] + ([Posix] if os.name == "posix" else []):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 17 + (1 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"Full-release CPU finalization discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
