"""Source fixtures for the inactive app owner; no native tools or installation."""
import ctypes
from contextlib import contextmanager, ExitStack
import errno
import gzip
import hashlib
import os
from pathlib import Path, PurePosixPath
import plistlib
import stat
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch

import macos_app_pkg as author
import macos_app_pkg_codec as codec
import macos_cpu_component as component
from test_macos_cpu_pkg_profile import odc_archive, odc_record
from test_macos_cpu_pkg import encode, source_stat_metadata
from test_macos_metal_pkg_entry import Portable as FullFixture
from test_macos_cpu_pkg_final import TEAM
import xml.etree.ElementTree as ET
import zlib


def digest(data):
    return hashlib.sha256(data).hexdigest()


def fixture():
    bodies = {"./Scribe.app/Contents/MacOS/Scribe": b"inert desktop bytes",
              "./Scribe.app/Contents/Resources/" + "/".join(("a" * 90, "b" * 90, "c" * 70)) + "/data": b"x" * 65537}
    expected = {".": (0o40755, "", 0)}
    for name, data in bodies.items():
        expected[name] = (0o100755 if name.endswith("/Scribe") else 0o100644, digest(data), len(data))
        for parent in PurePosixPath(name).parents:
            if parent.as_posix() != ".":
                expected["./" + parent.as_posix()] = (0o40755, "", 0)
    return expected, bodies


def archive(expected, bodies, *, mutate=None, padding=0):
    records = []
    for index, (name, (mode, _sha, _size)) in enumerate(expected.items()):
        args = dict(name=name.encode(), body=bodies.get(name, b""), mode=mode,
                    uid=0, gid=0, links=1 if mode & 0o170000 == 0o100000 else 3, ino=index + 1)
        if mutate:
            mutate(name, args)
        records.append(odc_record(**args))
    return odc_archive(*records, padding=padding)


def app_info(count, *, mutate=None):
    root = ET.Element("pkg-info", {"identifier": "com.scribe.local-transcriber.application", "version": "0.1.0",
        "install-location": "/Applications", "auth": "root", "format-version": "2", "overwrite-permissions": "true",
        "postinstall-action": "none", "relocatable": "false", "generator-version": "synthetic-source-fixture"})
    ET.SubElement(root, "payload", numberOfFiles=str(count), installKBytes="1")
    ET.SubElement(root, "bundle", id="com.scribe.local-transcriber", path="./Scribe.app",
                  CFBundleVersion="0.1.0", CFBundleShortVersionString="0.1.0")
    for tag in ("bundle-version", "upgrade-bundle", "update-bundle", "atomic-update-bundle", "strict-identifier", "relocate"):
        node = ET.SubElement(root, tag)
        if tag in ("upgrade-bundle", "strict-identifier"):
            ET.SubElement(node, "bundle", id="com.scribe.local-transcriber")
    if mutate:
        mutate(root)
    return ET.tostring(root)


def app_xar(expected, bodies, *, info=None, mutate=None, **kwargs):
    bom = b"synthetic owned Bom; system interpretation is mocked"
    def app_members(toc):
        node = next(node for node in toc.findall("file") if node.find("name").text == "Scripts")
        node.find("name").text = "Payload"
        if mutate:
            mutate(toc)
    return encode(info=app_info(len(expected)) if info is None else info, odc=archive(expected, bodies),
                  extra_members=(("Bom", zlib.compress(bom), bom, "application/x-gzip"),), mutate=app_members, **kwargs)


class CodecPortable(unittest.TestCase):
    def setUp(self):
        self.expected, self.bodies = fixture()

    def test_long_paths_and_file_bytes_stream_across_chunk_boundaries(self):
        data = archive(self.expected, self.bodies)
        self.assertTrue(any(len(name) > 256 for name in self.expected))
        for size in (1, 3, codec.CHUNK - 1, codec.CHUNK):
            codec._odc((data[index:index + size] for index in range(0, len(data), size)), self.expected)

    def test_regular_permissions_ownership_links_device_size_and_bytes_are_bound(self):
        target = next(iter(self.bodies))
        for field, value in (("mode", 0o100644), ("uid", 1), ("gid", 1), ("links", 2), ("rdev", 1),
                             ("body", b"changed bytes"), ("name", b"./Scribe.app/Contents/MacOS/scribe")):
            def change(name, args):
                if name == target:
                    args[field] = value
            with self.subTest(field=field), self.assertRaises(component.ComponentError):
                codec._odc([archive(self.expected, self.bodies, mutate=change)], self.expected)

    def test_directory_data_and_noncanonical_paths_reject(self):
        for field, value in (("mode", 0o40700), ("body", b"x"), ("links", 0),
                             ("name", b"./Scribe.app/../PrivateSentinel")):
            def change(name, args):
                if name == "./Scribe.app":
                    args[field] = value
            with self.assertRaises(component.ComponentError) as caught:
                codec._odc([archive(self.expected, self.bodies, mutate=change)], self.expected)
            self.assertNotIn("PrivateSentinel", str(caught.exception))

    def test_missing_duplicate_extra_and_reordered_nodes(self):
        original = archive(self.expected, self.bodies)
        extra = odc_record(b"./Scribe.app/PrivateSentinel", b"private", uid=0, gid=0)
        duplicate = odc_record(b".", b"", mode=0o40755, uid=0, gid=0, links=3)
        for data in (extra + original, duplicate + original,
                     odc_archive(odc_record(b".", b"", mode=0o40755, uid=0, gid=0, links=3))):
            with self.assertRaises(component.ComponentError):
                codec._odc([data], self.expected)
        # Order is not authority. Exact closed nodes/bytes remain mandatory.
        reverse = dict(reversed(list(self.expected.items())))
        codec._odc([archive(reverse, self.bodies)], self.expected)

    def test_payload_header_numeric_and_name_terminator_errors(self):
        data = archive(self.expected, self.bodies)
        for changed in (b"badbad" + data[6:], data[:6] + b"9" + data[7:], data[:76] + b"X" + data[77:],
                        data[:10], data[:-1]):
            with self.assertRaises(component.ComponentError):
                codec._odc([changed], self.expected)

    def test_complete_compression_and_padding_are_consumed(self):
        data = archive(self.expected, self.bodies)
        codec._odc(codec.streams._inflate([gzip.compress(data)], codec._payload_bound(self.expected), 31), self.expected)
        for changed in (gzip.compress(data)[:-1], gzip.compress(data) + b"tail",
                        gzip.compress(data) + gzip.compress(b"\0"), gzip.compress(data + b"nonzero")):
            with self.assertRaises(component.ComponentError):
                codec._odc(codec.streams._inflate([changed], codec._payload_bound(self.expected), 31), self.expected)
        codec._odc([archive(self.expected, self.bodies, padding=65536)], self.expected)
        with self.assertRaises(component.ComponentError):
            codec._odc([archive(self.expected, self.bodies, padding=65537)], self.expected)

    def test_large_aggregate_bounds_use_arithmetic_not_large_allocations(self):
        expected = {".": (0o40755, "", 0), "./Scribe.app": (0o40755, "", 0),
                    "./Scribe.app/one": (0o100755, "0" * 64, 2 * 1024**3),
                    "./Scribe.app/two": (0o100755, "1" * 64, 2 * 1024**3)}
        self.assertGreater(codec._payload_bound(expected), 2**32)
        self.assertGreater(codec._package_bound(expected), codec._payload_bound(expected))
        self.assertLess(codec._bom_bound(expected), 1024**2)
        self.assertLess(codec._listing_bound(expected), 1024)

    def test_expected_inventory_rejects_wrong_roots_aliases_types_and_boolean_sizes(self):
        for mutation in ("no-root", "no-app", "alias", "dot", "traversal", "bad-type", "bool-size", "missing-parent"):
            expected = dict(self.expected)
            if mutation == "no-root":
                del expected["."]
            elif mutation == "no-app":
                del expected["./Scribe.app"]
            elif mutation in ("alias", "dot", "traversal"):
                expected[{"alias": "./Scribe.appx/data", "dot": "./Scribe.app/./data",
                          "traversal": "./Scribe.app/../PrivateSentinel"}[mutation]] = (0o100644, "0" * 64, 1)
            elif mutation == "missing-parent":
                expected["./Scribe.app/missing/data"] = (0o100644, "0" * 64, 1)
            else:
                expected[next(iter(self.bodies))] = (0o120755 if mutation == "bad-type" else 0o100755,
                                                    "0" * 64, True if mutation == "bool-size" else 1)
            with self.subTest(mutation=mutation), self.assertRaises(component.ComponentError):
                codec._payload_bound(expected)

    def listing(self):
        return b"".join((f"{name}\t{mode:o}\t0\t0\t" +
                         (f"{size}\t123" if name in self.bodies else "\t") + "\n").encode()
                        for name, (mode, _, size) in self.expected.items())

    def test_bom_inventory_typed_metadata_and_source_checksums_match(self):
        codec._bom_listing(self.listing(), self.expected, {name: 123 for name in self.bodies})
        root = Path.cwd() / "private-fixture"
        output = b"".join(b"123 " + str(len(data)).encode() + b" " +
                          os.fsencode(root / name.removeprefix("./")) + b"\n" for name, data in self.bodies.items())
        self.assertEqual(codec._source_checksums(output, root, self.expected), {name: 123 for name in self.bodies})

    def test_bom_omissions_aliases_fields_modes_owners_sizes_and_crc_reject(self):
        data = self.listing()
        for changed in (data.split(b"\n", 1)[1], data + b"extra\n", data + data, data[:-1],
                        data.replace(b"Scribe.app", b"scribe.app"), data.replace(b"40755", b"40700", 1),
                        data.replace(b"\t0\t0\t", b"\t1\t0\t", 1),
                        data.replace(b"\t123\n", b"\t124\n", 1),
                        data.replace(b"65537\t", b"65536\t", 1), data.replace(b"\t\t\n", b"\t0\t0\n", 1),
                        data.replace(b".\t", b"./PrivateSentinel\t", 1)):
            with self.assertRaises(component.ComponentError) as caught:
                codec._bom_listing(changed, self.expected, {name: 123 for name in self.bodies})
            self.assertNotIn("PrivateSentinel", str(caught.exception))
        with self.assertRaises(component.ComponentError):
            codec._bom_listing(data, self.expected, {})

    def test_source_checksum_unknown_duplicate_or_incomplete_rows_reject(self):
        root = Path.cwd() / "private-fixture"
        output = b"".join(b"123 " + str(len(data)).encode() + b" " +
                          os.fsencode(root / name.removeprefix("./")) + b"\n" for name, data in self.bodies.items())
        for changed in (output[:-1], output + output, b"1 1 /PrivateSentinel\n", output.replace(b"123 ", b"4294967296 ", 1),
                        output.replace(b"123 ", b"00123 ", 1), output.replace(b"65537 ", b"65536 ", 1)):
            with self.assertRaises(component.ComponentError) as caught:
                codec._source_checksums(changed, root, self.expected)
            self.assertNotIn("PrivateSentinel", str(caught.exception))


class PackageInspection(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scribe-app-codec-source-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.package = self.root / "app.pkg"
        self.expected, self.bodies = fixture()

    def inspect(self, data):
        self.package.write_bytes(data)
        bom = self.root / "inspected.bom"
        if bom.exists():
            bom.unlink()  # Only this test's previously extracted fixture.
        return codec._inspect_fresh(self.package, self.expected, "0.1.0", self.root)

    def reject(self, data):
        with self.assertRaises((component.ComponentError, OSError)) as caught:
            self.inspect(data)
        self.assertNotIn("PrivateSentinel", str(caught.exception))

    def test_complete_package_binds_independent_bom_readback_and_descriptor(self):
        bom, identity = self.inspect(app_xar(self.expected, self.bodies))
        self.assertEqual(bom, self.root / "inspected.bom")
        self.assertEqual(identity, (*component.hash_file(bom, codec._bom_bound(self.expected)),
                                    component.fingerprint(bom.lstat())))

    def test_bom_replaced_or_corrupted_before_first_readback_rejects(self):
        original = component.hash_file
        for replacement in (False, True):
            def changed(path, maximum):
                if path.name == "inspected.bom":
                    if replacement:
                        alternate = self.root / "alternate"
                        alternate.write_bytes(path.read_bytes())
                        os.replace(alternate, path)
                    else:
                        path.write_bytes(b"corrupted decoded Bom")
                return original(path, maximum)
            with patch.object(component, "hash_file", side_effect=changed):
                self.reject(app_xar(self.expected, self.bodies))

    def test_package_and_bom_reads_are_bounded(self):
        original, reads = component.open_regular, []
        class Bounded:
            def __init__(self, handle):
                self.handle = handle
            def __enter__(self):
                return self
            def __exit__(self, *args):
                return self.handle.__exit__(*args)
            def read(self, count=-1):
                reads.append(count)
                if not 0 <= count <= 65536:
                    raise AssertionError("unbounded owned package read")
                return self.handle.read(count)
            def __getattr__(self, name):
                return getattr(self.handle, name)
        def opened(*args):
            handle, identity = original(*args)
            return Bounded(handle), identity
        with patch.object(component, "open_regular", side_effect=opened):
            self.inspect(app_xar(self.expected, self.bodies))
        self.assertTrue(reads)

    def test_physical_truncations_trailing_bytes_and_header_limits_reject(self):
        value = app_xar(self.expected, self.bodies)
        for length in range(len(value)):
            self.reject(value[:length])
        for tail in (b"\0", b"x", value):
            self.reject(value + tail)
        header = list(struct.unpack(">4sHHQQI", value[:28]))
        for index, replacement in ((0, b"bad!"), (1, 27), (2, 2), (3, 0), (3, 65537),
                                   (4, 0), (4, 65537), (4, header[4] + 1), (5, 0), (5, 2)):
            changed = header.copy()
            changed[index] = replacement
            self.reject(struct.pack(">4sHHQQI", *changed) + value[28:])

    def test_toc_and_all_member_checksums_reject_independently(self):
        self.reject(app_xar(self.expected, self.bodies, heap_transform=lambda heap: b"x" * 20 + heap[20:]))
        for member in range(3):
            for tag in ("archived-checksum", "extracted-checksum"):
                def change(toc):
                    toc.findall("file")[member].find("data/" + tag).text = "0" * 40
                self.reject(app_xar(self.expected, self.bodies, mutate=change))

    def test_heap_overlap_gaps_outside_declared_lengths_and_encodings_reject(self):
        for member, tag, value in ((0, "offset", "19"), (0, "offset", "21"), (1, "offset", "0"),
                (2, "offset", "9999999"), (0, "length", "0"), (1, "length", "9999999"),
                (1, "size", "1"), (0, "size", "65537"), (2, "size", "9999999")):
            def change(toc):
                toc.findall("file")[member].find("data/" + tag).text = value
            self.reject(app_xar(self.expected, self.bodies, mutate=change))
        for member in range(3):
            self.reject(app_xar(self.expected, self.bodies, mutate=lambda toc:
                toc.findall("file")[member].find("data/encoding").set("style", "unsupported")))

    def test_member_inventory_scripts_signatures_links_duplicates_and_aliases_reject(self):
        mutations = [lambda toc: ET.SubElement(toc, "signature"),
            lambda toc: toc.remove(toc.findall("file")[2]),
            lambda toc: toc.append(ET.fromstring(ET.tostring(toc.findall("file")[0]))),
            lambda toc: setattr(toc.findall("file")[1].find("name"), "text", "Scripts"),
            lambda toc: setattr(toc.findall("file")[1].find("name"), "text", "./Payload"),
            lambda toc: setattr(toc.findall("file")[1].find("type"), "text", "symlink"),
            lambda toc: ET.SubElement(toc.findall("file")[1], "PrivateSentinel"),
            lambda toc: ET.SubElement(toc.findall("file")[1], "name"),
            lambda toc: toc.findall("file")[1].set("id", "0"),
            lambda toc: ET.SubElement(toc.findall("file")[1].find("data"), "offset"),
            lambda toc: setattr(toc.findall("file")[1].find("mode"), "text", "0755")]
        for change in mutations:
            self.reject(app_xar(self.expected, self.bodies, mutate=change))

    def test_xar_xml_root_attributes_entities_and_toc_compression_reject(self):
        for transform in (lambda data: data.replace(b"<xar>", b'<xar extra="PrivateSentinel">'),
                lambda data: data.replace(b"<toc>", b'<toc extra="PrivateSentinel">'),
                lambda data: b'<!DOCTYPE xar [<!ENTITY x "PrivateSentinel">]>' + data,
                lambda data: data + b"trailing"):
            self.reject(app_xar(self.expected, self.bodies, toc_transform=transform))
        self.reject(app_xar(self.expected, self.bodies, compressed_scripts=b"not-gzip"))
        self.reject(app_xar(self.expected, self.bodies, compressed_scripts=gzip.compress(
            archive(self.expected, self.bodies)) + gzip.compress(b"\0")))

    def test_inert_native_stat_metadata_accepts_only_closed_shapes_and_bounds(self):
        def known(toc):
            for index in range(3):
                member = source_stat_metadata(toc, member_index=index)
                for tag, value in (("uid", "0"), ("gid", "0"), ("user", "root"), ("group", "wheel"),
                                   ("mtime", "2000-02-29T12:34:56"), ("atime", "2000-02-29T12:34:56"),
                                   ("ctime", "2000-02-29T12:34:56")):
                    ET.SubElement(member, tag).text = value
        self.inspect(app_xar(self.expected, self.bodies, mutate=known))
        for change in (lambda member: setattr(member.find("inode"), "text", str(2**63)),
                lambda member: setattr(member.find("deviceno"), "text", str(2**31)),
                lambda member: setattr(member.find("FinderCreateTime/nanoseconds"), "text", "1000000000"),
                lambda member: ET.SubElement(member.find("FinderCreateTime"), "time"),
                lambda member: member.find("FinderCreateTime/time").set("extra", "PrivateSentinel")):
            def mutated(toc):
                known(toc)
                change(toc.findall("file")[1])
            self.reject(app_xar(self.expected, self.bodies, mutate=mutated))

    def test_package_info_fixed_controls_and_optional_generator(self):
        codec._package_info(app_info(len(self.expected)), "0.1.0", self.expected)
        codec._package_info(app_info(len(self.expected), mutate=lambda root: root.attrib.pop("generator-version")),
                            "0.1.0", self.expected)
        codec._package_info(app_info(len(self.expected), mutate=lambda root: root.set("generator-version", "a" * 128)),
                            "0.1.0", self.expected)

    def test_package_info_identity_bundle_paths_controls_text_and_unknowns_reject(self):
        mutations = [lambda root: root.set("identifier", "PrivateSentinel"),
            lambda root: root.set("version", "0.2.0"), lambda root: root.set("install-location", "/"),
            lambda root: root.set("relocatable", "true"), lambda root: root.set("auth", "none"),
            lambda root: root.set("generator-version", "a" * 129), lambda root: root.set("extra", "PrivateSentinel"),
            lambda root: ET.SubElement(root, "scripts"), lambda root: root.remove(root.find("strict-identifier")),
            lambda root: setattr(root, "text", "PrivateSentinel"),
            lambda root: root.find("bundle").set("CFBundleVersion", "0.2.0"),
            lambda root: root.find("bundle").set("id", "PrivateSentinel"),
            lambda root: root.find("upgrade-bundle/bundle").set("id", "PrivateSentinel"),
            lambda root: ET.SubElement(root.find("relocate"), "bundle"),
            lambda root: root.find("bundle-version").set("extra", "PrivateSentinel")]
        mutations += [lambda root, path=path: root.find("bundle").set("path", path) for path in
                      ("Scribe.app", "/Applications/Scribe.app", "./scribe.app", "./Scribe.app/../PrivateSentinel")]
        for change in mutations:
            self.reject(app_xar(self.expected, self.bodies, info=app_info(len(self.expected), mutate=change)))

    def test_package_info_payload_count_and_space_hint_bounds_reject(self):
        for field, values in (("numberOfFiles", ("0", str(len(self.expected) - 1), str(len(self.expected) + 1), "01", "-1")),
                               ("installKBytes", ("0", "-1", "01", "9" * 20))):
            for value in values:
                self.reject(app_xar(self.expected, self.bodies, info=app_info(len(self.expected),
                    mutate=lambda root: root.find("payload").set(field, value))))


class Function:
    def __init__(self, implementation):
        self.implementation = implementation
    def __call__(self, *args):
        return self.implementation(*args)


class OwnerPortable(unittest.TestCase):
    def test_darwin_xattr_abi_requires_zero_and_never_allocates_or_ignores_errors(self):
        for count in (0, 1, 4096, 2**40, -1):
            calls = []
            class Library:
                pass
            library = Library()
            def queried(*args):
                calls.append(args)
                ctypes.set_errno(errno.ENOTSUP)
                return count
            library.flistxattr = Function(queried)
            with patch.object(author.ctypes, "CDLL", return_value=library):
                if count == 0:
                    author._empty_xattrs(7)
                else:
                    with self.assertRaisesRegex(component.ComponentError, "extended attributes present or unreadable"):
                        author._empty_xattrs(7)
            self.assertEqual(calls, [(7, None, 0, 0x20)])
            self.assertEqual(library.flistxattr.argtypes, [ctypes.c_int, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int])
            self.assertIs(library.flistxattr.restype, ctypes.c_ssize_t)
        with patch.object(author.ctypes, "CDLL", return_value=object()), self.assertRaisesRegex(
                component.ComponentError, "descriptor xattr API unavailable"):
            author._empty_xattrs(7)
        with patch.object(author.ctypes, "CDLL", side_effect=OSError("PrivateSentinel")), self.assertRaisesRegex(
                component.ComponentError, "descriptor xattr API unavailable") as caught:
            author._empty_xattrs(7)
        self.assertNotIn("PrivateSentinel", str(caught.exception))
        self.assertTrue(caught.exception.__suppress_context__)

    def test_default_publisher_denies_before_inputs_or_tools(self):
        with patch.object(component, "capture") as tool, patch.object(component, "private_workspace") as workspace, \
             patch.object(author.full, "release_inputs") as inputs, self.assertRaisesRegex(
                 component.ComponentError, "publisher is not provisioned"):
            author.build_component(None, None, None)
        for operation in (tool, workspace, inputs):
            operation.assert_not_called()

    def test_empty_acl_contract_checks_errors_and_releases_allocated_objects(self):
        for case in ("no-acl", "read-error", "empty", "invalid", "ace", "enumeration-error", "free-error"):
            calls = []
            class Library:
                pass
            library = Library()
            def get_acl(_fd, kind):
                self.assertEqual(kind, 0x100)
                ctypes.set_errno(errno.ENOENT if case == "no-acl" else errno.EPERM)
                return None if case in ("no-acl", "read-error") else 123
            def first(_acl, entry, _out):
                self.assertEqual(entry, 0)
                ctypes.set_errno(errno.EIO if case == "enumeration-error" else errno.EINVAL)
                return 0 if case == "ace" else -1
            def free(_acl):
                calls.append("free")
                return -1 if case == "free-error" else 0
            library.acl_get_fd_np = Function(get_acl)
            library.acl_valid = Function(lambda _acl: -1 if case == "invalid" else 0)
            library.acl_get_entry = Function(first)
            library.acl_free = Function(free)
            with patch.object(author.ctypes, "CDLL", return_value=library):
                if case in ("no-acl", "empty"):
                    author._empty_acl(7)
                else:
                    with self.assertRaises(component.ComponentError):
                        author._empty_acl(7)
            self.assertEqual(calls, [] if case in ("no-acl", "read-error") else ["free"])


class FullOwner(FullFixture):
    def setUp(self):
        super().setUp()
        info = self.app / "Contents/Info.plist"
        info.write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.scribe.local-transcriber",
            "CFBundleVersion": "0.1.0", "CFBundleShortVersionString": "0.1.0"}))
        self.policy = self.root / "publisher.json"
        self.policy.write_text('{"schema_version":1,"mode":"developer_id","publisher_team_id":"' + TEAM + '"}')
        self.operations, self.stage_expected, self.stage_bodies = [], None, None

    @contextmanager
    def context(self, **kwargs):
        with ExitStack() as stack:
            stack.enter_context(super().context(**kwargs))
            stack.enter_context(patch.object(author.signing, "POLICY_PATH", self.policy))
            if os.name != "posix":
                def metadata(path, mode, maximum):
                    info = path.lstat()
                    component.check_node(info, stat.S_ISDIR(mode), maximum)
                    return component.fingerprint(info)
                stack.enter_context(patch.object(author, "_metadata", side_effect=metadata))
            yield

    def execute(self, args, **kwargs):
        operation = kwargs["operation"]
        if operation == "metal-content-verification" and self.failure == "pack-" + args[-1]:
            super().execute(args, **kwargs)
            raise component.ToolExitError(operation, 1)
        if operation not in ("final-entry-signature", "app-source-checksums", "app-component-pkg-build", "app-bom-listing"):
            return super().execute(args, **kwargs)
        component.validate_tool_operation(operation)
        self.operations.append((operation, list(args)))
        if operation == "final-entry-signature":
            self.assertEqual(args[5], author.signing._requirement(TEAM, "application"))
            self.assertEqual(args[:5], ["/usr/bin/codesign", "--verify", "--strict", "--all-architectures", "--test-requirement"])
            if self.failure == "publisher":
                raise component.ToolExitError(operation, 1)
        elif operation == "app-source-checksums":
            self.assertEqual(args[0], "/usr/bin/cksum")
            return b"".join(b"123 " + str(Path(name).stat().st_size).encode() + b" " + os.fsencode(name) + b"\n"
                            for name in args[1:])
        elif operation == "app-component-pkg-build":
            self.assertEqual([arch for _, arch in self.verifications], list(author.full.ARCHES))
            root = Path(args[args.index("--root") + 1])
            controls = Path(args[args.index("--component-plist") + 1])
            self.assertEqual(plistlib.loads(controls.read_bytes()), author.CONTROLS)
            self.assertEqual(args, ["/usr/bin/pkgbuild", "--root", str(root), "--component-plist", str(controls),
                "--ownership", "recommended", "--compression", "legacy", "--identifier", author.IDENTIFIER,
                "--version", "0.1.0", "--install-location", "/Applications", args[-1]])
            files = author.full.input_files({pack["target_arch"]: (pack, release) for pack, release in zip(self.packs, self.releases)})
            bodies = {"./Scribe.app/" + name: (root / "Scribe.app" / name).read_bytes() for name in files}
            expected = {".": (0o40755, "", 0)}
            executables = {"Contents/MacOS/Scribe", "Contents/MacOS/scribe-inference-worker"}
            executables.update("Contents/Resources/" + pack["root"] + "/worker/scribe-inference-worker" for pack in self.packs)
            for name, body in bodies.items():
                relative = name.removeprefix("./Scribe.app/")
                expected[name] = (0o100755 if relative in executables else 0o100644, digest(body), len(body))
                for parent in PurePosixPath(name).parents:
                    if parent.as_posix() != ".":
                        expected["./" + parent.as_posix()] = (0o40755, "", 0)
            self.stage_expected, self.stage_bodies = expected, bodies
            if self.failure == "uncertain-pkg":
                raise component.CleanupUncertain("synthetic app construction uncertainty")
            Path(args[-1]).write_bytes(b"malformed" if self.failure == "bad-pkg" else app_xar(expected, bodies))
            if self.failure == "source-change":
                (self.app / "Contents/Info.plist").write_bytes(b"changed")
            if self.failure == "stage-change":
                (root / "Scribe.app/Contents/Info.plist").write_bytes(b"changed")
            if self.failure == "stage-extra":
                (root / "PrivateSentinel").write_bytes(b"unexpected")
            if self.failure == "controls-change":
                controls.write_bytes(b"changed controls")
            if self.failure == "policy-change":
                self.policy.write_bytes(b"changed publisher policy")
        elif operation == "app-bom-listing":
            self.assertEqual(args[:3], ["/usr/bin/lsbom", "-p", "fmugsc"])
            result = b"".join((f"{name}\t{mode:o}\t0\t0\t" +
                                (f"{size}\t123" if stat.S_ISREG(mode) else "\t") + "\n").encode()
                               for name, (mode, _, size) in self.stage_expected.items())
            if self.failure == "bad-bom":
                return result[:-1]
            if self.failure in ("bom-replace", "pkg-replace"):
                path = Path(args[-1]) if self.failure == "bom-replace" else Path(args[-1]).parent / "app.pkg"
                alternate = path.parent / "alternate"
                alternate.write_bytes(path.read_bytes())
                os.replace(alternate, path)
            return result
        return b""

    def build(self, **kwargs):
        self.verifications.clear()
        self.operations.clear()
        self.builds.clear()
        self.calls.clear()
        def publish(source, output):
            if self.failure == "collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic app output collision")
            source.rename(output)
            if self.failure == "publication-uncertain":
                raise OSError("synthetic post-publication uncertainty")
        with self.context(**kwargs), patch.object(component, "publish_no_replace", side_effect=publish):
            return author.build_component(self.app, self.workspace, self.frozen)

    def test_full_owner_admits_both_packs_and_publishes_only_unsigned_app_component(self):
        before = author.full.snapshot(self.app, author.full.input_files({
            pack["target_arch"]: (pack, release) for pack, release in zip(self.packs, self.releases)}))
        output, sha, size = self.build()
        self.assertEqual(output, self.workspace / "app.pkg")
        self.assertEqual(component.hash_file(output, codec._package_bound(self.stage_expected)), (sha, size))
        self.assertEqual(list(self.workspace.iterdir()), [output])
        self.assertEqual([operation for operation, _ in self.operations], ["final-entry-signature"] * 2 +
                         ["app-source-checksums", "app-component-pkg-build", "app-bom-listing"])
        self.assertEqual(author.full.snapshot(self.app, author.full.input_files({
            pack["target_arch"]: (pack, release) for pack, release in zip(self.packs, self.releases)})), before)

    def test_each_pack_failure_blocks_all_app_construction(self):
        for arch in author.full.ARCHES:
            self.failure = "pack-" + arch
            with self.assertRaises(component.ComponentError):
                self.build()
            self.assertFalse(any(operation == "app-component-pkg-build" for operation, _ in self.operations))
            self.assertFalse(list(self.workspace.iterdir()))

    def test_publisher_archive_and_listing_failures_publish_nothing(self):
        for failure in ("publisher", "bad-pkg", "bad-bom"):
            self.failure = failure
            with self.subTest(failure=failure), self.assertRaises(component.ComponentError):
                self.build()
            self.assertFalse(list(self.workspace.iterdir()))

    def test_stage_source_bom_and_package_replacements_reject(self):
        info = self.app / "Contents/Info.plist"
        original_info, original_policy = info.read_bytes(), self.policy.read_bytes()
        for failure in ("bom-replace", "pkg-replace", "stage-extra", "controls-change", "stage-change", "source-change", "policy-change"):
            self.failure = failure
            with self.subTest(failure=failure), self.assertRaises(component.ComponentError):
                self.build()
            self.assertTrue(any(operation == "app-component-pkg-build" for operation, _ in self.operations))
            self.assertFalse(list(self.workspace.iterdir()))
            info.write_bytes(original_info)
            self.policy.write_bytes(original_policy)

    def test_replaced_bom_and_package_block_listing_interpretation(self):
        for failure in ("bom-replace", "pkg-replace"):
            self.failure = failure
            with patch.object(codec, "_bom_listing") as parse, self.assertRaises(component.ComponentError):
                self.build()
            parse.assert_not_called()
            self.assertFalse(list(self.workspace.iterdir()))

    def test_bom_change_during_package_hash_is_not_a_new_baseline(self):
        original = component.hash_file
        changed = []
        def measured(path, maximum):
            if path.name == "app.pkg" and (path.parent / "inspected.bom").exists() and not changed:
                changed.append(True)
                (path.parent / "inspected.bom").write_bytes(b"changed between codec and lsbom")
            return original(path, maximum)
        with patch.object(component, "hash_file", side_effect=measured), self.assertRaises(component.ComponentError):
            self.build()
        self.assertEqual(changed, [True])
        self.assertFalse(any(operation == "app-bom-listing" for operation, _ in self.operations))
        self.assertFalse(list(self.workspace.iterdir()))

    def test_wrong_private_bom_path_rejects_before_listing(self):
        original = codec._inspect_fresh
        def inspect(*args):
            bom, identity = original(*args)
            return bom.parent / "other.bom", identity
        with patch.object(codec, "_inspect_fresh", side_effect=inspect), self.assertRaises(component.ComponentError):
            self.build()
        self.assertFalse(any(operation == "app-bom-listing" for operation, _ in self.operations))

    def test_candidate_change_after_codec_before_first_hash_blocks_listing(self):
        original = codec._inspect_fresh
        for replacement in (False, True):
            reached = []
            def inspect(package, *args):
                result = original(package, *args)
                reached.append(True)
                if replacement:
                    alternate = package.parent / "alternate-package"
                    alternate.write_bytes(package.read_bytes())
                    os.replace(alternate, package)
                else:
                    value = package.read_bytes()
                    package.write_bytes(bytes([value[0] ^ 1]) + value[1:])
                return result
            with patch.object(codec, "_inspect_fresh", side_effect=inspect), self.assertRaisesRegex(
                    component.ComponentError, "app package changed during inspection"):
                self.build()
            self.assertEqual(reached, [True])
            self.assertFalse(any(operation == "app-bom-listing" for operation, _ in self.operations))
            self.assertFalse(list(self.workspace.iterdir()))

    def test_admission_exit_precedes_publication_and_exit_failure_prevents_it(self):
        original = author.full.owned_release
        for fail in (True, False):
            completed = []
            @contextmanager
            def admitted(*args):
                with original(*args) as owned:
                    yield owned
                if fail:
                    raise component.ComponentError("synthetic admission exit failure")
                completed.append(True)
            def published(source, output):
                self.assertEqual(completed, [True])
                source.rename(output)
            self.verifications.clear()
            with self.context(), patch.object(author.full, "owned_release", side_effect=admitted), \
                 patch.object(component, "publish_no_replace", side_effect=published) as publish:
                if fail:
                    with self.assertRaises(component.ComponentError):
                        author.build_component(self.app, self.workspace, self.frozen)
                    publish.assert_not_called()
                else:
                    author.build_component(self.app, self.workspace, self.frozen)
                    publish.assert_called_once()
            if not fail:
                (self.workspace / "app.pkg").unlink()  # Only this mock-published fixture.

    def test_collision_and_ambiguous_publication_retain_existing_final(self):
        original = self.fake_private
        @contextmanager
        def cleanup(parent, prefix):
            with original(parent, prefix) as private:
                yield private
            if self.failure == "cleanup-after-publication" and prefix == ".scribe-app-pkg-":
                raise OSError("synthetic outer cleanup failure after publication")
        for failure in ("collision", "publication-uncertain", "cleanup-after-publication"):
            self.failure = failure
            output = self.workspace / "app.pkg"
            with patch.object(self, "fake_private", side_effect=cleanup), self.assertRaises(OSError):
                self.build()
            self.assertTrue(output.is_file())
            if failure == "collision":
                self.assertEqual(output.read_bytes(), b"retain concurrent output")
            output.unlink()  # Only this test's mock-published fixture.

    def test_post_admission_exit_mutations_fail_final_binding_before_publication(self):
        original = author.full.owned_release
        info = self.app / "Contents/Info.plist"
        original_info, original_policy = info.read_bytes(), self.policy.read_bytes()
        for target, message in (("source", "full release changed during app packaging"),
                ("policy", "entry signing policy changed during finalization"),
                ("stage", "retained staging changed before publication"),
                ("package", "app package changed during inspection"),
                ("bom", "owned Bom changed during inspection")):
            reached = []
            @contextmanager
            def admitted(app, workspace, frozen):
                with original(app, workspace, frozen) as owned:
                    yield owned
                private = workspace.parent
                paths = {"source": info, "policy": self.policy,
                         "stage": private / "payload/Scribe.app/Contents/Info.plist",
                         "package": private / "app.pkg", "bom": private / "inspected.bom"}
                path = paths[target]
                value = path.read_bytes()
                path.write_bytes(bytes([value[0] ^ 1]) + value[1:])
                reached.append(target)
            self.verifications.clear()
            try:
                with self.context(), patch.object(author.full, "owned_release", side_effect=admitted), \
                     patch.object(component, "publish_no_replace") as publish, self.assertRaisesRegex(
                         component.ComponentError, message):
                    author.build_component(self.app, self.workspace, self.frozen)
                self.assertEqual(reached, [target])
                publish.assert_not_called()
                self.assertFalse(list(self.workspace.iterdir()))
            finally:
                info.write_bytes(original_info)
                self.policy.write_bytes(original_policy)


class PosixOwner(FullOwner):
    def test_uncertain_construction_retains_both_private_scopes(self):
        self.workspace.chmod(0o700)
        self.failure = "uncertain-pkg"
        with self.assertRaises(component.CleanupUncertain):
            self.build(real_workspace=True)
        outer = list(self.workspace.glob(".scribe-app-pkg-*"))
        self.assertEqual(len(outer), 1)
        self.assertTrue((outer[0] / "payload/Scribe.app").is_dir())
        inner = list((outer[0] / "admission").glob(".scribe-full-release-entry-*"))
        self.assertEqual(len(inner), 1)
        self.assertTrue((inner[0] / "app").is_dir())
        self.assertFalse((self.workspace / "app.pkg").exists())

    def test_staged_metadata_rejects_modes_links_xattrs_and_descriptor_changes(self):
        path = self.root / "metadata-fixture"
        path.write_bytes(b"fixture")
        path.chmod(0o644)
        baseline = author._metadata(path, 0o100644, 7)
        self.assertTrue(baseline)
        path.chmod(0o600)
        with self.assertRaises(component.ComponentError):
            author._metadata(path, 0o100644, 7)
        path.chmod(0o644)
        alias = self.root / "hardlink-fixture"
        os.link(path, alias)
        with self.assertRaises(component.ComponentError):
            author._metadata(path, 0o100644, 7)
        alias.unlink()
        alias.symlink_to(path)
        with self.assertRaises(component.ComponentError):
            author._metadata(alias, 0o100644, 7)
        attribute_gate = (patch.object(author, "_empty_xattrs", side_effect=component.ComponentError("synthetic attribute"))
                          if component.NATIVE_DARWIN else patch.object(os, "listxattr", return_value=["PrivateSentinel"]))
        with attribute_gate, self.assertRaises(component.ComponentError):
            author._metadata(path, 0o100644, 7)
        original = os.fstat
        def replaced(descriptor):
            alternate = self.root / "alternate-metadata"
            alternate.write_bytes(path.read_bytes())
            alternate.chmod(0o644)
            os.replace(alternate, path)
            return original(descriptor)
        with patch.object(os, "fstat", side_effect=replaced), self.assertRaises(component.ComponentError):
            author._metadata(path, 0o100644, 7)


class NativeMetadata(unittest.TestCase):
    def test_native_descriptor_gate_rejects_file_and_directory_xattrs_without_python_api(self):
        # Only our own unprivileged temporary nodes; never a signed app, a
        # protected path or attribute removal. The whole fixture self-cleans.
        library = ctypes.CDLL(None, use_errno=True)
        library.fsetxattr.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_void_p,
                                     ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
        library.fsetxattr.restype = ctypes.c_int
        with tempfile.TemporaryDirectory(prefix="scribe-native-xattr-source-") as temporary:
            root = Path(temporary)
            for directory in (False, True):
                path = root / ("directory" if directory else "file")
                if directory:
                    path.mkdir(mode=0o755)
                    path.chmod(0o755)
                else:
                    path.write_bytes(b"fixture")
                    path.chmod(0o644)
                mode = 0o40755 if directory else 0o100644
                with patch.object(os, "listxattr", create=True) as python_api:
                    author._metadata(path, mode, 7)
                    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | (os.O_DIRECTORY if directory else 0))
                    try:
                        value = ctypes.create_string_buffer(b"fixture")
                        self.assertEqual(library.fsetxattr(descriptor, b"com.scribe.source-test.xattr", value, 7, 0, 0x2), 0)
                        with self.assertRaisesRegex(component.ComponentError, "extended attributes present or unreadable"):
                            author._empty_xattrs(descriptor)
                    finally:
                        os.close(descriptor)
                    with self.assertRaisesRegex(component.ComponentError, "extended attributes present or unreadable"):
                        author._metadata(path, mode, 7)
                    python_api.assert_not_called()
                # Also exercise the real Python surface after restoring it:
                # official Mac Python currently has no os.listxattr attribute.
                # A has-API guard must not skip the populated-node rejection.
                with self.assertRaisesRegex(component.ComponentError, "extended attributes present or unreadable"):
                    author._metadata(path, mode, 7)


def suite():
    result = unittest.TestSuite()
    classes = (CodecPortable, PackageInspection, OwnerPortable, FullOwner)
    if os.name == "posix":
        classes += (PosixOwner,)
    if sys.platform == "darwin":
        classes += (NativeMetadata,)
    for cls in classes:
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 37 + (2 if os.name == "posix" else 0) + (1 if sys.platform == "darwin" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"App package source discovery mismatch: expected {expected}")
    return result


if __name__ == "__main__":
    result = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if result.wasSuccessful() else 1)
