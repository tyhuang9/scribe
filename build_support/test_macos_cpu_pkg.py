"""Synthetic author/codec tests: never native acceptance, signing or installation."""
from contextlib import ExitStack, contextmanager
import gzip
import hashlib
import os
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zlib

import macos_cpu_component as component
import macos_cpu_pkg as pkg

VERSION = "0.1.0"
REVISION = "cpu-component-source-fixture"
WORKER = b"synthetic CPU worker"
DIGEST = hashlib.sha256(WORKER).hexdigest()
ENTRY = b"synthetic universal installer entry; never execute"


def sha1(data):
    return hashlib.sha1(data, usedforsecurity=False).hexdigest()


def record(name=b"./postinstall", body=ENTRY, mode=0o100755, links=1):
    """Independent literal POSIX ODC encoder, without production parser constants."""
    values = ((1, 6), (2, 6), (mode, 6), (3, 6), (4, 6), (links, 6), (0, 6),
              (5, 11), (len(name) + 1, 6), (len(body), 11))
    header = b"070707" + b"".join(f"{value:0{width}o}".encode() for value, width in values)
    assert len(header) == 76
    return header + name + b"\0" + body


def archive(body=ENTRY, *, root=True, padding=0):
    return ((record(b".", b"", 0o040700, 3) if root else b"") + record(body=body)
            + record(b"TRAILER!!!", b"", 0) + b"\0" * padding)


def package_info():
    return (f'<pkg-info identifier="{pkg.IDENTIFIER}" version="{VERSION}" install-location="/">'
            '<scripts><postinstall file="./postinstall"/></scripts></pkg-info>').encode()


def encode(body=ENTRY, *, info=None, odc=None, compressed_scripts=None, mutate=None,
           toc_transform=None, heap_transform=None):
    """Independent XAR writer with actual compressed-TOC/member SHA1 checksums.

    This encodes a narrow SOURCE fixture, not claimed native PackageInfo output.
    Mutations recompute the TOC checksum so deeper negative cases are reachable.
    """
    info = package_info() if info is None else info
    odc = archive(body) if odc is None else odc
    scripts = gzip.compress(odc, mtime=0) if compressed_scripts is None else compressed_scripts
    items = [("PackageInfo", zlib.compress(info), info, "application/x-gzip"),
             ("Scripts", scripts, scripts, "application/octet-stream")]
    root = ET.Element("xar")
    toc = ET.SubElement(root, "toc")
    checksum = ET.SubElement(toc, "checksum", style="sha1")
    ET.SubElement(checksum, "offset").text = "0"
    ET.SubElement(checksum, "size").text = "20"
    ET.SubElement(toc, "creation-time").text = "2000-01-01T00:00:00"
    heap, offset = [], 20
    for index, (name, stored, decoded, encoding) in enumerate(items):
        node = ET.SubElement(toc, "file", id=str(index))
        ET.SubElement(node, "name").text = name
        ET.SubElement(node, "type").text = "file"
        if name == "Scripts":
            ET.SubElement(node, "mode").text = "0644"
        data = ET.SubElement(node, "data")
        for tag, value in [("offset", offset), ("length", len(stored)), ("size", len(decoded))]:
            ET.SubElement(data, tag).text = str(value)
        ET.SubElement(data, "encoding", style=encoding)
        ET.SubElement(data, "archived-checksum", style="sha1").text = sha1(stored)
        ET.SubElement(data, "extracted-checksum", style="sha1").text = sha1(decoded)
        heap.append(stored)
        offset += len(stored)
    if mutate:
        mutate(toc)
    xml = ET.tostring(root, encoding="utf-8")
    if toc_transform:
        xml = toc_transform(xml)
    compressed = zlib.compress(xml)
    contents = bytes.fromhex(sha1(compressed)) + b"".join(heap)
    if heap_transform:
        contents = heap_transform(contents)
    return struct.pack(">4sHHQQI", b"xar!", 28, 1, len(compressed), len(xml), 1) + compressed + contents


def source_stat_metadata(toc, *, member_index=1, fields=("inode", "deviceno", "FinderCreateTime"),
                         inode="13", deviceno="17", timestamp="2000-02-29T12:34:56", nanoseconds="42"):
    """Independent documented XAR stat fixture, never actual filesystem values."""
    member = toc.findall("file")[member_index]
    for tag, value in [("inode", inode), ("deviceno", deviceno)]:
        if tag in fields:
            ET.SubElement(member, tag).text = value
    if "FinderCreateTime" in fields:
        created = ET.SubElement(member, "FinderCreateTime")
        ET.SubElement(created, "time").text = timestamp
        ET.SubElement(created, "nanoseconds").text = nanoseconds
    return member


class Inspection(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-pkg-source-")
        self.addCleanup(self.temporary.cleanup)
        self.path = Path(self.temporary.name) / "fresh.pkg"

    def inspect(self, data, body=ENTRY):
        self.path.write_bytes(data)
        pkg._inspect_fresh(self.path, hashlib.sha256(body).hexdigest(), len(body), VERSION)

    def reject(self, data, body=ENTRY):
        with self.assertRaises((component.ComponentError, OSError)):
            self.inspect(data, body)

    def test_literal_odc_layout_and_real_toc_checksum(self):
        literal = (b"070707000001000002100755000003000004000001000000"
                   b"0000000000500001600000000003")
        self.assertEqual(len(literal), 76)
        self.assertEqual(record(body=b"abc"), literal + b"./postinstall\0abc")
        value = encode()
        compressed_size = struct.unpack(">4sHHQQI", value[:28])[3]
        compressed = value[28:28 + compressed_size]
        self.assertEqual(value[28 + compressed_size:48 + compressed_size], bytes.fromhex(sha1(compressed)))
        self.assertNotEqual(sha1(compressed), sha1(zlib.decompress(compressed)))
        self.inspect(value)

    def test_chunk_boundaries_optional_root_and_padding(self):
        for size in [1, 63, 65535, 65536, 65537, 131072, 131089]:
            body = b"a" * size
            for root, padding in [(False, 0), (True, 194), (True, 65536)]:
                with self.subTest(size=size, root=root, padding=padding):
                    self.inspect(encode(body, odc=archive(body, root=root, padding=padding)), body)

    def test_large_entry_uses_only_bounded_file_reads(self):
        body = b"abcdefgh" * (256 * 1024)
        self.path.write_bytes(encode(body))
        original = component.open_regular
        reads = []

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
                    raise AssertionError("unbounded package read")
                return self.handle.read(count)
            def __getattr__(self, name):
                return getattr(self.handle, name)

        def opened(*args):
            handle, identity = original(*args)
            return Bounded(handle), identity
        with patch.object(component, "open_regular", side_effect=opened):
            pkg._inspect_fresh(self.path, hashlib.sha256(body).hexdigest(), len(body), VERSION)
        self.assertTrue(reads)

    def test_every_physical_truncation_and_extra_byte_rejects(self):
        value = encode()
        for length in range(len(value)):
            with self.subTest(length=length):
                self.reject(value[:length])
        for tail in [b"\0", b"x", encode()]:
            self.reject(value + tail)

    def test_header_and_toc_bounds_reject(self):
        value = encode()
        header = list(struct.unpack(">4sHHQQI", value[:28]))
        for index, replacement in [(0, b"bad!"), (1, 27), (2, 2), (3, 0), (3, 65537),
                                   (4, 0), (4, 65537), (4, header[4] + 1), (5, 0), (5, 2)]:
            changed = header.copy()
            changed[index] = replacement
            self.reject(struct.pack(">4sHHQQI", *changed) + value[28:])

    def test_toc_and_independent_member_checksums_reject(self):
        self.reject(encode(heap_transform=lambda heap: b"x" * 20 + heap[20:]))
        for member in (0, 1):
            for tag in ["archived-checksum", "extracted-checksum"]:
                def mutate(toc, member=member, tag=tag):
                    toc.findall("file")[member].find("data/" + tag).text = "0" * 40
                self.reject(encode(mutate=mutate))
        self.reject(encode(heap_transform=lambda heap: heap[:21] + bytes([heap[21] ^ 1]) + heap[22:]))

    def test_heap_overlap_gap_outside_and_unreferenced_bytes_reject(self):
        for member, tag, value in [(0, "offset", "19"), (0, "offset", "21"),
                                   (1, "offset", "0"), (1, "offset", "9999999"),
                                   (0, "length", "0"), (1, "length", "9999999"),
                                   (1, "size", "1"), (0, "size", "65537")]:
            def mutate(toc, member=member, tag=tag, value=value):
                toc.findall("file")[member].find("data/" + tag).text = value
            self.reject(encode(mutate=mutate))
        self.reject(encode(mutate=lambda toc: setattr(toc.find("checksum/offset"), "text", "1")))

    def test_outer_inventory_types_modes_aliases_and_properties_reject(self):
        mutations = [lambda toc: ET.SubElement(toc, "signature"),
                     lambda toc: ET.SubElement(toc, "file"),
                     lambda toc: toc.findall("file")[0].set("href", "outside"),
                     lambda toc: toc.findall("file")[1].set("id", "0"),
                     lambda toc: ET.SubElement(toc.findall("file")[0], "link"),
                     lambda toc: ET.SubElement(toc.findall("file")[0], "name")]
        for tag, value in [("name", "Payload"), ("name", "../Scripts"), ("name", "scripts"),
                           ("type", "symlink"), ("type", "hardlink"), ("mode", "0755")]:
            mutations.append(lambda toc, tag=tag, value=value:
                             setattr(toc.findall("file")[1].find(tag), "text", value))
        for mutate in mutations:
            self.reject(encode(mutate=mutate))

    def test_member_metadata_diagnostic_separates_duplicates_and_unsupported_fields_without_values(self):
        for duplicate, unsupported in [(True, False), (False, True), (True, True)]:
            def mutate(toc):
                member = toc.findall("file")[1]
                if duplicate:
                    ET.SubElement(member, "mtime").text = "private-time-one"
                    ET.SubElement(member, "mtime").text = "private-time-two"
                if unsupported:
                    ET.SubElement(member, "future-stat-field").text = "987654321012345"
                    ET.SubElement(member, "private-tag-sentinel").text = "private-value-sentinel"
            with self.subTest(duplicate=duplicate, unsupported=unsupported), \
                 self.assertRaises(component.ComponentError) as result:
                self.inspect(encode(mutate=mutate))
            self.assertEqual(str(result.exception), "CPU component package: unsupported member metadata "
                             f"(member-index=1; duplicate-fields={int(duplicate)}; "
                             f"unsupported-fields={2 * int(unsupported)})")
            for sentinel in ("private", "987654321012345", "inode", "mtime"):
                self.assertNotIn(sentinel, str(result.exception) + repr(result.exception.args))

    def test_source_stat_fields_are_optional_scripts_only_and_do_not_bind_identity(self):
        for fields in [(), ("inode",), ("deviceno",), ("FinderCreateTime",),
                       ("inode", "deviceno", "FinderCreateTime")]:
            with self.subTest(fields=fields):
                self.inspect(encode(mutate=lambda toc: source_stat_metadata(toc, fields=fields)))
        for field in ("inode", "deviceno", "FinderCreateTime"):
            self.reject(encode(mutate=lambda toc: source_stat_metadata(toc, member_index=0, fields=(field,))))

    def test_serialized_stat_integers_use_signed_source_bounds_and_reject_noncanonical_text(self):
        for tag, low, high in [("inode", -(2**63), 2**63 - 1), ("deviceno", -(2**31), 2**31 - 1)]:
            for value in [str(low), "-1", "0", str(high)]:
                with self.subTest(tag=tag, value=value):
                    self.inspect(encode(mutate=lambda toc: source_stat_metadata(toc, fields=(tag,), **{tag: value})))
            for value in [str(low - 1), str(high + 1), "+1", "-0", "01", "-01", " 1", "1 ",
                          "1e1", "1.0", "", "\u0661", "9" * 129]:
                with self.subTest(tag=tag, value=value):
                    self.reject(encode(mutate=lambda toc: source_stat_metadata(toc, fields=(tag,), **{tag: value})))

    def test_finder_create_time_is_opaque_bounded_text_with_bounded_nanoseconds(self):
        for timestamp in ["x", "x" * 128, "2000-02-29T12:34:56", "2000-02-29T12:34:56Z",
                          "2000-02-29T12:34:56+01:30", "2000-02-29T12:34:56.123456789Z",
                          "1900-02-29T24:60:61", "private-time-sentinel", "../../postinstall",
                          "$(ignored-command)", "<ignored/>", " ",
                          "".join(chr(value) for value in range(32, 127))]:
            for nanoseconds in ["0", "999999999"]:
                with self.subTest(timestamp=timestamp, nanoseconds=nanoseconds):
                    self.inspect(encode(mutate=lambda toc: source_stat_metadata(
                        toc, fields=("FinderCreateTime",), timestamp=timestamp, nanoseconds=nanoseconds)))
        for timestamp in ["", "x" * 129, "\0", "\x01", "\t", "\n", "\r", "\x1f", "\x7f",
                          "\u0661", "\u00e9", "\u200b"]:
            with self.subTest(timestamp=timestamp):
                self.reject(encode(mutate=lambda toc: source_stat_metadata(toc, timestamp=timestamp)))
        for nanoseconds in ["-1", "+1", "01", "1000000000", "1.0", "", "\u0661"]:
            self.reject(encode(mutate=lambda toc: source_stat_metadata(toc, nanoseconds=nanoseconds)))

    def test_source_stat_metadata_tree_ambiguities_and_unknown_same_shape_reject(self):
        def remove(toc, path):
            parent, name = path.rsplit("/", 1)
            node = toc.findall("file")[1].find(parent)
            node.remove(node.find(name))
        mutations = [lambda toc: setattr(toc.findall("file")[1].find("FinderCreateTime"), "tag", "OtherTime"),
                     lambda toc: remove(toc, "FinderCreateTime/time"),
                     lambda toc: remove(toc, "FinderCreateTime/nanoseconds"),
                     lambda toc: ET.SubElement(toc.findall("file")[1].find("FinderCreateTime"), "extra")]
        for path in ["inode", "deviceno", "FinderCreateTime", "FinderCreateTime/time",
                     "FinderCreateTime/nanoseconds"]:
            mutations.extend([
                lambda toc, path=path: toc.findall("file")[1].find(path).set("private", "private-attribute"),
                lambda toc, path=path: ET.SubElement(toc.findall("file")[1].find(path), "private-child"),
                lambda toc, path=path: setattr(toc.findall("file")[1].find(path), "tail", "private-tail"),
            ])
        for path in ["inode", "deviceno", "FinderCreateTime"]:
            mutations.append(lambda toc, path=path: ET.SubElement(toc.findall("file")[1], path))
        for path in ["time", "nanoseconds"]:
            mutations.append(lambda toc, path=path: ET.SubElement(
                toc.findall("file")[1].find("FinderCreateTime"), path))
        mutations.append(lambda toc: setattr(toc.findall("file")[1].find("FinderCreateTime"),
                                             "text", "private-container-text"))
        for index, change in enumerate(mutations):
            def mutate(toc):
                source_stat_metadata(toc)
                change(toc)
            with self.subTest(case=index):
                self.reject(encode(mutate=mutate))

    def test_source_stat_metadata_never_weakens_retained_entry_or_archive_checks(self):
        def opaque_metadata(toc):
            return source_stat_metadata(toc, timestamp="ignored-not-a-date")
        self.reject(encode(b"x" * len(ENTRY), mutate=opaque_metadata))
        for change in [lambda member: setattr(member.find("type"), "text", "hardlink"),
                       lambda member: member.find("type").set("link", "original"),
                       lambda member: ET.SubElement(member, "link"),
                       lambda member: setattr(member.find("data/archived-checksum"), "text", "0" * 40)]:
            def mutate(toc):
                change(opaque_metadata(toc))
            self.reject(encode(mutate=mutate))

    def test_source_stat_metadata_errors_omit_all_scalar_values(self):
        for field in ["inode", "deviceno", "time", "nanoseconds"]:
            def mutate(toc):
                member = source_stat_metadata(toc)
                path = "FinderCreateTime/" + field if field in ("time", "nanoseconds") else field
                member.find(path).text = "private-value-sentinel" * (7 if field == "time" else 1)
            with self.subTest(field=field), self.assertRaises(component.ComponentError) as result:
                self.inspect(encode(mutate=mutate))
            self.assertNotIn("private-value-sentinel", str(result.exception) + repr(result.exception.args))

    def test_unsupported_xml_and_ambiguous_scalars_reject(self):
        for transform in [lambda xml: b'<!DOCTYPE xar [<!ENTITY x "y">]>' + xml,
                          lambda xml: xml.replace(b"<xar>", b'<xar xmlns="urn:other">'),
                          lambda xml: xml + b"\0", lambda xml: xml.decode().encode("utf-16")]:
            self.reject(encode(toc_transform=transform))
        for text in ["+20", "020", "-1", " 20", "1e1", "999999999999", ""]:
            self.reject(encode(mutate=lambda toc, text=text:
                               setattr(toc.find("checksum/offset"), "text", text)))

    def test_checksum_correct_wrong_entry_bytes_or_size_reject(self):
        self.reject(encode(b"x" * len(ENTRY)))
        self.reject(encode(ENTRY + b"x"))
        self.reject(encode(ENTRY[:-1]))

    def test_package_identity_and_operation_policy_reject(self):
        valid = package_info()
        for old, new in [(pkg.IDENTIFIER.encode(), b"other"), (VERSION.encode(), b"9.9.9"),
                         (b'location="/"', b'location="/tmp"'), (b"postinstall", b"preinstall"),
                         (b'file="./postinstall"', b'file="postinstall"'),
                         (b"</scripts>", b'<preinstall file="./preinstall"/></scripts>'),
                         (b"</pkg-info>", b"<bundle/></pkg-info>"),
                         (b"<scripts>", b'<scripts href="outside">'),
                         (b"</scripts>", b"</scripts><scripts/>"),
                         (b"<pkg-info ", b'<pkg-info unknown="value" ')]:
            self.reject(encode(info=valid.replace(old, new)))
        self.inspect(encode(info=valid.replace(b"</pkg-info>",
                                              b'<payload installKBytes="0" numberOfFiles="0"/></pkg-info>')))

    def test_known_extra_package_info_attributes_remain_rejected_with_closed_labels(self):
        for name, value in [("overwrite-permissions", "true"), ("relocatable", "false"),
                            ("postinstall-action", "none"), ("generator-version", "private-generator")]:
            root = ET.fromstring(package_info())
            root.set(name, value)
            with self.subTest(name=name):
                with self.assertRaisesRegex(component.ComponentError,
                                            f"known-extra={name}, unrecognized=0"):
                    self.inspect(encode(info=ET.tostring(root)))

    def test_package_info_error_redacts_unknown_names_namespaces_and_all_values(self):
        root = ET.fromstring(package_info())
        root.set("generator-version", "private-generator-value")
        root.set("private-field-name", "private-field-value")
        root.set("{urn:private-namespace}private-name", "private-namespaced-value")
        with self.assertRaises(component.ComponentError) as caught:
            self.inspect(encode(info=ET.tostring(root)))
        self.assertEqual(str(caught.exception), "CPU component package: unsupported PackageInfo attribute "
                         "(known-extra=generator-version, unrecognized=2)")
        self.assertNotIn("private", str(caught.exception))

    def test_odc_types_links_names_duplicates_and_order_reject(self):
        trailer = record(b"TRAILER!!!", b"", 0)
        for content in [record(mode=0o120755) + trailer, record(links=2) + trailer,
                        record(b"postinstall") + trailer, record(b"../postinstall") + trailer,
                        record(b"./POSTINSTALL") + trailer, record(b"./postinstall\0alias") + trailer,
                        record() + record() + trailer, record() + record(b".", b"", 0o040700, 3) + trailer,
                        record(b".", b"", 0o040755, 3) + record() + trailer,
                        record(b".", b"", 0o040700, 2) + record() + trailer,
                        record() + record(b"./other", b"") + trailer, trailer,
                        record() + record(b"TRAILER!!!", b"", 0, 2)]:
            self.reject(encode(odc=content))

    def test_odc_all_numeric_fields_and_record_padding_reject(self):
        original = archive(root=False)
        for offset in [6, 12, 18, 24, 30, 36, 42, 48, 59, 65]:
            for value in [b"8", b"-", b" ", b"\0"]:
                self.reject(encode(odc=original[:offset] + value + original[offset + 1:]))
        self.reject(encode(odc=record() + b"\0" + record(b"TRAILER!!!", b"", 0)))
        self.reject(encode(odc=archive() + b"x"))
        self.reject(encode(odc=archive(padding=65537)))

    def test_gzip_completion_crc_concatenation_and_tail_reject(self):
        compressed = gzip.compress(archive(), mtime=0)
        for value in [compressed[:-1], compressed[:-8], compressed + b"\0", compressed + b"x",
                      compressed + gzip.compress(b"", mtime=0),
                      compressed[:-8] + bytes([compressed[-8] ^ 1]) + compressed[-7:],
                      zlib.compress(archive()), archive()]:
            self.reject(encode(compressed_scripts=value))
        for length in [0, 1, 75, 76, 77, len(archive()) - 1]:
            self.reject(encode(odc=archive()[:length]))

    def test_inflate_exact_chunk_boundaries_limits_and_split_input(self):
        for size in [0, 65535, 65536, 65537, 131072]:
            value = b"x" * size
            compressed = zlib.compress(value)
            for step in [1, 7, 65536]:
                blocks = list(pkg._inflate((compressed[i:i + step] for i in range(0, len(compressed), step)), size, 15))
                self.assertEqual(b"".join(blocks), value)
                self.assertTrue(all(len(block) <= 65536 for block in blocks))
            if size:
                with self.assertRaises(component.ComponentError):
                    list(pkg._inflate([compressed], size - 1, 15))
        for value in [zlib.compress(b"x")[:-1], zlib.compress(b"x") + b"x"]:
            with self.assertRaises(component.ComponentError):
                list(pkg._inflate([value], 100, 15))

    def test_source_package_mutation_and_named_replacement_reject(self):
        original = pkg._package_info
        for replace in [False, True]:
            def changed(data, version):
                original(data, version)
                if replace:
                    alternate = self.path.with_suffix(".replacement")
                    alternate.write_bytes(self.path.read_bytes())
                    os.replace(alternate, self.path)
                else:
                    with self.path.open("ab") as handle:
                        handle.write(b"x")
            with patch.object(pkg, "_package_info", side_effect=changed):
                self.reject(encode())

    def test_per_entry_package_bound_keeps_worker_writer_limits_unchanged(self):
        for invalid in [True, 0, -1, pkg.entry.MAX_ENTRY + 1]:
            with self.assertRaises(component.ComponentError):
                pkg._package_bound(invalid)
        self.assertGreater(pkg._package_bound(pkg.entry.MAX_ENTRY), 1024 * 1024 * 1024)
        self.assertEqual(component.MAX_WORKER, 512 * 1024 * 1024)
        self.assertEqual(component.MAX_WRITER, 64 * 1024 * 1024)


class Author(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-pkg-author-source-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.app = self.root / "Scribe.app"
        for relative in component.CPU_APP_INPUTS:
            path = self.app / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(WORKER if relative == component.CPU_APP_INPUTS[0] else b"synthetic input")
        self.workspace = self.root / "author"
        self.workspace.mkdir()
        self.calls, self.entries, self.failure = [], [], None

    @contextmanager
    def fake_private(self, parent, prefix):
        with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as private:
            yield Path(private)

    def factory(self, app, scripts, digest, revision):
        self.assertEqual((app, digest, revision), (self.app, DIGEST, REVISION))
        operation = scripts / "postinstall"
        operation.write_bytes(ENTRY)
        operation.chmod(0o755)
        self.entries.append(operation)
        if self.failure == "entry-uncertain":
            raise component.CleanupUncertain("synthetic nested constructor uncertainty")
        if self.failure == "extra-operation":
            (scripts / "preinstall").write_bytes(b"unexpected")
        if self.failure == "entry-hardlink":
            os.link(operation, self.root / "entry-link")
        if self.failure == "entry-symlink":
            operation.unlink()
            operation.symlink_to(self.root / "entry-source")
        if self.failure == "nonexec":
            operation.chmod(0o644)
        return (scripts / "wrong" if self.failure == "entry-path" else operation,
                "0" * 64 if self.failure == "entry-hash" else hashlib.sha256(ENTRY).hexdigest(),
                len(ENTRY) + (self.failure == "entry-size"))

    def execute(self, arguments, **kwargs):
        self.calls.append((list(arguments), kwargs))
        self.assertEqual(arguments[0], "/usr/bin/pkgbuild")
        if self.failure in ["tool", "deadline", "uncertain"]:
            error = component.CleanupUncertain if self.failure == "uncertain" else component.ComponentError
            raise error("synthetic builder failure")
        if self.failure == "missing":
            return b""
        scripts = Path(arguments[arguments.index("--scripts") + 1])
        body = (scripts / "postinstall").read_bytes()
        Path(arguments[-1]).write_bytes(encode(b"x" * len(body) if self.failure == "wrong-body" else body,
                                              info=b"<wrong/>" if self.failure == "wrong-info" else None))
        if self.failure == "app-change":
            (self.app / component.CPU_APP_INPUTS[1]).write_bytes(b"changed")
        if self.failure in ["entry-change", "entry-replace"]:
            operation = scripts / "postinstall"
            if self.failure == "entry-replace":
                replacement = scripts / "replacement"
                replacement.write_bytes(body)
                os.replace(replacement, operation)
            else:
                operation.write_bytes(b"changed")
        return b""

    @contextmanager
    def context(self, *, real_workspace=False):
        with ExitStack() as stack:
            stack.enter_context(patch.object(pkg.sys, "platform", "darwin"))
            stack.enter_context(patch.object(pkg.os, "geteuid", return_value=getattr(os, "geteuid", lambda: 501)() or 501,
                                             create=True))
            stack.enter_context(patch.object(component, "verify_cpu_app", return_value=VERSION))
            stack.enter_context(patch.object(pkg.entry, "build_entry", side_effect=self.factory))
            stack.enter_context(patch.object(component, "capture", side_effect=self.execute))
            if not real_workspace:
                stack.enter_context(patch.object(component, "private_workspace", side_effect=self.fake_private))
            yield

    def build(self, *, real_workspace=False):
        def publish(candidate, output):
            if self.failure == "collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic collision")
            candidate.rename(output)
            if self.failure == "publish-uncertain":
                raise OSError("synthetic post-rename ambiguity")
        with self.context(real_workspace=real_workspace), patch.object(component, "publish_no_replace", side_effect=publish):
            return pkg.build_component(self.app, self.workspace, DIGEST, REVISION)

    def test_mocked_author_returns_only_presign_content_bound_package(self):
        output, digest, size = self.build()
        self.assertEqual(output, self.workspace / "cpu-worker.pkg")
        self.assertEqual((digest, size), (hashlib.sha256(output.read_bytes()).hexdigest(), output.stat().st_size))
        self.assertEqual(list(self.workspace.iterdir()), [output])
        self.assertEqual(len(self.calls), 1)
        arguments, options = self.calls[0]
        self.assertEqual(arguments, ["/usr/bin/pkgbuild", "--nopayload", "--scripts", str(self.entries[0].parent),
                                    "--identifier", pkg.IDENTIFIER, "--version", VERSION, "--install-location", "/",
                                    str(self.entries[0].parent.parent / "cpu-worker.pkg")])
        self.assertEqual((options["timeout"], options["limit"], options["error_limit"], options["operation"]),
                         (120, 8192, 8192, "cpu-component-pkg-build"))

    def test_foreign_host_and_root_reject_before_inputs_or_tools(self):
        for platform, uid in [("win32", 501), ("linux", 501), ("darwin", 0)]:
            with patch.object(pkg.sys, "platform", platform), patch.object(pkg.os, "geteuid", return_value=uid, create=True), \
                 patch.object(component, "capture") as capture, self.assertRaises(component.ComponentError):
                pkg.build_component(self.root / "absent", self.root / "absent", DIGEST, REVISION)
            capture.assert_not_called()

    def test_invalid_identity_and_workspace_reject_before_tools(self):
        with self.context():
            for workspace in [self.app, self.app / "Contents", Path("relative")]:
                with self.assertRaises(component.ComponentError):
                    pkg.build_component(self.app, workspace, DIGEST, REVISION)
            for digest, revision in [("bad", REVISION), (DIGEST, "short")]:
                with self.assertRaises(component.ComponentError):
                    pkg.build_component(self.app, self.workspace, digest, revision)
        self.assertEqual(self.calls, [])

    def test_existing_output_is_preserved_before_factory(self):
        output = self.workspace / "cpu-worker.pkg"
        output.write_bytes(b"retain")
        with self.assertRaises(component.ComponentError):
            self.build()
        self.assertEqual(output.read_bytes(), b"retain")
        self.assertEqual(self.entries, [])

    def test_frozen_worker_and_app_verification_fail_before_factory(self):
        with self.context(), patch.object(component, "verify_cpu_app", side_effect=component.ComponentError("reject")), \
             self.assertRaises(component.ComponentError):
            pkg.build_component(self.app, self.workspace, DIGEST, REVISION)
        (self.app / component.CPU_APP_INPUTS[0]).write_bytes(b"different")
        with self.assertRaises(component.ComponentError):
            self.build()
        self.assertEqual(self.entries, [])
        self.assertEqual(self.calls, [])

    def test_factory_path_hash_size_inventory_and_links_reject(self):
        for failure in ["entry-path", "entry-hash", "entry-size", "extra-operation", "entry-hardlink"]:
            with self.subTest(failure=failure):
                self.failure = failure
                with self.assertRaises((component.ComponentError, OSError)):
                    self.build()
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())
                self.assertFalse(list(self.workspace.glob(".scribe-cpu-pkg-*")))
        self.assertEqual(self.calls, [])

    def test_builder_errors_and_packaged_content_mismatch_do_not_publish(self):
        for failure in ["tool", "deadline", "missing", "wrong-body", "wrong-info", "uncertain", "entry-uncertain"]:
            with self.subTest(failure=failure):
                self.failure = failure
                with self.assertRaises((component.ComponentError, OSError)):
                    self.build()
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_app_entry_mutation_or_same_byte_replacement_reject(self):
        for failure in ["entry-change", "entry-replace", "app-change"]:
            with self.subTest(failure=failure):
                self.failure = failure
                with self.assertRaises(component.ComponentError):
                    self.build()
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())

    def test_publication_collision_and_postrename_ambiguity_preserve_final(self):
        for failure in ["collision", "publish-uncertain"]:
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace = Path(directory)
                self.failure = failure
                with self.assertRaises(OSError):
                    self.build()
                final = self.workspace / "cpu-worker.pkg"
                self.assertTrue(final.exists())
                self.assertEqual(final.read_bytes(), b"retain concurrent output" if failure == "collision" else encode())

    def test_ambient_compiler_loader_backend_and_signing_overrides_are_absent(self):
        overrides = {name: "untrusted" for name in ["DYLD_INSERT_LIBRARIES", "DYLD_LIBRARY_PATH", "RUSTFLAGS",
            "VK_LAYER_PATH", "VK_ICD_FILENAMES", "CUDA_PATH", "SCRIBE_MACOS_SIGNING_IDENTITY",
            "SCRIBE_GPU_PACK_RELEASE_AUTHORITY", "TMPDIR", "PATH"]}
        with patch.dict(os.environ, overrides):
            self.build()
        environment = self.calls[0][1]["environment"]
        self.assertEqual(set(environment), {"PATH", "LC_ALL", "LANG", "TMPDIR"})
        self.assertNotIn("untrusted", environment.values())
        self.assertEqual(environment["TMPDIR"], str(self.entries[0].parent.parent / "temp"))


class Posix(Author):
    def test_nonexecutable_or_symlink_entry_rejects_before_pkgbuild(self):
        (self.root / "entry-source").write_bytes(ENTRY)
        for failure in ["nonexec", "entry-symlink"]:
            self.failure = failure
            with self.assertRaises(component.ComponentError):
                self.build()
        self.assertEqual(self.calls, [])

    def test_uncertainty_from_builder_or_nested_factory_retains_owned_workspace(self):
        for failure in ["uncertain", "entry-uncertain"]:
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace = Path(directory)
                self.failure = failure
                with self.assertRaises(component.CleanupUncertain):
                    self.build(real_workspace=True)
                retained = list(self.workspace.glob(".scribe-cpu-pkg-*"))
                self.assertEqual(len(retained), 1)
                self.assertTrue((retained[0] / "scripts/postinstall").exists())
                self.assertFalse((self.workspace / "cpu-worker.pkg").exists())
                # Every child was mocked: no live or uncertain process exists.


def suite():
    result = unittest.TestSuite()
    classes = [Inspection, Author] + ([Posix] if os.name == "posix" else [])
    for cls in classes:
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 36 + (2 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"CPU component PKG discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
