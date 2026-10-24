"""Inactive full-release CPU finalized-entry/component owner; never root trust.

The fixed publisher policy denies before caller inputs or tools. Only our own
fresh CPU entry from the complete frozen release reaches signing/packaging.
The result is an UNSIGNED PKG, not authenticated installation input. No CLI,
prebuilt input, policy override, key discovery or installation exists.
"""
from __future__ import annotations

import os
from pathlib import Path
import plistlib
import re
import sys

import macos_cpu_component as component
import macos_cpu_pkg as package
import macos_cpu_pkg_entry as cpu
import macos_cpu_pkg_final as signing
import macos_full_release as full
import macos_full_release_cpu_entry as entry


def _unchanged(app: Path, files: tuple, before: tuple) -> None:
    component.require(tuple(sorted(full.snapshot(app, dict(files)).items())) == before,
                      "full release changed during CPU finalization or packaging")


def build_component(app: Path, workspace: Path, frozen: full.FrozenInputs,
                    signing_identity_sha1: str) -> tuple[Path, str, int]:
    """Finalize our fresh CPU entry and return only unsigned package bytes."""
    publisher, policy_before = signing._publisher()  # Before platform, inputs or tools.
    component.require(sys.platform == "darwin" and os.geteuid() != 0,
                      "full-release CPU finalization requires an unprivileged Mac author")
    component.require(type(frozen) is full.FrozenInputs, "invalid full frozen inputs")
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
    output = workspace / "cpu-worker.pkg"
    component.require(not os.path.lexists(output), "CPU component output already exists")
    # Retain ONE complete inventory/byte/node baseline across ALL phases. This
    # snapshot is not admission: the fixed constructor below verifies the app
    # and both production packs itself, with its fresh native Rust verifier.
    packs, _ = full.release_inputs(app, frozen)
    files = tuple(sorted(full.input_files(packs).items()))
    before = tuple(sorted(full.snapshot(app, dict(files)).items()))
    info = plistlib.loads(component.read_bounded(app / "Contents/Info.plist", component.MAX_JSON))
    version = info.get("CFBundleShortVersionString") if type(info) is dict else None
    component.require(type(version) is str and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None,
                      "invalid application version")
    signing._signature(app, publisher, "application")
    _unchanged(app, files, before)
    with component.private_workspace(workspace, ".scribe-full-cpu-final-") as private:
        scripts = private / "scripts"
        scripts.mkdir(mode=0o700)
        operation, presign_digest, presign_size = entry.build_entry(app, scripts, frozen)
        component.require(operation == scripts / "postinstall", "CPU constructor returned wrong entry path")
        signing._only_entry(scripts)
        component.require(component.hash_file(operation, cpu.MAX_ENTRY) == (presign_digest, presign_size)
                          and os.access(operation, os.X_OK), "CPU constructor identity or mode differs")
        presign_identity = component.fingerprint(operation.lstat())
        _unchanged(app, files, before)
        signing._unchanged_policy(policy_before)
        component.require(component.hash_file(operation, cpu.MAX_ENTRY) == (presign_digest, presign_size)
                          and component.fingerprint(operation.lstat()) == presign_identity,
                          "CPU entry changed before signing")
        component.capture(["/usr/bin/codesign", "--force", "--sign", signing_identity_sha1,
                           "--options", "runtime", "--timestamp", "--identifier",
                           signing.CODE_IDENTIFIERS["cpu"], str(operation)], timeout=120,
                          limit=8192, error_limit=8192, operation="final-entry-signing")
        finalized = component.hash_file(operation, cpu.MAX_ENTRY)
        identity = component.fingerprint(operation.lstat())
        signing._only_entry(scripts)
        signing._signature(operation, publisher, "cpu")
        for architecture in signing.ARCHITECTURES:
            signing._properties(component.capture(["/usr/bin/codesign", "--display", "--verbose=4", "--arch",
                                                   architecture, str(operation)], limit=8192,
                                                  error_limit=signing.MAX_PROPERTIES,
                                                  operation="final-entry-properties", signature_properties=True))
            signing._entitlements(component.capture(["/usr/bin/codesign", "--display", "--arch", architecture,
                                                     "--entitlements", "-", "--xml", str(operation)],
                                                    limit=component.MAX_JSON, error_limit=8192,
                                                    operation="final-entry-entitlements"))
        component.inspect_universal_structure(operation, [frozen.cpu_worker_sha256.encode(), frozen.revision.encode(),
                                                          component.CPU_ROOT.encode()], private, cpu.MAX_ENTRY)
        _unchanged(app, files, before)
        signing._unchanged_policy(policy_before)
        signing._only_entry(scripts)
        component.require(component.hash_file(operation, cpu.MAX_ENTRY) == finalized
                          and component.fingerprint(operation.lstat()) == identity,
                          "CPU finalized entry changed before packaging")
        temporary = private / "temp"
        temporary.mkdir(mode=0o700)
        candidate = private / "cpu-worker.pkg"
        component.capture(["/usr/bin/pkgbuild", "--nopayload", "--scripts", str(scripts), "--identifier",
                           package.IDENTIFIER, "--version", version, "--install-location", "/", str(candidate)],
                          timeout=120, environment={**component.probe_environment(), "TMPDIR": str(temporary)},
                          limit=8192, error_limit=8192, operation="cpu-component-pkg-build")
        package_identity = component.fingerprint(candidate.lstat())
        package._inspect_fresh(candidate, *finalized, version)
        measured = component.hash_file(candidate, package._package_bound(finalized[1]))
        component.require(component.fingerprint(candidate.lstat()) == package_identity,
                          "CPU package changed across inspection and measurement")
        _unchanged(app, files, before)
        signing._unchanged_policy(policy_before)
        signing._only_entry(scripts)
        component.require(component.hash_file(operation, cpu.MAX_ENTRY) == finalized
                          and component.fingerprint(operation.lstat()) == identity,
                          "CPU retained entry changed during packaging")
        component.require(component.hash_file(candidate, package._package_bound(finalized[1])) == measured
                          and component.fingerprint(candidate.lstat()) == package_identity,
                          "CPU package changed during final admission")
        component.publish_no_replace(candidate, output)
    return output, *measured
