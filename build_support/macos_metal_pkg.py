"""Inactive owned Metal component construction; unsigned PKG, never root trust.

Only this owner's freshly finalized entry may reach its fixed packaging tail.
No CLI, prebuilt entry, policy override, key discovery or installation exists.
The shared publisher policy denies before input access or native tools.
"""
from __future__ import annotations

import os
from pathlib import Path
import plistlib
import re
import sys

import macos_cpu_component as component
import macos_cpu_pkg as package
import macos_cpu_pkg_final as signing
import macos_metal_pkg_entry as entry
import macos_metal_pkg_final as final

IDENTIFIER = package._METAL_IDENTIFIER


def build_component(app: Path, workspace: Path, frozen: entry.FrozenInputs,
                    signing_identity_sha1: str) -> tuple[Path, str, int]:
    """Return an unsigned package containing only our own post-sign entry."""
    _, policy_before = signing._publisher()  # Default denial precedes caller inputs/tools.
    component.require(sys.platform == "darwin" and os.geteuid() != 0,
                      "Metal component construction requires an unprivileged Mac author")
    component.require(type(frozen) is entry.FrozenInputs, "invalid Metal frozen inputs")
    for digest in (frozen.cpu_worker_sha256, frozen.catalog_sha256, frozen.authority_sha256,
                   frozen.aarch64_manifest_sha256, frozen.x86_64_manifest_sha256):
        component.validate_identity(digest, frozen.revision)
    component.require(type(signing_identity_sha1) is str
                      and re.fullmatch(r"[0-9a-f]{40}", signing_identity_sha1) is not None,
                      "invalid entry signing identity selector")
    component.require(app.is_absolute() and app.is_dir() and not app.is_symlink(), "unsafe application input")
    app = app.resolve(strict=True)
    component.require(workspace.is_absolute() and workspace.is_dir() and not workspace.is_symlink()
                      and not any(ord(char) < 32 for char in str(workspace)), "unsafe author workspace")
    workspace = workspace.resolve(strict=True)
    component.require_outside_app(workspace, app)
    output = workspace / "metal-worker.pkg"
    component.require(not os.path.lexists(output), "Metal component output already exists")
    before = final._source_snapshot(app, frozen)
    info = plistlib.loads(component.read_bounded(app / "Contents/Info.plist", component.MAX_JSON))
    version = info.get("CFBundleShortVersionString") if type(info) is dict else None
    component.require(type(version) is str and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None,
                      "invalid application version")
    with component.private_workspace(workspace, ".scribe-metal-pkg-") as private:
        scripts = private / "scripts"
        scripts.mkdir(mode=0o700)
        operation, sha, size = final.build_entry(app, scripts, frozen, signing_identity_sha1)
        component.require(operation == scripts / "postinstall", "Metal finalizer returned wrong entry path")
        signing._only_entry(scripts)
        component.require(component.hash_file(operation, entry.MAX_ENTRY) == (sha, size)
                          and os.access(operation, os.X_OK), "Metal finalized entry identity or mode differs")
        identity = component.fingerprint(operation.lstat())
        component.require(final._source_snapshot(app, frozen) == before, "Metal inputs changed before packaging")
        signing._unchanged_policy(policy_before)
        temporary = private / "temp"
        temporary.mkdir(mode=0o700)
        candidate = private / "metal-worker.pkg"
        component.capture(["/usr/bin/pkgbuild", "--nopayload", "--scripts", str(scripts), "--identifier", IDENTIFIER,
                           "--version", version, "--install-location", "/", str(candidate)], timeout=120,
                          environment={**component.probe_environment(), "TMPDIR": str(temporary)},
                          limit=8192, error_limit=8192, operation="metal-component-pkg-build")
        package_identity = component.fingerprint(candidate.lstat())
        package._inspect_metal_fresh(candidate, sha, size, version)
        measured = component.hash_file(candidate, package._metal_package_bound(size))
        component.require(component.fingerprint(candidate.lstat()) == package_identity,
                          "Metal package changed across inspection and measurement")
        component.require(final._source_snapshot(app, frozen) == before, "Metal inputs changed during packaging")
        signing._unchanged_policy(policy_before)
        signing._only_entry(scripts)
        component.require(component.hash_file(operation, entry.MAX_ENTRY) == (sha, size)
                          and component.fingerprint(operation.lstat()) == identity,
                          "Metal retained entry changed during packaging")
        component.require(component.hash_file(candidate, package._metal_package_bound(size)) == measured
                          and component.fingerprint(candidate.lstat()) == package_identity,
                          "Metal package changed during final admission")
        component.publish_no_replace(candidate, output)
    return output, *measured
