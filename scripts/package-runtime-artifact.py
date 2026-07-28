#!/usr/bin/env python3
"""Package one portable runtime and update a build-embedded artifact catalog."""

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
    parser.add_argument("--runtime-dir", required=True, type=Path)
    parser.add_argument("--runtime-id", required=True, choices=sorted(RUNTIME_IDS))
    parser.add_argument("--version", required=True)
    parser.add_argument("--os", required=True, choices=("linux", "macos", "windows"))
    parser.add_argument("--arch", required=True, choices=("x86_64", "aarch64"))
    parser.add_argument("--device", required=True, choices=("cpu", "gpu"))
    parser.add_argument("--entrypoint", required=True)
    parser.add_argument("--release-base-url", required=True)
    parser.add_argument("--catalog-version", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
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
    try:
        result = subprocess.run(
            [str(executable), "--help"],
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


def load_catalog(path: Path, version: str) -> dict:
    if not path.exists():
        return {"schema_version": 1, "catalog_version": version, "artifacts": []}
    catalog = json.loads(path.read_text(encoding="utf-8"))
    if catalog.get("schema_version") != 1 or not isinstance(catalog.get("artifacts"), list):
        raise ValueError("existing catalog has an unsupported schema")
    if catalog.get("catalog_version") != version:
        raise ValueError("existing catalog version does not match --catalog-version")
    return catalog


def main() -> int:
    args = parse_args()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,127}", args.version) or not args.catalog_version.strip():
        raise ValueError("version values cannot be empty")
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
    catalog_tmp = args.catalog.with_suffix(args.catalog.suffix + ".tmp")
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
        catalog = load_catalog(args.catalog, args.catalog_version)
        key = (args.runtime_id, args.os, args.arch, args.device)
        if any(
            (item.get("runtime_id"), item.get("os"), item.get("arch"), item.get("device")) == key
            for item in catalog["artifacts"]
        ):
            raise ValueError(f"catalog already contains artifact tuple {key}")
        catalog["artifacts"].append(artifact)
        catalog["artifacts"].sort(key=lambda item: (item["runtime_id"], item["os"], item["arch"], item["device"]))
        args.catalog.parent.mkdir(parents=True, exist_ok=True)
        catalog_tmp.write_text(json.dumps(catalog, indent=2) + "\n", encoding="utf-8")
        os.replace(temporary, archive_path)
        archive_published = True
        os.replace(catalog_tmp, args.catalog)
        archive_published = False
    except Exception:
        if archive_published:
            archive_path.unlink(missing_ok=True)
        raise
    finally:
        temporary.unlink(missing_ok=True)
        catalog_tmp.unlink(missing_ok=True)

    print(json.dumps({"archive": str(archive_path), "catalog": str(args.catalog), **artifact}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
