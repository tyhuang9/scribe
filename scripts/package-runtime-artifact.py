#!/usr/bin/env python3
"""Package one portable runtime and emit a parallel-safe catalog fragment."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import stat
import subprocess
import sys
import tempfile
from urllib.parse import quote, urlparse
import zipfile

MAX_ARCHIVE_BYTES = 8 * 1024 * 1024 * 1024
MAX_UNPACKED_BYTES = 16 * 1024 * 1024 * 1024
MAX_ENTRIES = 100_000
RUNTIME_IDS = {
    "whisper_cpp",
    "faster_whisper",
    "vosk",
    "sherpa_onnx",
    "moonshine",
    "parakeet",
}
GPU_RUNTIME_IDS = {"whisper_cpp", "faster_whisper"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--merge-catalog-fragments", action="store_true")
    parser.add_argument("--runtime-dir", type=Path)
    parser.add_argument("--runtime-id", choices=sorted(RUNTIME_IDS))
    parser.add_argument("--version")
    parser.add_argument("--os", choices=("linux", "macos", "windows"))
    parser.add_argument("--arch", choices=("x86_64", "aarch64"))
    parser.add_argument("--device", choices=("cpu", "gpu"))
    parser.add_argument("--entrypoint")
    parser.add_argument("--release-base-url")
    parser.add_argument("--catalog-version", required=True)
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--catalog", required=True, type=Path)
    return parser.parse_args()


def validate_base_url(value: str) -> str:
    parsed = urlparse(value)
    host = (parsed.hostname or "").lower()
    if (
        parsed.scheme != "https"
        or not host
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or host == "localhost"
        or host.endswith(".invalid")
        or host.endswith(".test")
        or host.endswith(".example")
        or host in {"example.com", "example.net", "example.org"}
    ):
        raise ValueError("release base URL must be a real immutable HTTPS release directory")
    return value.rstrip("/")


def normalized_entrypoint(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if not value or "\\" in value or path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise ValueError("entrypoint must be a normalized relative POSIX path")
    return path


def runtime_files(root: Path) -> list[Path]:
    if not root.is_dir() or root.is_symlink():
        raise ValueError("runtime directory must be a real directory")
    files: list[Path] = []
    for directory, dirnames, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        for name in [*dirnames, *filenames]:
            path = current / name
            if path.is_symlink():
                raise ValueError(f"runtime contains a symbolic link: {path}")
        for name in filenames:
            path = current / name
            if name.lower() == "pyvenv.cfg":
                raise ValueError("raw Python virtual environments are development-only")
            if not path.is_file():
                raise ValueError(f"runtime contains a non-regular file: {path}")
            files.append(path)
    files.sort(key=lambda path: path.relative_to(root).as_posix())
    if not files or len(files) > MAX_ENTRIES:
        raise ValueError(f"runtime must contain 1-{MAX_ENTRIES} regular files")
    return files


def validate_manifest(root: Path, args: argparse.Namespace, entrypoint: PurePosixPath) -> None:
    manifest_path = root / "runtime-manifest.json"
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"runtime-manifest.json is required and must be valid JSON: {error}") from error
    expected = {
        "manifest_version": 1,
        "runtime_id": args.runtime_id,
        "version": args.version,
        "platform": f"{args.os}-{args.arch}",
        "device": args.device,
        "entrypoint": entrypoint.as_posix(),
        "portable": True,
    }
    if any(manifest.get(key) != value for key, value in expected.items()):
        raise ValueError(f"runtime manifest does not match requested artifact identity: {expected}")


def smoke_validate(root: Path, entrypoint: PurePosixPath) -> None:
    executable = root / Path(*entrypoint.parts)
    command = [str(executable), "--help"]
    if os.name == "nt" and executable.suffix.lower() in {".bat", ".cmd"}:
        command = [os.environ.get("COMSPEC", "cmd.exe"), "/d", "/c", str(executable), "--help"]
    try:
        result = subprocess.run(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError(f"runtime entrypoint failed target-native smoke validation: {error}") from error
    if result.returncode != 0:
        raise ValueError(f"runtime entrypoint --help returned {result.returncode}")


def write_archive(root: Path, files: list[Path], destination: Path) -> int:
    unpacked = sum(path.stat().st_size for path in files)
    if unpacked > MAX_UNPACKED_BYTES:
        raise ValueError(f"runtime exceeds the {MAX_UNPACKED_BYTES} byte unpacked limit")
    with zipfile.ZipFile(destination, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in files:
            relative = path.relative_to(root).as_posix()
            info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IMODE(path.stat().st_mode) | stat.S_IFREG) << 16
            with path.open("rb") as source, archive.open(info, "w") as target:
                while chunk := source.read(1024 * 1024):
                    target.write(chunk)
    if destination.stat().st_size > MAX_ARCHIVE_BYTES:
        raise ValueError(f"archive exceeds the {MAX_ARCHIVE_BYTES} byte compressed limit")
    return unpacked


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def fragment_directory(catalog: Path) -> Path:
    return catalog.with_suffix(catalog.suffix + ".d")


def write_json_atomic(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as target:
            json.dump(value, target, indent=2)
            target.write("\n")
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def merge_catalog_fragments(catalog: Path, version: str) -> None:
    artifacts = []
    keys = set()
    fragments = sorted(fragment_directory(catalog).glob("*.json"))
    if not fragments:
        raise ValueError(f"no catalog fragments found for {catalog}")
    for fragment in fragments:
        artifact = json.loads(fragment.read_text(encoding="utf-8"))
        key = tuple(artifact.get(field) for field in ("runtime_id", "os", "arch", "device"))
        if None in key or key in keys:
            raise ValueError(f"duplicate or invalid artifact tuple in {fragment}: {key}")
        keys.add(key)
        artifacts.append(artifact)
    artifacts.sort(key=lambda item: (item["runtime_id"], item["os"], item["arch"], item["device"]))
    write_json_atomic(
        catalog,
        {"schema_version": 1, "catalog_version": version, "artifacts": artifacts},
    )


def main() -> int:
    args = parse_args()
    if not args.catalog_version.strip():
        raise ValueError("catalog version cannot be empty")
    if args.merge_catalog_fragments:
        merge_catalog_fragments(args.catalog, args.catalog_version)
        print(json.dumps({"catalog": str(args.catalog)}))
        return 0
    required = {
        "--runtime-dir": args.runtime_dir,
        "--runtime-id": args.runtime_id,
        "--version": args.version,
        "--os": args.os,
        "--arch": args.arch,
        "--device": args.device,
        "--entrypoint": args.entrypoint,
        "--release-base-url": args.release_base_url,
        "--output-dir": args.output_dir,
    }
    missing = [name for name, value in required.items() if value is None]
    if missing:
        raise ValueError(f"packaging mode requires: {', '.join(missing)}")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}", args.version):
        raise ValueError("runtime version is invalid")
    if args.device == "gpu" and args.runtime_id not in GPU_RUNTIME_IDS:
        raise ValueError(f"{args.runtime_id} does not support GPU packs")
    native_os = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(sys.platform)
    native_arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(platform.machine().lower())
    if (args.os, args.arch) != (native_os, native_arch):
        raise ValueError("runtime artifacts must be packaged and smoke-tested on their target OS and architecture")
    base_url = validate_base_url(args.release_base_url)
    entrypoint = normalized_entrypoint(args.entrypoint)
    files = runtime_files(args.runtime_dir)
    if args.runtime_dir / Path(*entrypoint.parts) not in files:
        raise ValueError("entrypoint is not a regular file in the runtime directory")
    validate_manifest(args.runtime_dir, args, entrypoint)
    smoke_validate(args.runtime_dir, entrypoint)

    args.output_dir.mkdir(parents=True, exist_ok=True)
    archive_name = f"{args.runtime_id}-{args.version}-{args.os}-{args.arch}-{args.device}.zip"
    archive_path = args.output_dir / archive_name
    if archive_path.exists():
        raise ValueError(f"refusing to overwrite existing archive: {archive_path}")

    descriptor, temporary_name = tempfile.mkstemp(prefix=f".{archive_name}.", dir=args.output_dir)
    os.close(descriptor)
    temporary = Path(temporary_name)
    temporary.unlink()
    archive_published = False
    try:
        unpacked_size = write_archive(args.runtime_dir, files, temporary)
        artifact = {
            "runtime_id": args.runtime_id,
            "version": args.version,
            "os": args.os,
            "arch": args.arch,
            "device": args.device,
            "url": f"{base_url}/{quote(archive_name, safe='')}",
            "sha256": sha256(temporary),
            "size_bytes": temporary.stat().st_size,
            "unpacked_size_bytes": unpacked_size,
            "entrypoint": entrypoint.as_posix(),
        }
        os.link(temporary, archive_path)
        archive_published = True
        fragment = fragment_directory(args.catalog) / f"{archive_name}.json"
        write_json_atomic(fragment, artifact)
        archive_published = False
    except Exception:
        if archive_published:
            archive_path.unlink(missing_ok=True)
        raise
    finally:
        temporary.unlink(missing_ok=True)

    print(json.dumps({"archive": str(archive_path), "fragment": str(fragment), **artifact}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
