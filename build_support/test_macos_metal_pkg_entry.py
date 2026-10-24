"""Mock-only positives: no native build, pack signature, entry or worker runs."""
from contextlib import ExitStack, contextmanager
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch

import macos_cpu_component as component
import macos_full_release as full
import macos_metal_pkg_entry as entry

REVISION = "source-metal-construction-fixture"
CPU = f"scribe-inference-worker@0.1.0#{REVISION}".encode()
SHA = hashlib.sha256(CPU).hexdigest()
IMAGE = f"synthetic entry {REVISION} {entry.METAL_ROOT}".encode()


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode()


def digest(value):
    return hashlib.sha256(value).hexdigest()


class Portable(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="scribe-metal-construction-source-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.app = self.root / "Scribe.app"
        self.workspace = self.root / "author"
        self.workspace.mkdir()
        self.cargo = str(self.root / "reviewed-cargo")
        context = {
            component.CPU_APP_INPUTS[0]: CPU,
            component.CPU_APP_INPUTS[1]: f"local-transcriber@0.1.0#{REVISION} {SHA}".encode(),
            "Contents/Info.plist": plistlib.dumps({"CFBundleShortVersionString": "0.1.0"}),
            "Contents/_CodeSignature/CodeResources": b"synthetic context, not an actual signature",
            "Contents/embedded.provisionprofile": b"synthetic profile, not authenticated",
        }
        for name, data in context.items():
            path = self.app / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.packs, self.releases, self.manifests = [], [], {}
        for arch in entry.ARCHES:
            pack_digest = digest(arch.encode())
            root = f"workers/packs/metal-{arch}/1.0.0/{pack_digest}"
            pack_root = self.app / "Contents/Resources" / root
            (pack_root / "worker").mkdir(parents=True)
            manifest = {"app_build": f"local-transcriber@0.1.0#{REVISION}",
                        "worker_build": f"scribe-inference-worker@0.1.0#{REVISION}",
                        "app_protocol_version": 5, "target_arch": arch}
            # Deliberately NOT valid manifests/signatures. Acceptance is mocked;
            # this cannot serve as production-positive cryptographic evidence.
            manifest_bytes = encoded(manifest)
            for name, data in [("pack-manifest.json", manifest_bytes), ("pack-manifest.sig", b"not-a-signature"),
                               ("worker/scribe-inference-worker", b"synthetic native Metal worker")]:
                (pack_root / name).write_bytes(data)
            self.manifests[arch] = digest(manifest_bytes)
            pack = dict(pack_id=f"metal-{arch}", pack_version="1.0.0", pack_digest=pack_digest,
                        security_epoch=1, runtime_abi_version=1, backend="metal", provider="transcribe-cpp-metal",
                        target_os="macos", target_arch=arch, worker_relative_path="worker/scribe-inference-worker",
                        root=root, installed_size_bytes=sum((pack_root / name).stat().st_size for name in entry.PACK_FILES),
                        compressed_size_bytes=1, files=sorted(f"{root}/{name}" for name in entry.PACK_FILES))
            self.packs.append(pack)
            self.releases.append({**pack, "app_version": "0.1.0", "build_revision": REVISION, "app_protocol_version": 5})
        self.refresh_documents()
        self.calls, self.builds, self.verifications = [], [], []
        self.failure = None

    def refresh_documents(self):
        catalog = encoded({"schema_version": 1, "packs": self.packs})
        authority = encoded({"schema_version": 2, "catalog_sha256": digest(catalog), "release_security_epoch": 1,
                             "keychain_access_group": "FAKETEAM01.com.scribe.local-transcriber", "entries": self.releases})
        (self.app / entry.CATALOG).write_bytes(catalog)
        (self.app / entry.AUTHORITY).write_bytes(authority)
        self.frozen = entry.FrozenInputs(REVISION, SHA, digest(catalog), digest(authority),
                                        self.manifests["aarch64"], self.manifests["x86_64"])

    @contextmanager
    def fake_private(self, parent, prefix):
        with tempfile.TemporaryDirectory(prefix=prefix, dir=parent) as name:
            yield Path(name)

    @contextmanager
    def context(self, *, real_workspace=False):
        with ExitStack() as stack:
            stack.enter_context(patch.object(full.sys, "platform", "darwin"))
            stack.enter_context(patch.object(full.os, "geteuid", return_value=getattr(os, "geteuid", lambda: 501)() or 501,
                                             create=True))
            stack.enter_context(patch.object(full.platform, "machine", return_value="arm64"))
            stack.enter_context(patch.object(full.shutil, "which", return_value=self.cargo))
            stack.enter_context(patch.object(component, "capture", side_effect=self.execute))
            if not real_workspace:
                stack.enter_context(patch.object(component, "private_workspace", side_effect=self.fake_private))
            yield

    def execute(self, args, **kwargs):
        self.calls.append((list(args), kwargs))
        component.validate_tool_operation(kwargs["operation"])
        tool = args[0]
        if tool == self.cargo:
            env = dict(kwargs["environment"])
            self.builds.append((list(args), env))
            self.assertEqual(kwargs["cwd"], component.REPO)
            if self.failure in ("build", "deadline", "uncertain") or (self.failure == "entry-build" and "metal-entry-build" in kwargs["operation"]):
                error = component.CleanupUncertain if self.failure == "uncertain" else component.ComponentError
                raise error("synthetic bounded build failure")
            target = args[args.index("--target") + 1]
            binary = Path(env["CARGO_TARGET_DIR"]) / target / "release" / args[args.index("--bin") + 1]
            binary.parent.mkdir(parents=True)
            if self.failure == "missing" and "metal-entry-build" in kwargs["operation"]:
                return b""
            if self.failure == "hardlink" and "metal-entry-build" in kwargs["operation"]:
                os.link(self.app / component.CPU_APP_INPUTS[0], binary)
            elif self.failure == "symlink" and "metal-entry-build" in kwargs["operation"]:
                binary.symlink_to(self.app / component.CPU_APP_INPUTS[0])
            else:
                binary.write_bytes(IMAGE)
            if self.failure in ("private-change", "source-change", "source-replace") and len(self.builds) == 3:
                changed = Path(env["SCRIBE_GPU_PACK_CATALOG"]) if self.failure == "private-change" else self.app / entry.CATALOG
                data = changed.read_bytes()
                if self.failure == "source-replace":
                    changed.unlink()
                changed.write_bytes(data if self.failure == "source-replace" else b"tampered")
            return b""
        if Path(tool).name == "scribe-worker-pack-tool":
            self.assertEqual(len(args), 6)
            self.assertEqual(args[1], "verify-production-macos-metal")
            self.assertEqual(args[2], "--pack-root")
            self.assertEqual(args[4], "--target-arch")
            root, arch = Path(args[3]), args[5]
            self.verifications.append((root, arch))
            self.assertNotIn(self.app, root.parents)
            if self.failure == "content":
                raise component.ToolExitError("metal-content-verification", 1)
            pack = next(pack for pack in self.packs if pack["target_arch"] == arch)
            descriptor = {key: pack[key] for key in ("pack_id", "pack_version", "pack_digest", "security_epoch",
                "runtime_abi_version", "backend", "provider", "target_os", "target_arch", "worker_relative_path")}
            descriptor["root"] = str(root)
            if self.failure == "descriptor":
                descriptor["provider"] = "wrong"
            if self.failure == "verifier-change":
                Path(tool).write_bytes(b"changed verifier")
            return encoded(descriptor)
        if tool == "/usr/bin/lipo":
            if args[1] == "-create":
                if self.failure == "combine":
                    raise component.ComponentError("synthetic combine failure")
                Path(args[3]).write_bytes(IMAGE)
            elif args[-1] == "-archs":
                name = Path(args[1])
                if name.name in ("scribe-worker-pack-tool", "scribe-macos-metal-pack-installer"):
                    role = "author" if name.name == "scribe-worker-pack-tool" else "entry"
                    if self.failure == role + "-wrong-arch":
                        return b"arm64e"
                    if self.failure == role + "-fat-arch":
                        return b"arm64 x86_64"
                    return b"arm64" if name.parent.parent.name.startswith("aarch64") else b"x86_64"
                return b"x86_64" if self.failure == "missing-universal" and name.name == "postinstall" else b"arm64 x86_64"
            else:
                self.assertEqual(args[2], "-thin")
                Path(args[-1]).write_bytes(Path(args[1]).read_bytes())
            return b""
        if tool == "/usr/bin/otool":
            if args[1] == "-l":
                return b"minos 12.0\n" if self.failure == "minos" and "postinstall" in args[-1] else b"minos 13.0\n"
            dependency = "/System/Library/Frameworks/Metal.framework/Metal" if self.failure == "gpu-dependency" \
                and "postinstall" in args[-1] else "/usr/lib/libSystem.B.dylib"
            return f"binary:\n\t{dependency} (compatibility version 1.0)\n".encode()
        if tool == "/usr/bin/codesign":
            self.assertEqual(args[1:3], ["--verify", "--strict"])
            if self.failure == "signature":
                raise component.ComponentError("synthetic signature failure")
            return b""
        self.assertEqual(tool, "/bin/bash")
        self.assertEqual(args[1:], [str(component.REPO / "scripts/verify-macos-release-package.sh"), "--app", str(self.app)])
        if self.failure == "application":
            raise component.ComponentError("synthetic full application policy rejection")
        return b""

    def build(self, **kwargs):
        def publish(source, output):
            if self.failure == "collision":
                output.write_bytes(b"retain concurrent output")
                raise FileExistsError("synthetic collision")
            source.rename(output)
            if self.failure == "publish-uncertain":
                raise OSError("synthetic ambiguous publication")
        with self.context(**kwargs), patch.object(component, "publish_no_replace", side_effect=publish):
            return entry.build_entry(self.app, self.workspace, self.frozen)

    def reject(self):
        with self.assertRaises((component.ComponentError, OSError)):
            self.build()
        self.assertFalse((self.workspace / "postinstall").exists())
        self.assertFalse(list(self.workspace.glob(".scribe-full-release-entry-*")))

    def test_intel_host_builds_a_fresh_native_x64_verifier(self):
        with self.context(), patch.object(full.platform, "machine", return_value="x86_64"), \
             patch.object(component, "publish_no_replace", side_effect=lambda source, output: source.rename(output)):
            entry.build_entry(self.app, self.workspace, self.frozen)
        args, _ = self.builds[0]
        self.assertEqual(args[args.index("--target") + 1], "x86_64-apple-darwin")
        self.assertEqual(len(self.verifications), 2)

    def test_mocked_construction_binds_two_native_packs_and_one_frozen_release(self):
        output, sha, size = self.build()
        self.assertEqual((sha, size), (digest(IMAGE), len(IMAGE)))
        self.assertEqual(output.read_bytes(), IMAGE)
        self.assertEqual([arch for _, arch in self.verifications], list(entry.ARCHES))
        self.assertEqual(len(self.builds), 3)
        release_paths = set()
        for args, env in self.builds:
            for flag in ("--locked", "--offline", "--release", "--no-default-features"):
                self.assertIn(flag, args)
            self.assertEqual(env["SCRIBE_BUILD_REVISION"], REVISION)
            self.assertEqual(env["SCRIBE_BUNDLED_WORKER_SHA256"], SHA)
            self.assertEqual(env["MACOSX_DEPLOYMENT_TARGET"], "13.0")
            if "--features" in args:
                self.assertEqual(args[args.index("--features") + 1], "installer-package-entry")
                arch = args[args.index("--target") + 1].split("-apple")[0]
                self.assertEqual(env["SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256"], self.manifests[arch])
                self.assertIn(f"metal-{arch}", env["SCRIBE_MACOS_METAL_PACKAGE_PACK"])
                release_paths.add((env["SCRIBE_GPU_PACK_CATALOG"], env["SCRIBE_GPU_PACK_RELEASE_AUTHORITY"]))
        self.assertEqual(len(release_paths), 1)
        for args, _ in self.calls:
            self.assertFalse({"--sign", "install", "describe", "pkgbuild", "productbuild"}.intersection(args))
            self.assertNotIn(Path(args[0]).name, ("postinstall", "scribe-inference-worker", "scribe-macos-metal-pack-installer"))
        signed = [args[-1] for args, _ in self.calls if args[0] == "/usr/bin/codesign"]
        self.assertEqual(len(signed), 2)
        self.assertEqual(signed[0], str(self.app / component.CPU_APP_INPUTS[1]))
        self.assertFalse(list(self.workspace.iterdir()) == [])
        self.assertEqual(list(self.workspace.iterdir()), [output])

    def test_foreign_root_and_unknown_host_reject_before_tools(self):
        for host, uid in [("linux", 501), ("win32", 501), ("darwin", 0)]:
            with self.subTest(host=host), self.context(), patch.object(full.sys, "platform", host), \
                 patch.object(full.os, "geteuid", return_value=uid), self.assertRaises(component.ComponentError):
                entry.build_entry(self.app, self.workspace, self.frozen)
        with self.context(), patch.object(full.platform, "machine", return_value="arm64e"), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, self.frozen)
        self.assertEqual(self.calls, [])

    def test_frozen_document_cpu_and_arch_manifest_mismatches_reject(self):
        saved = self.frozen
        for field in ("cpu_worker_sha256", "catalog_sha256", "authority_sha256", "aarch64_manifest_sha256", "x86_64_manifest_sha256"):
            self.frozen = replace(saved, **{field: "0" * 64})
            self.reject()
        self.frozen = replace(saved, aarch64_manifest_sha256=saved.x86_64_manifest_sha256,
                              x86_64_manifest_sha256=saved.aarch64_manifest_sha256)
        self.reject()
        self.assertEqual(self.calls, [])

    def test_invalid_frozen_identity_rejects(self):
        for change in [dict(revision="short"), dict(catalog_sha256="invalid")]:
            saved = self.frozen
            self.frozen = replace(saved, **change)
            self.reject()
            self.frozen = saved
        self.assertEqual(self.calls, [])

    def test_missing_duplicate_and_foreign_architectures_reject(self):
        saved = list(self.packs)
        for packs in [saved[:1], [saved[0], saved[0]], [saved[0], {**saved[1], "target_arch": "arm64"}]]:
            self.packs = packs
            self.refresh_documents()
            self.reject()
        self.assertEqual(self.calls, [])

    def test_release_join_provider_layout_and_revision_reject(self):
        saved_pack, saved_release = dict(self.packs[0]), dict(self.releases[0])
        for field, value in [("provider", "wrong"), ("root", "../escape"), ("pack_id", "../escape"),
                             ("backend", "cpu"), ("target_os", "windows"), ("worker_relative_path", "other"),
                             ("security_epoch", True), ("runtime_abi_version", 0), ("files", [])]:
            self.packs[0] = {**saved_pack, field: value}
            self.releases[0] = {**saved_release, field: value}
            self.refresh_documents()
            self.reject()
        self.packs[0] = saved_pack
        for field, value in [("build_revision", "wrong-build-revision"), ("pack_digest", "0" * 64)]:
            self.releases[0] = {**saved_release, field: value}
            self.refresh_documents()
            self.reject()
        self.assertEqual(self.calls, [])

    def test_empty_release_and_terminal_lf_do_not_reach_tools(self):
        for name in (entry.CATALOG, entry.AUTHORITY):
            saved = (self.app / name).read_bytes()
            (self.app / name).write_bytes(saved + b"\n")
            self.reject()
            (self.app / name).write_bytes(saved)
        self.packs, self.releases = [], []
        self.refresh_documents()
        self.reject()
        self.assertEqual(self.calls, [])

    def test_shared_json_bound_is_explicit_closed_and_cpu_default_unchanged(self):
        large = encoded({"padding": "x" * component.MAX_JSON})
        with self.assertRaises(component.ComponentError):
            component.strict_json(large)
        self.assertEqual(component.strict_json(large, entry.MAX_DOCUMENT)["padding"], "x" * component.MAX_JSON)
        for maximum in [0, -1, True, entry.MAX_DOCUMENT + 1]:
            with self.assertRaises(component.ComponentError):
                component.strict_json(b"{}", maximum)
        for data in [b'{"x":1,"x":2}', b'{"x":NaN}', b'{"x":Infinity}', b"[]", b"\xff"]:
            with self.assertRaises(component.ComponentError):
                component.strict_json(data, entry.MAX_DOCUMENT)

    def test_exact_application_tree_rejects_extra_empty_directory_missing_file_and_hardlink(self):
        extra = self.app / "extra"
        extra.mkdir()
        self.reject()
        extra.rmdir()
        path = self.app / "Contents/embedded.provisionprofile"
        saved = path.read_bytes()
        path.unlink()
        self.reject()
        path.write_bytes(saved)
        linked = self.root / "linked-profile"
        os.link(path, linked)
        self.reject()
        self.assertEqual(self.calls, [])

    def test_existing_output_and_app_workspace_are_preserved(self):
        output = self.workspace / "postinstall"
        output.write_bytes(b"retain")
        with self.context(), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, self.frozen)
        self.assertEqual(output.read_bytes(), b"retain")
        for workspace in [self.app, self.app / "Contents", Path("relative")]:
            with self.context(), self.assertRaises(component.ComponentError):
                entry.build_entry(self.app, workspace, self.frozen)
        self.assertEqual(self.calls, [])

    def test_application_content_descriptor_and_signature_failures_never_publish(self):
        for failure in ["application", "signature", "content", "descriptor"]:
            with self.subTest(failure=failure):
                self.failure = failure
                self.reject()
        self.assertEqual(len(self.builds), 2)  # only the two content-verifier builds

    def test_build_join_checks_version_protocol_and_installed_size(self):
        saved = dict(self.releases[0])
        for field, value in [("app_version", "9.0.0"), ("app_protocol_version", True)]:
            self.releases[0] = {**saved, field: value}
            self.refresh_documents()
            self.reject()
        self.releases[0] = saved
        self.packs[0]["installed_size_bytes"] += 1
        self.releases[0]["installed_size_bytes"] += 1
        self.refresh_documents()
        self.reject()

    def test_original_private_and_verifier_changes_never_publish(self):
        for failure in ["private-change", "source-change", "source-replace", "verifier-change"]:
            with self.subTest(failure=failure):
                self.failure = failure
                self.builds.clear()
                self.reject()
                self.refresh_documents()

    def test_wrong_or_universal_native_tool_and_slice_architectures_reject(self):
        for failure in ["author-wrong-arch", "author-fat-arch", "entry-wrong-arch", "entry-fat-arch", "missing-universal"]:
            self.failure = failure
            self.builds.clear()
            self.verifications.clear()
            self.reject()
            self.assertEqual(len(self.builds), 3 if failure == "missing-universal" else 2 if failure.startswith("entry-") else 1)
            self.assertEqual(len(self.verifications), 2 if failure == "missing-universal" or failure.startswith("entry-") else 0)

    def test_missing_hardlinked_and_oversized_outputs_reject(self):
        for failure in ["missing", "hardlink"]:
            self.failure = failure
            self.reject()
        self.failure = None
        with patch.object(entry, "MAX_SLICE", 1):
            self.reject()
        with patch.object(entry, "MAX_ENTRY", 1):
            self.reject()

    def test_outer_entry_gpu_dependency_or_wrong_deployment_target_rejects(self):
        for failure in ["gpu-dependency", "minos"]:
            self.failure = failure
            self.calls.clear()
            self.reject()
            self.assertTrue(any(args[:2] == ["/usr/bin/lipo", "-create"] for args, _ in self.calls))
            self.assertTrue(any(args[:2] == ["/usr/bin/otool", "-l"] and "postinstall" in args[-1] for args, _ in self.calls))

    def test_tool_failures_deadline_and_unconfirmed_cleanup_do_not_publish(self):
        for failure in ["build", "entry-build", "deadline", "combine", "uncertain"]:
            self.failure = failure
            self.reject()
        with self.context(), patch.object(full.shutil, "which", return_value=None), self.assertRaises(component.ComponentError):
            entry.build_entry(self.app, self.workspace, self.frozen)

    def test_publication_collision_and_ambiguous_rename_retain_final(self):
        for failure, expected in [("collision", b"retain concurrent output"), ("publish-uncertain", IMAGE)]:
            with tempfile.TemporaryDirectory(dir=self.root) as temporary:
                self.workspace = Path(temporary)
                self.failure = failure
                with self.assertRaises(OSError):
                    self.build()
                self.assertEqual((self.workspace / "postinstall").read_bytes(), expected)

    def test_ambient_loader_compiler_signing_and_release_overrides_are_sanitized(self):
        variables = ["DYLD_INSERT_LIBRARIES", "RUSTFLAGS", "RUSTC_WRAPPER", "CARGO_TARGET_DIR",
                     "SCRIBE_GPU_PACK_CATALOG", "SCRIBE_GPU_PACK_RELEASE_AUTHORITY",
                     "SCRIBE_MACOS_METAL_PACKAGE_PACK", "SCRIBE_MACOS_SIGNING_IDENTITY"]
        with patch.dict(os.environ, {name: "untrusted" for name in variables}):
            self.build()
        for _, env in self.builds:
            for name in variables:
                self.assertNotEqual(env.get(name), "untrusted")
            self.assertEqual(env["CARGO_NET_OFFLINE"], "true")
        for args, kwargs in self.calls:
            if Path(args[0]).name == "scribe-worker-pack-tool":
                self.assertNotIn("environment", kwargs)  # sterile capture default

    def test_metal_bounds_do_not_weaken_cpu_worker_or_writer(self):
        self.assertEqual(component.MAX_WORKER, 512 * 1024 * 1024)
        self.assertEqual(component.MAX_WRITER, 64 * 1024 * 1024)
        self.assertEqual(entry.MAX_SLICE, entry.MAX_METAL_WORKER + entry.MAX_MANIFEST + entry.MAX_SIGNATURE
                         + 2 * entry.MAX_DOCUMENT + 32 * 1024 * 1024)
        self.assertEqual(entry.MAX_ENTRY, 2 * entry.MAX_SLICE)


class Posix(Portable):
    def test_symlinked_input_and_output_reject(self):
        extra = self.app / "link"
        extra.symlink_to(self.root, target_is_directory=True)
        self.reject()
        extra.unlink()
        self.failure = "symlink"
        self.reject()

    def test_uncertain_cleanup_retains_owned_private_workspace(self):
        self.failure = "uncertain"
        with self.context(real_workspace=True), self.assertRaises(component.CleanupUncertain):
            entry.build_entry(self.app, self.workspace, self.frozen)
        retained = list(self.workspace.glob(".scribe-full-release-entry-*"))
        self.assertEqual(len(retained), 1)
        self.assertTrue((retained[0] / "app" / entry.CATALOG).is_file())
        self.assertFalse((self.workspace / "postinstall").exists())
        # All children mocked; outer fixture cleanup cannot affect live code.


def suite():
    result = unittest.TestSuite()
    for cls in [Portable] + ([Posix] if os.name == "posix" else []):
        result.addTests(cls(name) for name in sorted(cls.__dict__) if name.startswith("test_"))
    expected = 21 + (2 if os.name == "posix" else 0)
    if result.countTestCases() != expected:
        raise RuntimeError(f"Metal construction discovery mismatch: expected {expected}, got {result.countTestCases()}")
    return result


if __name__ == "__main__":
    outcome = unittest.TextTestRunner(verbosity=2).run(suite())
    raise SystemExit(0 if outcome.wasSuccessful() else 1)
