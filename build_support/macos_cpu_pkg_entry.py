"""Internal construction of the real future CPU postinstall, without execution.

No CLI, package/envelope format, signing or installation. The future PKG author
owns the output workspace and must authenticate/sign the finalized entry later.
Build provenance/toolchain and frozen inputs are trusted; markers alone do not
authenticate arbitrary binaries. Source fixtures never build real native code.
"""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import sys

import macos_cpu_component as component

# Each slice embeds the SAME complete universal worker. Reserve explicit code
# and alignment space per slice, without weakening the ordinary writer's bound.
SLICE_CODE_AND_ALIGNMENT = 32 * 1024 * 1024
MAX_SLICE = component.MAX_WORKER + SLICE_CODE_AND_ALIGNMENT
MAX_ENTRY = 2 * MAX_SLICE


def build_entry(app: Path, workspace: Path, digest: str, revision: str) -> tuple[Path, str, int]:
    """Construct postinstall inside an author-owned workspace; never run it.

    Return the candidate path and measured PRE-SIGN hash/size. These are not
    installation authority. Signing changes the entry hash; the actual PKG
    author must remeasure/reverify finalized bytes, without changing the worker.
    """
    component.require(sys.platform == "darwin", "CPU entry construction requires macOS")
    component.require(os.geteuid() != 0, "CPU entry construction must be unprivileged")
    component.validate_identity(digest, revision)
    component.require(app.is_dir() and not app.is_symlink(), "unsafe application input")
    app = app.resolve(strict=True)
    component.require(workspace.is_absolute() and workspace.is_dir() and not workspace.is_symlink()
                      and not any(ord(c) < 32 for c in str(workspace)), "unsafe author workspace")
    workspace = workspace.resolve(strict=True)
    component.require_outside_app(workspace, app)
    output = workspace / "postinstall"
    component.require(not os.path.lexists(output), "CPU entry output already exists")
    with component.private_workspace(workspace, ".scribe-cpu-pkg-entry-") as private:
        worker = private / "scribe-inference-worker"
        desktop = private / "Scribe"
        component.snapshot_cpu_app_inputs(app, worker, desktop, private, digest, revision)
        candidate, entry_digest, entry_size = _compile_owned_worker(worker, private, digest, revision)
        component.publish_no_replace(candidate, output)
    return output, entry_digest, entry_size


def _compile_owned_worker(worker: Path, private: Path, digest: str, revision: str) -> tuple[Path, str, int]:
    """Fixed compilation tail for this caller's own freshly admitted CPU copy.

    Private source callers must own the enclosing verification/workspace scope.
    This is not an alternate public worker admission or prebuilt-image API.
    """
    environment = component.build_environment(digest, revision)
    environment["SCRIBE_MACOS_CPU_PACKAGE_WORKER"] = str(worker)
    cargo = shutil.which("cargo")
    component.require(cargo is not None, "reviewed Rust toolchain is required")
    slices = []
    for target in component.TARGETS:
        directory = private / ("build-" + target)
        environment["CARGO_TARGET_DIR"] = str(directory)
        component.capture([cargo, "build", "--locked", "--offline", "--release", "--target", target,
                           "--manifest-path", str(component.REPO / "tools/macos-cpu-worker-installer/Cargo.toml"),
                           "--bin", "scribe-macos-cpu-worker-installer", "--no-default-features",
                           "--features", "installer-package-entry"],
                          environment=environment, cwd=component.REPO, timeout=1200,
                          limit=4 * 1024 * 1024, error_limit=4 * 1024 * 1024,
                          operation="entry-build-arm64" if target == "aarch64-apple-darwin" else "entry-build-x86_64")
        binary = directory / target / "release/scribe-macos-cpu-worker-installer"
        component.hash_file(binary, MAX_SLICE)
        expected = "arm64" if target == "aarch64-apple-darwin" else "x86_64"
        actual = component.capture(["/usr/bin/lipo", str(binary), "-archs"],
                                   operation="artifact-architectures").decode("ascii").split()
        component.require(actual == [expected], "fresh CPU entry slice has the wrong architecture")
        slices.append(str(binary))
    candidate = private / "postinstall"
    component.capture(["/usr/bin/lipo", "-create", "-output", str(candidate), *slices], timeout=60,
                      operation="entry-combine")
    component.inspect_universal_structure(candidate, [digest.encode(), revision.encode(), component.CPU_ROOT.encode()],
                                          private, MAX_ENTRY)
    component.require(component.hash_file(worker, component.MAX_WORKER)[0] == digest,
                      "frozen worker changed during entry construction")
    return candidate, *component.hash_file(candidate, MAX_ENTRY)
