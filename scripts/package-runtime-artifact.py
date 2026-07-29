#!/usr/bin/env python3
"""Package one portable runtime and emit a parallel-safe catalog fragment."""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
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
    "voice_intent_llama_cpp",
}
GPU_RUNTIME_IDS = {"whisper_cpp", "faster_whisper"}
VOICE_RUNTIME_PROVENANCE = {
    "upstream_repository": "ggml-org/llama.cpp",
    "upstream_revision": "aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3",
    "upstream_asset": "llama-b9637-bin-win-cpu-x64.zip",
    "upstream_sha256": "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e",
    "upstream_size_bytes": 16_906_751,
    "license": "MIT",
    "license_sha256": "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d",
}
VOICE_RUNTIME_ATTESTATION = ".scribe-llama-runtime-attestation.json"
VOICE_RUNTIME_SOURCE_ARCHIVE = ".scribe-llama-runtime-source.zip"
VOICE_RUNTIME_LICENSE_SIZE = 1_078


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
    try:
        loopback = ipaddress.ip_address(host).is_loopback
    except ValueError:
        loopback = False
    if (
        parsed.scheme != "https"
        or not host
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
        or host == "localhost"
        or host.endswith(".localhost")
        or loopback
        or host.endswith(".invalid")
        or host.endswith(".test")
        or host.endswith(".example")
        or any(
            host == reserved or host.endswith(f".{reserved}")
            for reserved in ("example.com", "example.net", "example.org")
        )
    ):
        raise ValueError("release base URL must be a real immutable HTTPS release directory")
    return value.rstrip("/")


def normalized_entrypoint(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    reserved = {"CON", "PRN", "AUX", "NUL"} | {
        f"{prefix}{number}" for prefix in ("COM", "LPT") for number in range(1, 10)
    }
    parts = value.split("/")
    if (
        not value
        or "\\" in value
        or path.is_absolute()
        or any(
            part in {"", ".", ".."}
            or ":" in part
            or part.endswith((" ", "."))
            or any(ord(character) < 32 or ord(character) == 127 for character in part)
            or part.split(".", 1)[0].upper() in reserved
            for part in parts
        )
    ):
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
            try:
                normalized_entrypoint(path.relative_to(root).as_posix())
            except ValueError as error:
                raise ValueError(f"runtime contains an unsafe portable path: {path}: {error}") from error
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
    if args.runtime_id == "voice_intent_llama_cpp":
        expected.update(VOICE_RUNTIME_PROVENANCE)
    if any(manifest.get(key) != value for key, value in expected.items()):
        raise ValueError(f"runtime manifest does not match requested artifact identity: {expected}")


def verify_voice_runtime_attestation(
    root: Path, files: list[Path], entrypoint: PurePosixPath
) -> tuple[list[Path], dict[str, tuple[str, int]]]:
    attestation_path = root / VOICE_RUNTIME_ATTESTATION
    source_archive = root / VOICE_RUNTIME_SOURCE_ARCHIVE
    if attestation_path not in files:
        raise ValueError("prepared llama runtime attestation is required")
    if source_archive not in files:
        raise ValueError("verified pinned llama source archive is required")
    if entrypoint.as_posix() != "bin/llama-server.exe":
        raise ValueError("prepared llama runtime must use bin/llama-server.exe")
    try:
        attestation = json.loads(attestation_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"prepared llama runtime attestation is invalid: {error}") from error
    expected_identity = {
        "attestation_version": 1,
        "runtime_id": "voice_intent_llama_cpp",
        "version": "b9637",
        "platform": "windows-x86_64",
        "device": "cpu",
        "entrypoint": entrypoint.as_posix(),
        **VOICE_RUNTIME_PROVENANCE,
    }
    if set(attestation) != {*expected_identity, "files"} or any(
        attestation.get(key) != value for key, value in expected_identity.items()
    ):
        raise ValueError("prepared llama runtime attestation has an unapproved identity")
    records = attestation.get("files")
    if not isinstance(records, list) or not records:
        raise ValueError("prepared llama runtime attestation has no file digests")
    expected_files = {}
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "size_bytes", "sha256"}:
            raise ValueError("prepared llama runtime attestation has an invalid file record")
        try:
            relative = normalized_entrypoint(record["path"])
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError("prepared llama runtime attestation has an unsafe file path") from error
        relative_name = relative.as_posix()
        if relative_name == VOICE_RUNTIME_ATTESTATION or relative_name in expected_files:
            raise ValueError("prepared llama runtime attestation has a duplicate file record")
        size = record["size_bytes"]
        digest = record["sha256"]
        if (
            not isinstance(size, int)
            or isinstance(size, bool)
            or size < 0
            or not isinstance(digest, str)
            or not re.fullmatch(r"[0-9a-f]{64}", digest)
        ):
            raise ValueError("prepared llama runtime attestation has an invalid file digest")
        expected_files[relative_name] = (digest, size)
    payload = [path for path in files if path not in (attestation_path, source_archive)]
    actual_names = {path.relative_to(root).as_posix() for path in payload}
    if actual_names != set(expected_files):
        raise ValueError("prepared llama runtime files do not match the attestation")
    for path in payload:
        relative = path.relative_to(root).as_posix()
        if hash_and_size(path) != expected_files[relative]:
            raise ValueError(f"prepared llama runtime file digest mismatch: {relative}")
    authenticated_files = authenticated_voice_runtime_files(source_archive)
    if set(expected_files) != {
        *authenticated_files,
        "LICENSE.llama.cpp",
        "runtime-manifest.json",
    }:
        raise ValueError("prepared llama runtime contains files not authenticated by its sources")
    for relative, fingerprint in authenticated_files.items():
        if expected_files.get(relative) != fingerprint:
            raise ValueError(
                f"prepared llama runtime file is not authenticated by the pinned source archive: {relative}"
            )
    license_fingerprint = expected_files.get("LICENSE.llama.cpp")
    if license_fingerprint != (
        VOICE_RUNTIME_PROVENANCE["license_sha256"],
        VOICE_RUNTIME_LICENSE_SIZE,
    ):
        raise ValueError("prepared llama runtime license does not match the pinned license")
    return payload, expected_files


def authenticated_voice_runtime_files(source_archive: Path) -> dict[str, tuple[str, int]]:
    if hash_and_size(source_archive) != (
        VOICE_RUNTIME_PROVENANCE["upstream_sha256"],
        VOICE_RUNTIME_PROVENANCE["upstream_size_bytes"],
    ):
        raise ValueError("llama source archive does not match the pinned upstream bytes")
    authenticated = {}
    try:
        with zipfile.ZipFile(source_archive) as archive:
            for info in archive.infolist():
                name = info.filename
                path = PurePosixPath(name)
                if (
                    info.is_dir()
                    or path.is_absolute()
                    or len(path.parts) != 1
                    or path.parts[0] in (".", "..")
                    or "\\" in name
                ):
                    continue
                folded = name.casefold()
                if folded != "llama-server.exe" and not folded.endswith(".dll"):
                    continue
                relative = f"bin/{name}"
                if relative.casefold() in {candidate.casefold() for candidate in authenticated}:
                    raise ValueError("pinned llama source archive has duplicate runtime files")
                digest = hashlib.sha256()
                size = 0
                with archive.open(info) as source:
                    while chunk := source.read(1024 * 1024):
                        size += len(chunk)
                        digest.update(chunk)
                if size != info.file_size:
                    raise ValueError(f"pinned llama source entry size mismatch: {name}")
                authenticated[relative] = (digest.hexdigest(), size)
    except zipfile.BadZipFile as error:
        raise ValueError(f"pinned llama source archive is invalid: {error}") from error
    if "bin/llama-server.exe" not in authenticated or not any(
        relative.casefold().endswith(".dll") for relative in authenticated
    ):
        raise ValueError("pinned llama source archive lacks the required runtime files")
    return authenticated


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


def write_archive(
    root: Path,
    files: list[Path],
    destination: Path,
    attested_files: dict[str, tuple[str, int]] | None = None,
) -> int:
    unpacked = 0
    with zipfile.ZipFile(destination, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in files:
            relative = path.relative_to(root).as_posix()
            info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (stat.S_IMODE(path.stat().st_mode) | stat.S_IFREG) << 16
            digest = hashlib.sha256()
            size = 0
            with path.open("rb") as source, archive.open(info, "w") as target:
                while chunk := source.read(1024 * 1024):
                    size += len(chunk)
                    digest.update(chunk)
                    target.write(chunk)
            if attested_files is not None and (digest.hexdigest(), size) != attested_files[relative]:
                raise ValueError(f"prepared llama runtime changed during packaging: {relative}")
            unpacked += size
            if unpacked > MAX_UNPACKED_BYTES:
                raise ValueError(f"runtime exceeds the {MAX_UNPACKED_BYTES} byte unpacked limit")
    if destination.stat().st_size > MAX_ARCHIVE_BYTES:
        raise ValueError(f"archive exceeds the {MAX_ARCHIVE_BYTES} byte compressed limit")
    return unpacked


def sha256(path: Path) -> str:
    return hash_and_size(path)[0]


def hash_and_size(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            size += len(chunk)
            digest.update(chunk)
    return digest.hexdigest(), size


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
    if args.runtime_id == "voice_intent_llama_cpp" and args.version != "b9637":
        raise ValueError("voice_intent_llama_cpp must use the approved b9637 build")
    if args.runtime_id == "voice_intent_llama_cpp" and (
        args.os,
        args.arch,
        args.device,
    ) != ("windows", "x86_64", "cpu"):
        raise ValueError("the approved b9637 voice intent runtime is Windows x86_64 CPU-only")
    native_os = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(sys.platform)
    native_arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(platform.machine().lower())
    if (args.os, args.arch) != (native_os, native_arch):
        raise ValueError("runtime artifacts must be packaged and smoke-tested on their target OS and architecture")
    base_url = validate_base_url(args.release_base_url)
    entrypoint = normalized_entrypoint(args.entrypoint)
    if args.os == "windows" and entrypoint.suffix.lower() != ".exe":
        raise ValueError("Windows runtime entrypoints must be native .exe files")
    files = runtime_files(args.runtime_dir)
    if args.runtime_dir / Path(*entrypoint.parts) not in files:
        raise ValueError("entrypoint is not a regular file in the runtime directory")
    validate_manifest(args.runtime_dir, args, entrypoint)
    attested_files = None
    if args.runtime_id == "voice_intent_llama_cpp":
        files, attested_files = verify_voice_runtime_attestation(
            args.runtime_dir, files, entrypoint
        )
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
        unpacked_size = write_archive(args.runtime_dir, files, temporary, attested_files)
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
        if args.runtime_id == "voice_intent_llama_cpp":
            artifact.update(VOICE_RUNTIME_PROVENANCE)
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
