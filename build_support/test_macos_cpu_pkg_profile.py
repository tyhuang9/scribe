"""Source fixtures plus one native, unprivileged, never-installed PKG observation."""
from contextlib import contextmanager
import gzip
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zlib

import macos_cpu_pkg_profile as profile


def synthetic_package(*, encoding="application/octet-stream", inner="gzip", mutate=None, scripts=None):
    """Synthetic observation bytes, NOT an authentic PKG or a valid CPIO archive."""
    info = (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" '
            'install-location="/"><scripts><postinstall file="./postinstall"/>'
            '</scripts></pkg-info>').encode()
    archive = b"070707synthetic prefix only\0" + profile.FIXTURE if scripts is None else scripts
    encoded_archive = {"gzip": gzip.compress, "zlib": zlib.compress, "none": lambda value: value}[inner](archive)
    payloads = {"PackageInfo": info, "Scripts": encoded_archive}
    root = ET.Element("xar")
    toc = ET.SubElement(root, "toc")
    heap = b""
    for name, value in payloads.items():
        file = ET.SubElement(toc, "file")
        for key, text in [("name", name), ("type", "file"), ("mode", "0644")]:
            ET.SubElement(file, key).text = text
        data = ET.SubElement(file, "data")
        encoded = zlib.compress(value) if encoding == "application/x-gzip" else value
        for key, number in [("offset", len(heap)), ("length", len(encoded)), ("size", len(value))]:
            ET.SubElement(data, key).text = str(number)
        ET.SubElement(data, "encoding", style=encoding)
        ET.SubElement(data, "archived-checksum", style="sha1").text = "synthetic"
        heap += encoded
    if mutate:
        mutate(toc)
    xml = ET.tostring(root, encoding="utf-8")
    compressed = zlib.compress(xml)
    return profile.HEADER.pack(b"xar!", profile.HEADER.size, 1, len(compressed), len(xml), 1) + compressed + heap


class Portable(unittest.TestCase):
    def test_observation_is_sanitized_and_not_authentication(self):
        report = profile.observe_generated(synthetic_package())
        self.assertTrue(report["observation_only"])
        self.assertEqual(report["fixture_occurrences"], 1)
        self.assertEqual(report["scripts_prefix_hex"], b"070707synthetic ".hex())
        self.assertEqual([member["name"] for member in report["members"]], ["PackageInfo", "Scripts"])
        self.assertEqual(report["postinstall_reference"], "./postinstall")
        self.assertNotIn("synthetic", json.dumps(report))
        self.assertNotIn("trusted", report)

    def test_outer_zlib_member_is_observed(self):
        for outer in ["application/octet-stream", "application/x-gzip"]:
            for inner in ["none", "gzip", "zlib"]:
                with self.subTest(outer=outer, inner=inner):
                    report = profile.observe_generated(synthetic_package(encoding=outer, inner=inner))
                    self.assertTrue(all(member["encoding"] == outer for member in report["members"]))
                    self.assertEqual(report["scripts_inner_compression"], inner)

    def test_inflate_rejects_truncation_trailing_concatenation_and_bombs(self):
        for source in [b"invalid", zlib.compress(b"x")[:-1], zlib.compress(b"x") + b"extra",
                       gzip.compress(b"x") + gzip.compress(b"x"), zlib.compress(b"x" * 1025)]:
            with self.subTest(source=source[:12]), self.assertRaises(profile.component.ComponentError):
                profile.inflate(source, 1024)
        for maximum in [0, -1, profile.MAX_TOC + 1]:
            with self.assertRaises(profile.component.ComponentError):
                profile.inflate(zlib.compress(b"x"), maximum)

    def test_package_header_and_size_bounds(self):
        data = synthetic_package()
        for value in [b"", data[:27], b"bad!" + data[4:],
                      profile.HEADER.pack(b"xar!", 29, 1, 1, 1, 1) + b"x",
                      profile.HEADER.pack(b"xar!", 28, 2, 1, 1, 1) + b"x",
                      profile.HEADER.pack(b"xar!", 28, 1, profile.MAX_TOC + 1, 1, 1) + b"x",
                      profile.HEADER.pack(b"xar!", 28, 1, 1, profile.MAX_TOC + 1, 1) + b"x",
                      data + b"x" * profile.MAX_PACKAGE]:
            with self.subTest(length=len(value)), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(value)

    def test_toc_length_mismatch_and_trailing_stream_reject(self):
        data = synthetic_package()
        magic, size, version, compressed, expanded, checksum = profile.HEADER.unpack(data[:28])
        for value in [profile.HEADER.pack(magic, size, version, compressed, expanded + 1, checksum) + data[28:],
                      profile.HEADER.pack(magic, size, version, compressed + 3, expanded, checksum)
                      + data[28:28 + compressed] + b"bad" + data[28 + compressed:]]:
            with self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(value)

    def test_xml_rejects_entities_encoding_depth_and_node_bombs(self):
        entity = "<!DOCTYPE x [<!ENTITY e 'bounded'>]><x>&e;</x>"
        for value in [b"<!DOCTYPE x [<!ENTITY a 'b'>]><x>&a;</x>", b"\xff", b"<x>",
                      entity.encode("utf-16-le"), entity.encode("utf-16-be"),
                      entity.encode("utf-32-le"), entity.encode("utf-32-be"),
                      b"<x>" * 10 + b"</x>" * 10,
                      b"<x>" + b"<x/>" * 65 + b"</x>",
                      b"<x>" + (b"<x>" + b"<x/>" * 50 + b"</x>") * 6 + b"</x>"]:
            with self.subTest(value=value[:20]), self.assertRaises(profile.component.ComponentError):
                profile.xml_tree(value)

    def test_unsafe_duplicate_case_alias_and_nested_members_reject(self):
        for value in ["../Scripts", "/Scripts", "Scripts/name", "PackageInfo", "packageinfo"]:
            def mutation(toc):
                toc.findall("file")[1].find("name").text = value
            with self.subTest(value=value), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(synthetic_package(mutate=mutation))
        def nested(toc):
            ET.SubElement(toc.find("file"), "file")
        with self.assertRaises(profile.component.ComponentError):
            profile.observe_generated(synthetic_package(mutate=nested))

    def test_member_types_and_unknown_encodings_reject(self):
        for kind in ["directory", "symlink", "hardlink", "fifo"]:
            def mutation(toc):
                toc.find("file/type").text = kind
            with self.subTest(kind=kind), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(synthetic_package(mutate=mutation))
        with self.assertRaises(profile.component.ComponentError):
            profile.observe_generated(synthetic_package(encoding="application/x-bzip2"))

    def test_member_numeric_bounds_overlap_and_truncation_reject(self):
        for key, value in [("length", "0"), ("length", str(profile.MAX_MEMBER + 1)),
                           ("length", "9999999999999999999"), ("offset", "-1"),
                           ("offset", "01"), ("offset", str(profile.MAX_PACKAGE)),
                           ("size", "0"), ("size", str(profile.MAX_MEMBER + 1))]:
            def mutation(toc):
                toc.find("file/data/" + key).text = value
            with self.subTest(key=key, value=value), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(synthetic_package(mutate=mutation))
        def overlap(toc):
            toc.findall("file")[1].find("data/offset").text = "0"
        with self.assertRaises(profile.component.ComponentError):
            profile.observe_generated(synthetic_package(mutate=overlap))
        with self.assertRaises(profile.component.ComponentError):
            profile.observe_generated(synthetic_package()[:-1])

    def test_scalar_and_encoding_ambiguity_reject(self):
        for tag in ["name", "type", "data"]:
            def mutation(toc):
                ET.SubElement(toc.find("file"), tag).text = "duplicate"
            with self.subTest(tag=tag), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(synthetic_package(mutate=mutation))
        def duplicate_encoding(toc):
            ET.SubElement(toc.find("file/data"), "encoding", style="application/octet-stream")
        with self.assertRaises(profile.component.ComponentError):
            profile.observe_generated(synthetic_package(mutate=duplicate_encoding))

    def test_scripts_require_one_fixed_fixture(self):
        for scripts in [b"absent", profile.FIXTURE * 2, b"x" * (profile.MAX_MEMBER + 1)]:
            with self.subTest(length=len(scripts)), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(synthetic_package(scripts=scripts))

    def test_required_members_and_fixed_metadata_reject_changes(self):
        def missing(toc):
            toc.remove(toc.findall("file")[1])
        with self.assertRaises(profile.component.ComponentError):
            profile.observe_generated(synthetic_package(mutate=missing))
        data = synthetic_package()
        for before, after in [(b'install-location="/"', b'install-location="x"'),
                              (b'version="1.0.0"', b'version="2.0.0"'),
                              (b'file="./postinstall"', b'file="./preinstall!"')]:
            with self.subTest(before=before), self.assertRaises(profile.component.ComponentError):
                profile.observe_generated(data.replace(before, after))

    def mocked_probe(self, workspace, execute):
        @contextmanager
        def private(_parent, prefix):
            self.assertEqual(prefix, "scribe-pkg-profile-")
            yield workspace
        return (patch.object(profile.sys, "platform", "darwin"),
                patch.object(profile.os, "geteuid", return_value=501, create=True),
                patch.object(profile.os, "fchmod", create=True),
                patch.object(profile.component, "private_workspace", side_effect=private),
                patch.object(profile.component, "capture", side_effect=execute))

    def run_mocked_probe(self, workspace, execute):
        platform, uid, chmod, private, capture = self.mocked_probe(workspace, execute)
        with platform, uid, chmod, private, capture:
            return profile.probe()

    def test_probe_invokes_only_exact_metadata_tools_and_never_entry(self):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary)
            calls = []
            def execute(arguments, **kwargs):
                calls.append(arguments)
                if arguments[0] == "/usr/bin/pkgbuild":
                    self.assertEqual(arguments, ["/usr/bin/pkgbuild", "--nopayload", "--scripts",
                        str(workspace / "scripts"), "--identifier", profile.IDENTIFIER,
                        "--version", profile.VERSION, "--install-location", "/", str(workspace / "fixture.pkg")])
                    self.assertEqual((workspace / "scripts/postinstall").read_bytes(), profile.FIXTURE)
                    self.assertEqual(kwargs["timeout"], 30)
                    (workspace / "fixture.pkg").write_bytes(synthetic_package())
                    return b""
                self.assertEqual(arguments, ["/usr/bin/xar", "-tf", str(workspace / "fixture.pkg")])
                return b"PackageInfo\nScripts\n"
            report = self.run_mocked_probe(workspace, execute)
            self.assertEqual(len(calls), 2)
            self.assertTrue(report["observation_only"])

    def test_probe_tool_failure_and_listing_mismatch_fail_closed(self):
        for failure in ["builder", "listing"]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                workspace = Path(temporary)
                def execute(arguments, **_kwargs):
                    if arguments[0] == "/usr/bin/pkgbuild":
                        if failure == "builder":
                            raise profile.component.ComponentError("tool failure")
                        (workspace / "fixture.pkg").write_bytes(synthetic_package())
                        return b""
                    return b"Scripts\nScripts\n"
                with self.assertRaises(profile.component.ComponentError):
                    self.run_mocked_probe(workspace, execute)

    def test_root_and_foreign_hosts_reject_before_workspace_or_tools(self):
        for platform, uid in [("win32", 501), ("linux", 501), ("darwin", 0)]:
            with self.subTest(platform=platform, uid=uid), \
                 patch.object(profile.sys, "platform", platform), \
                 patch.object(profile.os, "geteuid", return_value=uid, create=True), \
                 patch.object(profile.component, "private_workspace") as private, \
                 patch.object(profile.component, "capture") as execute, \
                 self.assertRaises(profile.component.ComponentError):
                profile.probe()
            private.assert_not_called()
            execute.assert_not_called()

    def test_main_accepts_no_external_archive_path_or_override(self):
        for arguments in [["probe", "candidate.pkg"], ["probe", "--root", "/"], ["probe", "--help"]]:
            with self.subTest(arguments=arguments), patch.object(profile.sys, "argv", arguments), \
                 patch.object(profile, "probe") as probe, self.assertRaises(profile.component.ComponentError):
                profile.main()
            probe.assert_not_called()
        with patch.object(profile.sys, "argv", ["probe"]), \
             patch.object(profile, "probe", return_value={"observation_only": True}), \
             patch("sys.stdout", new_callable=io.StringIO) as output:
            profile.main()
        self.assertEqual(json.loads(output.getvalue()), {"observation_only": True})


class NativeMac(unittest.TestCase):
    def test_actual_unsigned_nopayload_format_without_installation(self):
        report = profile.probe()
        # The path-free report is evidence of this tool version's output, not
        # a frozen accepted profile or authenticated installation evidence.
        print("SCRIBE_NATIVE_CPU_PKG_PROFILE=" + json.dumps(report, sort_keys=True), flush=True)
        self.assertTrue(report["observation_only"])


def suite():
    result = unittest.defaultTestLoader.loadTestsFromTestCase(Portable)
    if sys.platform == "darwin":
        result.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(NativeMac))
    expected = 16 + (1 if sys.platform == "darwin" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"PKG profile discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
