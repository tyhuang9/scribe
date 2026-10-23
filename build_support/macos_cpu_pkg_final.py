"""Inactive, build-only CPU entry finalization; never Installer/root authority.

The checked-in publisher policy denies before input access or native tools.
Only this owner's freshly built entry can be signed. No CLI, policy override,
prebuilt-entry input, signing callback, key discovery or installation exists.
Synthetic tests cannot qualify native signatures or a production publisher.
"""
from __future__ import annotations

import os
from pathlib import Path
import re
import sys

import macos_cpu_component as component
import macos_cpu_pkg as package
import macos_cpu_pkg_entry as entry
from macos_cpu_pkg_profile import xml_tree

POLICY_PATH = component.REPO / "runtime-manifests/macos-installer-entry-signing-policy.json"
CODE_IDENTIFIERS = {"application": "com.scribe.local-transcriber",
                    "cpu": "com.scribe.local-transcriber.installer.cpu",
                    "metal": "com.scribe.local-transcriber.installer.metal"}
ARCHITECTURES = ("arm64", "x86_64")
MAX_PROPERTIES = 16384


def _publisher() -> tuple[str, tuple]:
    try:
        identity = component.fingerprint(POLICY_PATH.lstat())
        policy = component.strict_json(component.read_bounded(POLICY_PATH, component.MAX_JSON))
        measured = component.hash_file(POLICY_PATH, component.MAX_JSON)
        component.require(component.fingerprint(POLICY_PATH.lstat()) == identity,
                          "entry signing policy changed during admission")
    except OSError:
        raise component.ComponentError("entry signing policy is unavailable") from None
    component.require(set(policy) == {"schema_version", "mode", "publisher_team_id"}
                      and type(policy["schema_version"]) is int and policy["schema_version"] == 1,
                      "invalid entry signing policy")
    if policy["mode"] == "default_deny" and policy["publisher_team_id"] == "":
        raise component.ComponentError("entry signing publisher is not provisioned")
    component.require(policy["mode"] == "developer_id" and type(policy["publisher_team_id"]) is str
                      and re.fullmatch(r"[A-Z0-9]{10}", policy["publisher_team_id"]) is not None,
                      "invalid entry signing policy")
    return policy["publisher_team_id"], (*measured, identity)


def _requirement(publisher: str, role: str) -> str:
    component.require(type(publisher) is str and re.fullmatch(r"[A-Z0-9]{10}", publisher) is not None
                      and type(role) is str and role in CODE_IDENTIFIERS,
                      "invalid entry signature requirement")
    # Apple TN3127: the OID tests are meaningful only with the Apple anchor.
    # No Mac App Store/development/Installer-certificate alternative is allowed.
    return ('=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists '
            'and certificate leaf[field.1.2.840.113635.100.6.1.13] exists '
            f'and certificate leaf[subject.OU] = "{publisher}" '
            f'and identifier "{CODE_IDENTIFIERS[role]}"')


def _signature(path: Path, publisher: str, role: str) -> None:
    component.capture(["/usr/bin/codesign", "--verify", "--strict", "--all-architectures",
                       "--test-requirement", _requirement(publisher, role), str(path)],
                      operation="final-entry-signature", limit=8192, error_limit=8192)


def _properties(data: bytes) -> None:
    """Closed checks on bounded trusted-tool display, never publisher authority.

    Signature/requirement verification is independent. Unknown display formats
    fail closed; native qualification remains required before policy activation.
    Neither certificate text nor timestamp spelling is retained or reported.
    """
    component.require(type(data) is bytes and 0 < len(data) <= MAX_PROPERTIES,
                      "invalid finalized entry properties")
    try:
        lines = data.decode("utf-8").splitlines()
    except UnicodeError:
        raise component.ComponentError("invalid finalized entry properties") from None
    directories = [line for line in lines if line.startswith("CodeDirectory ")]
    component.require(len(directories) == 1 and re.fullmatch(
        r"CodeDirectory v=[0-9]{1,8} size=[0-9]{1,12} flags=0x10000\(runtime\) "
        r"hashes=[0-9]{1,12}\+[0-9]{1,4} location=embedded", directories[0]) is not None,
        "finalized entry requires exact hardened-runtime properties")
    timestamps = [line[len("Timestamp="):] for line in lines if line.startswith("Timestamp=")]
    component.require(len(timestamps) == 1 and 0 < len(timestamps[0]) <= 128
                      and all(32 <= ord(char) < 127 for char in timestamps[0])
                      and timestamps[0].strip().lower() not in ("", "none", "<none>", "not set")
                      and not any(line.startswith("Signed Time=") for line in lines),
                      "finalized entry requires verified timestamp metadata")


def _entitlements(data: bytes) -> None:
    component.require(type(data) is bytes and len(data) <= component.MAX_JSON,
                      "invalid finalized entry entitlements")
    if data == b"":  # codesign emits no plist when the signature has no entitlements.
        return
    standard_doctype = b'<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
    without_standard = data.replace(standard_doctype, b"", 1)
    try:
        root = xml_tree(without_standard)
    except component.ComponentError:
        raise component.ComponentError("invalid finalized entry entitlements") from None
    # plistlib keeps the LAST top-level value. Check the actual one-value XML
    # grammar instead; duplicate dicts/scalars/attributes/text must not disappear.
    component.require(root.tag == "plist" and root.attrib == {"version": "1.0"}
                      and not (root.text or "").strip() and not (root.tail or "").strip()
                      and len(root) == 1 and root[0].tag == "dict" and not root[0].attrib
                      and not len(root[0]) and not (root[0].text or "").strip()
                      and not (root[0].tail or "").strip(), "finalized entry must have no entitlements")


def _only_entry(scripts: Path) -> None:
    with os.scandir(scripts) as inventory:
        first, second = next(inventory, None), next(inventory, None)
        component.require(first is not None and first.name == "postinstall" and second is None,
                          "finalized entry inventory differs")


def _unchanged_policy(expected: tuple) -> None:
    component.require((*component.hash_file(POLICY_PATH, component.MAX_JSON),
                       component.fingerprint(POLICY_PATH.lstat())) == expected,
                      "entry signing policy changed during finalization")


def build_component(app: Path, workspace: Path, digest: str, revision: str,
                    signing_identity_sha1: str) -> tuple[Path, str, int]:
    """Build an unsigned component containing this owner's finalized CPU entry.

    The public certificate fingerprint selects a key; it grants no authority.
    Only the separate reviewed publisher requirement decides acceptance. The
    result is still an UNSIGNED package, not authenticated installation input.
    """
    publisher, policy_before = _publisher()  # Before platform, caller inputs, or tools.
    component.require(sys.platform == "darwin" and os.geteuid() != 0,
                      "entry finalization requires an unprivileged Mac author")
    component.validate_identity(digest, revision)
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
    component.require(not os.path.lexists(output), "output already exists")
    before = component.cpu_app_input_snapshot(app)
    component.require(before[component.CPU_APP_INPUTS[0]][0] == digest, "worker differs from frozen input")
    version = component.verify_cpu_app(app)  # Existing CPU-only/default-deny admission, unchanged.
    _signature(app, publisher, "application")
    component.require(component.cpu_app_input_snapshot(app) == before, "app changed before entry construction")
    with component.private_workspace(workspace, ".scribe-cpu-final-") as private:
        scripts = private / "scripts"
        scripts.mkdir(mode=0o700)
        candidate, presign_digest, presign_size = entry.build_entry(app, scripts, digest, revision)
        component.require(candidate == scripts / "postinstall", "entry constructor returned wrong path")
        _only_entry(scripts)
        component.require(component.hash_file(candidate, entry.MAX_ENTRY) == (presign_digest, presign_size)
                          and os.access(candidate, os.X_OK), "entry constructor identity or mode differs")
        component.capture(["/usr/bin/codesign", "--force", "--sign", signing_identity_sha1,
                           "--options", "runtime", "--timestamp", "--identifier", CODE_IDENTIFIERS["cpu"], str(candidate)],
                          timeout=120, limit=8192, error_limit=8192, operation="final-entry-signing")
        # Remeasure after signing, then prove every verification kept those bytes.
        finalized = component.hash_file(candidate, entry.MAX_ENTRY)
        identity = component.fingerprint(candidate.lstat())
        _only_entry(scripts)
        _signature(candidate, publisher, "cpu")
        for architecture in ARCHITECTURES:
            _properties(component.capture(["/usr/bin/codesign", "--display", "--verbose=4", "--arch",
                                           architecture, str(candidate)], limit=8192, error_limit=MAX_PROPERTIES,
                                          operation="final-entry-properties", signature_properties=True))
            _entitlements(component.capture(["/usr/bin/codesign", "--display", "--arch", architecture,
                                             "--entitlements", "-", "--xml", str(candidate)],
                                            limit=component.MAX_JSON, error_limit=8192,
                                            operation="final-entry-entitlements"))
        component.inspect_universal_structure(candidate, [digest.encode(), revision.encode(), component.CPU_ROOT.encode()],
                                              private, entry.MAX_ENTRY)
        component.require(component.hash_file(candidate, entry.MAX_ENTRY) == finalized
                          and component.fingerprint(candidate.lstat()) == identity,
                          "entry changed during final signature verification")
        component.verify_cpu_app(app)
        component.require(component.cpu_app_input_snapshot(app) == before, "app changed during entry finalization")
        _unchanged_policy(policy_before)
        _only_entry(scripts)
        package_path, package_digest, package_size = package._package_constructed_entry(
            app, private, before, version, (candidate, *finalized))
        _unchanged_policy(policy_before)
        component.publish_no_replace(package_path, output)
    return output, package_digest, package_size
