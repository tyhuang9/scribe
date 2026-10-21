"""Internal, non-installing construction of a universal Metal package entry.

No command-line interface, signing or PKG assembly. The finalized app, frozen
inputs, reviewed source/toolchain/Cargo configuration and non-hostile same-user
and root writers are trusted build inputs. Before/after snapshots cannot defeat
swap-and-restore by a hostile build owner. Production signature verification is
delegated to the freshly built existing Rust verifier, never Python crypto.
"""
from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import sys

import macos_cpu_component as component

MAX_DOCUMENT = 512 * 1024
MAX_MANIFEST = 256 * 1024
MAX_SIGNATURE = 4 * 1024
MAX_METAL_WORKER = 2 * 1024 * 1024 * 1024
MAX_SLICE = MAX_METAL_WORKER + MAX_MANIFEST + MAX_SIGNATURE + 2 * MAX_DOCUMENT + 32 * 1024 * 1024
MAX_ENTRY = 2 * MAX_SLICE
PACK_FILES = {"pack-manifest.json": MAX_MANIFEST, "pack-manifest.sig": MAX_SIGNATURE,
              "worker/scribe-inference-worker": MAX_METAL_WORKER}
CATALOG = "Contents/Resources/worker-pack-catalog.json"
AUTHORITY = "Contents/Resources/gpu-pack-release-authority.json"
METAL_ROOT = "/Library/Application Support/Scribe/WorkerPacks"
ARCHES = ("aarch64", "x86_64")


@dataclass(frozen=True)
class FrozenInputs:
    revision: str
    cpu_worker_sha256: str
    catalog_sha256: str
    authority_sha256: str
    aarch64_manifest_sha256: str
    x86_64_manifest_sha256: str


def snapshot(root: Path, files: dict[str, int]) -> dict:
    """Bounded exact tree plus file content and node identity; no aliases."""
    directories = {"": set()}
    for name in files:
        parts = name.split("/")
        for index, part in enumerate(parts):
            parent = "/".join(parts[:index])
            directories.setdefault(parent, set()).add(part)
            if index < len(parts) - 1:
                directories.setdefault("/".join(parts[:index + 1]), set())
    nodes = {}
    for name, expected in directories.items():
        directory = root / name
        component.check_node(directory.lstat(), True)
        with os.scandir(directory) as entries:
            actual = []
            for node in entries:
                actual.append(node.name)
                component.require(len(actual) <= len(expected), "unexpected entry in frozen input tree")
        component.require(set(actual) == expected, "frozen input tree inventory differs")
        nodes[name] = component.fingerprint(directory.lstat())
    for name, maximum in files.items():
        path = root / name
        nodes[name] = (*component.hash_file(path, maximum), component.fingerprint(path.lstat()))
    return nodes


def release_inputs(app: Path, frozen: FrozenInputs) -> tuple[dict, dict]:
    catalog_bytes = component.read_bounded(app / CATALOG, MAX_DOCUMENT)
    authority_bytes = component.read_bounded(app / AUTHORITY, MAX_DOCUMENT)
    # Rust embedding strips one LF. Require identical source/compiled bytes
    # here, rather than silently changing a frozen catalog's hash.
    component.require(not catalog_bytes.endswith(b"\n") and not authority_bytes.endswith(b"\n"),
                      "release documents must equal their compiled bytes")
    catalog = component.strict_json(catalog_bytes, MAX_DOCUMENT)
    authority = component.strict_json(authority_bytes, MAX_DOCUMENT)
    component.require(type(catalog.get("schema_version")) is int and catalog["schema_version"] == 1
                      and type(catalog.get("packs")) is list and len(catalog["packs"]) == 2,
                      "Metal construction requires exactly two catalog packs")
    component.require(type(authority.get("schema_version")) is int and authority["schema_version"] == 2
                      and type(authority.get("entries")) is list and len(authority["entries"]) == 2
                      and type(authority.get("release_security_epoch")) is int
                      and 0 < authority["release_security_epoch"] <= 9007199254740991
                      and authority.get("catalog_sha256") == frozen.catalog_sha256,
                      "Metal construction requires matching positive release authority")
    packs = {}
    for pack, release in zip(catalog["packs"], authority["entries"]):
        component.require(type(pack) is dict and type(release) is dict, "invalid Metal release entry")
        arch = pack.get("target_arch")
        component.require(type(arch) is str and arch in ARCHES and arch not in packs, "missing or duplicate Metal architecture")
        for field in ("pack_id", "pack_version"):
            value = pack.get(field)
            component.require(type(value) is str and re.fullmatch(r"[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?", value)
                              is not None, "unsafe Metal store component")
        digest = pack.get("pack_digest")
        component.validate_identity(digest, frozen.revision)
        root = f"workers/packs/{pack['pack_id']}/{pack['pack_version']}/{digest}"
        component.require(pack.get("root") == root and pack.get("backend") == "metal"
                          and pack.get("provider") == "transcribe-cpp-metal" and pack.get("target_os") == "macos"
                          and pack.get("worker_relative_path") == "worker/scribe-inference-worker"
                          and pack.get("files") == sorted(f"{root}/{name}" for name in PACK_FILES)
                          and type(pack.get("security_epoch")) is int
                          and pack["security_epoch"] == authority["release_security_epoch"],
                          "Metal pack provider or layout differs")
        for field in ("runtime_abi_version", "installed_size_bytes", "compressed_size_bytes"):
            component.require(type(pack.get(field)) is int and pack[field] > 0, "invalid Metal release size or ABI")
        component.require(component.typed_equal({key: release.get(key) for key in pack}, pack)
                          and release.get("build_revision") == frozen.revision,
                          "Metal release entry differs from frozen catalog/build")
        packs[arch] = (pack, release)
    return packs, authority


def verify_pack(tool: Path, root: Path, arch: str, pack: dict, release: dict, version: str,
                frozen: FrozenInputs) -> None:
    descriptor = component.strict_json(component.capture(
        [str(tool), "verify-production-macos-metal", "--pack-root", str(root), "--target-arch", arch],
        timeout=120, operation="metal-content-verification"))
    expected = {key: pack[key] for key in ("pack_id", "pack_version", "pack_digest", "security_epoch",
        "runtime_abi_version", "backend", "provider", "target_os", "target_arch", "worker_relative_path")}
    expected["root"] = str(root)
    component.require(component.typed_equal(descriptor, expected), "Metal content descriptor differs from release")
    manifest = component.strict_json(component.read_bounded(root / "pack-manifest.json", MAX_MANIFEST), MAX_MANIFEST)
    component.require(manifest.get("app_build") == f"local-transcriber@{version}#{frozen.revision}"
                      and manifest.get("worker_build") == f"scribe-inference-worker@{version}#{frozen.revision}"
                      and release.get("app_version") == version
                      and component.typed_equal(release.get("app_protocol_version"), manifest.get("app_protocol_version")),
                      "Metal pack build differs from application release")
    component.require(sum(component.hash_file(root / name, maximum)[1] for name, maximum in PACK_FILES.items())
                      == pack["installed_size_bytes"], "Metal installed size differs from release")


def build_entry(app: Path, workspace: Path, frozen: FrozenInputs) -> tuple[Path, str, int]:
    """Return only the candidate path and PRE-SIGN hash/size, never authority."""
    component.require(sys.platform == "darwin", "Metal entry construction requires macOS")
    component.require(os.geteuid() != 0, "Metal entry construction must be unprivileged")
    for digest in (frozen.cpu_worker_sha256, frozen.catalog_sha256, frozen.authority_sha256,
                   frozen.aarch64_manifest_sha256, frozen.x86_64_manifest_sha256):
        component.validate_identity(digest, frozen.revision)
    host = {"arm64": "aarch64", "x86_64": "x86_64"}.get(platform.machine())
    component.require(host is not None, "unsupported native Metal author architecture")
    component.require(app.is_absolute() and app.is_dir() and not app.is_symlink(), "unsafe application input")
    app = app.resolve(strict=True)
    component.require(workspace.is_absolute() and workspace.is_dir() and not workspace.is_symlink()
                      and not any(ord(c) < 32 for c in str(workspace)), "unsafe author workspace")
    workspace = workspace.resolve(strict=True)
    component.require_outside_app(workspace, app)
    output = workspace / "postinstall"
    component.require(not os.path.lexists(output), "Metal entry output already exists")
    packs, _ = release_inputs(app, frozen)
    files = {name: component.MAX_WORKER for name in component.CPU_APP_INPUTS}
    files.update({CATALOG: MAX_DOCUMENT, AUTHORITY: MAX_DOCUMENT, "Contents/embedded.provisionprofile": MAX_DOCUMENT})
    for pack, _ in packs.values():
        files.update({f"Contents/Resources/{pack['root']}/{name}": maximum for name, maximum in PACK_FILES.items()})
    before = snapshot(app, files)
    for name, digest in ((component.CPU_APP_INPUTS[0], frozen.cpu_worker_sha256),
                         (CATALOG, frozen.catalog_sha256), (AUTHORITY, frozen.authority_sha256)):
        component.require(before[name][0] == digest, "application differs from frozen inputs")
    for arch, (pack, _) in packs.items():
        name = f"Contents/Resources/{pack['root']}/pack-manifest.json"
        component.require(before[name][0] == getattr(frozen, arch + "_manifest_sha256"), "Metal manifest differs from frozen input")
    with component.private_workspace(workspace, ".scribe-metal-pkg-entry-") as private:
        copied = private / "app"
        for name, maximum in files.items():
            destination = copied / name
            destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            component.require(component.copy_new(app / name, destination, maximum) == before[name][:2],
                              "application copy differs from frozen source")
        copies = snapshot(copied, files)
        component.capture(["/bin/bash", str(component.REPO / "scripts/verify-macos-release-package.sh"), "--app", str(app)],
                          timeout=60, environment={**component.probe_environment(), "PATH": os.environ.get("PATH", "")},
                          operation="application-verification")
        component.capture(["/usr/bin/codesign", "--verify", "--strict", str(app / component.CPU_APP_INPUTS[1])],
                          operation="desktop-bundle-signature-verification")
        component.require(snapshot(app, files) == before, "application changed during verification")
        info = plistlib.loads(component.read_bounded(copied / "Contents/Info.plist", component.MAX_JSON))
        version = info.get("CFBundleShortVersionString")
        component.require(type(version) is str and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None,
                          "invalid application version")
        component.inspect_universal(copied / component.CPU_APP_INPUTS[0],
            [f"scribe-inference-worker@{version}#{frozen.revision}".encode()], private, component.MAX_WORKER,
            signature_operation="copied-worker-signature-verification")
        component.inspect_universal_structure(copied / component.CPU_APP_INPUTS[1],
            [f"local-transcriber@{version}#{frozen.revision}".encode(), frozen.cpu_worker_sha256.encode()], private, component.MAX_WORKER)
        environment = component.build_environment(frozen.cpu_worker_sha256, frozen.revision)
        cargo = shutil.which("cargo")
        component.require(cargo is not None and Path(cargo).is_absolute(), "reviewed Rust toolchain is required")
        target = host + "-apple-darwin"
        author_env = {**environment, "CARGO_TARGET_DIR": str(private / "author-build")}
        component.capture([cargo, "build", "--locked", "--offline", "--release", "--target", target,
            "--manifest-path", str(component.REPO / "tools/worker-pack-author/Cargo.toml"),
            "--bin", "scribe-worker-pack-tool", "--no-default-features"], environment=author_env,
            cwd=component.REPO, timeout=1200, limit=4 * 1024 * 1024, error_limit=4 * 1024 * 1024,
            operation="metal-author-build")
        tool = Path(author_env["CARGO_TARGET_DIR"]) / target / "release/scribe-worker-pack-tool"
        tool_identity = component.hash_file(tool, component.MAX_WRITER)
        component.require(component.capture(["/usr/bin/lipo", str(tool), "-archs"], operation="artifact-architectures")
                          .decode("ascii").split() == ["arm64" if host == "aarch64" else host], "wrong native verifier architecture")
        slices = []
        for arch in ARCHES:
            pack, release = packs[arch]
            root = copied / "Contents/Resources" / pack["root"]
            component.require(component.hash_file(tool, component.MAX_WRITER) == tool_identity, "fresh verifier changed")
            verify_pack(tool, root, arch, pack, release, version, frozen)
            component.require(snapshot(copied, files) == copies, "private application changed during content verification")
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
        component.require(snapshot(copied, files) == copies and snapshot(app, files) == before
                          and component.hash_file(tool, component.MAX_WRITER) == tool_identity,
                          "frozen inputs changed during Metal entry construction")
        digest, size = component.hash_file(candidate, MAX_ENTRY)
        component.publish_no_replace(candidate, output)
    return output, digest, size
