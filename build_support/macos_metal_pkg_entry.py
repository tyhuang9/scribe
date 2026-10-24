"""Internal PRE-SIGN Metal entry construction from one fully admitted release.

FrozenInputs and release-layout helper imports remain compatible. Admission
owns exact copies and both fresh Rust pack verifications; no signing/install.
"""
from __future__ import annotations

from pathlib import Path

import macos_cpu_component as component
import macos_full_release as full
# Compatibility exports used by the existing finalizer and source fixtures.
from macos_full_release import (ARCHES, AUTHORITY, CATALOG, FrozenInputs, MAX_DOCUMENT, MAX_MANIFEST,
                                MAX_METAL_WORKER, MAX_SIGNATURE, METAL_ROOT, PACK_FILES,
                                input_files, release_inputs, snapshot, verify_pack)

MAX_SLICE = MAX_METAL_WORKER + MAX_MANIFEST + MAX_SIGNATURE + 2 * MAX_DOCUMENT + 32 * 1024 * 1024
MAX_ENTRY = 2 * MAX_SLICE


def build_entry(app: Path, workspace: Path, frozen: FrozenInputs) -> tuple[Path, str, int]:
    """Return only own PRE-SIGN path/hash/size, never installation authority."""
    with full.owned_release(app, workspace, frozen) as owned:
        private, copied, output, packs = owned.private, owned.copied, owned.output, owned.packs
        environment = component.build_environment(frozen.cpu_worker_sha256, frozen.revision)
        cargo = owned.cargo
        slices = []
        for arch in ARCHES:
            pack, _ = packs[arch]
            root = copied / "Contents/Resources" / pack["root"]
            target = arch + "-apple-darwin"
            build_env = {**environment, "CARGO_TARGET_DIR": str(private / ("entry-build-" + arch)),
                "SCRIBE_GPU_PACK_CATALOG": str(copied / CATALOG),
                "SCRIBE_GPU_PACK_RELEASE_AUTHORITY": str(copied / AUTHORITY),
                "SCRIBE_MACOS_METAL_PACKAGE_PACK": str(root),
                "SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256": getattr(frozen, arch + "_manifest_sha256")}
            component.capture([cargo, "build", "--locked", "--offline", "--release", "--target", target,
                "--manifest-path", str(component.REPO / "tools/macos-metal-pack-installer/Cargo.toml"),
                "--bin", "scribe-macos-metal-pack-installer", "--no-default-features", "--features", "installer-package-entry"],
                environment=build_env, cwd=component.REPO, timeout=1200,
                limit=4 * 1024 * 1024, error_limit=4 * 1024 * 1024,
                operation="metal-entry-build-arm64" if arch == "aarch64" else "metal-entry-build-x86_64")
            binary = Path(build_env["CARGO_TARGET_DIR"]) / target / "release/scribe-macos-metal-pack-installer"
            component.hash_file(binary, MAX_SLICE)
            component.require(component.capture(["/usr/bin/lipo", str(binary), "-archs"], operation="artifact-architectures")
                              .decode("ascii").split() == ["arm64" if arch == "aarch64" else arch], "wrong Metal entry slice architecture")
            slices.append(str(binary))
        candidate = private / "postinstall"
        component.capture(["/usr/bin/lipo", "-create", "-output", str(candidate), *slices], timeout=60,
                          operation="metal-entry-combine")
        component.inspect_universal_structure(candidate, [frozen.revision.encode(), METAL_ROOT.encode()], private, MAX_ENTRY)
        owned.check_unchanged()
        digest, size = component.hash_file(candidate, MAX_ENTRY)
        component.publish_no_replace(candidate, output)
    return output, digest, size
