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


def odc_record(name=b"./postinstall", body=profile.FIXTURE, *, mode=0o100755, links=1,
               dev=1, ino=2, uid=3, gid=4, rdev=0, mtime=5):
    """Independent test encoder: explicit POSIX ODC widths, not production constants."""
    numbers = ((dev, 6), (ino, 6), (mode, 6), (uid, 6), (gid, 6), (links, 6), (rdev, 6),
               (mtime, 11), (len(name) + 1, 6), (len(body), 11))
    header = b"070707" + b"".join(format(value, f"0{width}o").encode("ascii") for value, width in numbers)
    if len(header) != 76:
        raise ValueError("test ODC field does not fit its width")
    return header + name + b"\0" + body


def odc_archive(*records, padding=0):
    return (b"".join(records or (odc_record(),))
            + odc_record(b"TRAILER!!!", b"", mode=0) + b"\0" * padding)


def synthetic_package(*, encoding="application/octet-stream", inner="gzip", mutate=None, scripts=None):
    """Synthetic unsigned XAR envelope containing independently encoded valid ODC."""
    info = (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" '
            'install-location="/"><scripts><postinstall file="./postinstall"/>'
            '</scripts></pkg-info>').encode()
    archive = odc_archive() if scripts is None else scripts
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
        self.assertEqual(report["schema_version"], 3)
        self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])
        self.assertEqual(report["scripts_inventory"]["format"], "odc")
        self.assertEqual([member["name"] for member in report["members"]], ["PackageInfo", "Scripts"])
        self.assertEqual(report["postinstall_reference"], "./postinstall")
        self.assertNotIn("synthetic", json.dumps(report))
        self.assertNotIn("trusted", report)

    def test_odc_literal_header_and_unaligned_records(self):
        # This literal catches a shared encoder/decoder field-layout mistake.
        header = (b"070707" b"000001" b"000002" b"100755" b"000003" b"000004" b"000001" b"000000"
                  b"00000000005" b"000016" b"00000000003")
        self.assertEqual(len(header), 76)
        self.assertEqual(odc_record(body=b"abc"), header + b"./postinstall\0abc")
        records, trailer, padding = profile._decode_odc(odc_archive(header + b"./postinstall\0abc"))
        self.assertEqual((records[0]["name"], records[0]["mode"], records[0]["links"], records[0]["data"]),
                         (b"./postinstall", 0o100755, 1, b"abc"))
        self.assertEqual((records[0]["header_offset"], records[0]["data_offset"], records[0]["end_offset"]),
                         (0, 90, 93))
        self.assertEqual((trailer["header_offset"], padding), (93, 0))
        for name, body in [(b"a", b"x"), (b"ab", b"xy"), (b"abc", b"xyz")]:
            item = odc_record(name, body)
            entries, end, _ = profile._decode_odc(odc_archive(item))
            self.assertEqual(entries[0]["data"], body)
            self.assertEqual(end["header_offset"], len(item))

    def test_odc_every_numeric_field_rejects_non_octal(self):
        original = odc_archive()
        for offset, width in [(6, 6), (12, 6), (18, 6), (24, 6), (30, 6), (36, 6),
                              (42, 6), (48, 11), (59, 6), (65, 11)]:
            for byte in (b"8", b"9", b"g", b"+", b"-", b" ", b"\0", b"\xff"):
                for position in (offset, offset + width - 1):
                    value = original[:position] + byte + original[position + 1:]
                    with self.subTest(position=position, byte=byte), \
                         self.assertRaises(profile.component.ComponentError):
                        profile._decode_odc(value)

    def test_odc_bounds_and_wrong_dialects_reject(self):
        archive = odc_archive()
        for value in (None, bytearray(archive), b"", b"x" * (profile.MAX_MEMBER + 1),
                      b"070701" + archive[6:], b"070702" + archive[6:], b"x" + archive):
            with self.subTest(kind=type(value).__name__), self.assertRaises(profile.component.ComponentError):
                profile._decode_odc(value)
        for offset, width, number in [(59, 6, 0), (59, 6, 1), (59, 6, 257),
                                      (65, 11, profile.MAX_MEMBER + 1), (65, 11, 0o77777777777)]:
            value = archive[:offset] + format(number, f"0{width}o").encode() + archive[offset + width:]
            with self.subTest(offset=offset, number=number), self.assertRaises(profile.component.ComponentError):
                profile._decode_odc(value)
        largest = odc_archive(padding=profile.MAX_MEMBER - len(archive))
        self.assertEqual(len(largest), profile.MAX_MEMBER)
        self.assertEqual(profile._decode_odc(largest)[2], profile.MAX_MEMBER - len(archive))

    def test_odc_all_truncations_name_terminators_and_entry_padding_reject(self):
        archive = odc_archive()
        for length in range(len(archive)):
            with self.subTest(length=length), self.assertRaises(profile.component.ComponentError):
                profile._decode_odc(archive[:length])
        record = odc_record()
        for value in (archive[:77] + b"\0" + archive[78:], archive[:89] + b"X" + archive[90:],
                      odc_archive(record + b"\0"), odc_archive(record[:90] + b"\0" + record[90:])):
            with self.assertRaises(profile.component.ComponentError):
                profile._decode_odc(value)

    def test_odc_record_count_and_trailer_requirements(self):
        records = [odc_record(f"record-{index}".encode(), b"") for index in range(17)]
        self.assertEqual(len(profile._decode_odc(odc_archive(*records[:16]))[0]), 16)
        for value in (odc_archive(*records), odc_record(), odc_record(b"TRAILER!!!", b"x", mode=0),
                      odc_archive() + odc_record(b"TRAILER!!!", b"", mode=0), odc_archive() + odc_archive()):
            with self.assertRaises(profile.component.ComponentError):
                profile._decode_odc(value)
        # Name and payload bounds are independent of the fixed-fixture policy.
        longest = odc_record(b"n" * 255, b"x")
        self.assertEqual(profile._decode_odc(odc_archive(longest))[0][0]["name_bytes"], 256)

    def test_odc_zero_tail_is_observed_without_alignment_assumptions(self):
        for padding in (0, 1, 7, 511, 512, 1025):
            self.assertEqual(profile._decode_odc(odc_archive(padding=padding))[2], padding)
        for tail in (b"x", b"\0x", b"\0" * 512 + b"070707"):
            with self.assertRaises(profile.component.ComponentError):
                profile._decode_odc(odc_archive() + tail)

    def test_fixed_inventory_preserves_root_spelling_and_order(self):
        for file in (b"postinstall", b"./postinstall"):
            for root in (None, b".", b"./"):
                for root_first in (False, True):
                    records = [odc_record(file)]
                    if root:
                        directory = odc_record(root, b"", mode=0o040750, links=2)
                        records.insert(0 if root_first else 1, directory)
                    report = profile._observe_scripts_fixture(odc_archive(*records, padding=3))
                    expected = [file.decode()] if not root else ([root.decode(), file.decode()] if root_first
                                                                else [file.decode(), root.decode()])
                    self.assertEqual([item["name"] for item in report["records"]], expected)
                    self.assertEqual(report["trailing_zero_bytes"], 3)
                    self.assertTrue(report["fixture_exact_bytes_match"])

    def test_fixed_inventory_rejects_aliases_extra_records_types_and_partial_payloads(self):
        failures = [odc_archive(odc_record(), odc_record()),
                    odc_archive(odc_record(b"postinstall"), odc_record()),
                    odc_archive(odc_record(), odc_record(b".", b"", mode=0o040755),
                                odc_record(b"./", b"", mode=0o040755)),
                    odc_archive(odc_record(b".", b"", mode=0o040755)),
                    odc_archive(odc_record(), odc_record(b".", b"x", mode=0o040755)),
                    odc_record(b"TRAILER!!!", b"", mode=0)]
        failures.extend(odc_archive(odc_record(name)) for name in
                        (b"preinstall", b"Payload", b"../postinstall", b"/postinstall", b"Postinstall",
                         b"postinstall/child", b"./postinstall/.", b"\xff", b"/private-path-sentinel"))
        failures.extend(odc_archive(odc_record(mode=mode)) for mode in
                        (0, 0o040755, 0o120755, 0o010755, 0o020755, 0o060755, 0o140755))
        failures.extend(odc_archive(odc_record(body=body)) for body in
                        (b"", b"prefix" + profile.FIXTURE, profile.FIXTURE + b"suffix", profile.FIXTURE * 2))
        for value in failures:
            with self.assertRaises(profile.component.ComponentError) as error:
                profile._observe_scripts_fixture(value)
            self.assertNotIn("private-path-sentinel", str(error.exception))

    def test_link_counts_and_permissions_are_observations_not_admission(self):
        for links in (0, 1, 2, 0o777777):
            for mode in (0o100600, 0o100755, 0o104755):
                report = profile._observe_scripts_fixture(odc_archive(odc_record(links=links, mode=mode)))
                self.assertEqual(report["records"][0]["links"], links)
                self.assertEqual(report["records"][0]["mode_octal"], format(mode, "06o"))
                self.assertFalse({"safe", "admitted", "authenticated", "trusted"}.intersection(report))

    def test_full_report_omits_payload_prefixes_and_private_numeric_metadata(self):
        record = odc_record(dev=0o654321, ino=0o765432, uid=0o456123, gid=0o345612,
                            rdev=0o567123, mtime=0o12345670123)
        report = profile.observe_generated(synthetic_package(scripts=odc_archive(record)))
        inventory = report["scripts_inventory"]
        allowed = {"name", "type", "mode_octal", "links", "data_bytes", "header_offset", "name_bytes",
                   "data_offset", "end_offset"}
        self.assertEqual(set(inventory["records"][0]), allowed)
        self.assertEqual(set(inventory["trailer"]), allowed)
        encoded = json.dumps(report)
        for member in report["members"]:
            for shape in member["metadata_shapes"]:
                self.assertEqual(set(shape), {"tag", "child_count", "attribute_count", "has_text"})
                self.assertIn(shape["tag"], profile.METADATA_TAG_LABELS | {"unrecognized"})
        # Closed labels may name stat fields; no value-bearing keys or values
        # may appear. ODC identities above are never used as report fields.
        for forbidden in ("prefix_hex", '"uid":', '"gid":', '"device":', '"inode":', '"rdev":', '"mtime":',
                          '"data":', profile.FIXTURE.decode(), str(0o12345670123)):
            self.assertNotIn(forbidden, encoded)
        self.assertTrue(report["observation_only"])

    def test_member_metadata_shapes_use_closed_labels_without_stat_values(self):
        values = {"inode": "987654321012345", "deviceno": "876543210123456", "uid": "765432101234567",
                  "gid": "654321012345678", "user": "private-owner-sentinel",
                  "group": "private-group-sentinel", "mtime": "private-time-sentinel"}
        def mutate(toc):
            for tag, value in values.items():
                ET.SubElement(toc.findall("file")[1], tag).text = value
        report = profile.observe_generated(synthetic_package(mutate=mutate))
        shapes = report["members"][1]["metadata_shapes"]
        for tag in values:
            self.assertIn({"tag": tag, "child_count": 0, "attribute_count": 0, "has_text": True}, shapes)
        for value in values.values():
            self.assertNotIn(value, json.dumps(report))
        self.assertTrue(report["observation_only"])

    def test_member_metadata_shapes_preserve_duplicates_and_sort_deterministically(self):
        def mutate(toc, reverse=False):
            member = toc.findall("file")[1]
            ET.SubElement(member, "inode").text = "first-private-inode"
            field = ET.SubElement(member, "inode", private="private-attribute")
            ET.SubElement(field, "private-child").text = "private-nested-text"
            if reverse:
                member[:] = list(reversed(member))
        forward = profile.observe_generated(synthetic_package(mutate=mutate))
        reverse = profile.observe_generated(synthetic_package(mutate=lambda toc: mutate(toc, True)))
        shapes = forward["members"][1]["metadata_shapes"]
        self.assertEqual(shapes, reverse["members"][1]["metadata_shapes"])
        self.assertEqual([shape for shape in shapes if shape["tag"] == "inode"], [
            {"tag": "inode", "child_count": 0, "attribute_count": 0, "has_text": True},
            {"tag": "inode", "child_count": 1, "attribute_count": 1, "has_text": False}])
        for sentinel in ("first-private-inode", "private-attribute", "private-child", "private-nested-text"):
            self.assertNotIn(sentinel, json.dumps(forward))

    def test_unknown_member_metadata_shapes_redact_names_attributes_and_text(self):
        def mutate(toc):
            member = toc.findall("file")[1]
            field = ET.SubElement(member, "private-tag-sentinel", {"private-attribute-name": "private-value"})
            field.text = "private-text-sentinel"
            ET.SubElement(field, "private-child-name").text = "private-child-value"
            ET.SubElement(member, "another-private-tag").text = "another-private-value"
        report = profile.observe_generated(synthetic_package(mutate=mutate))
        unknown = [shape for shape in report["members"][1]["metadata_shapes"] if shape["tag"] == "unrecognized"]
        self.assertEqual(unknown, [
            {"tag": "unrecognized", "child_count": 0, "attribute_count": 0, "has_text": True},
            {"tag": "unrecognized", "child_count": 1, "attribute_count": 1, "has_text": True}])
        self.assertNotIn("private", json.dumps(report))

    def test_finder_create_time_label_exposes_shape_without_timestamp_or_nested_names(self):
        def mutate(toc):
            created = ET.SubElement(toc.findall("file")[1], "FinderCreateTime")
            ET.SubElement(created, "time").text = "private-time-sentinel"
            ET.SubElement(created, "nanoseconds").text = "987654321"
        report = profile.observe_generated(synthetic_package(mutate=mutate))
        self.assertIn({"tag": "FinderCreateTime", "child_count": 2,
                       "attribute_count": 0, "has_text": False}, report["members"][1]["metadata_shapes"])
        for sentinel in ("private-time-sentinel", "987654321", '"time"', '"nanoseconds"'):
            self.assertNotIn(sentinel, json.dumps(report))
        self.assertTrue(report["observation_only"])

    def test_outer_zlib_member_is_observed(self):
        for outer in ["application/octet-stream", "application/x-gzip"]:
            for inner in ["none", "gzip", "zlib"]:
                with self.subTest(outer=outer, inner=inner):
                    report = profile.observe_generated(synthetic_package(encoding=outer, inner=inner))
                    self.assertTrue(all(member["encoding"] == outer for member in report["members"]))
                    self.assertEqual(report["scripts_inner_compression"], inner)

    def test_absent_mode_is_reported_as_unknown_without_inferred_permissions(self):
        for absent in [(0,), (1,), (0, 1)]:
            def mutation(toc):
                for index in absent:
                    file = toc.findall("file")[index]
                    file.remove(file.find("mode"))
            with self.subTest(absent=absent):
                report = profile.observe_generated(synthetic_package(mutate=mutation))
                modes = [member["mode"] for member in report["members"]]
                self.assertEqual(modes, [None if index in absent else "0644" for index in range(2)])
                self.assertTrue(report["observation_only"])
                encoded = json.dumps(report)
                self.assertEqual(encoded.count('"mode": null'), len(absent))
                self.assertNotIn('"mode": "0755"', encoded)

    def test_present_mode_keeps_strict_scalar_validation(self):
        for index in (0, 1):
            for malformed in ("duplicate", "nested", "empty", "unsafe", "oversized"):
                def mutation(toc):
                    file = toc.findall("file")[index]
                    mode = file.find("mode")
                    if malformed == "duplicate":
                        ET.SubElement(file, "mode").text = "0755"
                    elif malformed == "nested":
                        ET.SubElement(mode, "value").text = "0755"
                    else:
                        mode.text = {"empty": "", "unsafe": "mode with spaces", "oversized": "0" * 97}[malformed]
                with self.subTest(index=index, malformed=malformed), \
                     self.assertRaises(profile.component.ComponentError):
                    profile.observe_generated(synthetic_package(mutate=mutation))
        report = profile.observe_generated(synthetic_package())
        self.assertEqual([member["mode"] for member in report["members"]], ["0644", "0644"])

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
        self.assertEqual(report["schema_version"], 3)
        self.assertEqual(report["scripts_inventory"]["format"], "odc")
        self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])


def suite():
    result = unittest.defaultTestLoader.loadTestsFromTestCase(Portable)
    if sys.platform == "darwin":
        result.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(NativeMac))
    expected = 32 + (1 if sys.platform == "darwin" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"PKG profile discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
