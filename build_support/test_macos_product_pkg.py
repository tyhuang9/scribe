"""Owned-product source fixtures: never real signing, native build or installation."""
from contextlib import contextmanager
from dataclasses import replace
import hashlib
import os
from pathlib import Path
import plistlib
import struct
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET
import zlib

import macos_cpu_component as component
import macos_cpu_pkg as worker_codec
import macos_cpu_pkg_final as signing
import macos_full_release as full
import macos_product_pkg as author
import macos_product_pkg_codec as codec
from test_macos_app_pkg import FullOwner as AppFixture, app_xar, digest, fixture
from test_macos_cpu_pkg import encode, package_info
from test_macos_cpu_pkg_final import PROPERTIES, SELECTOR, TEAM
from test_macos_full_release_cpu_entry import IMAGE as CPU_IMAGE
from test_macos_metal_pkg import metal_info
from test_macos_metal_pkg_entry import IMAGE as METAL_IMAGE


def tree_xar(files, *, mutate=None, heap_transform=None, toc_transform=None):
    """Independent literal XAR tree/checksum writer, not a native fixture."""
    root = ET.Element("xar")
    toc = ET.SubElement(root, "toc")
    checksum = ET.SubElement(toc, "checksum", style="sha1")
    ET.SubElement(checksum, "offset").text = "0"
    ET.SubElement(checksum, "size").text = "20"
    ET.SubElement(toc, "creation-time").text = "2000-01-01T00:00:00"
    directories, pieces, offset = {}, [], 20
    for path, body in files.items():
        parent = toc
        if "/" in path:
            directory, leaf = path.split("/", 1)
            if directory not in directories:
                node = ET.SubElement(toc, "file")
                ET.SubElement(node, "name").text = directory
                ET.SubElement(node, "type").text = "directory"
                directories[directory] = node
            parent, name = directories[directory], leaf
        else:
            name = path
        node = ET.SubElement(parent, "file")
        ET.SubElement(node, "name").text = name
        if parent is not toc:
            ET.SubElement(node, "name").text = name
        ET.SubElement(node, "type").text = "file"
        record = ET.SubElement(node, "data")
        style = "application/octet-stream" if name in ("Scripts", "Payload") else "application/x-gzip"
        stored = body if style == "application/octet-stream" else zlib.compress(body)
        for tag, value in (("offset", offset), ("length", len(stored)), ("size", len(body))):
            ET.SubElement(record, tag).text = str(value)
        ET.SubElement(record, "encoding", style=style)
        for tag, value in (("archived-checksum", stored), ("extracted-checksum", body)):
            ET.SubElement(record, tag, style="sha1").text = hashlib.sha1(value, usedforsecurity=False).hexdigest()
        pieces.append(stored)
        offset += len(stored)
    if mutate:
        mutate(toc)
    xml = ET.tostring(root)
    if toc_transform:
        xml = toc_transform(xml)
    packed = zlib.compress(xml)
    heap = hashlib.sha1(packed, usedforsecurity=False).digest() + b"".join(pieces)
    if heap_transform:
        heap = heap_transform(heap)
    return struct.pack(">4sHHQQI", b"xar!", 28, 1, len(packed), len(xml), 1) + packed + heap


def component_bytes():
    expected, bodies = fixture()
    entries = {"cpu-worker.pkg": encode(CPU_IMAGE, info=package_info(payload=True, empty_controls=True)),
               "metal-worker.pkg": encode(METAL_IMAGE, info=metal_info(payload=True, empty_controls=True)),
               "app.pkg": app_xar(expected, bodies)}
    return entries


def decode_fixture(data):
    """Independent small SOURCE fixture decoder; production never calls it."""
    _magic, size, _version, packed, _expanded, _algorithm = struct.unpack(">4sHHQQI", data[:28])
    root = ET.fromstring(zlib.decompress(data[size:size + packed]))
    result = {}
    for node in root.findall("toc/file"):
        record = node.find("data")
        offset, length = int(record.find("offset").text), int(record.find("length").text)
        body = data[size + packed + offset:size + packed + offset + length]
        if record.find("encoding").get("style") == "application/x-gzip":
            body = zlib.decompress(body)
        result[node.find("name").text] = body
    return result


def generated_distribution(source, infos, *, additions=True):
    root = ET.fromstring(source)
    for node in root.findall("pkg-ref"):
        node.text = "#" + node.text
    if additions:
        for name in reversed(codec.NAMES):
            identifier, _space, bundles = infos[name]
            reference = ET.Element("pkg-ref", id=identifier)
            container = ET.SubElement(reference, "bundle-version")
            for bundle in bundles:
                container.append(ET.fromstring(bundle))
            root.insert(0, reference)
    return ET.tostring(root)


class Codec(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scribe-product-codec-source-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.records = {}
        self.files = component_bytes()
        for name, data in self.files.items():
            path = self.root / name
            path.write_bytes(data)
            self.records[name] = (path, max(len(data), 1024 * 1024), digest(data), len(data), component.fingerprint(path.lstat()))
        self.members, self.infos = codec._components(self.records, "0.1.0")
        self.source = codec._distribution_source("0.1.0", self.infos)
        self.distribution = generated_distribution(self.source, self.infos)
        self.product = self.root / "Scribe.pkg"

    def outer(self, *, changes=None, mutate=None, **kwargs):
        files = {"Distribution": self.distribution}
        for name, original in self.files.items():
            files.update({name + "/" + leaf: body for leaf, body in decode_fixture(original).items()})
        files.update(changes or {})
        return tree_xar(files, mutate=mutate, **kwargs)

    def inspect(self, data):
        self.product.write_bytes(data)
        codec._inspect_product(self.product, self.records, self.members, self.infos, self.source)

    def reject(self, data):
        with self.assertRaises((component.ComponentError, OSError)) as caught:
            self.inspect(data)
        self.assertNotIn("PrivateSentinel", str(caught.exception))

    def test_all_seven_decoded_members_and_checked_distribution_additions_match(self):
        self.assertEqual(len(self.members), 7)
        self.inspect(self.outer())
        self.distribution = generated_distribution(self.source, self.infos, additions=False)
        self.inspect(self.outer())

    def test_each_embedded_member_change_or_wrong_role_rejects(self):
        for name in self.members:
            self.reject(self.outer(changes={name: b"different decoded member"}))
        self.reject(self.outer(changes={"cpu-worker.pkg/PackageInfo": decode_fixture(self.files["metal-worker.pkg"])["PackageInfo"]}))

    def test_missing_extra_components_raw_archive_fallback_and_case_alias_reject(self):
        for change in (lambda toc: toc.remove(toc.findall("file")[-1]),
                lambda toc: ET.SubElement(toc, "signature"),
                lambda toc: setattr(toc.findall("file")[1].find("name"), "text", "CPU-worker.pkg"),
                lambda toc: ET.SubElement(toc.findall("file")[1], "data")):
            self.reject(self.outer(mutate=change))
        self.reject(self.outer(changes={"PrivateSentinel": b"extra"}))
        self.reject(tree_xar({"Distribution": self.distribution, **self.files}))

    def test_embedded_name_counts_equality_attributes_and_child_structure_reject(self):
        for change in (lambda leaf: leaf.remove(leaf.findall("name")[1]),
                lambda leaf: ET.SubElement(leaf, "name"),
                lambda leaf: setattr(leaf.findall("name")[1], "text", "PrivateSentinel"),
                lambda leaf: leaf.findall("name")[1].set("extra", "PrivateSentinel"),
                lambda leaf: ET.SubElement(leaf.findall("name")[1], "child"),
                lambda leaf: setattr(leaf.find("type"), "text", "hardlink"),
                lambda leaf: ET.SubElement(leaf, "file")):
            self.reject(self.outer(mutate=lambda toc: change(toc.findall("file")[1].find("file"))))

    def test_checksum_heap_bounds_truncations_and_compression_reject(self):
        value = self.outer()
        for size in range(len(value)):
            self.reject(value[:size])
        self.reject(value + b"tail")
        self.reject(self.outer(heap_transform=lambda heap: b"x" * 20 + heap[20:]))
        for index in range(8):  # Distribution and every one of the seven leaves.
            for tag in ("archived-checksum", "extracted-checksum"):
                def changed(toc):
                    leaves = [node for node in toc.iter("file") if node.find("data") is not None]
                    self.assertEqual(len(leaves), 8)
                    leaves[index].find("data/" + tag).text = "0" * 40
                with self.assertRaisesRegex(component.ComponentError, "member checksum or length differs"):
                    self.inspect(self.outer(mutate=changed))
        for field, content in (("offset", "0"), ("length", "0"), ("length", "9999999999"),
                               ("size", "1"), ("size", "65537")):
            self.reject(self.outer(mutate=lambda toc: setattr(toc.find("file/data/" + field), "text", content)))
        self.reject(self.outer(toc_transform=lambda xml: b'<!DOCTYPE xar [<!ENTITY e "PrivateSentinel">]>' + xml))
        header = list(struct.unpack(">4sHHQQI", value[:28]))
        for index, replacement in ((0, b"bad!"), (1, 27), (2, 2), (3, 0), (3, 65537),
                                   (4, 0), (4, 65537), (4, header[4] + 1), (5, 0), (5, 2)):
            changed = header.copy()
            changed[index] = replacement
            self.reject(struct.pack(">4sHHQQI", *changed) + value[28:])

    def test_distribution_unknown_controls_urls_version_size_and_missing_choices_reject(self):
        changes = [lambda root: root.find("options").set("customize", "allow"),
            lambda root: root.find("options").set("require-scripts", "true"),
            lambda root: root.find("options").set("allow-external-scripts", "true"),
            lambda root: root.find("domains").set("enable_anywhere", "true"),
            lambda root: root.find("domains").set("enable_currentUserHome", "true"),
            lambda root: root.find("choice").set("selected", "false"),
            lambda root: ET.SubElement(root, "script"),
            lambda root: root.remove(root.find("choices-outline")),
            lambda root: root.findall("pkg-ref")[-1].set("version", "9.9.9"),
            lambda root: root.findall("pkg-ref")[-1].set("installKBytes", "999"),
            lambda root: root.findall("pkg-ref")[-1].set("PrivateSentinel", "private"),
            lambda root: setattr(root.findall("pkg-ref")[-1], "text", "https://PrivateSentinel/package.pkg")]
        for change in changes:
            root = ET.fromstring(self.distribution)
            change(root)
            self.reject(self.outer(changes={"Distribution": ET.tostring(root)}))

    def test_generated_bundle_metadata_is_exact_source_derived_and_not_opaque(self):
        for change in (lambda root: root.find("pkg-ref/bundle-version/bundle").set("path", "PrivateSentinel"),
                lambda root: root.find("pkg-ref/bundle-version/bundle").set("CFBundleVersion", "9.9.9"),
                lambda root: ET.SubElement(root.find("pkg-ref/bundle-version"), "PrivateSentinel"),
                lambda root: root.find("pkg-ref").set("version", "0.1.0"),
                lambda root: root.append(ET.fromstring(ET.tostring(root.find("pkg-ref"))))):
            root = ET.fromstring(self.distribution)
            change(root)
            self.reject(self.outer(changes={"Distribution": ET.tostring(root)}))

    def test_large_aggregate_bound_and_all_reads_remain_bounded(self):
        self.assertGreater(codec._product_bound((2**32, 2**32, 2**32)), 3 * 2**32)
        for sizes in ((), (True, 1, 1), (0, 1, 1), (1.0, 1, 1), [1, 1, 1]):
            with self.assertRaises(component.ComponentError):
                codec._product_bound(sizes)
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
                    raise AssertionError("unbounded product read")
                return self.handle.read(count)
            def __getattr__(self, name):
                return getattr(self.handle, name)
        def opened(*args):
            handle, identity = original(*args)
            return Bounded(handle), identity
        with patch.object(component, "open_regular", side_effect=opened):
            codec._components(self.records, "0.1.0")
            self.inspect(self.outer())
        self.assertTrue(reads)

    def test_component_roles_versions_and_no_payload_contracts_reject(self):
        path, maximum, sha, size, node = self.records["cpu-worker.pkg"]
        for info in (metal_info(), package_info().replace(b'0.1.0', b'9.9.9'),
                     package_info(payload=True).replace(b'installKBytes="0"', b'installKBytes="1"')):
            path.write_bytes(encode(CPU_IMAGE, info=info))
            with self.assertRaises(component.ComponentError):
                codec._components(self.records, "0.1.0")

    def test_oversized_decoded_metadata_and_bom_reject_before_materializing(self):
        end = b"</installer-gui-script>"
        exact = self.distribution.replace(end, b" " * (65536 - len(self.distribution)) + end)
        self.assertEqual(len(exact), 65536)
        ET.fromstring(exact)  # Valid semantics, not an invalid-XML negative.
        self.inspect(self.outer(changes={"Distribution": exact}))
        oversized = exact.replace(end, b" " + end)
        self.assertEqual(len(oversized), 65537)
        ET.fromstring(oversized)
        with self.assertRaisesRegex(component.ComponentError, "decimal metadata exceeds bound"):
            self.inspect(self.outer(changes={"Distribution": oversized}))
        # Direct indexing avoids a small original Bom's final equality check
        # masking the exact decoded-size boundary under test.
        layout = {"Bom": ("application/x-gzip", codec.MAX_BOM)}
        for size in (codec.MAX_BOM, codec.MAX_BOM + 1):
            value = tree_xar({"Bom": b"x" * size})
            self.product.write_bytes(value)
            if size == codec.MAX_BOM:
                identities, _metadata = codec._index(self.product, len(value), layout)
                self.assertEqual(identities["Bom"], (digest(b"x" * size), size))
            else:
                with self.assertRaisesRegex(component.ComponentError, "decimal metadata exceeds bound"):
                    codec._index(self.product, len(value), layout)

    def test_large_opaque_entry_streams_across_real_read_boundaries(self):
        body = b"".join(hashlib.sha256(index.to_bytes(4, "big")).digest() for index in range(65536))
        data = encode(body, info=package_info(payload=True, empty_controls=True))
        self.files["cpu-worker.pkg"] = data
        path = self.root / "cpu-worker.pkg"
        path.write_bytes(data)
        self.records["cpu-worker.pkg"] = (path, len(data), digest(data), len(data), component.fingerprint(path.lstat()))
        self.members, self.infos = codec._components(self.records, "0.1.0")
        self.assertGreater(self.members["cpu-worker.pkg/Scripts"][1], 65536)
        self.inspect(self.outer())


class Owner(AppFixture):
    def setUp(self):
        super().setUp()
        self.roles, self.product_calls, self.child_calls = {}, [], []

    def execute(self, args, **kwargs):
        operation = kwargs["operation"]
        component.validate_tool_operation(operation)
        if operation.startswith("final-entry-"):
            path = Path(args[-1])
            if operation == "final-entry-signing":
                role = next(role for role in ("cpu", "metal") if args[args.index("--identifier") + 1] == signing.CODE_IDENTIFIERS[role])
                self.assertEqual(args[args.index("--sign") + 1], SELECTOR)
                self.roles[path] = role
                path.write_bytes(path.read_bytes() + b" synthetic final signature")
            elif operation == "final-entry-signature":
                role = self.roles.get(path, "application")
                self.assertEqual(args[5], signing._requirement(TEAM, role))
            elif operation == "final-entry-properties":
                return PROPERTIES
            elif operation == "final-entry-entitlements":
                return b""
            else:
                self.fail("unexpected signing fixture phase")
            return b""
        if operation in ("entry-build-arm64", "entry-build-x86_64"):
            env = kwargs["environment"]
            self.assertFalse(any(name.startswith(("SCRIBE_GPU_PACK", "SCRIBE_MACOS_METAL")) for name in env))
            target = args[args.index("--target") + 1]
            path = Path(env["CARGO_TARGET_DIR"]) / target / "release/scribe-macos-cpu-worker-installer"
            path.parent.mkdir(parents=True)
            path.write_bytes(CPU_IMAGE)
            self.builds.append((list(args), dict(env)))
            return b""
        if args[:2] == ["/usr/bin/lipo", "-create"]:
            path = Path(args[3])
            path.write_bytes(CPU_IMAGE if "scribe-macos-cpu-worker-installer" in args[4] else METAL_IMAGE)
            path.chmod(0o755)
            return b""
        if args[0] == "/usr/bin/lipo" and args[-1] == "-archs" and Path(args[1]).name == "scribe-macos-cpu-worker-installer":
            return b"arm64" if Path(args[1]).parent.parent.name.startswith("aarch64") else b"x86_64"
        if operation in ("cpu-component-pkg-build", "metal-component-pkg-build"):
            self.child_calls.append(operation)
            scripts = Path(args[args.index("--scripts") + 1])
            info = package_info(payload=True, empty_controls=True) if operation.startswith("cpu") else metal_info(payload=True, empty_controls=True)
            Path(args[-1]).write_bytes(encode((scripts / "postinstall").read_bytes(), info=info))
            return b""
        if operation == "app-component-pkg-build":
            self.child_calls.append(operation)
            root = Path(args[args.index("--root") + 1])
            files = full.input_files({pack["target_arch"]: (pack, release) for pack, release in zip(self.packs, self.releases)})
            bodies = {"./Scribe.app/" + name: (root / "Scribe.app" / name).read_bytes() for name in files}
            expected = {".": (0o40755, "", 0)}
            executables = {"Contents/MacOS/Scribe", "Contents/MacOS/scribe-inference-worker"} | {
                "Contents/Resources/" + pack["root"] + "/worker/scribe-inference-worker" for pack in self.packs}
            for name, body in bodies.items():
                expected[name] = (0o100755 if name.removeprefix("./Scribe.app/") in executables else 0o100644, digest(body), len(body))
                for parent in Path(name).parents:
                    if parent.as_posix() != ".":
                        expected["./" + parent.as_posix().removeprefix("./")] = (0o40755, "", 0)
            self.stage_expected, self.stage_bodies = expected, bodies
            Path(args[-1]).write_bytes(app_xar(expected, bodies))
            return b""
        if operation == "product-component-pkg-build":
            self.product_calls.append(list(args))
            controls, packages = Path(args[args.index("--distribution") + 1]), Path(args[args.index("--package-path") + 1])
            self.assertEqual(args, ["/usr/bin/productbuild", "--distribution", str(controls), "--package-path", str(packages), args[-1]])
            self.assertEqual(set(kwargs["environment"]), {"PATH", "LC_ALL", "LANG", "TMPDIR"})
            self.assertEqual(set(path.name for path in packages.iterdir()), set(codec.NAMES))
            members = {"Distribution": generated_distribution(controls.read_bytes(), self.product_infos)}
            for path in packages.iterdir():
                members.update({path.name + "/" + leaf: value for leaf, value in decode_fixture(path.read_bytes()).items()})
            if self.failure == "uncertain-product":
                raise component.CleanupUncertain("synthetic product process uncertainty")
            if self.failure == "failed-product":
                raise component.ToolExitError(operation, 1)
            Path(args[-1]).write_bytes(b"malformed" if self.failure == "malformed-product" else tree_xar(members))
            targets = {"product-source": self.app / "Contents/Info.plist", "product-policy": self.policy,
                       "product-controls": controls, "product-component": packages / "cpu-worker.pkg"}
            if self.failure == "product-private":
                matches = list((Path(args[-1]).parent / "admission").glob(".scribe-full-release-entry-*/app/Contents/Info.plist"))
                self.assertEqual(len(matches), 1)
                targets[self.failure] = matches[0]
            if self.failure in targets:
                path = targets[self.failure]
                value = path.read_bytes()
                alternate = self.root / "changed-during-product"
                alternate.write_bytes(bytes([value[0] ^ 1]) + value[1:])
                os.replace(alternate, path)
            return b""
        return super().execute(args, **kwargs)

    def build(self, **kwargs):
        self.verifications.clear()
        self.product_calls.clear()
        self.child_calls.clear()
        original = codec._components
        def indexed(*args):
            result = original(*args)
            self.product_infos = result[1]
            return result
        def publish(source, output):
            if output == self.workspace / "Scribe.pkg" and self.failure == "collision":
                output.write_bytes(b"retain concurrent product")
                raise FileExistsError("synthetic collision")
            source.rename(output)
            if output == self.workspace / "Scribe.pkg" and self.failure == "publication-uncertain":
                raise OSError("synthetic post-publication uncertainty")
        with self.context(**kwargs), patch.object(codec, "_components", side_effect=indexed), \
             patch.object(component, "publish_no_replace", side_effect=publish):
            return author.build_product(self.app, self.workspace, self.frozen, SELECTOR)

    def test_real_source_owners_share_one_retained_baseline_and_publish_unsigned_product(self):
        before = full.snapshot(self.app, full.input_files(full.release_inputs(self.app, self.frozen)[0]))
        output, sha, size = self.build()
        self.assertEqual(output, self.workspace / "Scribe.pkg")
        self.assertEqual((sha, size), (digest(output.read_bytes()), output.stat().st_size))
        self.assertEqual(list(self.workspace.iterdir()), [output])
        self.assertEqual(self.child_calls, ["cpu-component-pkg-build", "metal-component-pkg-build", "app-component-pkg-build"])
        self.assertEqual([arch for _, arch in self.verifications], list(full.ARCHES) * 4)
        self.assertEqual(full.snapshot(self.app, full.input_files(full.release_inputs(self.app, self.frozen)[0])), before)

    def test_default_denial_precedes_all_inputs_owners_and_tools(self):
        with patch.object(full, "owned_release") as admission, patch.object(component, "capture") as tool, \
             patch.object(author.cpu_component, "build_component") as cpu, self.assertRaisesRegex(
                 component.ComponentError, "publisher is not provisioned"):
            author.build_product(None, None, None, None)
        for operation in (admission, tool, cpu):
            operation.assert_not_called()

    def test_outer_construction_failures_publish_nothing(self):
        for failure in ("failed-product", "malformed-product"):
            self.failure = failure
            with self.assertRaises(component.ComponentError):
                self.build()
            self.assertEqual(len(self.product_calls), 1)
            self.assertFalse(list(self.workspace.iterdir()))

    def test_mutation_between_each_real_child_owner_rejects_original_baseline(self):
        info = self.app / "Contents/Info.plist"
        saved = info.read_bytes()
        for module in (author.cpu_component, author.metal_component, author.app_component):
            original = module.build_component
            for replacement in (False, True):
                reached = []
                def changed(*args):
                    result = original(*args)
                    value = info.read_bytes()
                    alternate = self.root / "changed-app"
                    alternate.write_bytes(value if replacement else bytes([value[0] ^ 1]) + value[1:])
                    os.replace(alternate, info)
                    reached.append(True)
                    return result
                try:
                    with patch.object(module, "build_component", side_effect=changed), self.assertRaisesRegex(
                            component.ComponentError, "full same-app baseline changed"):
                        self.build()
                    self.assertEqual(reached, [True])
                    self.assertFalse(self.product_calls)
                    self.assertFalse(list(self.workspace.iterdir()))
                finally:
                    info.write_bytes(saved)

    def test_collision_and_ambiguous_publication_retain_final(self):
        original = self.fake_private
        @contextmanager
        def cleanup(parent, prefix):
            with original(parent, prefix) as private:
                yield private
            if prefix == ".scribe-product-pkg-" and self.failure == "post-publication-cleanup":
                raise OSError("synthetic successful publication then cleanup failure")
        for failure in ("collision", "publication-uncertain", "post-publication-cleanup"):
            self.failure = failure
            with patch.object(self, "fake_private", side_effect=cleanup), self.assertRaises(OSError):
                self.build()
            path = self.workspace / "Scribe.pkg"
            self.assertTrue(path.is_file())
            if failure == "collision":
                self.assertEqual(path.read_bytes(), b"retain concurrent product")
            path.unlink()  # Only this test's mock-published final.

    def test_invalid_frozen_inputs_and_selector_precede_native_work(self):
        frozen = self.frozen
        for value in (None, True, {}, replace(frozen, revision="short"), replace(frozen, cpu_worker_sha256="bad")):
            self.frozen = value
            with self.assertRaises(component.ComponentError):
                self.build()
            self.assertFalse(self.child_calls or self.product_calls)
        self.frozen = frozen
        with self.context(), patch.object(component, "capture") as native:
            for selector in (None, "-", "label", "A" * 40, SELECTOR + "\n"):
                with self.assertRaises(component.ComponentError):
                    author.build_product(self.app, self.workspace, frozen, selector)
            native.assert_not_called()

    def test_top_admission_rejects_either_pack_before_any_child(self):
        for arch in full.ARCHES:
            self.failure = "pack-" + arch
            with self.assertRaises(component.ComponentError):
                self.build()
            self.assertFalse(self.child_calls or self.product_calls)
            self.assertFalse(list(self.workspace.iterdir()))

    def test_each_constructor_wrong_path_hash_size_and_boolean_size_rejects(self):
        for module in (author.cpu_component, author.metal_component, author.app_component):
            original = module.build_component
            for case in ("path", "hash", "size", "bool-size"):
                reached = []
                def changed(*args):
                    path, sha, size = original(*args)
                    reached.append(True)
                    return (path.parent / "wrong.pkg" if case == "path" else path,
                            "0" * 64 if case == "hash" else sha,
                            True if case == "bool-size" else size + (case == "size"))
                with patch.object(module, "build_component", side_effect=changed), self.assertRaises(component.ComponentError):
                    self.build()
                self.assertEqual(reached, [True])
                self.assertFalse(self.product_calls)
                self.assertFalse(list(self.workspace.iterdir()))

    def test_retained_cpu_component_change_during_later_child_rejects(self):
        original = author.metal_component.build_component
        for replacement in (False, True):
            reached = []
            def changed(app, workspace, *args):
                result = original(app, workspace, *args)
                path = workspace / "cpu-worker.pkg"
                value = path.read_bytes()
                alternate = self.root / "changed-component"
                alternate.write_bytes(value if replacement else bytes([value[0] ^ 1]) + value[1:])
                os.replace(alternate, path)
                reached.append(True)
                return result
            with patch.object(author.metal_component, "build_component", side_effect=changed), self.assertRaisesRegex(
                    component.ComponentError, "owned component changed"):
                self.build()
            self.assertEqual(reached, [True])
            self.assertFalse(self.product_calls)

    def test_changes_during_productbuild_prevent_outer_interpretation(self):
        source = self.app / "Contents/Info.plist"
        original_source, original_policy = source.read_bytes(), self.policy.read_bytes()
        for failure in ("product-source", "product-policy", "product-controls", "product-component", "product-private"):
            self.failure = failure
            try:
                with patch.object(codec, "_inspect_product") as inspect, self.assertRaises(component.ComponentError):
                    self.build()
                self.assertEqual(len(self.product_calls), 1)
                inspect.assert_not_called()
                self.assertFalse(list(self.workspace.iterdir()))
            finally:
                source.write_bytes(original_source)
                self.policy.write_bytes(original_policy)

    def test_product_mutation_after_actual_readback_before_measurement_rejects(self):
        original = codec._inspect_product
        for replacement in (False, True):
            reached = []
            def inspect(path, *args):
                original(path, *args)
                value = path.read_bytes()
                alternate = self.root / "changed-product"
                alternate.write_bytes(value if replacement else bytes([value[0] ^ 1]) + value[1:])
                os.replace(alternate, path)
                reached.append(True)
            with patch.object(codec, "_inspect_product", side_effect=inspect), self.assertRaisesRegex(
                    component.ComponentError, "product changed during inspection"):
                self.build()
            self.assertEqual(reached, [True])
            self.assertFalse(list(self.workspace.iterdir()))

    def test_post_top_admission_exit_mutations_reject_before_publication(self):
        original = full.owned_release
        source = self.app / "Contents/Info.plist"
        original_source, original_policy = source.read_bytes(), self.policy.read_bytes()
        for target, message in (("source", "full same-app baseline changed"),
                ("policy", "entry signing policy changed during finalization"),
                ("component", "owned component changed"), ("controls", "owned distribution controls changed"),
                ("product", "product changed before publication"), ("exit", "synthetic outer admission exit failure")):
            reached = []
            @contextmanager
            def admitted(app, workspace, frozen):
                with original(app, workspace, frozen) as owned:
                    yield owned
                    if workspace.name == "admission" and workspace.parent.name.startswith(".scribe-product-pkg-"):
                        self.assertFalse((self.workspace / "Scribe.pkg").exists())
                if workspace.name != "admission" or not workspace.parent.name.startswith(".scribe-product-pkg-"):
                    return
                reached.append(target)
                if target == "exit":
                    raise component.ComponentError(message)
                private = workspace.parent
                path = {"source": source, "policy": self.policy, "component": private / "packages/cpu-worker.pkg",
                        "controls": private / "Distribution.xml", "product": private / "Scribe.pkg"}[target]
                value = path.read_bytes()
                path.write_bytes(bytes([value[0] ^ 1]) + value[1:])
            try:
                with patch.object(full, "owned_release", side_effect=admitted), self.assertRaisesRegex(component.ComponentError, message):
                    self.build()
                self.assertEqual(reached, [target])
                self.assertFalse(list(self.workspace.iterdir()))
            finally:
                source.write_bytes(original_source)
                self.policy.write_bytes(original_policy)


class PosixOwner(Owner):
    def test_uncertain_outer_process_retains_full_admission_and_all_components(self):
        self.workspace.chmod(0o700)
        self.failure = "uncertain-product"
        with self.assertRaises(component.CleanupUncertain):
            self.build(real_workspace=True)
        outer = list(self.workspace.glob(".scribe-product-pkg-*"))
        self.assertEqual(len(outer), 1)
        self.assertEqual(set(path.name for path in (outer[0] / "packages").iterdir()), set(codec.NAMES))
        admitted = list((outer[0] / "admission").glob(".scribe-full-release-entry-*/app"))
        self.assertEqual(len(admitted), 1)
        self.assertFalse((self.workspace / "Scribe.pkg").exists())


def suite():
    result = unittest.TestSuite()
    classes = (Codec, Owner) + ((PosixOwner,) if os.name == "posix" else ())
    for cls in classes:
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 23 + (1 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"Product source discovery mismatch: expected {expected}")
    return result


if __name__ == "__main__":
    result = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if result.wasSuccessful() else 1)
