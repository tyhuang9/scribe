"""Synthetic finalization only: no native signing, keys, Installer or execution."""
from contextlib import ExitStack, contextmanager
import hashlib
import json
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch

import macos_cpu_component as component
import macos_cpu_pkg_final as final
from test_macos_cpu_pkg import Author, DIGEST, ENTRY, REVISION, VERSION

TEAM = "TESTTEAM01"
SELECTOR = "a" * 40
SIGNED = ENTRY + b" synthetic final signature; never execute"
PROPERTIES = (b"Executable=/private/synthetic-only\n"
              b"CodeDirectory v=20500 size=822 flags=0x10000(runtime) hashes=14+7 location=embedded\n"
              b"Timestamp=Oct 23, 2026 at 12:00:00 PM\nTeamIdentifier=NOT-AUTHORITY\n")


class Policy(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-final-policy-source-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.policy = self.root / "policy.json"

    def test_checked_in_policy_denies_before_inputs_workspaces_or_tools(self):
        with patch.object(component, "capture") as tool, patch.object(final.entry, "build_entry") as builder, \
             patch.object(component, "private_workspace") as workspace, \
             patch.object(component, "cpu_app_input_snapshot") as snapshot, \
             self.assertRaisesRegex(component.ComponentError, "publisher is not provisioned"):
            final.build_component(None, None, None, None, None)
        for operation in (tool, builder, workspace, snapshot):
            operation.assert_not_called()

    def test_environment_cannot_provision_or_select_publisher_policy(self):
        overrides = {name: str(self.policy) for name in ("SCRIBE_MACOS_ENTRY_SIGNING_POLICY",
                     "SCRIBE_MACOS_INSTALLER_TEAM", "SCRIBE_MACOS_SIGNING_IDENTITY", "DYLD_INSERT_LIBRARIES")}
        self.policy.write_text(json.dumps({"schema_version": 1, "mode": "developer_id",
                                          "publisher_team_id": TEAM}), encoding="utf-8")
        with patch.dict(os.environ, overrides), patch.object(component, "capture") as tool, \
             self.assertRaisesRegex(component.ComponentError, "publisher is not provisioned"):
            final.build_component(None, None, None, None, None)
        tool.assert_not_called()

    def test_policy_is_closed_typed_and_has_no_input_override(self):
        bad = [{}, {"schema_version": True, "mode": "developer_id", "publisher_team_id": TEAM},
               {"schema_version": 1, "mode": "developer_id", "publisher_team_id": ""},
               {"schema_version": 1, "mode": "default_deny", "publisher_team_id": TEAM},
               {"schema_version": 1, "mode": "developer_id", "publisher_team_id": TEAM, "extra": True}]
        for team in [None, 1, True, [], {}, "testteam01", "A" * 9, "A" * 11, TEAM + "\n", 'A" or true']:
            bad.append({"schema_version": 1, "mode": "developer_id", "publisher_team_id": team})
        for value in bad:
            self.policy.write_text(json.dumps(value), encoding="utf-8")
            with patch.object(final, "POLICY_PATH", self.policy), patch.object(component, "capture") as tool, \
                 self.assertRaises(component.ComponentError):
                final.build_component(None, None, None, None, None)
            tool.assert_not_called()
        self.policy.write_text('{"schema_version":1,"mode":"developer_id","publisher_team_id":"TESTTEAM01",'
                               '"publisher_team_id":"OTHERTEAM1"}', encoding="utf-8")
        with patch.object(final, "POLICY_PATH", self.policy), self.assertRaises(component.ComponentError):
            final._publisher()

    def test_missing_oversized_and_linked_policy_fail_closed(self):
        with patch.object(final, "POLICY_PATH", self.policy), \
             self.assertRaisesRegex(component.ComponentError, "policy is unavailable") as caught:
            final._publisher()
        self.assertIsNone(caught.exception.__cause__)
        valid = json.dumps({"schema_version": 1, "mode": "developer_id", "publisher_team_id": TEAM}).encode()
        self.policy.write_bytes(valid + b" " * component.MAX_JSON)
        with patch.object(final, "POLICY_PATH", self.policy), \
             self.assertRaisesRegex(component.ComponentError, "size is outside its bound"):
            final._publisher()
        self.policy.write_bytes(valid)
        linked = self.root / "linked"
        os.link(self.policy, linked)
        with patch.object(final, "POLICY_PATH", linked), \
             self.assertRaisesRegex(component.ComponentError, "hard-linked component file"):
            final._publisher()

    def test_requirements_pin_apple_developer_id_publisher_and_separate_roles(self):
        requirements = [final._requirement(TEAM, role) for role in ("application", "cpu", "metal")]
        self.assertEqual(len(set(requirements)), 3)
        for requirement, role in zip(requirements, ("application", "cpu", "metal")):
            self.assertTrue(requirement.startswith("=anchor apple generic and "))
            self.assertIn("certificate 1[field.1.2.840.113635.100.6.2.6] exists", requirement)
            self.assertIn("certificate leaf[field.1.2.840.113635.100.6.1.13] exists", requirement)
            self.assertIn(f'certificate leaf[subject.OU] = "{TEAM}"', requirement)
            self.assertIn(f'identifier "{final.CODE_IDENTIFIERS[role]}"', requirement)
            self.assertNotIn(" or ", requirement)
        for publisher, role in [(TEAM, "private-role"), ("private-value", "cpu"), (TEAM, None), (None, "cpu")]:
            with self.assertRaisesRegex(component.ComponentError, "invalid entry signature requirement"):
                final._requirement(publisher, role)

    def test_properties_require_runtime_and_timestamp_without_displayed_identity_authority(self):
        final._properties(PROPERTIES)
        bad = [b"", b"x" * (final.MAX_PROPERTIES + 1), b"\xff", PROPERTIES * 2,
               PROPERTIES.replace(b"0x10000(runtime)", b"0x0(none)"),
               PROPERTIES.replace(b"0x10000(runtime)", b"0x10002(adhoc,runtime)"),
               PROPERTIES.replace(b"CodeDirectory ", b"private-CodeDirectory "),
               PROPERTIES.replace(b"Timestamp=", b"Signed Time="),
               PROPERTIES.replace(b"Timestamp=Oct 23, 2026 at 12:00:00 PM", b"Timestamp=none"),
               PROPERTIES.replace(b"Timestamp=Oct 23, 2026 at 12:00:00 PM", b"Timestamp="),
               PROPERTIES + b"Timestamp=private-value\n"]
        for data in bad:
            with self.subTest(length=len(data)), self.assertRaises(component.ComponentError) as caught:
                final._properties(data)
            self.assertNotIn("private", str(caught.exception))

    def test_entitlements_must_be_absent_or_empty_not_false_valued_privileges(self):
        final._entitlements(b"")
        final._entitlements(plistlib.dumps({}))
        for value in [{"com.apple.security.get-task-allow": False}, {"keychain-access-groups": []},
                      {"com.apple.security.cs.disable-library-validation": True}, [], True, "private-value"]:
            with self.assertRaises(component.ComponentError) as caught:
                final._entitlements(plistlib.dumps(value))
            self.assertNotIn("private", str(caught.exception))
        for data in [b"private-value", b"<?xml version='1.0'?><plist><dict>",
                     b"<!DOCTYPE plist [<!ENTITY private 'expanded'>]><plist><dict/></plist>",
                     b'<plist version="1.0"><dict><key>com.apple.security.get-task-allow</key><true/></dict><dict/></plist>',
                     b'<plist version="1.0"><string>private-value</string><dict/></plist>',
                     b'<plist version="1.0"><dict/><dict/></plist>',
                     b'<plist version="1.0" private="value"><dict/></plist>',
                     b'<plist version="1.0"><dict private="value"/></plist>',
                     b'<plist version="1.0">private-text<dict/></plist>',
                     b'<plist version="1.0"><dict/>private-tail</plist>',
                     b'<plist version="1.0"><dict>private-text</dict></plist>',
                     plistlib.dumps({}, fmt=plistlib.FMT_BINARY),
                     b"x" * (component.MAX_JSON + 1)]:
            with self.subTest(length=len(data)), self.assertRaises(component.ComponentError) as caught:
                final._entitlements(data)
            self.assertNotIn("private", str(caught.exception))


class Owner(Author):
    # Discover only methods defined here, not inherited codec/author tests.
    def setUp(self):
        super().setUp()
        self.policy = self.root / "policy.json"
        self.policy.write_text(json.dumps({"schema_version": 1, "mode": "developer_id",
                                          "publisher_team_id": TEAM}), encoding="utf-8")
        self.operation_timeout = "600"
        self.operations = []

    def execute(self, arguments, **kwargs):
        operation = kwargs.get("operation")
        self.operations.append((operation, list(arguments), kwargs))
        if operation == "cpu-component-pkg-build":
            result = super().execute(arguments, **kwargs)
            if self.failure == "package-policy-change":
                self.policy.write_text('{"schema_version":1,"mode":"default_deny","publisher_team_id":""}',
                                       encoding="utf-8")
            if self.failure == "package-policy-replace":
                replacement = self.root / "replacement-policy"
                replacement.write_bytes(self.policy.read_bytes())
                os.replace(replacement, self.policy)
            return result
        self.assertEqual(arguments[0], "/usr/bin/codesign")
        path = Path(arguments[-1])
        if operation == "final-entry-signature":
            self.assertEqual(arguments[1:5], ["--verify", "--strict", "--all-architectures", "--test-requirement"])
            self.assertEqual(arguments[5], final._requirement(TEAM, "application" if path == self.app else "cpu"))
            if path != self.app and self.failure in ("wrong-signer", "wrong-role", "wrong-identifier", "non-apple", "unsigned"):
                raise component.ToolExitError(operation, 1)
            if path == self.app and self.failure == "wrong-app-publisher":
                raise component.ToolExitError(operation, 1)
        elif operation == "final-entry-signing":
            self.assertEqual(arguments[1:-1], ["--force", "--sign", SELECTOR, "--options", "runtime",
                                             "--timestamp", "--identifier", final.CODE_IDENTIFIERS["cpu"]])
            path.write_bytes(SIGNED)
            if self.failure == "source-change":
                (self.app / component.CPU_APP_INPUTS[1]).write_bytes(b"changed source")
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
            if self.failure == "uncertain-signing":
                raise component.CleanupUncertain("synthetic child cleanup uncertainty")
        elif operation == "final-entry-properties":
            self.assertTrue(kwargs["signature_properties"])
            self.assertIn(arguments[4], ("arm64", "x86_64"))
            if arguments[4] == "x86_64":
                if self.failure == "foreign-slice-runtime":
                    return PROPERTIES.replace(b"0x10000(runtime)", b"0x0(none)")
                if self.failure == "foreign-slice-timestamp":
                    return PROPERTIES.replace(b"Timestamp=", b"Signed Time=")
                if self.failure == "entry-replace-during-verification":
                    replacement = path.parent / "replacement"
                    replacement.write_bytes(path.read_bytes())
                    os.replace(replacement, path)
            return PROPERTIES
        elif operation == "final-entry-entitlements":
            self.assertEqual(arguments[1:3], ["--display", "--arch"])
            self.assertEqual(arguments[4:-1], ["--entitlements", "-", "--xml"])
            if arguments[3] == "x86_64" and self.failure == "foreign-slice-entitlements":
                return plistlib.dumps({"com.apple.security.get-task-allow": True})
            return b""
        else:
            self.fail("unexpected native operation")
        return b""

    @contextmanager
    def context(self, *, real_workspace=False):
        with ExitStack() as stack:
            stack.enter_context(super().context(real_workspace=real_workspace))
            stack.enter_context(patch.object(final, "POLICY_PATH", self.policy))
            stack.enter_context(patch.object(component, "inspect_universal_structure"))
            yield

    def build(self, *, selector=SELECTOR):
        def publish(candidate, output):
            if self.failure == "collision":
                output.write_bytes(b"retain concurrent package")
                raise FileExistsError("synthetic collision")
            candidate.rename(output)
            if self.failure == "publish-uncertain":
                raise OSError("synthetic post-rename uncertainty")
        with self.context(), patch.object(component, "publish_no_replace", side_effect=publish):
            return final.build_component(self.app, self.workspace, DIGEST, REVISION, selector)

    def test_owned_fresh_entry_finalized_bytes_not_presign_measurement_reach_package(self):
        output, digest, size = self.build()
        self.assertEqual((digest, size), (hashlib.sha256(output.read_bytes()).hexdigest(), output.stat().st_size))
        self.assertEqual(list(self.workspace.iterdir()), [output])
        self.assertEqual(len(self.entries), 1)
        # Independent codec inspection proves the signed (different-length) body.
        from macos_cpu_pkg import _inspect_fresh
        _inspect_fresh(output, hashlib.sha256(SIGNED).hexdigest(), len(SIGNED), VERSION)
        self.assertEqual([op for op, _, _ in self.operations], ["final-entry-signature", "final-entry-signing",
                         "final-entry-signature", "final-entry-properties", "final-entry-entitlements",
                         "final-entry-properties", "final-entry-entitlements", "cpu-component-pkg-build"])

    def test_wrong_publisher_role_identifier_or_unsigned_entry_never_reaches_packaging(self):
        for failure in ("wrong-signer", "wrong-role", "wrong-identifier", "non-apple", "unsigned", "wrong-app-publisher"):
            with self.subTest(failure=failure):
                self.failure = failure
                self.operations.clear()
                with self.assertRaises(component.ToolExitError):
                    self.build()
                self.assertNotIn("cpu-component-pkg-build", [op for op, _, _ in self.operations])
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_constructor_path_hash_size_link_and_inventory_errors_precede_signing(self):
        for failure in ("entry-path", "entry-hash", "entry-size", "entry-hardlink", "extra-operation"):
            self.failure = failure
            self.operations.clear()
            with self.subTest(failure=failure), self.assertRaises(component.ComponentError):
                self.build()
            self.assertNotIn("final-entry-signing", [op for op, _, _ in self.operations])
            self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_unsafe_output_and_existing_package_fail_before_construction(self):
        original = self.workspace
        for workspace in (self.app, self.app / "Contents", Path("relative")):
            self.workspace = workspace
            with self.subTest(workspace=str(workspace)), self.assertRaises(component.ComponentError):
                self.build()
            self.assertEqual(self.entries, [])
            self.assertEqual(self.operations, [])
        self.workspace = original
        output = self.workspace / "cpu-worker.pkg"
        output.write_bytes(b"retain existing")
        with self.assertRaisesRegex(component.ComponentError, "output already exists"):
            self.build()
        self.assertEqual(output.read_bytes(), b"retain existing")
        self.assertEqual(self.entries, [])

    def test_packaged_or_postpackaging_entry_and_app_mutations_fail(self):
        desktop = self.app / component.CPU_APP_INPUTS[1]
        original = desktop.read_bytes()
        for failure in ("wrong-body", "wrong-info", "app-change", "entry-change", "entry-replace"):
            desktop.write_bytes(original)
            self.failure = failure
            with self.subTest(failure=failure), self.assertRaises(component.ComponentError):
                self.build()
            self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_foreign_slice_security_properties_fail_closed(self):
        for failure in ("foreign-slice-runtime", "foreign-slice-timestamp", "foreign-slice-entitlements"):
            with self.subTest(failure=failure):
                self.failure = failure
                self.operations.clear()
                with self.assertRaises(component.ComponentError):
                    self.build()
                self.assertNotIn("cpu-component-pkg-build", [op for op, _, _ in self.operations])
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_source_replacement_inventory_and_signing_failure_never_publish(self):
        desktop = self.app / component.CPU_APP_INPUTS[1]
        original = desktop.read_bytes()
        for failure in ("source-change", "entry-replace-during-verification", "signed-extra-operation", "sign-failure"):
            with self.subTest(failure=failure):
                desktop.write_bytes(original)
                self.failure = failure
                self.operations.clear()
                with self.assertRaises(component.ComponentError):
                    self.build()
                self.assertIn("final-entry-signing", [op for op, _, _ in self.operations])
                self.assertNotIn("cpu-component-pkg-build", [op for op, _, _ in self.operations])
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_signing_selector_cannot_be_a_name_flag_adhoc_or_private_path(self):
        for selector in ("-", "Developer ID Application", "/private/identity", "A" * 40, "a" * 39, SELECTOR + "\n", None):
            with self.subTest(selector=selector), self.assertRaisesRegex(component.ComponentError, "identity selector"):
                self.build(selector=selector)
            self.assertEqual(self.operations, [])
            self.assertEqual(self.entries, [])

    def test_uncertain_child_cleanup_propagates_without_publication(self):
        self.failure = "uncertain-signing"
        with self.assertRaises(component.CleanupUncertain):
            self.build()
        self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_policy_mutation_or_same_byte_replacement_during_finalization_or_packaging_rejects(self):
        original = self.policy.read_bytes()
        for failure in ("policy-change", "policy-replace", "package-policy-change", "package-policy-replace"):
            with tempfile.TemporaryDirectory(dir=self.root) as directory, self.subTest(failure=failure):
                self.workspace = Path(directory)
                self.policy.write_bytes(original)
                self.failure = failure
                self.operations.clear()
                with self.assertRaisesRegex(component.ComponentError, "policy changed during finalization"):
                    self.build()
                reached = "cpu-component-pkg-build" in [op for op, _, _ in self.operations]
                self.assertEqual(reached, failure.startswith("package-"))
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_collision_and_ambiguous_publication_preserve_final(self):
        for failure in ("collision", "publish-uncertain"):
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace = Path(directory)
                self.failure = failure
                with self.assertRaises(OSError):
                    self.build()
                output = self.workspace / "cpu-worker.pkg"
                self.assertTrue(output.exists())
                if failure == "collision":
                    self.assertEqual(output.read_bytes(), b"retain concurrent package")


def suite():
    result = unittest.TestSuite()
    for cls in (Policy, Owner):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 18
    if result.countTestCases() != expected:
        raise RuntimeError(f"CPU finalization discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
