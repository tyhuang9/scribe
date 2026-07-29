#!/usr/bin/env python3
"""Prepare the pinned llama.cpp Windows CPU asset for Scribe packaging."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import sys
import zipfile


VERSION = "b9637"
UPSTREAM_REPOSITORY = "ggml-org/llama.cpp"
UPSTREAM_REVISION = "aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3"
UPSTREAM_ASSET = "llama-b9637-bin-win-cpu-x64.zip"
UPSTREAM_SIZE = 16_906_751
UPSTREAM_SHA256 = "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e"
UPSTREAM_ENTRIES = 51
UPSTREAM_UNPACKED_SIZE = 43_983_896
LICENSE_SIZE = 1_078
LICENSE_SHA256 = "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d"
ENTRYPOINT = "bin/llama-server.exe"
ATTESTATION_FILENAME = ".scribe-llama-runtime-attestation.json"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--license-file", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    return parser.parse_args()


def hash_and_size(path: Path) -> tuple[str, int]:
    if not path.is_file() or path.is_symlink():
        raise ValueError(f"input must be a regular non-link file: {path}")
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            size += len(chunk)
            digest.update(chunk)
    return digest.hexdigest(), size


def verify_file(path: Path, expected_size: int, expected_sha256: str, label: str) -> None:
    actual_sha256, actual_size = hash_and_size(path)
    if (actual_size, actual_sha256) != (expected_size, expected_sha256):
        raise ValueError(
            f"{label} does not match pinned upstream bytes: expected "
            f"{expected_size}/{expected_sha256}, received {actual_size}/{actual_sha256}"
        )


def selected_payload(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    names = set()
    selected = []
    infos = archive.infolist()
    if len(infos) != UPSTREAM_ENTRIES:
        raise ValueError(
            f"llama archive entry count mismatch: expected {UPSTREAM_ENTRIES}, received {len(infos)}"
        )
    if sum(info.file_size for info in infos) != UPSTREAM_UNPACKED_SIZE:
        raise ValueError("llama archive unpacked size does not match the pinned release")
    for info in infos:
        name = info.filename
        path = PurePosixPath(name)
        unix_type = (info.external_attr >> 16) & 0o170000
        if (
            not name
            or "\\" in name
            or path.is_absolute()
            or len(path.parts) != 1
            or path.parts[0] in (".", "..")
            or ":" in name
            or any(ord(character) < 32 or ord(character) == 127 for character in name)
            or unix_type not in (0, stat.S_IFREG, stat.S_IFDIR)
        ):
            raise ValueError(f"unsafe llama archive entry: {name!r}")
        folded = name.casefold()
        if folded in names:
            raise ValueError(f"duplicate llama archive entry: {name!r}")
        names.add(folded)
        if info.is_dir():
            continue
        if folded == "llama-server.exe" or folded.endswith(".dll"):
            selected.append(info)
    if not any(info.filename.casefold() == "llama-server.exe" for info in selected):
        raise ValueError("pinned llama archive lacks llama-server.exe")
    if not any(info.filename.casefold().endswith(".dll") for info in selected):
        raise ValueError("pinned llama archive lacks its runtime DLLs")
    return selected


def write_member(archive: zipfile.ZipFile, info: zipfile.ZipInfo, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with archive.open(info) as source, destination.open("xb") as target:
        copied = 0
        while chunk := source.read(1024 * 1024):
            copied += len(chunk)
            if copied > info.file_size:
                raise ValueError(f"archive entry exceeded declared size: {info.filename}")
            target.write(chunk)
        target.flush()
        os.fsync(target.fileno())
    if copied != info.file_size:
        raise ValueError(f"archive entry size mismatch: {info.filename}")
    with destination.open("rb") as payload:
        if payload.read(2) != b"MZ":
            raise ValueError(f"runtime payload is not a Windows PE image: {info.filename}")


def copy_file_no_replace(source: Path, destination: Path) -> None:
    with source.open("rb") as input_file, destination.open("xb") as output_file:
        shutil.copyfileobj(input_file, output_file, length=1024 * 1024)
        output_file.flush()
        os.fsync(output_file.fileno())


def write_manifest(output_dir: Path) -> None:
    manifest = {
        "manifest_version": 1,
        "runtime_id": "voice_intent_llama_cpp",
        "version": VERSION,
        "platform": "windows-x86_64",
        "device": "cpu",
        "entrypoint": ENTRYPOINT,
        "portable": True,
        "upstream_repository": UPSTREAM_REPOSITORY,
        "upstream_revision": UPSTREAM_REVISION,
        "upstream_asset": UPSTREAM_ASSET,
        "upstream_sha256": UPSTREAM_SHA256,
        "upstream_size_bytes": UPSTREAM_SIZE,
        "license": "MIT",
        "license_sha256": LICENSE_SHA256,
    }
    path = output_dir / "runtime-manifest.json"
    with path.open("x", encoding="utf-8", newline="\n") as target:
        json.dump(manifest, target, indent=2)
        target.write("\n")
        target.flush()
        os.fsync(target.fileno())


def write_attestation(output_dir: Path) -> None:
    files = []
    for path in sorted(output_dir.rglob("*")):
        if path.is_symlink() or (path.exists() and not path.is_file() and not path.is_dir()):
            raise ValueError(f"prepared runtime contains a non-regular entry: {path}")
        if not path.is_file():
            continue
        digest, size = hash_and_size(path)
        files.append(
            {
                "path": path.relative_to(output_dir).as_posix(),
                "size_bytes": size,
                "sha256": digest,
            }
        )
    attestation = {
        "attestation_version": 1,
        "runtime_id": "voice_intent_llama_cpp",
        "version": VERSION,
        "platform": "windows-x86_64",
        "device": "cpu",
        "entrypoint": ENTRYPOINT,
        "upstream_repository": UPSTREAM_REPOSITORY,
        "upstream_revision": UPSTREAM_REVISION,
        "upstream_asset": UPSTREAM_ASSET,
        "upstream_sha256": UPSTREAM_SHA256,
        "upstream_size_bytes": UPSTREAM_SIZE,
        "license": "MIT",
        "license_sha256": LICENSE_SHA256,
        "files": files,
    }
    path = output_dir / ATTESTATION_FILENAME
    with path.open("x", encoding="utf-8", newline="\n") as target:
        json.dump(attestation, target, indent=2)
        target.write("\n")
        target.flush()
        os.fsync(target.fileno())


def main() -> int:
    args = parse_args()
    verify_file(args.archive, UPSTREAM_SIZE, UPSTREAM_SHA256, "llama.cpp archive")
    verify_file(args.license_file, LICENSE_SIZE, LICENSE_SHA256, "llama.cpp license")
    if args.output_dir.exists():
        raise ValueError(f"refusing to overwrite runtime output: {args.output_dir}")
    args.output_dir.mkdir(parents=True)
    try:
        with zipfile.ZipFile(args.archive) as archive:
            for info in selected_payload(archive):
                write_member(archive, info, args.output_dir / "bin" / info.filename)
        copy_file_no_replace(args.license_file, args.output_dir / "LICENSE.llama.cpp")
        write_manifest(args.output_dir)
        write_attestation(args.output_dir)
    except Exception:
        shutil.rmtree(args.output_dir)
        raise
    print(
        json.dumps(
            {
                "runtime_dir": str(args.output_dir),
                "version": VERSION,
                "entrypoint": ENTRYPOINT,
                "source_revision": UPSTREAM_REVISION,
            }
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
