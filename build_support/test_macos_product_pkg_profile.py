"""Portable synthetic observation plus one native fixed, never-installed fixture."""
from contextlib import ExitStack, contextmanager
import gzip
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zlib

import macos_cpu_component as component
import macos_product_pkg_profile as profile
from test_macos_cpu_pkg_profile import odc_archive, odc_record

DISTRIBUTION = b'<installer-gui-script minSpecVersion="1"><choices-outline><line choice="all"/></choices-outline></installer-gui-script>'


def xar(files, *, encoding=None, mutate=None):
    """Independent synthetic tree encoder; not a native or authentication fixture."""
    root = ET.Element("xar")
    toc = ET.SubElement(root, "toc")
    nodes, heap = {"": toc}, b""
    for path, value in files.items():
        parent = ""
        parts = path.split("/")
        for part in parts[:-1]:
            name = parent + part
            if name not in nodes:
                node = ET.SubElement(nodes[parent], "file")
                ET.SubElement(node, "name").text = part
                ET.SubElement(node, "type").text = "directory"
                nodes[name + "/"] = node
                nodes[name] = node
            parent = name + "/"
        node = ET.SubElement(nodes[parent], "file")
        ET.SubElement(node, "name").text = parts[-1]
        ET.SubElement(node, "type").text = "directory" if value is None else "file"
        if value is None:
            continue
        record = ET.SubElement(node, "data")
        style = (encoding or {}).get(path, "application/octet-stream")
        stored = zlib.compress(value) if style == "application/x-gzip" else value
        for tag, number in (("offset", len(heap)), ("length", len(stored)), ("size", len(value))):
            ET.SubElement(record, tag).text = str(number)
        ET.SubElement(record, "encoding", style=style)
        heap += stored
    if mutate:
        mutate(toc)
    xml = ET.tostring(root)
    compressed = zlib.compress(xml)
    return profile.cpu_profile.HEADER.pack(b"xar!", 28, 1, len(compressed), len(xml), 0) + compressed + heap


def component_members(role):
    info = f'<pkg-info identifier="{profile.IDENTIFIERS[role]}" version="1.0.0" install-location="/">'.encode()
    if role == "app":
        payload = odc_archive(*(odc_record(("./" + name).encode(), data) for name, data in profile.APP_FILES.items()))
        return {"PackageInfo": info + b'<bundle path="private-path"/></pkg-info>', "Bom": b"BOMStore synthetic",
                "Payload": gzip.compress(payload, mtime=0)}
    return {"PackageInfo": info + b'<scripts><postinstall file="./postinstall"/></scripts></pkg-info>',
            "Scripts": gzip.compress(odc_archive(odc_record(body=profile.OPERATIONS[role])), mtime=0)}


def fixture(*, raw=False, changes=None, encoding=None, unknown=False):
    originals = {role: xar(component_members(role)) for role in profile.ROLES}
    members = {"Distribution": DISTRIBUTION}
    for role in profile.ROLES:
        if raw:
            members[role + ".pkg"] = originals[role]
        else:
            for name, value in component_members(role).items():
                members[role + ".pkg/" + name] = value
    members.update(changes or {})
    if unknown:
        members["PrivateSentinel/member"] = b"private content"
    return originals, xar(members, encoding=encoding)


class Portable(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scribe-product-profile-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.calls, self.workspace = [], None
        self.failure = None

    def execute(self, args, **kwargs):
        self.calls.append((list(args), kwargs))
        self.workspace = kwargs["cwd"]
        self.assertEqual(set(kwargs["environment"]), {"PATH", "LC_ALL", "LANG", "TMPDIR"})
        self.assertEqual(kwargs["timeout"], 30)
        if args[0] == "/usr/bin/pkgbuild":
            role = next(role for role in profile.ROLES if args[args.index("--identifier") + 1] == profile.IDENTIFIERS[role])
            expected = ["/usr/bin/pkgbuild"] + (["--root", str(self.workspace / "payload")] if role == "app" else
                ["--nopayload", "--scripts", str(self.workspace / (role + "-scripts"))])
            expected += ["--identifier", profile.IDENTIFIERS[role], "--version", "1.0.0", "--install-location",
                         "/Applications" if role == "app" else "/", str(self.workspace / (role + ".pkg"))]
            self.assertEqual(args, expected)
            for name, data in profile.APP_FILES.items():
                self.assertEqual((self.workspace / "payload" / name).read_bytes(), data)
            if role != "app":
                self.assertEqual((self.workspace / (role + "-scripts") / "postinstall").read_bytes(), profile.OPERATIONS[role])
            if self.failure == "builder":
                raise component.ToolExitError("tool", 1)
            Path(args[-1]).write_bytes(xar(component_members(role)))
            if self.failure == "fixture-bytes":
                (self.workspace / "cpu-scripts/postinstall").write_bytes(b"changed")
            if self.failure == "fixture-inventory":
                (self.workspace / "cpu-scripts/preinstall").write_bytes(b"extra")
        elif args[1] == "--synthesize":
            self.assertEqual(args, ["/usr/bin/productbuild", "--synthesize", *[item for role in profile.ROLES
                for item in ("--package", str(self.workspace / (role + ".pkg")))], str(self.workspace / "Distribution.xml")])
            Path(args[-1]).write_bytes(DISTRIBUTION)
        else:
            self.assertEqual(args, ["/usr/bin/productbuild", "--distribution", str(self.workspace / "Distribution.xml"),
                                    "--package-path", str(self.workspace), str(self.workspace / "product.pkg")])
            if self.failure == "uncertain":
                raise component.CleanupUncertain("synthetic product child uncertainty")
            if self.failure == "uncertain-caused":
                error = component.CleanupUncertain("synthetic product child uncertainty")
                self.original_uncertainty = error
                cause = subprocess.TimeoutExpired(["/usr/bin/productbuild", "/PrivateSentinel/fixture"], 2)
                raise error from cause
            if self.failure == "filesystem":
                raise OSError("PrivateSentinel private filesystem path")
            Path(args[-1]).write_bytes(fixture()[1])
            if self.failure in ("input-change", "input-replace", "distribution-replace"):
                path = self.workspace / ("Distribution.xml" if self.failure == "distribution-replace" else "app.pkg")
                alternate = self.workspace / "replacement"
                alternate.write_bytes(path.read_bytes() + (b"changed" if self.failure == "input-change" else b""))
                os.replace(alternate, path)
        return b""

    def run_probe(self, *, real_workspace=False):
        @contextmanager
        def workspace(*_args):
            with tempfile.TemporaryDirectory(prefix="scribe-product-profile-", dir=self.root) as directory:
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

    def test_raw_archive_and_component_member_tree_are_distinct_observations(self):
        for raw in (False, True):
            report = profile._observe(*fixture(raw=raw))
            self.assertTrue(report["observation_only"])
            self.assertFalse(report["checksum_or_signature_verification"])
            for item in report["components"]:
                self.assertEqual(item["embedding_class"], "raw-xar-file" if raw else "component-member-tree")
                self.assertIs(item["raw_archive_bytes_match"], True if raw else None)
                self.assertTrue(all(member["decoded_bytes_match"] for member in item["member_comparisons"]))
        originals, _ = fixture(raw=True)
        changed = {**component_members("app"), "PrivateSentinel": b"private",
                   "PackageInfo": b'<pkg-info secret="PrivateSentinel"><PrivateSentinel path="/PrivateSentinel"/></pkg-info>',
                   "Payload": gzip.compress(odc_archive(odc_record(b"./PrivateSentinel", b"different")), mtime=0),
                   "Bom": b"unsupported BOM format"}
        observed = profile._observe(originals, fixture(raw=True, changes={"app.pkg": xar(changed)})[1])["components"][2]
        self.assertFalse(observed["raw_archive_bytes_match"])
        self.assertTrue(observed["input_payload"]["fixture_files_match"])
        self.assertFalse(observed["embedded_payload"]["fixture_files_match"])
        self.assertEqual(observed["input_payload"]["bom_format_class"], "bomstore")
        self.assertEqual(observed["embedded_payload"]["bom_format_class"], "unobserved")
        self.assertNotEqual(observed["input_package_info_shapes"], observed["embedded_package_info_shapes"])
        self.assertTrue(any(item["name_class"] == "unrecognized" for item in observed["embedded_members"]))
        self.assertFalse(any(item["name_class"] == "unrecognized" for item in observed["input_members"]))
        self.assertTrue(any(item["tag"] == "unrecognized" for item in observed["embedded_package_info_shapes"]))
        self.assertFalse(any(item["decoded_bytes_match"] for item in observed["member_comparisons"]))
        self.assertNotIn("PrivateSentinel", json.dumps(observed))

    def test_transformed_member_bytes_are_reported_not_silently_equated(self):
        report = profile._observe(*fixture(changes={"cpu.pkg/Scripts": b"different"}))
        comparison = next(item for item in report["components"][0]["member_comparisons"] if item["member"] == "Scripts")
        self.assertFalse(comparison["decoded_bytes_match"])

    def test_swapped_component_contents_are_not_matched_by_basename(self):
        report = profile._observe(*fixture(changes={"cpu.pkg/Scripts": component_members("metal")["Scripts"]}))
        self.assertFalse(report["components"][0]["member_comparisons"][1]["decoded_bytes_match"])

    def test_unknown_encodings_remain_opaque_without_successful_comparison(self):
        report = profile._observe(*fixture(encoding={"app.pkg/Payload": "PrivateSentinel"}))
        payload = next(item for item in report["components"][2]["member_comparisons"] if item["member"] == "Payload")
        self.assertIsNone(payload["decoded_bytes_match"])
        self.assertNotIn("PrivateSentinel", json.dumps(report))

    def test_payload_compression_and_bom_classes_do_not_claim_authentication(self):
        report = profile._observe(*fixture())
        payload = report["components"][2]["input_payload"]
        self.assertEqual(payload["compression_class"], "gzip")
        self.assertTrue(payload["fixture_files_match"])
        self.assertEqual(payload["bom_format_class"], "bomstore")
        self.assertEqual(payload, report["components"][2]["embedded_payload"])
        self.assertIsNone(report["components"][2]["embedded_signature_present"])
        for value, label in ((b"pbzx opaque", "pbzx-unobserved"), (b"private unsupported", "unobserved")):
            item = profile._xar(xar({"Payload": value}))
            self.assertEqual(profile._payload_report(item["members"])["compression_class"], label)
            self.assertIsNone(profile._payload_report(item["members"])["fixture_files_match"])

    def test_payload_mismatch_is_observed_and_never_extracted(self):
        body = gzip.compress(odc_archive(odc_record(b"./PrivateSentinel", b"private")), mtime=0)
        members = profile._xar(xar({"Payload": body}))["members"]
        report = profile._payload_report(members)
        self.assertFalse(report["fixture_files_match"])
        self.assertNotIn("PrivateSentinel", json.dumps(report))

    def test_unsafe_duplicate_and_case_alias_names_reject_with_closed_errors(self):
        for value in ("../PrivateSentinel", "/PrivateSentinel", "private:stream", "..", "PrivateSentinel\\child"):
            with self.assertRaises(component.ComponentError) as caught:
                profile._xar(xar({"Safe": b"x"}, mutate=lambda toc: setattr(toc.find("file/name"), "text", value)))
            self.assertNotIn("PrivateSentinel", str(caught.exception))
        for files in ({"Scripts": b"a", "scripts": b"b"}, {"app.pkg/Payload": b"a", "APP.pkg/PackageInfo": b"b"}):
            with self.assertRaises(component.ComponentError):
                profile._xar(xar(files))

    def test_nested_depth_count_and_non_regular_member_types_are_bounded(self):
        self.assertEqual(len(profile._xar(xar({"a/b/c/d": b"x"}))["members"]), 4)
        with self.assertRaisesRegex(component.ComponentError, "member count or depth exceeds bound"):
            profile._xar(xar({"a/b/c/d/e": b"x"}))
        boundary = {"directory" + str(i): None for i in range(32)}
        self.assertEqual(len(profile._xar(xar(boundary))["members"]), 32)
        with self.assertRaisesRegex(component.ComponentError, "member count or depth exceeds bound"):
            profile._xar(xar({**boundary, "extra": None}))
        for kind in ("symlink", "hardlink", "fifo"):
            with self.assertRaises(component.ComponentError):
                profile._xar(xar({"Scripts": b"x"}, mutate=lambda toc: setattr(toc.find("file/type"), "text", kind)))

    def test_member_ranges_overlap_lengths_and_missing_encoding_reject(self):
        def overlap(toc):
            toc.findall("file")[1].find("data/offset").text = "0"
        def encoding(toc):
            toc.find("file/data").remove(toc.find("file/data/encoding"))
        for mutate in (overlap, encoding,
                       lambda toc: setattr(toc.find("file/data/size"), "text", str(profile.MAX_MEMBER + 1)),
                       lambda toc: setattr(toc.find("file/data/length"), "text", "999999")):
            with self.assertRaises(component.ComponentError):
                profile._xar(xar({"Scripts": b"a", "PackageInfo": b"b"}, mutate=mutate))

    def test_header_toc_and_package_bounds_reject(self):
        valid = xar({"Scripts": b"x"})
        for data in (b"", valid[:27], b"bad!" + valid[4:], valid[:-1], valid + b"x" * profile.MAX_PACKAGE):
            with self.assertRaises(component.ComponentError):
                profile._xar(data)

    def test_compression_bombs_trailing_and_concatenated_streams_reject(self):
        for data in (b"bad", zlib.compress(b"x")[:-1], zlib.compress(b"x") + b"tail",
                     gzip.compress(b"x") + gzip.compress(b"x"), zlib.compress(b"x" * 1000)):
            with self.assertRaises(component.ComponentError):
                profile._inflate(data, 50)
        self.assertEqual(profile._inflate(zlib.compress(b"x"), 1), b"x")

    def test_xml_entities_encoding_depth_and_node_bombs_reject_privately(self):
        for data in (b'<!DOCTYPE x [<!ENTITY e "PrivateSentinel">]><x>&e;</x>',
                     b"<x>" * 10 + b"</x>" * 10, b"<x>" + b"<x/>" * 300 + b"</x>", b"\xff"):
            with self.assertRaises(component.ComponentError) as caught:
                profile._xml_shapes(data)
            self.assertNotIn("PrivateSentinel", str(caught.exception))

    def test_archive_decoded_and_report_aggregate_limits_are_executable(self):
        inputs, product = fixture(raw=True)
        for constant in ("MAX_ARCHIVES", "MAX_ARCHIVE_BYTES", "MAX_DECODED_BYTES", "MAX_REPORT"):
            with patch.object(profile, constant, 1), self.assertRaises(component.ComponentError):
                profile._observe(inputs, product)
        # Independent lengths: the outer flattened product and three original
        # archives, PLUS both actual uncompressed app payloads. Reject only at
        # the new inner-payload accounting, not an earlier XAR/XML/member cap.
        inputs, product = fixture()
        declared = len(DISTRIBUTION) + 2 * sum(len(value) for role in profile.ROLES
                                             for value in component_members(role).values())
        payload_bytes = len(gzip.decompress(component_members("app")["Payload"]))
        exact = declared + 2 * payload_bytes
        with patch.object(profile, "MAX_DECODED_BYTES", exact):
            self.assertTrue(profile._observe(inputs, product)["observation_only"])
        with patch.object(profile, "MAX_DECODED_BYTES", exact - 1), \
             self.assertRaisesRegex(component.ComponentError, "aggregate decoded bytes exceed bound"):
            profile._observe(inputs, product)

    def test_unknown_names_attributes_values_and_paths_never_enter_report(self):
        report = profile._observe(*fixture(unknown=True))
        self.assertNotIn("PrivateSentinel", json.dumps(report))
        shapes = profile._xml_shapes(b'<PrivateSentinel secret="PrivateSentinel" path="/PrivateSentinel">PrivateSentinel</PrivateSentinel>')
        self.assertEqual(shapes[0]["tag"], "unrecognized")
        self.assertEqual(shapes[0]["unknown_attribute_count"], 1)
        self.assertNotIn("PrivateSentinel", json.dumps(shapes))

    def test_probe_runs_only_three_pkgbuilds_and_two_productbuilds(self):
        report = self.run_probe()
        self.assertTrue(report["owned_inputs_unchanged"])
        self.assertEqual([args[0] for args, _ in self.calls], ["/usr/bin/pkgbuild"] * 3 + ["/usr/bin/productbuild"] * 2)
        self.assertFalse(list(self.root.iterdir()))
        encoded = json.dumps(report)
        self.assertNotIn(str(self.root), encoded)
        for args, _ in self.calls:
            self.assertFalse({"--sign", "--expand", "--expand-full", "installer", "postinstall"}.intersection(args))

    def test_component_and_distribution_input_content_or_node_changes_reject(self):
        for failure in ("input-change", "input-replace", "distribution-replace"):
            self.failure = failure
            with self.assertRaisesRegex(component.ComponentError, "owned fixture input changed"):
                self.run_probe()
            self.assertFalse(list(self.root.iterdir()))

    def test_original_fixture_bytes_and_directory_inventory_are_bound(self):
        for failure in ("fixture-bytes", "fixture-inventory"):
            self.failure = failure
            with self.assertRaisesRegex(component.ComponentError, "owned fixture input changed"):
                self.run_probe()
            self.assertEqual(len(self.calls), 1)
            self.calls.clear()

    def test_output_replacement_after_observation_rejects_before_report(self):
        original = profile._observe
        for replacement in (False, True):
            def observed(*args):
                result = original(*args)
                path = self.workspace / "product.pkg"
                alternate = self.workspace / "alternate"
                alternate.write_bytes(path.read_bytes() + (b"" if replacement else b"changed"))
                os.replace(alternate, path)
                return result
            with patch.object(profile, "_observe", side_effect=observed), \
                 self.assertRaisesRegex(component.ComponentError, "owned product changed"):
                self.run_probe()

    def test_platform_root_and_cli_arguments_reject_before_work(self):
        for platform, uid in (("win32", 501), ("linux", 501), ("darwin", 0)):
            with patch.object(profile.sys, "platform", platform), patch.object(profile.os, "geteuid", return_value=uid, create=True), \
                 patch.object(component, "private_workspace") as workspace, patch.object(component, "capture") as tool, \
                 self.assertRaises(component.ComponentError):
                profile.probe()
            workspace.assert_not_called()
            tool.assert_not_called()
        for args in (["probe", "external.pkg"], ["probe", "--help"], ["probe", "--sign", "secret"]):
            with patch.object(profile.sys, "argv", args), patch.object(profile, "probe") as probe, \
                 self.assertRaises(component.ComponentError):
                profile.main()
            probe.assert_not_called()

    def test_tool_and_filesystem_errors_are_redacted_without_masking_uncertainty(self):
        for failure, error in (("builder", component.ToolExitError), ("filesystem", component.ComponentError),
                               ("uncertain", component.CleanupUncertain)):
            self.failure = failure
            with self.assertRaises(error) as caught:
                self.run_probe()
            self.assertNotIn("PrivateSentinel", str(caught.exception))
        with patch.object(profile.sys, "argv", ["probe"]), patch.object(profile, "probe", return_value={"observation_only": True}), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            profile.main()
        self.assertEqual(json.loads(output.getvalue()), {"observation_only": True})


class Posix(Portable):
    def test_uncertain_product_child_retains_private_workspace(self):
        self.failure = "uncertain-caused"
        with self.assertRaisesRegex(component.CleanupUncertain, "synthetic product child uncertainty"):
            try:
                self.run_probe(real_workspace=True)
            except component.CleanupUncertain as error:
                self.assertIs(error, self.original_uncertainty)
                rendered = "".join(traceback.format_exception(error))
                self.assertNotIn("PrivateSentinel", rendered)
                self.assertNotIn("TimeoutExpired", rendered)
                raise
        retained = list(self.root.glob("scribe-product-profile-*"))
        self.assertEqual(len(retained), 1)
        self.assertTrue((retained[0] / "cpu.pkg").is_file())
        # All children were mocked; fixture cleanup cannot affect a live process.


class NativeMac(unittest.TestCase):
    def test_actual_fixed_unsigned_product_without_execution_or_installation(self):
        report = profile.probe()
        print("SCRIBE_NATIVE_PRODUCT_PKG_PROFILE=" + json.dumps(report, sort_keys=True), flush=True)
        self.assertTrue(report["observation_only"])
        self.assertTrue(report["owned_inputs_unchanged"])
        self.assertFalse(report["checksum_or_signature_verification"])
        self.assertEqual([item["role"] for item in report["components"]], list(profile.ROLES))
        # Layout/equality/encoding flags are observations, NOT acceptance rules.


def suite():
    result = unittest.TestSuite()
    classes = [Portable] + ([Posix] if os.name == "posix" else []) + ([NativeMac] if sys.platform == "darwin" else [])
    for cls in classes:
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 20 + (1 if os.name == "posix" else 0) + (1 if sys.platform == "darwin" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"Product profile discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
