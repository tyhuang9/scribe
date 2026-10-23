"""Mocked finalization only; no authentic signatures, keys or entry execution."""
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
import macos_cpu_pkg_final as signing
import macos_metal_pkg_entry as entry
import macos_metal_pkg_final as final
from test_macos_cpu_pkg_final import PROPERTIES, SELECTOR, TEAM
from test_macos_metal_pkg_entry import IMAGE, Portable, digest

SIGNED = IMAGE + b" synthetic final signature; never execute"


class Owner(Portable):
    # Explicit suite below excludes all inherited PRE-SIGN test methods.
    def setUp(self):
        super().setUp()
        self.policy = self.root / "policy.json"
        self.policy.write_text(json.dumps({"schema_version": 1, "mode": "developer_id",
                                          "publisher_team_id": TEAM}), encoding="utf-8")
        self.operations = []
        self.mutate_source = None

    @contextmanager
    def context(self, **kwargs):
        with ExitStack() as stack:
            stack.enter_context(super().context(**kwargs))
            stack.enter_context(patch.object(signing, "POLICY_PATH", self.policy))
            yield

    def execute(self, args, **kwargs):
        operation = kwargs["operation"]
        if not operation.startswith("final-entry-"):
            result = super().execute(args, **kwargs)
            if args[:2] == ["/usr/bin/lipo", "-create"]:
                # Model the executable output of native lipo, not a 0644 text fixture.
                Path(args[3]).chmod(0o700)
            return result
        component.validate_tool_operation(operation)
        self.operations.append((operation, list(args), kwargs))
        self.assertEqual(args[0], "/usr/bin/codesign")
        path = Path(args[-1])
        if operation == "final-entry-signature":
            role = "application" if path == self.app else "metal"
            self.assertEqual(args[1:5], ["--verify", "--strict", "--all-architectures", "--test-requirement"])
            self.assertEqual(args[5], signing._requirement(TEAM, role))
            if self.failure == "app-signature" or (role == "metal" and self.failure == "entry-signature"):
                raise component.ToolExitError(operation, 1)
        elif operation == "final-entry-signing":
            self.assertEqual(args[1:-1], ["--force", "--sign", SELECTOR, "--options", "runtime", "--timestamp",
                                         "--identifier", signing.CODE_IDENTIFIERS["metal"]])
            path.write_bytes(SIGNED)
            if self.mutate_source:
                source = self.app / self.mutate_source
                if self.failure == "final-source-replace":
                    replacement = self.root / "replacement-source"
                    replacement.write_bytes(source.read_bytes())
                    os.replace(replacement, source)
                else:
                    source.write_bytes(source.read_bytes() + b"tampered")
            if self.failure == "signed-extra-operation":
                (path.parent / "preinstall").write_bytes(b"never run")
            if self.failure == "policy-change":
                self.policy.write_text('{"schema_version":1,"mode":"default_deny","publisher_team_id":""}',
                                       encoding="utf-8")
            if self.failure == "policy-replace":
                replacement = self.root / "replacement-policy"
                replacement.write_bytes(self.policy.read_bytes())
                os.replace(replacement, self.policy)
            if self.failure == "sign-failure":
                raise component.ToolExitError(operation, 1)
            if self.failure == "cleanup-uncertain":
                raise component.CleanupUncertain("synthetic child cleanup uncertainty")
        elif operation == "final-entry-properties":
            self.assertTrue(kwargs["signature_properties"])
            self.assertEqual(args[1:4], ["--display", "--verbose=4", "--arch"])
            self.assertIn(args[4], signing.ARCHITECTURES)
            if args[4] == "x86_64":
                if self.failure == "foreign-runtime":
                    return PROPERTIES.replace(b"0x10000(runtime)", b"0x0(none)")
                if self.failure == "foreign-timestamp":
                    return PROPERTIES.replace(b"Timestamp=", b"Signed Time=")
                if self.failure == "entry-replace":
                    replacement = path.parent / "replacement-entry"
                    replacement.write_bytes(path.read_bytes())
                    os.replace(replacement, path)
                if self.failure == "entry-change":
                    path.write_bytes(SIGNED + b"changed")
            return PROPERTIES
        elif operation == "final-entry-entitlements":
            self.assertEqual(args[1:3], ["--display", "--arch"])
            self.assertEqual(args[4:-1], ["--entitlements", "-", "--xml"])
            if args[3] == "x86_64" and self.failure == "foreign-entitlements":
                return plistlib.dumps({"com.apple.security.get-task-allow": False})
            return b""
        else:
            self.fail("unexpected finalization operation")
        return b""

    def build(self, *, selector=SELECTOR, **kwargs):
        def publish(source, output):
            if output.parent == self.workspace and self.failure == "final-collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic collision")
            source.rename(output)
            if output.parent == self.workspace and self.failure == "final-publish-uncertain":
                raise OSError("synthetic ambiguous publication")
        with self.context(**kwargs), patch.object(component, "publish_no_replace", side_effect=publish):
            return final.build_entry(self.app, self.workspace, self.frozen, selector)

    def test_default_policy_denies_before_platform_inputs_workspace_or_tools(self):
        # Deliberately do not enter context(): the real checked-in policy is used.
        with patch.object(component, "capture") as tool, patch.object(entry, "build_entry") as builder, \
             patch.object(final, "_source_snapshot") as snapshot, patch.object(component, "private_workspace") as workspace, \
             self.assertRaisesRegex(component.ComponentError, "publisher is not provisioned"):
            final.build_entry(None, None, None, None)
        for operation in (tool, builder, snapshot, workspace):
            operation.assert_not_called()

    def test_owned_fresh_full_metal_construction_returns_only_postsign_entry(self):
        with patch.object(component, "verify_cpu_app") as cpu_admission:
            output, sha, size = self.build()
        cpu_admission.assert_not_called()
        self.assertEqual(output.read_bytes(), SIGNED)
        self.assertEqual((sha, size), (digest(SIGNED), len(SIGNED)))
        self.assertNotEqual(sha, digest(IMAGE))
        self.assertEqual(list(self.workspace.iterdir()), [output])
        self.assertEqual(len(self.builds), 3)
        self.assertEqual([arch for _, arch in self.verifications], list(entry.ARCHES))
        self.assertEqual([op for op, _, _ in self.operations], ["final-entry-signature", "final-entry-signing",
                         "final-entry-signature", "final-entry-properties", "final-entry-entitlements",
                         "final-entry-properties", "final-entry-entitlements"])
        self.assertEqual([args[4] for op, args, _ in self.operations if op == "final-entry-properties"],
                         list(signing.ARCHITECTURES))
        for args, _ in self.calls:
            self.assertNotIn(Path(args[0]).name, ("postinstall", "pkgbuild", "productbuild", "installer",
                                                "scribe-inference-worker", "scribe-macos-metal-pack-installer"))

    def test_invalid_frozen_type_identity_and_selector_precede_tools(self):
        saved = self.frozen
        for frozen in (None, {}, replace(saved, revision="short"), replace(saved, catalog_sha256="invalid")):
            self.frozen = frozen
            self.reject()
        self.frozen = saved
        for selector in (None, "-", "Developer ID Application", "/private/identity", "A" * 40, "a" * 39, SELECTOR + "\n"):
            with self.assertRaisesRegex(component.ComponentError, "identity selector"):
                self.build(selector=selector)
        self.assertEqual(self.operations, [])
        self.assertEqual(self.calls, [])

    def test_frozen_digest_mismatches_do_not_reach_signing(self):
        saved = self.frozen
        for field in ("cpu_worker_sha256", "catalog_sha256", "authority_sha256", "aarch64_manifest_sha256",
                      "x86_64_manifest_sha256"):
            with self.subTest(field=field):
                self.frozen = replace(saved, **{field: "0" * 64})
                self.reject()
        self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_native_or_production_pack_verification_failure_never_signs(self):
        for failure in ("app-signature", "application", "content", "descriptor", "build", "combine"):
            with self.subTest(failure=failure):
                self.failure = failure
                self.operations.clear()
                self.reject()
                self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_unsafe_workspace_and_existing_output_precede_construction(self):
        saved = self.workspace
        for workspace in (self.app, self.app / "Contents", Path("relative")):
            self.workspace = workspace
            with self.assertRaises(component.ComponentError):
                self.build()
        self.workspace = saved
        output = saved / "postinstall"
        output.write_bytes(b"retain existing")
        with self.assertRaisesRegex(component.ComponentError, "output already exists"):
            self.build()
        self.assertEqual(output.read_bytes(), b"retain existing")
        self.assertEqual(self.operations, [])
        self.assertEqual(self.builds, [])

    def test_constructor_path_hash_size_and_extra_inventory_reject_before_signing(self):
        original = entry.build_entry
        for failure in ("path", "hash", "size", "extra", "hardlink"):
            def constructor(*args):
                path, sha, size = original(*args)
                if failure == "extra":
                    (path.parent / "preinstall").write_bytes(b"never run")
                if failure == "hardlink":
                    os.link(path, self.root / "constructor-link")
                return (path.parent / "wrong" if failure == "path" else path,
                        "0" * 64 if failure == "hash" else sha, size + (failure == "size"))
            with self.subTest(failure=failure), patch.object(entry, "build_entry", side_effect=constructor):
                self.reject()
        self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_full_source_inventory_content_and_same_byte_replacement_remain_bound(self):
        packs, _ = entry.release_inputs(self.app, self.frozen)
        for relative in entry.input_files(packs):
            source = self.app / relative
            saved = source.read_bytes()
            for failure in ("final-source-change", "final-source-replace"):
                self.failure, self.mutate_source = failure, relative
                self.operations.clear()
                with self.subTest(relative=relative, failure=failure):
                    self.reject()
                    self.assertIn("final-entry-signing", [op for op, _, _ in self.operations])
                source.write_bytes(saved)

    def test_policy_content_and_same_byte_replacement_reject_after_signing(self):
        original = self.policy.read_bytes()
        for failure in ("policy-change", "policy-replace"):
            self.policy.write_bytes(original)
            self.failure = failure
            self.operations.clear()
            self.reject()
            self.assertIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_policy_change_after_construction_is_checked_before_signing(self):
        original = entry.build_entry
        def constructor(*args):
            result = original(*args)
            self.policy.write_bytes(b"changed policy")
            return result
        with patch.object(entry, "build_entry", side_effect=constructor):
            self.reject()
        self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_signed_inventory_signature_slice_properties_and_mutations_reject(self):
        for failure in ("signed-extra-operation", "entry-signature", "foreign-runtime", "foreign-timestamp",
                        "foreign-entitlements", "entry-change", "entry-replace", "sign-failure"):
            self.failure = failure
            self.operations.clear()
            with self.subTest(failure=failure):
                self.reject()
                self.assertIn("final-entry-signing", [op for op, _, _ in self.operations])

    def test_metal_size_bound_applies_to_signed_bytes_and_structure(self):
        self.operations.clear()
        with patch.object(entry, "MAX_ENTRY", len(IMAGE)), \
             self.assertRaisesRegex(component.ComponentError, "component file size is outside its bound"):
            self.build()  # PRE-SIGN fits; reject signature growth at the first post-sign measurement.
        self.assertEqual([op for op, _, _ in self.operations], ["final-entry-signature", "final-entry-signing"])
        self.assertFalse((self.workspace / "postinstall").exists())
        with patch.object(component, "inspect_universal_structure", wraps=component.inspect_universal_structure) as inspect:
            self.build()
        self.assertEqual(inspect.call_args.args[-1], entry.MAX_ENTRY)
        self.assertEqual(inspect.call_args.args[1], [self.frozen.revision.encode(), entry.METAL_ROOT.encode()])

    def test_uncertain_signing_cleanup_propagates_without_publication(self):
        self.failure = "cleanup-uncertain"
        with self.assertRaises(component.CleanupUncertain):
            self.build()
        self.assertFalse((self.workspace / "postinstall").exists())

    def test_final_collision_and_ambiguous_publication_preserve_final(self):
        for failure in ("final-collision", "final-publish-uncertain"):
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace, self.failure = Path(directory), failure
                with self.assertRaises(OSError):
                    self.build()
                output = self.workspace / "postinstall"
                self.assertEqual(output.read_bytes(), b"retain concurrent output" if failure == "final-collision" else SIGNED)


def suite():
    result = unittest.TestSuite(Owner(name) for name in sorted(Owner.__dict__) if name.startswith("test_"))
    expected = 14
    if result.countTestCases() != expected:
        raise RuntimeError(f"Metal finalization discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
