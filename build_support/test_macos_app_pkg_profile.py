"""Mocked contract tests and one input-free unprivileged native observation."""
from contextlib import contextmanager, ExitStack
import gzip
import io
import json
import os
from pathlib import Path
import plistlib
import stat
import sys
import tempfile
import traceback
import unittest
from unittest.mock import patch

import macos_app_pkg_profile as profile
import macos_cpu_component as component
from test_macos_cpu_pkg_profile import odc_archive, odc_record
from test_macos_product_pkg_profile import xar


def payload(**changes):
    return odc_archive(*(odc_record(name.encode(), data, mode=mode, uid=0, gid=0, **changes)
                         for name, (mode, data) in profile._inventory().items()))


def listing(*, omit=None):
    return b"".join((f"{name}\t{mode:o}\t0\t0\t{len(data)}\t123\n").encode()
                    for name, (mode, data) in profile._inventory().items() if name != omit)


def package():
    info = (f'<pkg-info auth="root" format-version="2" identifier="{profile.IDENTIFIER}" '
            f'version="{profile.VERSION}" install-location="/Applications">'
            '<payload numberOfFiles="12" installKBytes="4"/>'
            f'<bundle path="{profile.APP}" id="{profile.IDENTIFIER}"/>'
            f'<strict-identifier><bundle id="{profile.IDENTIFIER}"/></strict-identifier>'
            '<relocate/></pkg-info>').encode()
    return xar({"PackageInfo": info, "Payload": gzip.compress(payload(), mtime=0), "Bom": b"BOMStore synthetic"})


class Portable(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scribe-app-profile-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.calls, self.failure, self.workspace = [], None, None

    def execute(self, args, **kwargs):
        self.calls.append(args)
        self.workspace = kwargs["cwd"]
        self.assertEqual(kwargs["timeout"], 30)
        self.assertEqual(kwargs["limit"], 65536)
        self.assertEqual(set(kwargs["environment"]), {"PATH", "LC_ALL", "LANG", "TMPDIR"})
        if args[0] == "/usr/bin/pkgbuild":
            self.assertEqual(args, ["/usr/bin/pkgbuild", "--root", str(self.workspace / "payload"),
                "--component-plist", str(self.workspace / "components.plist"), "--ownership", "recommended",
                "--compression", "legacy", "--identifier", profile.IDENTIFIER, "--version", "1.0.0",
                "--install-location", "/Applications", str(self.workspace / "app.pkg")])
            self.assertEqual(plistlib.loads((self.workspace / "components.plist").read_bytes()), profile.CONTROLS)
            if os.name == "posix":
                for name, (mode, _) in profile._inventory().items():
                    self.assertEqual(stat.S_IMODE((self.workspace / "payload" / name).stat().st_mode), stat.S_IMODE(mode))
            Path(args[-1]).write_bytes(package())
            if self.failure == "source-mutation":
                (self.workspace / "payload" / next(iter(profile.FILES))).write_bytes(b"changed")
            if self.failure == "inventory-mutation":
                (self.workspace / "payload/extra").write_bytes(b"extra")
        elif args[0] == "/usr/bin/cksum":
            path = Path(args[1])
            self.assertEqual(path.read_bytes(), profile.FILES[path.relative_to(self.workspace / "payload").as_posix()])
            if self.failure == "checksum-output":
                return b"PrivateSentinel secret output\n"
            return b"123 " + str(path.stat().st_size).encode() + b" " + os.fsencode(path) + b"\n"
        else:
            self.assertEqual(args, ["/usr/bin/lsbom", "-p", "fmugsc", str(self.workspace / "owned.bom")])
            if self.failure in ("bom-mutation", "package-replace"):
                path = self.workspace / ("owned.bom" if self.failure == "bom-mutation" else "app.pkg")
                replacement = self.workspace / "replacement"
                replacement.write_bytes(path.read_bytes())
                os.replace(replacement, path)
            if self.failure == "uncertain":
                self.uncertainty = component.CleanupUncertain("synthetic app child uncertainty")
                raise self.uncertainty from OSError("PrivateSentinel private cause")
            return listing()
        if self.failure == "tool":
            raise component.ToolExitError("tool", 1)
        if self.failure == "filesystem":
            raise OSError("PrivateSentinel path")
        return b""

    def run_probe(self, *, real_workspace=False):
        @contextmanager
        def workspace(*_args):
            with tempfile.TemporaryDirectory(prefix="scribe-app-profile-", dir=self.root) as directory:
                yield Path(directory)
        with ExitStack() as stack:
            stack.enter_context(patch.object(profile.sys, "platform", "darwin"))
            stack.enter_context(patch.object(profile.os, "geteuid", return_value=getattr(os, "geteuid", lambda: 501)() or 501,
                                             create=True))
            stack.enter_context(patch.object(profile.tempfile, "gettempdir", return_value=str(self.root)))
            stack.enter_context(patch.object(component, "capture", side_effect=self.execute))
            if not real_workspace:
                stack.enter_context(patch.object(component, "private_workspace", side_effect=workspace))
            return profile.probe()

    def test_closed_probe_commands_controls_and_cleanup(self):
        report = self.run_probe()
        self.assertEqual([args[0] for args in self.calls], ["/usr/bin/pkgbuild"] + ["/usr/bin/cksum"] * 3 + ["/usr/bin/lsbom"])
        self.assertTrue(report["owned_inputs_unchanged"])
        self.assertTrue(report["observation_only"])
        self.assertFalse(report["package_admission"])
        self.assertFalse(report["signature_verification"])
        self.assertEqual(report["installer_effects"], "unobserved")
        self.assertEqual(report["payload_compression"], "gzip")
        self.assertEqual({row["name_class"] for row in report["members"]}, {"PackageInfo", "Payload", "Bom"})
        self.assertEqual(report["bom_listing"]["flags_acl_xattrs_hardlink_tables"], "unobserved")
        self.assertFalse(list(self.root.iterdir()))
        self.assertNotIn(str(self.root), json.dumps(report))

    def test_full_inventory_modes_and_long_names(self):
        result = profile._odc(payload())
        self.assertTrue(result["inventory_matches"])
        self.assertTrue(all(row["expected_mode_matches"] and row["bytes_match"] for row in result["records"]))
        self.assertTrue(any(row["long_name"] and row["path_class"] == "long-resource" for row in result["records"]))
        self.assertEqual(result["trailer_mode_class"], "zero")
        self.assertTrue(result["trailer_links_one"])

    def test_numeric_and_root_metadata_are_observed_not_assumed(self):
        data = odc_archive(odc_record(b".", b"", mode=0o40700, uid=123, gid=456, links=4, rdev=1))
        row = profile._odc(data)["records"][0]
        self.assertEqual(row["path_class"], "payload-root")
        self.assertEqual((row["mode_class"], row["uid_class"], row["gid_class"]), ("other", "nonzero", "nonzero"))
        self.assertFalse(row["rdev_zero"])
        self.assertEqual(row["links_count"], 4)
        self.assertNotIn("123", json.dumps(row))

    def test_payload_missing_extra_duplicate_alias_and_content_differences_are_visible(self):
        valid = payload()
        extra = odc_record(b"./PrivateSentinel", b"secret") + valid
        for value in (extra, odc_record(b"./privateSentinel", b"secret") + valid,
                      odc_record(b".", b"", mode=0o40755) + valid, odc_archive(odc_record(b"./unknown", b"secret"))):
            result = profile._odc(value)
            self.assertFalse(result["inventory_matches"])
            self.assertNotIn("PrivateSentinel", json.dumps(result))
        changed = payload().replace(b"Fixed long-path resource.", b"Other long-path resource.")
        self.assertTrue(profile._odc(changed)["inventory_matches"])
        self.assertFalse(all(row["bytes_match"] for row in profile._odc(changed)["records"]))

    def test_payload_bounds_truncation_trailer_and_numeric_failures_are_closed(self):
        valid = payload()
        for value in (b"", valid[:-1], valid + b"x", valid[:10], b"badbad" + valid[6:],
                      valid[:6] + b"9" + valid[7:], odc_archive(odc_record(b"x" * 1024, b"x")), b"x" * 65537):
            with self.subTest(size=len(value)), self.assertRaises(component.ComponentError):
                profile._odc(value)
        self.assertEqual(profile._odc(valid + b"\0" * 4096)["zero_padding_bytes"], 4096)
        with self.assertRaises(component.ComponentError):
            profile._odc(valid + b"\0" * 4097)
        # Structurally valid boundary fixtures: no other rejection may mask
        # the specific size, record-count or NUL-inclusive name guard.
        base = odc_archive(odc_record(b"./x", b""))
        exact = odc_archive(odc_record(b"./x", b"x" * (65536 - len(base))))
        self.assertEqual(len(exact), 65536)
        self.assertEqual(len(profile._odc(exact)["records"]), 1)
        over = odc_archive(odc_record(b"./x", b"x" * (65537 - len(base))))
        self.assertEqual(len(over), 65537)
        with self.assertRaisesRegex(component.ComponentError, "^app profile: payload bound$"):
            profile._odc(over)
        exact_records = odc_archive(*(odc_record(b"./x", b"") for _ in range(32)))
        self.assertEqual(len(profile._odc(exact_records)["records"]), 32)
        with self.assertRaisesRegex(component.ComponentError, "^app profile: ODC record count$"):
            profile._odc(odc_record(b"./x", b"") + exact_records)
        exact_name = odc_archive(odc_record(b"x" * 1023, b""))
        self.assertTrue(profile._odc(exact_name)["records"][0]["long_name"])
        with self.assertRaisesRegex(component.ComponentError, "^app profile: ODC record bound$"):
            profile._odc(odc_archive(odc_record(b"x" * 1024, b"")))

    def test_bom_inventory_modes_sizes_and_checksum_join(self):
        result = profile._bom_listing(listing(), {"./" + name: 123 for name in profile.FILES})
        self.assertTrue(result["inventory_matches"])
        self.assertTrue(all(row["expected_mode_matches"] for row in result["records"]))
        self.assertEqual(sum(row["checksum_matches"] is True for row in result["records"]), 3)
        self.assertFalse(any(row["checksum_matches"] is False for row in result["records"]))

    def test_bom_missing_extra_duplicates_and_case_alias_do_not_match(self):
        for value in (listing(omit="."), listing() + listing(), listing() + b"./PrivateSentinel\t100644\t1\t2\t3\t4\n",
                      listing().replace(b"ScribeSource", b"scribesource")):
            result = profile._bom_listing(value, {})
            self.assertFalse(result["inventory_matches"])
            self.assertNotIn("PrivateSentinel", json.dumps(result))

    def test_bom_short_rows_and_unexpected_values_remain_unobserved(self):
        for value in (b".\t40755\t0\t0\n", b".\t40755\t0\t0\t\t\n", b".\tPrivateSentinel\t123\t456\tX\tY\n"):
            row = profile._bom_listing(value, {})["records"][0]
            self.assertIsNone(row["checksum_matches"])
            self.assertNotIn("PrivateSentinel", json.dumps(row))
            self.assertNotIn("123", json.dumps(row))
        name = "./" + next(iter(profile.FILES))
        value = f"{name}\t100644\t0\t0\t1\t124\n".encode()
        row = profile._bom_listing(value, {name: 123})["records"][0]
        self.assertFalse(row["size_matches"])
        self.assertFalse(row["checksum_matches"])

    def test_bom_output_bounds_encoding_count_and_truncated_tail_reject(self):
        for data in (b"", b"PrivateSentinel", b"\xff\n", b".\n" * 33, b"x" * 65536 + b"\n"):
            with self.assertRaises(component.ComponentError) as caught:
                profile._bom_listing(data, {})
            self.assertNotIn("PrivateSentinel", str(caught.exception))

    def test_package_info_controls_preserve_multiplicity_and_redact_unknowns(self):
        tree = profile.product._xar(package())
        report = profile._package_info(tree["members"]["PackageInfo"]["decoded"])
        self.assertTrue(report["root_identity_matches"])
        self.assertEqual(report["number_of_files"], [12])
        for value in (b'<unknown secret="PrivateSentinel"><bundle id="PrivateSentinel" path="/PrivateSentinel"/></unknown>',
                      b'<pkg-info><relocate/><relocate/></pkg-info>'):
            report = profile._package_info(value)
            self.assertNotIn("PrivateSentinel", json.dumps(report))
        self.assertEqual(next(row for row in report["controls"] if row["tag"] == "relocate")["container_count"], 2)
        for data in (b'<pkg-info/>', b'<pkg-info><strict-identifier/></pkg-info>'):
            report = profile._package_info(data)
            self.assertTrue(all(row["identifiers_match"] is None for row in report["controls"]))
            self.assertTrue(all(row["paths_match"] is None for row in report["controls"]))
        report = profile._package_info(b'<pkg-info><bundle id="PrivateSentinel" path="/PrivateSentinel"/></pkg-info>')
        row = next(row for row in report["controls"] if row["tag"] == "bundle")
        self.assertFalse(row["identifiers_match"])
        self.assertFalse(row["paths_match"])
        self.assertNotIn("PrivateSentinel", json.dumps(report))

    def test_source_inventory_package_and_bom_replacement_reject(self):
        for failure in ("source-mutation", "inventory-mutation", "bom-mutation", "package-replace"):
            self.failure = failure
            with self.subTest(failure=failure), self.assertRaisesRegex(component.ComponentError, "owned input changed"):
                self.run_probe()
            self.assertFalse(list(self.root.iterdir()))
        self.failure = None
        self.calls.clear()
        original = profile.product._xar
        def changed_before_next_tool(data):
            result = original(data)
            (self.workspace / "app.pkg").write_bytes(b"changed")
            return result
        with patch.object(profile.product, "_xar", side_effect=changed_before_next_tool), \
             self.assertRaisesRegex(component.ComponentError, "owned input changed"):
            self.run_probe()
        self.assertEqual([args[0] for args in self.calls], ["/usr/bin/pkgbuild"])

    def test_tool_checksum_and_filesystem_errors_are_redacted(self):
        for failure in ("tool", "checksum-output", "filesystem"):
            self.failure = failure
            with self.assertRaises(component.ComponentError) as caught:
                self.run_probe()
            self.assertNotIn("PrivateSentinel", "".join(traceback.format_exception(caught.exception)))

    def test_platform_root_and_arguments_reject_before_work(self):
        for platform, uid in (("win32", 501), ("linux", 501), ("darwin", 0)):
            with patch.object(profile.sys, "platform", platform), patch.object(profile.os, "geteuid", return_value=uid, create=True), \
                 patch.object(profile, "_owned_probe") as owned, self.assertRaises(component.ComponentError):
                profile.probe()
            owned.assert_not_called()
        with patch.object(profile.sys, "argv", ["probe", "external.pkg"]), patch.object(profile, "probe") as owned, \
             self.assertRaises(component.ComponentError):
            profile.main()
        owned.assert_not_called()
        with patch.object(profile.sys, "argv", ["probe"]), patch.object(profile, "probe", return_value={"observation_only": True}), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            profile.main()
        self.assertEqual(json.loads(output.getvalue()), {"observation_only": True})

    def test_report_bound_rejects_without_publication(self):
        with patch.object(profile, "MAX_REPORT", 1), self.assertRaisesRegex(component.ComponentError, "report bound"):
            self.run_probe()
        self.assertFalse(list(self.root.iterdir()))


class Posix(unittest.TestCase):
    def test_uncertain_child_preserves_workspace_without_private_cause(self):
        case = Portable("test_closed_probe_commands_controls_and_cleanup")
        case.setUp()
        try:
            case.failure = "uncertain"
            with self.assertRaises(component.CleanupUncertain) as caught:
                case.run_probe(real_workspace=True)
            self.assertIs(caught.exception, case.uncertainty)
            self.assertNotIn("PrivateSentinel", "".join(traceback.format_exception(caught.exception)))
            self.assertEqual(len(list(case.root.glob("scribe-app-profile-*"))), 1)
        finally:
            case.doCleanups()  # All children are mocked; no live process exists.


class NativeMac(unittest.TestCase):
    def test_fixed_configured_app_observation_without_installation(self):
        report = profile.probe()
        print("SCRIBE_NATIVE_APP_PKG_PROFILE=" + json.dumps(report, sort_keys=True), flush=True)
        self.assertTrue(report["observation_only"])
        self.assertTrue(report["owned_inputs_unchanged"])
        self.assertFalse(report["package_admission"])


def suite():
    result = unittest.TestSuite()
    for cls in [Portable] + ([Posix] if os.name == "posix" else []) + ([NativeMac] if sys.platform == "darwin" else []):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 14 + (1 if os.name == "posix" else 0) + (1 if sys.platform == "darwin" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"App profile discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
