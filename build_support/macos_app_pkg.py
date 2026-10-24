"""Inactive app-component owner for its OWN freshly admitted full release.

No CLI, prebuilt package, policy override, signing selector or installation.
The fixed publisher policy denies before caller inputs or native tools. The
output, when independently provisioned, is an unsigned package, never root trust.
System tools and non-hostile same-user/root writers remain trusted assumptions.
"""
from __future__ import annotations

import ctypes
import errno
import os
from pathlib import Path
import plistlib
import stat
import sys

import macos_cpu_component as component
import macos_cpu_pkg_final as signing
import macos_full_release as full
import macos_app_pkg_codec as codec

APP_NAME = "Scribe.app"
IDENTIFIER = "com.scribe.local-transcriber.application"
CONTROLS = [{"RootRelativeBundlePath": APP_NAME, "BundleIsRelocatable": False,
             "BundleIsVersionChecked": False, "BundleHasStrictIdentifier": True,
             "BundleOverwriteAction": "upgrade"}]


def require(condition: bool, message: str) -> None:
    component.require(condition, "app component: " + message)


def _empty_acl(descriptor: int) -> None:
    """Darwin's descriptor ACL API, stricter than workspace-ancestor policy.

    Require zero ACEs, not merely no mutation grants. No ACL is silently removed.
    The existing native filesystem policy defines ENOENT/no-ACL and EINVAL/end
    enumeration. An allocated empty ACL is not claimed to lack object flags.
    """
    library = ctypes.CDLL(None, use_errno=True)
    pointer, integer = ctypes.c_void_p, ctypes.c_int
    library.acl_get_fd_np.argtypes, library.acl_get_fd_np.restype = [integer, integer], pointer
    library.acl_valid.argtypes, library.acl_valid.restype = [pointer], integer
    library.acl_get_entry.argtypes = [pointer, integer, ctypes.POINTER(pointer)]
    library.acl_get_entry.restype = integer
    library.acl_free.argtypes, library.acl_free.restype = [pointer], integer
    ctypes.set_errno(0)
    acl = library.acl_get_fd_np(descriptor, 0x100)
    if not acl:
        require(ctypes.get_errno() == errno.ENOENT, "cannot inspect staged ACL")
        return
    try:
        require(library.acl_valid(acl) == 0, "invalid staged ACL")
        entry = pointer()
        ctypes.set_errno(0)
        require(library.acl_get_entry(acl, 0, ctypes.byref(entry)) == -1
                and ctypes.get_errno() == errno.EINVAL, "staged node contains ACL entries")
    finally:
        require(library.acl_free(acl) == 0, "cannot release staged ACL")


def _metadata(path: Path, expected_mode: int, maximum: int) -> tuple:
    before = path.lstat()
    directory = stat.S_ISDIR(expected_mode)
    component.check_node(before, directory, maximum)
    require(before.st_mode == expected_mode and getattr(before, "st_flags", 0) == 0,
            "staged mode or flags differ")
    if os.name == "posix":
        require(before.st_uid == os.geteuid(), "staged node has a different owner")
        flags = os.O_RDONLY | os.O_NOFOLLOW | (os.O_DIRECTORY if directory else os.O_NONBLOCK)
        descriptor = os.open(path, flags)
        try:
            require(component.fingerprint(os.fstat(descriptor)) == component.fingerprint(before),
                    "staged node changed during metadata open")
            if component.NATIVE_DARWIN:
                _empty_acl(descriptor)
            if hasattr(os, "listxattr"):
                require(not os.listxattr(descriptor), "staged node contains extended attributes")
            require(component.fingerprint(os.fstat(descriptor)) == component.fingerprint(before),
                    "staged node changed during metadata inspection")
        finally:
            os.close(descriptor)
    require(component.fingerprint(path.lstat()) == component.fingerprint(before),
            "staged name changed during metadata inspection")
    return (component.fingerprint(before), getattr(before, "st_uid", None),
            getattr(before, "st_gid", None), getattr(before, "st_flags", 0))


def _inventory(files: dict[str, int], measured: dict, packs: dict) -> dict:
    executables = {component.CPU_APP_INPUTS[0], component.CPU_APP_INPUTS[1]}
    executables.update("Contents/Resources/" + pack["root"] + "/" + pack["worker_relative_path"]
                       for pack, _ in packs.values())
    require(executables <= set(files), "missing staged executable")
    expected = {".": (0o40755, "", 0), "./" + APP_NAME: (0o40755, "", 0)}
    for name in files:
        relative = "./" + APP_NAME + "/" + name
        expected[relative] = (0o100755 if name in executables else 0o100644, *measured[name][:2])
        for parent in Path(relative).parents:
            if parent.as_posix() != ".":
                expected["./" + parent.as_posix().removeprefix("./")] = (0o40755, "", 0)
    return expected


def _stage_snapshot(root: Path, expected: dict, files: dict[str, int]) -> tuple:
    with os.scandir(root) as entries:
        names = []
        for entry in entries:
            names.append(entry.name)
            require(len(names) <= 1, "unexpected staging root entry")
    require(names == [APP_NAME], "staging root inventory differs")
    snapshot = full.snapshot(root / APP_NAME, files)
    metadata = []
    for name, (mode, _digest, size) in sorted(expected.items()):
        metadata.append((name, _metadata(root / name.removeprefix("./"), mode, max(1, size))))
    return tuple(sorted(snapshot.items())), tuple(metadata)


def _stage(root: Path, source: Path, files: dict[str, int], measured: dict, expected: dict) -> tuple:
    root.mkdir(mode=0o755)
    root.chmod(0o755)
    for name, (mode, _digest, _size) in sorted(expected.items(), key=lambda item: (item[0].count("/"), item[0])):
        if name == ".":
            continue
        path = root / name.removeprefix("./")
        if stat.S_ISDIR(mode):
            path.mkdir(mode=0o755)
            path.chmod(0o755)
        else:
            original = name.removeprefix("./" + APP_NAME + "/")
            require(component.copy_new(source / original, path, files[original]) == measured[original][:2],
                    "staged copy differs from admitted release")
            path.chmod(stat.S_IMODE(mode))
    return _stage_snapshot(root, expected, files)


def build_component(app: Path, workspace: Path, frozen: full.FrozenInputs) -> tuple[Path, str, int]:
    """Own admission, canonical staging and inspection; publish only app.pkg."""
    publisher, policy = signing._publisher()  # Default denial before caller inputs.
    require(sys.platform == "darwin" and os.geteuid() != 0, "requires unprivileged Mac author")
    require(type(frozen) is full.FrozenInputs, "invalid full frozen inputs")
    for digest in (frozen.cpu_worker_sha256, frozen.catalog_sha256, frozen.authority_sha256,
                   frozen.aarch64_manifest_sha256, frozen.x86_64_manifest_sha256):
        component.validate_identity(digest, frozen.revision)
    require(app.is_absolute() and app.is_dir() and not app.is_symlink(), "unsafe app input")
    app = app.resolve(strict=True)
    require(workspace.is_absolute() and workspace.is_dir() and not workspace.is_symlink()
            and not any(ord(char) < 32 for char in str(workspace)), "unsafe author workspace")
    workspace = workspace.resolve(strict=True)
    component.require_outside_app(workspace, app)
    output = workspace / "app.pkg"
    require(not os.path.lexists(output), "app component output already exists")
    packs, _ = full.release_inputs(app, frozen)
    files = full.input_files(packs)
    before = full.snapshot(app, files)
    expected = _inventory(files, before, packs)

    def source_unchanged():
        require(full.snapshot(app, files) == before, "full release changed during app packaging")
        signing._unchanged_policy(policy)

    with component.private_workspace(workspace, ".scribe-app-pkg-") as private:
        admission = private / "admission"
        admission.mkdir(mode=0o700)
        with full.owned_release(app, admission, frozen) as owned:
            source_unchanged()
            signing._signature(app, publisher, "application")
            source_unchanged()
            owned.check_unchanged()
            info = plistlib.loads(component.read_bounded(owned.copied / "Contents/Info.plist", component.MAX_JSON))
            require(type(info) is dict and info.get("CFBundleIdentifier") == signing.CODE_IDENTIFIERS["application"]
                    and info.get("CFBundleVersion") == owned.version
                    and info.get("CFBundleShortVersionString") == owned.version, "app bundle identity differs")
            root = private / "payload"
            staged = _stage(root, owned.copied, files, before, expected)
            controls = private / "components.plist"
            with controls.open("xb") as handle:
                handle.write(plistlib.dumps(CONTROLS))
            controls.chmod(0o600)
            controls_identity = (*component.hash_file(controls, component.MAX_JSON), component.fingerprint(controls.lstat()))

            def unchanged():
                source_unchanged()
                owned.check_unchanged()
                require(_stage_snapshot(root, expected, files) == staged
                        and (*component.hash_file(controls, component.MAX_JSON),
                             component.fingerprint(controls.lstat())) == controls_identity,
                        "staged release or controls changed")

            unchanged()
            signing._signature(root / APP_NAME, publisher, "application")
            unchanged()
            source_paths = [root / name.removeprefix("./") for name, (mode, _, _) in sorted(expected.items())
                            if stat.S_ISREG(mode)]
            checksum_limit = sum(len(os.fsencode(path)) + 40 for path in source_paths)
            require(0 < checksum_limit <= 1024 * 1024, "source checksum output bound")
            source_output = component.capture(["/usr/bin/cksum", *map(str, source_paths)], timeout=120,
                limit=checksum_limit, error_limit=8192, environment=component.probe_environment(),
                operation="app-source-checksums")
            checksums = codec._source_checksums(source_output, root, expected)
            unchanged()
            temporary = private / "tmp"
            temporary.mkdir(mode=0o700)
            candidate = private / "app.pkg"
            component.capture(["/usr/bin/pkgbuild", "--root", str(root), "--component-plist", str(controls),
                "--ownership", "recommended", "--compression", "legacy", "--identifier", IDENTIFIER,
                "--version", owned.version, "--install-location", "/Applications", str(candidate)],
                timeout=120, limit=8192, error_limit=8192,
                environment={**component.probe_environment(), "TMPDIR": str(temporary)},
                operation="app-component-pkg-build")
            package_identity = component.fingerprint(candidate.lstat())
            unchanged()
            bom, bom_identity = codec._inspect_fresh(candidate, expected, owned.version, private)
            require(bom == private / "inspected.bom", "unexpected owned Bom path")
            measured = component.hash_file(candidate, codec._package_bound(expected))

            def inspected_unchanged():
                require(component.hash_file(candidate, codec._package_bound(expected)) == measured
                        and component.fingerprint(candidate.lstat()) == package_identity,
                        "app package changed during inspection")
                require((*component.hash_file(bom, codec._bom_bound(expected)), component.fingerprint(bom.lstat())) == bom_identity,
                        "owned Bom changed during inspection")

            unchanged()
            inspected_unchanged()
            listing = component.capture(["/usr/bin/lsbom", "-p", "fmugsc", str(bom)], timeout=30,
                limit=codec._listing_bound(expected), error_limit=8192, environment=component.probe_environment(),
                operation="app-bom-listing")
            unchanged()
            inspected_unchanged()
            codec._bom_listing(listing, expected, checksums)
        # Successful admission-scope exit, including its final recheck, precedes
        # publication. Its disposable copies/verifier are never caller receipts.
        source_unchanged()
        require(_stage_snapshot(root, expected, files) == staged, "retained staging changed before publication")
        inspected_unchanged()
        component.publish_no_replace(candidate, output)
    return output, *measured
