"""Inactive Metal finalized-entry owner; never a PKG or installation authority.

The shared source-fixed publisher policy denies before caller input access.
Only this owner's freshly constructed Metal entry can be finalized. No CLI,
prebuilt input, signing callback, policy override or production key use is added.
"""
from __future__ import annotations

import os
from pathlib import Path
import re
import sys

import macos_cpu_component as component
import macos_cpu_pkg_final as signing
import macos_metal_pkg_entry as entry


def _source_snapshot(app: Path, frozen: entry.FrozenInputs) -> dict:
    # This is only a full input snapshot. build_entry below still owns fresh
    # application/production-pack verification, including the Rust crypto path.
    packs, _ = entry.release_inputs(app, frozen)
    return entry.snapshot(app, entry.input_files(packs))


def build_entry(app: Path, workspace: Path, frozen: entry.FrozenInputs,
                signing_identity_sha1: str) -> tuple[Path, str, int]:
    """Return own finalized postinstall bytes, not package or execution trust."""
    publisher, policy_before = signing._publisher()  # Before platform, inputs or tools.
    component.require(sys.platform == "darwin" and os.geteuid() != 0,
                      "Metal entry finalization requires an unprivileged Mac author")
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
    output = workspace / "postinstall"
    component.require(not os.path.lexists(output), "Metal finalized entry output already exists")
    before = _source_snapshot(app, frozen)
    signing._signature(app, publisher, "application")
    component.require(_source_snapshot(app, frozen) == before, "Metal inputs changed before construction")
    with component.private_workspace(workspace, ".scribe-metal-final-") as private:
        scripts = private / "scripts"
        scripts.mkdir(mode=0o700)
        candidate, presign_digest, presign_size = entry.build_entry(app, scripts, frozen)
        component.require(candidate == scripts / "postinstall", "Metal constructor returned wrong entry path")
        signing._only_entry(scripts)
        component.require(component.hash_file(candidate, entry.MAX_ENTRY) == (presign_digest, presign_size)
                          and os.access(candidate, os.X_OK), "Metal constructor identity or mode differs")
        component.require(_source_snapshot(app, frozen) == before, "Metal inputs changed during construction")
        signing._unchanged_policy(policy_before)
        component.capture(["/usr/bin/codesign", "--force", "--sign", signing_identity_sha1,
                           "--options", "runtime", "--timestamp", "--identifier",
                           signing.CODE_IDENTIFIERS["metal"], str(candidate)],
                          timeout=120, limit=8192, error_limit=8192, operation="final-entry-signing")
        finalized = component.hash_file(candidate, entry.MAX_ENTRY)
        identity = component.fingerprint(candidate.lstat())
        signing._only_entry(scripts)
        signing._signature(candidate, publisher, "metal")
        for architecture in signing.ARCHITECTURES:
            signing._properties(component.capture(["/usr/bin/codesign", "--display", "--verbose=4", "--arch",
                                                   architecture, str(candidate)], limit=8192,
                                                  error_limit=signing.MAX_PROPERTIES,
                                                  operation="final-entry-properties", signature_properties=True))
            signing._entitlements(component.capture(["/usr/bin/codesign", "--display", "--arch", architecture,
                                                     "--entitlements", "-", "--xml", str(candidate)],
                                                    limit=component.MAX_JSON, error_limit=8192,
                                                    operation="final-entry-entitlements"))
        component.inspect_universal_structure(candidate, [frozen.revision.encode(), entry.METAL_ROOT.encode()],
                                              private, entry.MAX_ENTRY)
        component.require(_source_snapshot(app, frozen) == before, "Metal inputs changed during finalization")
        signing._unchanged_policy(policy_before)
        signing._only_entry(scripts)
        component.require(component.hash_file(candidate, entry.MAX_ENTRY) == finalized
                          and component.fingerprint(candidate.lstat()) == identity,
                          "Metal entry changed during finalization")
        component.publish_no_replace(candidate, output)
    return output, *finalized
