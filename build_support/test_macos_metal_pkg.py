"""Mocked owned Metal packaging; never authentic signing, Installer or execution."""
from dataclasses import replace
from enum import Enum
import hashlib
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

import macos_cpu_component as component
import macos_cpu_pkg as codec
import macos_cpu_pkg_final as signing
import macos_metal_pkg as author
import macos_metal_pkg_entry as entry
import macos_metal_pkg_final as final
from test_macos_cpu_pkg import VERSION, archive, encode, package_info, record
from test_macos_metal_pkg_entry import IMAGE, digest
from test_macos_metal_pkg_final import Owner as FinalOwner, SELECTOR, SIGNED


def metal_info(**kwargs):
    root = ET.fromstring(package_info(**kwargs))
    root.set("identifier", author.IDENTIFIER)
    return ET.tostring(root)


class Format(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scribe-metal-pkg-format-")
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "fresh.pkg"

    def inspect(self, data, body=SIGNED):
        self.path.write_bytes(data)
        codec._inspect_metal_fresh(self.path, digest(body), len(body), VERSION)

    def test_source_fixed_profiles_reject_cross_role_identity(self):
        for info, check in ((metal_info(), codec._inspect_fresh), (package_info(), codec._inspect_metal_fresh)):
            self.path.write_bytes(encode(SIGNED, info=info))
            with self.assertRaisesRegex(component.ComponentError, "package identity differs"):
                check(self.path, digest(SIGNED), len(SIGNED), VERSION)
        self.inspect(encode(SIGNED, info=metal_info(timeout="600", empty_controls=True)))
        self.assertNotEqual(author.IDENTIFIER, codec.IDENTIFIER)

    def test_invalid_or_duck_typed_format_rejects_before_file_access(self):
        class Foreign(Enum):
            METAL = "metal"
        for kind in (None, True, "metal", {}, (author.IDENTIFIER, entry.MAX_ENTRY), Foreign.METAL):
            with self.subTest(kind=type(kind).__name__), patch.object(component, "open_regular") as opened, \
                 self.assertRaisesRegex(component.ComponentError, "unsupported private component format"):
                codec._inspect_format(None, "0" * 64, 1, VERSION, kind)
            opened.assert_not_called()

    def test_large_bound_arithmetic_preserves_cpu_limit_without_large_allocation(self):
        self.assertGreater(entry.MAX_ENTRY, 2 ** 32)
        for size in (codec.entry.MAX_ENTRY + 1, 2 ** 32, 2 ** 32 + 1, entry.MAX_ENTRY):
            self.assertEqual(codec._metal_package_bound(size), size + size // 100 + 4 * 1024 * 1024)
            with self.assertRaises(component.ComponentError):
                codec._package_bound(size)
        for invalid in (False, 0, -1, 1.0, entry.MAX_ENTRY + 1):
            with self.assertRaises(component.ComponentError):
                codec._metal_package_bound(invalid)
        self.assertEqual(codec._package_bound(codec.entry.MAX_ENTRY),
                         codec.entry.MAX_ENTRY + codec.entry.MAX_ENTRY // 100 + 4 * 1024 * 1024)

    def test_metal_inspection_keeps_streaming_read_and_inflate_limits(self):
        body = b"".join(hashlib.sha256(index.to_bytes(4, "big")).digest() for index in range(65536))
        self.path.write_bytes(encode(body, info=metal_info()))
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
                    raise AssertionError("unbounded Metal package read")
                return self.handle.read(count)
            def __getattr__(self, name):
                return getattr(self.handle, name)
        def opened(*args):
            handle, info = original(*args)
            return Bounded(handle), info
        with patch.object(component, "open_regular", side_effect=opened):
            codec._inspect_metal_fresh(self.path, digest(body), len(body), VERSION)
        self.assertGreater(len(reads), 20)
        self.assertEqual(codec.CHUNK, 65536)

    def test_corruption_aliases_links_payload_and_operation_changes_reject(self):
        valid = encode(SIGNED, info=metal_info())
        info = metal_info()
        cases = [valid[:-1], valid + b"tail", b"invalid header", encode(IMAGE, info=info),
                 encode(SIGNED, info=b"<broken"), encode(SIGNED, info=info.replace(b'file="./postinstall"', b'file="other"')),
                 encode(SIGNED, info=metal_info(timeout="0600")),
                 encode(SIGNED, info=info.replace(b"</scripts>", b'<preinstall file="./preinstall"/></scripts>')),
                 encode(SIGNED, info=info.replace(b"</pkg-info>", b'<payload installKBytes="1" numberOfFiles="1"/></pkg-info>')),
                 encode(SIGNED, info=info, extra_members=(("Bom", b"x", b"x", "application/octet-stream"),)),
                 encode(SIGNED, info=info, odc=record(body=SIGNED, links=2) + record(b"TRAILER!!!", b"", 0)),
                 encode(SIGNED, info=info, odc=record(b"./alias", SIGNED) + record(b"TRAILER!!!", b"", 0)),
                 encode(SIGNED, info=info, odc=archive(SIGNED) + b"nonzero"),
                 valid[:-1] + bytes([valid[-1] ^ 1])]
        for index, data in enumerate(cases):
            with self.subTest(index=index), self.assertRaises((component.ComponentError, OSError)):
                self.inspect(data)

    def test_package_mutation_and_same_byte_replacement_during_inspection_reject(self):
        original = codec._package_info_for
        for replacement in (False, True):
            def changed(data, version, kind):
                original(data, version, kind)
                if replacement:
                    alternate = self.path.with_suffix(".replacement")
                    alternate.write_bytes(self.path.read_bytes())
                    os.replace(alternate, self.path)
                else:
                    with self.path.open("ab") as handle:
                        handle.write(b"changed")
            with patch.object(codec, "_package_info_for", side_effect=changed), \
                 self.assertRaises((component.ComponentError, OSError)):
                self.inspect(encode(SIGNED, info=metal_info()))


class Owner(FinalOwner):
    def setUp(self):
        super().setUp()
        self.packages = []
        self.package_source = None

    def execute(self, args, **kwargs):
        if kwargs["operation"] != "metal-component-pkg-build":
            return super().execute(args, **kwargs)
        component.validate_tool_operation(kwargs["operation"])
        self.operations.append((kwargs["operation"], list(args), kwargs))
        candidate, scripts = Path(args[-1]), Path(args[args.index("--scripts") + 1])
        self.assertEqual(args[:3], ["/usr/bin/pkgbuild", "--nopayload", "--scripts"])
        self.assertEqual(args[args.index("--identifier") + 1], author.IDENTIFIER)
        self.assertEqual(args[args.index("--version") + 1], VERSION)
        self.assertEqual(args[args.index("--install-location") + 1], "/")
        self.assertEqual(kwargs["timeout"], 120)
        self.assertEqual(set(kwargs["environment"]), {"PATH", "LC_ALL", "LANG", "TMPDIR"})
        body = (scripts / "postinstall").read_bytes()
        self.assertEqual(body, SIGNED)
        if self.failure == "pkg-uncertain":
            raise component.CleanupUncertain("synthetic packaging cleanup uncertainty")
        if self.failure == "pkg-failed":
            raise component.ToolExitError("metal-component-pkg-build", 1)
        if self.failure == "pkg-missing":
            return b""
        candidate.write_bytes(encode(IMAGE if self.failure == "pkg-presign" else body,
                                     info=package_info() if self.failure == "pkg-cpu-id" else metal_info(empty_controls=True)))
        self.packages.append(candidate)
        if self.failure in ("pkg-entry-change", "pkg-entry-replace"):
            operation = scripts / "postinstall"
            if self.failure.endswith("replace"):
                alternate = scripts / "alternate"
                alternate.write_bytes(operation.read_bytes())
                os.replace(alternate, operation)
            else:
                operation.write_bytes(SIGNED + b"changed")
        if self.failure == "pkg-extra-operation":
            (scripts / "preinstall").write_bytes(b"never run")
        if (self.failure or "").startswith("pkg-policy-"):
            data = self.policy.read_bytes() if self.failure.endswith("replace") else b"changed policy"
            alternate = self.root / "alternate-policy"
            alternate.write_bytes(data)
            os.replace(alternate, self.policy)
        if (self.failure or "").startswith("pkg-source-"):
            source = self.app / self.package_source
            data = source.read_bytes()
            alternate = self.root / "alternate-input"
            alternate.write_bytes(data if self.failure.endswith("replace") else data + b"changed")
            os.replace(alternate, source)
        return b""

    def build(self, *, selector=SELECTOR, **kwargs):
        def publish(source, output):
            if output == self.workspace / "metal-worker.pkg" and self.failure == "pkg-collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic collision")
            source.rename(output)
            if output == self.workspace / "metal-worker.pkg" and self.failure == "pkg-publish-uncertain":
                raise OSError("synthetic post-rename uncertainty")
        with self.context(**kwargs), patch.object(component, "publish_no_replace", side_effect=publish):
            return author.build_component(self.app, self.workspace, self.frozen, selector)

    def reject(self):
        with self.assertRaises((component.ComponentError, OSError)):
            self.build()
        self.assertFalse((self.workspace / "metal-worker.pkg").exists())

    def test_default_policy_denies_before_inputs_workspace_finalizer_or_tools(self):
        with patch.object(component, "capture") as tool, patch.object(final, "build_entry") as builder, \
             patch.object(final, "_source_snapshot") as snapshot, patch.object(component, "private_workspace") as workspace, \
             self.assertRaisesRegex(component.ComponentError, "publisher is not provisioned"):
            author.build_component(None, None, None, None)
        for operation in (tool, builder, snapshot, workspace):
            operation.assert_not_called()

    def test_owned_full_metal_chain_packages_only_postsign_bytes(self):
        with patch.object(component, "verify_cpu_app") as cpu:
            output, sha, size = self.build()
        cpu.assert_not_called()
        self.assertEqual((sha, size), (digest(output.read_bytes()), output.stat().st_size))
        codec._inspect_metal_fresh(output, digest(SIGNED), len(SIGNED), VERSION)
        self.assertEqual(len(self.builds), 3)
        self.assertEqual([arch for _, arch in self.verifications], list(entry.ARCHES))
        self.assertEqual([op for op, _, _ in self.operations][-1], "metal-component-pkg-build")
        self.assertEqual(list(self.workspace.iterdir()), [output])

    def test_invalid_identity_selector_and_workspace_precede_tools(self):
        saved = self.frozen
        for frozen in (None, {}, replace(saved, revision="short"), replace(saved, catalog_sha256="invalid")):
            self.frozen = frozen
            self.reject()
        self.frozen = saved
        for selector in (None, "-", "name", SELECTOR + "\n"):
            with self.assertRaises(component.ComponentError):
                self.build(selector=selector)
        for workspace in (Path("relative"), self.app, self.app / "Contents"):
            self.workspace = workspace
            with self.assertRaises(component.ComponentError):
                self.build()
        self.assertEqual(self.operations, [])

    def test_existing_final_output_preserved_before_construction(self):
        output = self.workspace / "metal-worker.pkg"
        output.write_bytes(b"retain existing")
        with self.assertRaisesRegex(component.ComponentError, "output already exists"):
            self.build()
        self.assertEqual(output.read_bytes(), b"retain existing")
        self.assertEqual(self.operations, [])

    def test_finalizer_or_full_pack_verification_failure_cannot_reach_pkgbuild(self):
        for failure in ("content", "application", "sign-failure", "entry-signature"):
            self.failure = failure
            self.operations.clear()
            self.reject()
            self.assertNotIn("metal-component-pkg-build", [op for op, _, _ in self.operations])

    def test_bad_finalizer_path_hash_size_link_or_inventory_precedes_pkgbuild(self):
        original = final.build_entry
        for failure in ("path", "hash", "size", "link", "inventory"):
            def builder(*args):
                path, sha, size = original(*args)
                if failure == "link":
                    os.link(path, self.root / "linked-final-entry")
                if failure == "inventory":
                    (path.parent / "preinstall").write_bytes(b"never run")
                return (path.parent / "wrong" if failure == "path" else path,
                        "0" * 64 if failure == "hash" else sha, size + (failure == "size"))
            self.operations.clear()
            with patch.object(final, "build_entry", side_effect=builder):
                self.reject()
            self.assertNotIn("metal-component-pkg-build", [op for op, _, _ in self.operations])

    def test_missing_failed_wrong_role_or_presign_package_rejects(self):
        for failure in ("pkg-missing", "pkg-failed", "pkg-cpu-id", "pkg-presign"):
            self.failure = failure
            self.operations.clear()
            self.reject()
            self.assertIn("metal-component-pkg-build", [op for op, _, _ in self.operations])

    def test_full_source_content_and_same_byte_replacement_during_packaging_reject(self):
        packs, _ = entry.release_inputs(self.app, self.frozen)
        for relative in entry.input_files(packs):
            source = self.app / relative
            saved = source.read_bytes()
            for failure in ("pkg-source-change", "pkg-source-replace"):
                self.failure, self.package_source = failure, relative
                self.operations.clear()
                self.reject()
                self.assertIn("metal-component-pkg-build", [op for op, _, _ in self.operations])
                source.write_bytes(saved)

    def test_policy_content_and_same_byte_replacement_during_packaging_reject(self):
        saved = self.policy.read_bytes()
        for failure in ("pkg-policy-change", "pkg-policy-replace"):
            self.policy.write_bytes(saved)
            self.failure = failure
            self.operations.clear()
            self.reject()
            self.assertIn("metal-component-pkg-build", [op for op, _, _ in self.operations])

    def test_retained_entry_and_inventory_mutations_after_packaging_reject(self):
        for failure in ("pkg-entry-change", "pkg-entry-replace", "pkg-extra-operation"):
            self.failure = failure
            self.operations.clear()
            self.reject()
            self.assertIn("metal-component-pkg-build", [op for op, _, _ in self.operations])

    def test_package_content_and_same_byte_replacement_after_inspection_reject(self):
        inspect = codec._inspect_metal_fresh
        for replacement in (False, True):
            def changed_at_return(package, *args):
                inspect(package, *args)
                alternate = package.with_suffix(".alternate")
                alternate.write_bytes(package.read_bytes() + (b"" if replacement else b"changed"))
                os.replace(alternate, package)
            with patch.object(codec, "_inspect_metal_fresh", side_effect=changed_at_return):
                self.reject()
        original = signing._unchanged_policy
        for replacement in (False, True):
            self.packages.clear()
            def changed(expected):
                original(expected)
                if self.packages:
                    package = self.packages[-1]
                    alternate = package.with_suffix(".alternate")
                    alternate.write_bytes(package.read_bytes() + (b"" if replacement else b"changed"))
                    os.replace(alternate, package)
            with patch.object(signing, "_unchanged_policy", side_effect=changed):
                self.reject()

    def test_collision_and_ambiguous_publication_preserve_final(self):
        for failure in ("pkg-collision", "pkg-publish-uncertain"):
            with tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.workspace, self.failure = Path(directory), failure
                with self.assertRaises(OSError):
                    self.build()
                output = self.workspace / "metal-worker.pkg"
                self.assertTrue(output.is_file())
                if failure == "pkg-collision":
                    self.assertEqual(output.read_bytes(), b"retain concurrent output")


class Posix(Owner):
    def test_packaging_cleanup_uncertainty_retains_private_workspace(self):
        self.failure = "pkg-uncertain"
        with self.assertRaises(component.CleanupUncertain):
            self.build(real_workspace=True)
        retained = list(self.workspace.glob(".scribe-metal-pkg-*"))
        self.assertEqual(len(retained), 1)
        self.assertTrue((retained[0] / "scripts/postinstall").exists())
        self.assertFalse((self.workspace / "metal-worker.pkg").exists())
        # All native children were mocked; no live/uncertain process is retained.


def suite():
    result = unittest.TestSuite()
    for cls in [Format, Owner] + ([Posix] if os.name == "posix" else []):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 18 + (1 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"Metal component discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
