"""Inactive complete-release unsigned product owner; no CLI or installation.

One retained full app/copy/verifier scope spans ALL freshly built components,
product construction and outer readback. Independent component constructors or
caller-supplied hash receipts never substitute for that lexical ownership.
The unchanged publisher policy denies before inputs/tools. No key discovery,
policy override, package signing, release activation or GPU Auto exists here.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import sys

import macos_app_pkg as app_component
import macos_app_pkg_codec as app_codec
import macos_cpu_component as component
import macos_cpu_pkg as worker_codec
import macos_cpu_pkg_entry as cpu_entry
import macos_cpu_pkg_final as signing
import macos_full_release as full
import macos_full_release_cpu_pkg_final as cpu_component
import macos_metal_pkg as metal_component
import macos_metal_pkg_entry as metal_entry
import macos_product_pkg_codec as codec


def require(condition: bool, message: str) -> None:
    component.require(condition, "product owner: " + message)


def build_product(app: Path, workspace: Path, frozen: full.FrozenInputs, signing_identity_sha1: str) -> tuple[Path, str, int]:
    """Own fresh CPU/Metal/app components and publish only unsigned Scribe.pkg."""
    publisher, policy = signing._publisher()
    require(sys.platform == "darwin" and os.geteuid() != 0, "requires an unprivileged Mac author")
    require(type(frozen) is full.FrozenInputs, "invalid full frozen inputs")
    for digest in (frozen.cpu_worker_sha256, frozen.catalog_sha256, frozen.authority_sha256,
                   frozen.aarch64_manifest_sha256, frozen.x86_64_manifest_sha256):
        component.validate_identity(digest, frozen.revision)
    require(type(signing_identity_sha1) is str and re.fullmatch(r"[0-9a-f]{40}", signing_identity_sha1) is not None,
            "invalid public signing identity selector")
    require(app.is_absolute() and app.is_dir() and not app.is_symlink(), "unsafe app input")
    app = app.resolve(strict=True)
    require(workspace.is_absolute() and workspace.is_dir() and not workspace.is_symlink()
            and not any(ord(char) < 32 for char in str(workspace)), "unsafe author workspace")
    workspace = workspace.resolve(strict=True)
    component.require_outside_app(workspace, app)
    output = workspace / "Scribe.pkg"
    require(not os.path.lexists(output), "product output already exists")
    packs, _ = full.release_inputs(app, frozen)
    files = full.input_files(packs)
    before = full.snapshot(app, files)
    bounds = dict(zip(codec.NAMES, (worker_codec._package_bound(cpu_entry.MAX_ENTRY),
        worker_codec._metal_package_bound(metal_entry.MAX_ENTRY),
        app_codec._package_bound(app_component._inventory(files, before, packs)))))

    def source_unchanged():
        require(full.snapshot(app, files) == before, "full same-app baseline changed")
        signing._unchanged_policy(policy)

    with component.private_workspace(workspace, ".scribe-product-pkg-") as private:
        admission, packages = private / "admission", private / "packages"
        admission.mkdir(mode=0o700)
        packages.mkdir(mode=0o700)
        records = {}

        def components_unchanged():
            with os.scandir(packages) as entries:
                names = []
                for node in entries:
                    names.append(node.name)
                    require(len(names) <= len(records), "unexpected component workspace entry")
            require(set(names) == set(records), "owned component inventory changed")
            for path, maximum, sha, size, identity in records.values():
                require(component.hash_file(path, maximum) == (sha, size)
                        and component.fingerprint(path.lstat()) == identity, "owned component changed")

        def retain(name, result):
            require(type(result) is tuple and len(result) == 3, "invalid own component result")
            path, sha, size = result
            require(path == packages / name and type(sha) is str and re.fullmatch(r"[0-9a-f]{64}", sha) is not None
                    and type(size) is int and 0 < size <= bounds[name], "own component path or identity differs")
            identity = component.fingerprint(path.lstat())
            require(component.hash_file(path, bounds[name]) == (sha, size)
                    and component.fingerprint(path.lstat()) == identity, "own component readback differs")
            records[name] = (path, bounds[name], sha, size, identity)

        with full.owned_release(app, admission, frozen) as owned:
            def unchanged():
                source_unchanged()
                owned.check_unchanged()
                components_unchanged()

            unchanged()
            signing._signature(app, publisher, "application")
            unchanged()
            retain("cpu-worker.pkg", cpu_component.build_component(app, packages, frozen, signing_identity_sha1))
            unchanged()
            retain("metal-worker.pkg", metal_component.build_component(app, packages, frozen, signing_identity_sha1))
            unchanged()
            retain("app.pkg", app_component.build_component(app, packages, frozen))
            unchanged()
            members, infos = codec._components(records, owned.version)
            unchanged()
            distribution = codec._distribution_source(owned.version, infos)
            controls = private / "Distribution.xml"
            with controls.open("xb") as handle:
                require(handle.write(distribution) == len(distribution), "short distribution write")
                handle.flush()
                os.fsync(handle.fileno())
                controls.chmod(0o644)
                controls_identity = (hashlib.sha256(distribution).hexdigest(), len(distribution),
                                     component.fingerprint(os.fstat(handle.fileno())))

            def controls_unchanged():
                require((*component.hash_file(controls, codec.MAX_METADATA), component.fingerprint(controls.lstat())) == controls_identity,
                        "owned distribution controls changed")

            unchanged()
            controls_unchanged()
            temporary = private / "tmp"
            temporary.mkdir(mode=0o700)
            candidate = private / "Scribe.pkg"
            component.capture(["/usr/bin/productbuild", "--distribution", str(controls), "--package-path", str(packages), str(candidate)],
                timeout=180, limit=8192, error_limit=8192, cwd=private,
                environment={**component.probe_environment(), "TMPDIR": str(temporary)}, operation="product-component-pkg-build")
            identity = component.fingerprint(candidate.lstat())
            unchanged()
            controls_unchanged()
            codec._inspect_product(candidate, records, members, infos, distribution)
            maximum = codec._product_bound(tuple(records[name][3] for name in codec.NAMES))
            measured = component.hash_file(candidate, maximum)
            require(component.fingerprint(candidate.lstat()) == identity, "product changed during inspection")
            unchanged()
            controls_unchanged()
        # All three builds and outer readback used ONE retained full baseline.
        # Its final admission check/cleanup must succeed before publication.
        source_unchanged()
        components_unchanged()
        controls_unchanged()
        require(component.hash_file(candidate, maximum) == measured
                and component.fingerprint(candidate.lstat()) == identity, "product changed before publication")
        component.publish_no_replace(candidate, output)
    return output, *measured
