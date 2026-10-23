"""Source fixtures plus one native, unprivileged, never-installed PKG observation."""
from contextlib import contextmanager
import gzip
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import traceback
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zlib

import macos_cpu_component as component
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


def synthetic_package(*, encoding="application/octet-stream", inner="gzip", mutate=None, scripts=None, info=None):
    """Synthetic unsigned XAR envelope containing independently encoded valid ODC."""
    info = info if info is not None else (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" '
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
        self.assertEqual(report["schema_version"], 8)
        self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])
        self.assertEqual(report["scripts_inventory"]["format"], "odc")
        self.assertEqual([member["name"] for member in report["members"]], ["PackageInfo", "Scripts"])
        self.assertEqual(report["postinstall_reference"], "./postinstall")
        self.assertNotIn("synthetic", json.dumps(report))
        self.assertNotIn("trusted", report)

    def observe_info(self, attributes):
        root = ET.Element("pkg-info", {"identifier": profile.IDENTIFIER,
                                      "version": profile.VERSION, "install-location": "/"})
        root.attrib.update(attributes)
        ET.SubElement(ET.SubElement(root, "scripts"), "postinstall", file="./postinstall")
        return profile.observe_generated(synthetic_package(info=ET.tostring(root)))

    def test_package_info_attributes_use_fixed_records_and_absence_classes(self):
        shape = self.observe_info({})["package_info_attributes"]
        self.assertEqual(set(shape), {"fields", "unknown_count"})
        expected = {"auth": "absent", "format-version": "absent", "generator-version": "absent",
                    "identifier": "redacted", "install-location": "redacted",
                    "overwrite-permissions": "absent", "postinstall-action": "absent",
                    "relocatable": "absent", "version": "redacted"}
        self.assertEqual(shape["fields"], [{"name": name, "value_class": expected[name]}
                                          for name in sorted(expected)])
        self.assertEqual(shape["unknown_count"], 0)

    def test_package_info_control_fields_use_key_specific_closed_value_classes(self):
        for name, literals in [("auth", ("root",)), ("format-version", ("2",)),
                               ("overwrite-permissions", ("true", "false")),
                               ("relocatable", ("true", "false")), ("postinstall-action", ("none",))]:
            for value in (*literals, "", "True", "private-control-value", "none", "2", "root"):
                with self.subTest(name=name, value=value):
                    fields = self.observe_info({name: value})["package_info_attributes"]["fields"]
                    observed = next(item["value_class"] for item in fields if item["name"] == name)
                    self.assertEqual(observed, "literal-" + value if value in literals else "other")

    def test_package_info_generator_shape_bounds_and_redacts_without_validation(self):
        for value in ["x", "x" * 128, " ", "$(private-command); /private/path", "", "x" * 129,
                      "private\nvalue", "private\tvalue", "private\x7fvalue", "private\u00e9value"]:
            with self.subTest(length=len(value)):
                shape = self.observe_info({"generator-version": value})["package_info_attributes"]
                observed = next(item["value_class"] for item in shape["fields"]
                                if item["name"] == "generator-version")
                expected = "bounded-printable" if 0 < len(value) <= 128 and all(32 <= ord(c) < 127 for c in value) else "other"
                self.assertEqual(observed, expected)
                self.assertNotIn("private", json.dumps(shape))

    def test_package_info_unknown_and_namespaced_names_only_increment_count(self):
        attrs = {"private-field-name": "private-field-value",
                 "{urn:private-namespace}private-name": "private-namespaced-value"}
        shape = self.observe_info(attrs)["package_info_attributes"]
        self.assertEqual(shape["unknown_count"], 2)
        self.assertEqual(len(shape["fields"]), 9)
        self.assertNotIn("private", json.dumps(shape))

    def test_package_info_shapes_are_deterministic_and_private_value_independent(self):
        first = {"generator-version": "private-one", "relocatable": "false",
                 "private-one": "private-alpha", "private-two": "private-beta"}
        second = {"private-four": "private-delta", "private-three": "private-gamma",
                  "relocatable": "false", "generator-version": "private-two"}
        self.assertEqual(self.observe_info(first)["package_info_attributes"],
                         self.observe_info(second)["package_info_attributes"])

    def test_package_info_report_size_is_fixed_and_oversized_input_still_rejects(self):
        shape = self.observe_info({f"private-field-{index}": "private-value" for index in range(200)})["package_info_attributes"]
        self.assertEqual(shape["unknown_count"], 200)
        self.assertEqual(len(shape["fields"]), 9)
        self.assertLess(len(json.dumps(shape)), 1024)
        self.assertNotIn("private", json.dumps(shape))
        with self.assertRaises(component.ComponentError):
            self.observe_info({"generator-version": "x" * 65536})

    def test_package_info_observation_keeps_identity_operation_and_duplicate_rejection(self):
        root = (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" '
                'install-location="/" relocatable="false"><scripts><postinstall file="./postinstall"/>'
                '</scripts></pkg-info>').encode()
        for before, after in [(b'relocatable="false"', b'relocatable="false" relocatable="true"'),
                              (profile.IDENTIFIER.encode(), b"private-wrong-identifier"),
                              (b'version="1.0.0"', b'version="9.0.0"'),
                              (b'location="/"', b'location="/private"'),
                              (b'file="./postinstall"', b'file="../private"'),
                              (b"</scripts>", b'<postinstall file="./postinstall"/></scripts>')]:
            with self.subTest(before=before):
                with self.assertRaises(component.ComponentError):
                    profile.observe_generated(synthetic_package(info=root.replace(before, after)))

    def test_package_info_children_preserve_every_direct_shape_and_order(self):
        labels = ("scripts", "payload", "bundle", "bundle-version", "upgrade-bundle", "update-bundle",
                  "atomic-update-bundle", "strict-identifier", "relocate")
        root = ET.Element("pkg-info")
        for label in labels:
            ET.SubElement(root, label)
        ET.SubElement(root, "bundle-version")  # duplicates must remain visible
        self.assertEqual(profile._package_info_child_shapes(root), [
            {"tag": label, "child_count": 0, "attribute_count": 0, "has_text": False, "has_tail": False}
            for label in (*labels, "bundle-version")])
        root[3].set("private-attribute", "private-value")
        root[3].text, root[3].tail = "private-text", "private-tail"
        ET.SubElement(root[3], "private-nested-name").text = "private-nested-text"
        shape = profile._package_info_child_shapes(root)
        self.assertEqual(shape[3], {"tag": "bundle-version", "child_count": 1, "attribute_count": 1,
                                   "has_text": True, "has_tail": True})
        self.assertEqual(len(shape), 10)  # no recursive subtree or duplicate suppression
        self.assertNotIn("private", json.dumps(shape))

    def test_package_info_children_redact_unknown_names_namespaces_and_values(self):
        def shapes(sentinel):
            root = ET.Element("pkg-info")
            for label in ("Bundle-version", "private-" + sentinel, "{urn:private}" + sentinel):
                child = ET.SubElement(root, label, **{"private-" + sentinel: sentinel})
                child.text, child.tail = sentinel, sentinel
                ET.SubElement(child, "private-" + sentinel)
            return profile._package_info_child_shapes(root)
        first, second = shapes("alpha"), shapes("beta")
        self.assertEqual(first, second)
        self.assertEqual(first, [{"tag": "unrecognized", "child_count": 1, "attribute_count": 1,
                                 "has_text": True, "has_tail": True}] * 3)
        self.assertNotIn("private", json.dumps(first))
        root = ET.fromstring('<pkg-info><bundle-version> \t\n </bundle-version> \n </pkg-info>')
        self.assertEqual(profile._package_info_child_shapes(root), [
            {"tag": "bundle-version", "child_count": 0, "attribute_count": 0,
             "has_text": False, "has_tail": False}])

    def test_package_info_children_full_report_is_observation_not_constructor_admission(self):
        root = ET.Element("pkg-info", {"identifier": profile.IDENTIFIER,
                                      "version": profile.VERSION, "install-location": "/"})
        ET.SubElement(root, "bundle-version")
        ET.SubElement(ET.SubElement(root, "scripts"), "postinstall", file="./postinstall")
        ET.SubElement(root, "private-field", **{"private-name": "private-value"}).text = "private-text"
        report = profile.observe_generated(synthetic_package(info=ET.tostring(root)))
        self.assertEqual(report["schema_version"], 8)
        self.assertTrue(report["observation_only"])
        self.assertEqual(report["package_info_children"], [
            {"tag": "bundle-version", "child_count": 0, "attribute_count": 0, "has_text": False, "has_tail": False},
            {"tag": "scripts", "child_count": 1, "attribute_count": 0, "has_text": False, "has_tail": False},
            {"tag": "unrecognized", "child_count": 0, "attribute_count": 1, "has_text": True, "has_tail": False}])
        self.assertNotIn("private", json.dumps(report))
        # Observation of an empty known tag must not silently change admission.
        import macos_cpu_pkg as constructor
        root.set("identifier", constructor.IDENTIFIER)
        root.remove(root.find("private-field"))  # otherwise this would mask accidental bundle admission
        with self.assertRaisesRegex(component.ComponentError, "construct \\(tag=bundle-version;"):
            constructor._package_info(ET.tostring(root), profile.VERSION)

    def test_package_info_children_share_existing_xml_and_output_bounds(self):
        root = ET.Element("pkg-info", {"identifier": profile.IDENTIFIER,
                                      "version": profile.VERSION, "install-location": "/"})
        ET.SubElement(ET.SubElement(root, "scripts"), "postinstall", file="./postinstall")
        for _ in range(63):
            ET.SubElement(root, "private-name")
        report = profile.observe_generated(synthetic_package(info=ET.tostring(root)))
        shapes = report["package_info_children"]
        self.assertEqual(len(shapes), 64)
        self.assertLess(len(json.dumps(shapes)), 9000)
        ET.SubElement(root, "bundle-version")
        with self.assertRaisesRegex(component.ComponentError, "XML tree exceeds its bound"):
            profile.observe_generated(synthetic_package(info=ET.tostring(root)))
        del root[-1]
        root[-1].text = "x" * 65536
        with self.assertRaises(component.ComponentError):
            profile.observe_generated(synthetic_package(info=ET.tostring(root)))

    def test_operation_shapes_classify_each_predicate_without_echoing_values(self):
        root = ET.fromstring('<pkg-info><scripts><postinstall file="./postinstall"/></scripts></pkg-info>')
        shape = profile._operation_shapes(root)
        self.assertEqual((shape["direct_scripts_count"], shape["global_scripts_count"],
                          shape["global_postinstall_count"]), (1, 1, 1))
        container = shape["containers"][0]
        self.assertTrue(container["is_direct"])
        operation = container["tree"]["children"][0]
        self.assertEqual((operation["tag"], operation["attribute_count"], operation["child_count"],
                          operation["has_text"], operation["has_tail"]), ("postinstall", 1, 0, False, False))
        self.assertEqual(operation["fields"][0], {"name": "file", "value_class": "exact-dot-postinstall"})

    def test_operation_shapes_preserve_ambiguity_nesting_and_closed_unknown_counts(self):
        root = ET.fromstring('<pkg-info><scripts private-name="private-value"><preinstall/>'
                            '<postinstall file="postinstall" type="binary" timeout="0" '
                            'component-id="private-id"><private-tag private-name="private-value">'
                            'private-text</private-tag></postinstall></scripts><private-container>'
                            '<scripts><postinstall file="private-file"/></scripts></private-container></pkg-info>')
        shape = profile._operation_shapes(root)
        self.assertEqual((shape["direct_scripts_count"], shape["global_scripts_count"],
                          shape["global_postinstall_count"]), (1, 2, 2))
        self.assertEqual([item["is_direct"] for item in shape["containers"]], [True, False])
        container = shape["containers"][0]["tree"]
        self.assertEqual(container["unknown_attribute_count"], 1)
        operation = container["children"][1]
        self.assertEqual(operation["child_count"], 1)
        self.assertEqual(operation["child_tags"], ["unrecognized"])
        self.assertEqual([item["value_class"] for item in operation["fields"]],
                         ["exact-basename", "literal-binary", "literal-0", "other"])
        self.assertNotIn("private", json.dumps(shape))

    def test_operation_value_classes_are_key_specific_and_bounded(self):
        for key in ("file", "type", "timeout", "component-id"):
            for value in ("", "./postinstall", "postinstall", "binary", "shell", "0", profile.IDENTIFIER,
                          "private-value", "x" * 4096):
                root = ET.Element("pkg-info")
                op = ET.SubElement(ET.SubElement(root, "scripts"), "postinstall", {key: value})
                op.text, op.tail = "private-text", "private-tail"
                shape = profile._operation_shapes(root)
                observed = shape["containers"][0]["tree"]["children"][0]
                self.assertTrue(observed["has_text"] and observed["has_tail"])
                fields = {item["name"]: item["value_class"] for item in observed["fields"]}
                expected = {"file": {"./postinstall": "exact-dot-postinstall", "postinstall": "exact-basename"},
                            "type": {"binary": "literal-binary", "shell": "literal-shell"},
                            "timeout": {"0": "literal-0"},
                            "component-id": {profile.IDENTIFIER: "exact-fixture-identifier"}}
                self.assertEqual(fields[key], expected[key].get(value, "other"))
                self.assertTrue(all(fields[name] == "absent" for name in fields if name != key))
                self.assertNotIn("private", json.dumps(shape))

    def test_compiled_fixture_bound_is_separate_from_metadata_and_text_bound(self):
        body = b"inert-compiled-fixture" * 5000
        archive = odc_archive(odc_record(body=body))
        report = profile.observe_generated(synthetic_package(scripts=archive), expected_fixture=body)
        self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])
        with self.assertRaises(component.ComponentError):
            profile.observe_generated(synthetic_package(scripts=archive))
        for expected in (b"", b"x" * (profile.MAX_COMPILED_FIXTURE + 1), bytearray(body)):
            with self.assertRaises(component.ComponentError):
                profile.observe_generated(synthetic_package(scripts=archive), expected_fixture=expected)
        with self.assertRaises(component.ComponentError):
            profile.observe_generated(synthetic_package(scripts=archive, info=b"x" * (profile.MAX_TOC + 1)),
                                      expected_fixture=body)
        for wrong in (body + b"x", body[:-1], b"y" * len(body), profile.FIXTURE):
            with self.assertRaises(component.ComponentError):
                profile.observe_generated(synthetic_package(scripts=odc_archive(odc_record(body=wrong))),
                                          expected_fixture=body)

    def test_operation_timeout_observes_only_bounded_canonical_postinstall_numbers(self):
        cases = [(None, "absent", None), ("0", "literal-0", None),
                 ("1", "bounded-positive-decimal", 1), ("600", "bounded-positive-decimal", 600),
                 ("86400", "bounded-positive-decimal", 86400)]
        cases += [(value, "other", None) for value in ("", "00", "01", "0600", "86401", "99999", "100000",
                  "-1", "+600", " 600", "600 ", "600\t", "600\n", "600.0", "6e2", "\u0666\u0660\u0660",
                  "\uff16\uff10\uff10", "private-timeout-sentinel", "9" * 65536)]
        for value, category, number in cases:
            with self.subTest(value=value[:24] if value else value):
                root = ET.fromstring('<pkg-info><scripts><postinstall file="./postinstall"/></scripts></pkg-info>')
                if value is not None:
                    root.find("scripts/postinstall").set("timeout", value)
                shape = profile._operation_shapes(root)
                field = shape["containers"][0]["tree"]["children"][0]["fields"][2]
                expected = {"name": "timeout", "value_class": category}
                if number is not None:
                    expected["numeric_value"] = number
                self.assertEqual(field, expected)
                self.assertNotIn("private", json.dumps(shape))
        self.assertEqual(profile.MAX_OBSERVED_OPERATION_TIMEOUT, 86400)
        info = (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" install-location="/">'
                '<scripts><postinstall file="./postinstall" timeout="600"/></scripts></pkg-info>').encode()
        report = profile.observe_generated(synthetic_package(info=info))
        self.assertEqual(report["package_info_operations"]["containers"][0]["tree"]["children"][0]["fields"][2],
                         {"name": "timeout", "value_class": "bounded-positive-decimal", "numeric_value": 600})
        root = ET.fromstring('<pkg-info xmlns:p="urn:private"><scripts timeout="600">'
                            '<postinstall p:timeout="600"/><preinstall timeout="600"/>'
                            '<postinstall timeout="7"/><postinstall timeout="600"/></scripts></pkg-info>')
        shape = profile._operation_shapes(root)
        self.assertNotIn("numeric_value", shape["containers"][0]["tree"]["fields"][2])
        fields = [item["fields"][2] for item in shape["containers"][0]["tree"]["children"]]
        self.assertEqual(fields[:2], [{"name": "timeout", "value_class": "absent"},
                                     {"name": "timeout", "value_class": "other"}])
        self.assertEqual([item["numeric_value"] for item in fields[2:]], [7, 600])
        self.assertEqual(shape["global_postinstall_count"], 3)
        self.assertNotIn("private", json.dumps(shape))

    def test_operation_shapes_keep_empty_duplicate_nested_and_unknown_names(self):
        for xml, counts in [('<pkg-info/>', (0, 0, 0)),
                            ('<pkg-info><scripts/></pkg-info>', (1, 1, 0)),
                            ('<pkg-info><scripts/><scripts/></pkg-info>', (2, 2, 0)),
                            ('<pkg-info><scripts><scripts><postinstall/></scripts></scripts></pkg-info>', (1, 2, 1)),
                            ('<pkg-info><scripts><postinstall/><postinstall/></scripts></pkg-info>', (1, 1, 2))]:
            shape = profile._operation_shapes(ET.fromstring(xml))
            self.assertEqual((shape["direct_scripts_count"], shape["global_scripts_count"],
                              shape["global_postinstall_count"]), counts)
        root = ET.fromstring('<pkg-info xmlns:p="urn:private"><scripts><postinstall p:file="private" '
                            'File="private" p:type="private"/></scripts></pkg-info>')
        shape = profile._operation_shapes(root)
        operation = shape["containers"][0]["tree"]["children"][0]
        self.assertEqual(operation["unknown_attribute_count"], 3)
        self.assertTrue(all(item["value_class"] == "absent" for item in operation["fields"]))
        self.assertNotIn("private", json.dumps(shape))
        # No recursive subtree duplication: bounded shallow container/operation
        # records even for deeply nested containers.
        root = profile.xml_tree(b"<pkg-info>" + b"<scripts>" * 7 + b"<preinstall/>" * 32
                                + b"</scripts>" * 7 + b"</pkg-info>")
        self.assertLess(len(json.dumps(profile._operation_shapes(root))), 24000)

    def test_operation_reference_failure_retains_only_closed_diagnostics(self):
        info = (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" install-location="/">'
                '<scripts><postinstall file="private-file" private-name="private-value"/></scripts></pkg-info>').encode()
        with self.assertRaises(component.ComponentError) as caught:
            profile.observe_generated(synthetic_package(info=info))
        self.assertIn('"global_postinstall_count":1', str(caught.exception))
        self.assertIn('"unknown_attribute_count":1', str(caught.exception))
        self.assertNotIn("private", str(caught.exception))

    def test_compiled_scripts_compression_and_overflow_do_not_widen_metadata_limits(self):
        largest_body = b"x" * profile.MAX_COMPILED_FIXTURE
        largest_archive = odc_archive(odc_record(body=largest_body))
        largest_archive += b"\0" * (profile.MAX_COMPILED_SCRIPTS - len(largest_archive))
        self.assertTrue(profile.observe_generated(synthetic_package(scripts=largest_archive),
                                                 expected_fixture=largest_body)["observation_only"])
        with self.assertRaises(component.ComponentError):
            profile.observe_generated(synthetic_package(scripts=largest_archive + b"\0"), expected_fixture=largest_body)
        valid_info = (f'<pkg-info identifier="{profile.IDENTIFIER}" version="{profile.VERSION}" install-location="/" '
                      'padding=""><scripts><postinstall file="./postinstall"/></scripts></pkg-info>').encode()
        valid_info = valid_info.replace(b'padding=""', b'padding="' + b"x" * (profile.MAX_TOC - len(valid_info)) + b'"')
        self.assertEqual(len(valid_info), profile.MAX_TOC)
        compiled_archive = odc_archive(odc_record(body=largest_body))
        self.assertTrue(profile.observe_generated(synthetic_package(info=valid_info, scripts=compiled_archive),
                                                 expected_fixture=largest_body)["observation_only"])
        with self.assertRaises(component.ComponentError):
            profile.observe_generated(synthetic_package(info=valid_info.replace(b'padding="', b'padding="x', 1),
                                                       scripts=compiled_archive),
                                      expected_fixture=largest_body)
        body = b"inert-compiled-fixture" * 5000
        for encoding in ("application/octet-stream", "application/x-gzip"):
            for inner in ("none", "gzip", "zlib"):
                archive = odc_archive(odc_record(body=body))
                report = profile.observe_generated(synthetic_package(scripts=archive, encoding=encoding, inner=inner),
                                                   expected_fixture=body)
                self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])
                for malformed in (archive + b"x", b"x" * (profile.MAX_COMPILED_SCRIPTS + 1)):
                    with self.assertRaises(component.ComponentError):
                        profile.observe_generated(synthetic_package(scripts=malformed, encoding=encoding, inner=inner),
                                                  expected_fixture=body)
                with self.assertRaises(component.ComponentError):
                    profile.observe_generated(synthetic_package(info=b"x" * (profile.MAX_TOC + 1),
                                                               encoding=encoding, inner=inner), expected_fixture=body)

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

    def creation_time_report(self, text, mutate=None):
        def change(toc):
            member = toc.findall("file")[1]
            created = ET.SubElement(member, "FinderCreateTime")
            time = ET.SubElement(created, "time")
            time.text = text
            ET.SubElement(created, "nanoseconds").text = "975318642"
            if mutate:
                mutate(member, created, time)
        return profile.observe_generated(synthetic_package(mutate=change))

    def test_creation_time_format_masks_characters_and_bounds_without_validating(self):
        cases = [
            ("2077-01-02T03:04:05", "dddd-dd-ddTdd:dd:dd"),
            ("2077-01-02T03:04:05Z", "dddd-dd-ddTdd:dd:ddZ"),
            ("2077-01-02T03:04:05.123456789+05:30", "dddd-dd-ddTdd:dd:dd.ddddddddd+dd:dd"),
            ("9999-99-99T99:99:99", "dddd-dd-ddTdd:dd:dd"),
            ("tz /_\n\t\r\u0661\uff11\u00e9\U0001f600", "?" * 12),
            ("7" * 64, "d" * 64),
            ("7" * 65, "d" * 64),
        ]
        for text, masked in cases:
            with self.subTest(text=text):
                report = self.creation_time_report(text)
                self.assertEqual(report["members"][1]["source_creation_time"],
                                 {"status": "observed", "masked": masked, "truncated": len(text) > 64})
                self.assertTrue(report["observation_only"])
                self.assertNotIn(text, json.dumps(report))
                self.assertNotIn("975318642", json.dumps(report))
        self.assertEqual(profile.observe_generated(synthetic_package())["members"][1]["source_creation_time"],
                         {"status": "absent"})

    def test_creation_time_format_ambiguous_or_nonscalar_never_selects_a_value(self):
        changes = [
            ("absent", lambda member, created, time: setattr(created, "tag", "OtherTime")),
            ("absent", lambda member, created, time: created.remove(time)),
            ("ambiguous", lambda member, created, time: ET.SubElement(member, "FinderCreateTime")),
            ("ambiguous", lambda member, created, time: ET.SubElement(created, "time")),
            ("non-scalar", lambda member, created, time: created.set("private-name", "private-value")),
            ("non-scalar", lambda member, created, time: time.set("private-name", "private-value")),
            ("non-scalar", lambda member, created, time: ET.SubElement(time, "private-child")),
            ("non-scalar", lambda member, created, time: setattr(time, "text", None)),
            ("non-scalar", lambda member, created, time: setattr(time, "text", "")),
            ("non-scalar", lambda member, created, time: setattr(time, "tail", "private-tail")),
            ("non-scalar", lambda member, created, time: setattr(created, "text", "private-text")),
            ("non-scalar", lambda member, created, time: setattr(created, "tail", "private-tail")),
        ]
        for index, (status, change) in enumerate(changes):
            with self.subTest(status=status, change=index):
                report = self.creation_time_report("private-time-sentinel", change)
                self.assertEqual(report["members"][1]["source_creation_time"], {"status": status})
                self.assertNotIn("private", json.dumps(report))

    def test_creation_time_format_full_report_omits_values_and_other_properties(self):
        def unrelated(member, created, time):
            other = ET.SubElement(member, "OtherTime", {"private-attribute": "private-value"})
            ET.SubElement(other, "time").text = "private-unrelated-time"
            ET.SubElement(created, "private-child").text = "private-unrelated-value"
            created.find("nanoseconds").set("private-attribute", "private-nsec-attribute")
        first = self.creation_time_report("2077-01-02T03:04:05Z", unrelated)
        second = self.creation_time_report("1888-09-08T07:06:05Z", unrelated)
        expected = {"status": "observed", "masked": "dddd-dd-ddTdd:dd:ddZ", "truncated": False}
        for report in (first, second):
            self.assertEqual(report["members"][1]["source_creation_time"], expected)
            self.assertEqual(report["members"][0]["source_creation_time"], {"status": "absent"})
            encoded = json.dumps(report)
            for value in ("2077", "1888", "975318642", "private", '"time"', '"nanoseconds"'):
                self.assertNotIn(value, encoded)
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

    def run_mocked_probe(self, workspace, execute, **kwargs):
        platform, uid, chmod, private, capture = self.mocked_probe(workspace, execute)
        with platform, uid, chmod, private, capture:
            return profile.probe(**kwargs)

    def compiled_probe_executor(self, workspace, calls, failure=None):
        body = b"inert-compiled-fixture" * 5000
        def execute(arguments, **kwargs):
            calls.append(arguments)
            self.assertEqual(kwargs["environment"], {**component.probe_environment(), "TMPDIR": str(workspace / "tmp")})
            self.assertEqual(kwargs["cwd"], workspace)
            self.assertLessEqual(kwargs["timeout"], 30)
            self.assertLessEqual(kwargs["error_limit"], 8192)
            self.assertTrue((workspace / "tmp").is_dir())
            if failure == "os-error":
                raise OSError("private-error-value", str(workspace / "private-file-sentinel"))
            if failure == len(calls):
                raise component.CleanupUncertain("owned process retirement uncertain")
            if arguments[0] == "/usr/bin/xcrun":
                architecture = ("arm64", "x86_64")[len(calls) - 1]
                self.assertEqual(arguments, ["/usr/bin/xcrun", "--sdk", "macosx", "clang", "-arch", architecture,
                                             "-mmacosx-version-min=13.0", str(workspace / "fixture.c"),
                                             "-o", str(workspace / architecture)])
                self.assertEqual((workspace / "fixture.c").read_bytes(), profile.COMPILED_SOURCE)
                if failure != "missing":
                    Path(arguments[-1]).write_bytes(b"thin")
                return b""
            if arguments[0] == "/usr/bin/lipo":
                if "-create" in arguments:
                    self.assertEqual(arguments, ["/usr/bin/lipo", "-create", str(workspace / "arm64"),
                                                 str(workspace / "x86_64"), "-output", str(workspace / "universal")])
                    Path(arguments[-1]).write_bytes(body if failure != "oversized" else b"x" * (profile.MAX_COMPILED_FIXTURE + 1))
                    return b""
                self.assertEqual(arguments, ["/usr/bin/lipo", str(workspace / "universal"), "-archs"])
                return b"arm64\n" if failure == "architecture" else b"x86_64 arm64\n"
            if arguments[0] == "/usr/bin/pkgbuild":
                self.assertEqual(sorted(item.name for item in (workspace / "scripts").iterdir()), ["postinstall"])
                self.assertEqual((workspace / "scripts/postinstall").read_bytes(), body)
                (workspace / "fixture.pkg").write_bytes(synthetic_package(scripts=odc_archive(odc_record(body=body))))
                return b""
            self.assertEqual(arguments, ["/usr/bin/xar", "-tf", str(workspace / "fixture.pkg")])
            return b"Scripts\nScripts\n" if failure == "listing" else b"PackageInfo\nScripts\n"
        return execute

    def test_compiled_probe_only_compiles_packages_lists_and_binds_retained_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            workspace, calls = Path(temporary), []
            report = self.run_mocked_probe(workspace, self.compiled_probe_executor(workspace, calls), native_binary=True)
            self.assertEqual(len(calls), 6)
            self.assertEqual(report["fixture_kind"], "universal-macho")
            self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])

    def test_compiled_probe_failures_stop_and_preserve_uncertain_cleanup(self):
        for failure in (1, 2, 3, 4, 5, 6, "missing", "os-error", "oversized", "architecture", "listing"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                workspace, calls = Path(temporary), []
                expected = component.CleanupUncertain if type(failure) is int else component.ComponentError
                with self.assertRaises(expected) as caught:
                    self.run_mocked_probe(workspace, self.compiled_probe_executor(workspace, calls, failure), native_binary=True)
                if type(failure) is int:
                    self.assertEqual(len(calls), failure)
                if failure in ("missing", "os-error"):
                    rendered = "".join(traceback.format_exception(caught.exception))
                    self.assertIn("fixed fixture filesystem operation failed", rendered)
                    for private in (str(workspace), "private-file-sentinel", "private-error-value"):
                        self.assertNotIn(private, rendered)
        if os.name == "posix":
            # Exercise the real descriptor-safe context, not the bare-yield
            # mock above. No child exists: uncertainty is injected by capture.
            uid = os.geteuid() or 501
            with tempfile.TemporaryDirectory() as temporary:
                owned = []
                def uncertain(_arguments, **kwargs):
                    owned.append(kwargs["cwd"])
                    raise component.CleanupUncertain("synthetic retirement uncertainty")
                with patch.object(profile.sys, "platform", "darwin"), \
                     patch.object(profile.os, "geteuid", return_value=uid), \
                     patch.object(profile.tempfile, "gettempdir", return_value=temporary), \
                     patch.object(profile.component, "capture", side_effect=uncertain), \
                     self.assertRaises(component.CleanupUncertain):
                    profile.probe(native_binary=True)
                self.assertEqual(len(owned), 1)
                self.assertTrue(owned[0].is_dir())
                self.assertEqual((owned[0] / "fixture.c").read_bytes(), profile.COMPILED_SOURCE)

    def test_pair_runs_both_fixed_probes_and_never_reports_partial_success(self):
        with patch.object(profile, "probe", side_effect=[{"fixture_kind": "inert-text"},
                                                        {"fixture_kind": "universal-macho"}]) as probe:
            report = profile.probe_pair()
            self.assertEqual(report["schema_version"], 8)
            self.assertEqual([item["fixture_kind"] for item in report["fixtures"]], ["inert-text", "universal-macho"])
            self.assertEqual(probe.call_args_list, [unittest.mock.call(), unittest.mock.call(native_binary=True)])
        with patch.object(profile, "probe", side_effect=[{}, component.CleanupUncertain("uncertain")]), \
             self.assertRaises(component.CleanupUncertain):
            profile.probe_pair()

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
        with patch.object(profile.sys, "platform", "darwin"), \
             patch.object(profile.os, "geteuid", return_value=0, create=True), \
             patch.object(profile.component, "private_workspace") as private, \
             patch.object(profile.component, "capture") as execute, \
             self.assertRaises(component.ComponentError):
            profile.probe(native_binary=True)
        private.assert_not_called()
        execute.assert_not_called()

    def test_main_accepts_no_external_archive_path_or_override(self):
        for arguments in [["probe", "candidate.pkg"], ["probe", "--root", "/"], ["probe", "--help"]]:
            with self.subTest(arguments=arguments), patch.object(profile.sys, "argv", arguments), \
                 patch.object(profile, "probe_pair") as probe, self.assertRaises(profile.component.ComponentError):
                profile.main()
            probe.assert_not_called()
        with patch.object(profile.sys, "argv", ["probe"]), \
             patch.object(profile, "probe_pair", return_value={"observation_only": True}), \
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
        self.assertEqual(report["schema_version"], 8)
        self.assertEqual(report["scripts_inventory"]["format"], "odc")
        self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])

    def test_actual_unsigned_universal_macho_format_without_execution(self):
        report = profile.probe(native_binary=True)
        print("SCRIBE_NATIVE_CPU_PKG_PROFILE=" + json.dumps(report, sort_keys=True), flush=True)
        self.assertEqual(report["schema_version"], 8)
        self.assertEqual(report["fixture_kind"], "universal-macho")
        self.assertTrue(report["observation_only"])
        self.assertTrue(report["scripts_inventory"]["fixture_exact_bytes_match"])


def suite():
    result = unittest.defaultTestLoader.loadTestsFromTestCase(Portable)
    if sys.platform == "darwin":
        result.addTests(unittest.defaultTestLoader.loadTestsFromTestCase(NativeMac))
    expected = 57 + (2 if sys.platform == "darwin" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"PKG profile discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
