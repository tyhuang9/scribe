#!/usr/bin/env python3
"""Mirror an approved Qwen GGUF into a direct Scribe release catalog.

Run the runtime packager and merge its fragments first. This tool then verifies
the official GGUF bytes, publishes them without overwriting an existing file,
and upgrades the catalog to schema 2. It never downloads from Hugging Face or
follows redirects; the release operator supplies the already-verified bytes.
"""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import platform
import shutil
import sys
import tempfile
from urllib.parse import quote, urlparse


RUNTIME_ID = "voice_intent_llama_cpp"
APPROVED_MODELS = {
    "compact": {
        "model_id": "qwen3_0_6b_q8_0",
        "version": "Qwen3-0.6B",
        "upstream_repository": "Qwen/Qwen3-0.6B-GGUF",
        "upstream_revision": "ef4088322893040952513f532f736ddeab518403",
        "upstream_filename": "Qwen3-0.6B-Q8_0.gguf",
        "license": "Apache-2.0",
        "license_sha256": "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd",
        "sha256": "12fae8b8f78f0360b498d04c8db7d33aff29ab7d8080231f93a17c18119e6735",
        "size_bytes": 804_753_088,
        "managed_relative_path": "voice-intent/Qwen3-0.6B-Q8_0.gguf",
    },
    "balanced": {
        "model_id": "qwen3_1_7b_q8_0",
        "version": "Qwen3-1.7B",
        "upstream_repository": "Qwen/Qwen3-1.7B-GGUF",
        "upstream_revision": "90862c4b9d2787eaed51d12237eafdfe7c5f6077",
        "upstream_filename": "Qwen3-1.7B-Q8_0.gguf",
        "license": "Apache-2.0",
        "license_sha256": "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd",
        "sha256": "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a",
        "size_bytes": 1_834_426_016,
        "managed_relative_path": "voice-intent/Qwen3-1.7B-Q8_0.gguf",
    },
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tier", choices=sorted(APPROVED_MODELS))
    parser.add_argument("--model-file", type=Path)
    parser.add_argument("--release-base-url")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--catalog", required=True, type=Path)
    parser.add_argument("--catalog-version", required=True)
    parser.add_argument("--verify-ready", action="store_true")
    parser.add_argument("--os", choices=("linux", "macos", "windows"))
    parser.add_argument("--arch", choices=("x86_64", "aarch64"))
    return parser.parse_args()


def validate_base_url(value: str) -> str:
    parsed = urlparse(value)
    host = (parsed.hostname or "").lower()
    try:
        loopback = ipaddress.ip_address(host).is_loopback
    except ValueError:
        loopback = False
    reserved = (
        host == "localhost"
        or host.endswith((".localhost", ".invalid", ".test", ".example"))
        or loopback
        or any(
            host == value or host.endswith(f".{value}")
            for value in ("example.com", "example.net", "example.org")
        )
    )
    if (
        parsed.scheme != "https"
        or not host
        or reserved
        or parsed.username
        or parsed.password
        or parsed.query
        or parsed.fragment
    ):
        raise ValueError("release base URL must be a real immutable HTTPS release directory")
    return value.rstrip("/")


def hash_and_size(path: Path) -> tuple[str, int]:
    if not path.is_file() or path.is_symlink():
        raise ValueError("model file must be a regular non-link file")
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            size += len(chunk)
            digest.update(chunk)
    return digest.hexdigest(), size


def read_catalog(path: Path, version: str) -> dict:
    try:
        catalog = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"runtime catalog must already exist and be valid JSON: {error}") from error
    if catalog.get("schema_version") not in (1, 2):
        raise ValueError("runtime catalog must use schema 1 or 2")
    if catalog.get("catalog_version") != version:
        raise ValueError("catalog version does not match the runtime catalog")
    if not isinstance(catalog.get("artifacts"), list):
        raise ValueError("runtime catalog artifacts must be an array")
    if not isinstance(catalog.get("intent_models", []), list):
        raise ValueError("runtime catalog intent_models must be an array")
    return catalog


def write_json_atomic(path: Path, value: object) -> None:
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


def publish_no_replace(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise ValueError(f"refusing to overwrite existing model artifact: {destination}")
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{destination.name}.", dir=destination.parent
    )
    temporary = Path(temporary_name)
    try:
        with source.open("rb") as model, os.fdopen(descriptor, "wb") as target:
            shutil.copyfileobj(model, target, length=1024 * 1024)
            target.flush()
            os.fsync(target.fileno())
        os.link(temporary, destination)
    finally:
        temporary.unlink(missing_ok=True)


def native_tuple() -> tuple[str, str]:
    os_name = {"linux": "linux", "darwin": "macos", "win32": "windows"}.get(sys.platform)
    arch = {
        "x86_64": "x86_64",
        "amd64": "x86_64",
        "arm64": "aarch64",
        "aarch64": "aarch64",
    }.get(platform.machine().lower())
    if not os_name or not arch:
        raise ValueError("unsupported release platform")
    return os_name, arch


def validate_ready(catalog: dict, os_name: str, arch: str) -> None:
    runtime_ready = any(
        artifact.get("runtime_id") == RUNTIME_ID
        and artifact.get("os") == os_name
        and artifact.get("arch") == arch
        and artifact.get("device") == "cpu"
        for artifact in catalog.get("artifacts", [])
    )
    if not runtime_ready:
        raise ValueError(f"missing {RUNTIME_ID} CPU runtime for {os_name}-{arch}")
    models = catalog.get("intent_models", [])
    for tier, approved in APPROVED_MODELS.items():
        model = next((model for model in models if model.get("tier") == tier), None)
        if not model or any(model.get(key) != value for key, value in approved.items()):
            raise ValueError(f"missing or unapproved {tier} voice intent model")
        if not isinstance(model.get("url"), str):
            raise ValueError(f"{tier} voice intent model lacks a direct release URL")
        validate_base_url(model["url"].rsplit("/", 1)[0])


def main() -> int:
    args = parse_args()
    if not args.catalog_version.strip():
        raise ValueError("catalog version cannot be empty")
    catalog = read_catalog(args.catalog, args.catalog_version)
    if args.verify_ready:
        os_name, arch = (args.os, args.arch) if args.os and args.arch else native_tuple()
        if not os_name or not arch:
            raise ValueError("--os and --arch must be supplied together")
        validate_ready(catalog, os_name, arch)
        print(json.dumps({"catalog": str(args.catalog), "voice_ai_ready": True}))
        return 0

    required = {
        "--tier": args.tier,
        "--model-file": args.model_file,
        "--release-base-url": args.release_base_url,
        "--output-dir": args.output_dir,
    }
    missing = [name for name, value in required.items() if value is None]
    if missing:
        raise ValueError(f"packaging mode requires: {', '.join(missing)}")
    approved = APPROVED_MODELS[args.tier]
    actual_hash, actual_size = hash_and_size(args.model_file)
    if (actual_hash, actual_size) != (approved["sha256"], approved["size_bytes"]):
        raise ValueError(
            f"{args.tier} GGUF does not match the approved upstream bytes: "
            f"expected {approved['size_bytes']} bytes/{approved['sha256']}, "
            f"received {actual_size} bytes/{actual_hash}"
        )
    if any(model.get("tier") == args.tier for model in catalog.get("intent_models", [])):
        raise ValueError(f"catalog already contains the {args.tier} voice intent tier")

    base_url = validate_base_url(args.release_base_url)
    destination = args.output_dir / approved["upstream_filename"]
    publish_no_replace(args.model_file, destination)
    record = {
        "runtime_id": RUNTIME_ID,
        "tier": args.tier,
        **approved,
        "url": f"{base_url}/{quote(approved['upstream_filename'], safe='')}",
    }
    catalog["schema_version"] = 2
    catalog.setdefault("intent_models", []).append(record)
    catalog["intent_models"].sort(key=lambda model: model["tier"])
    try:
        write_json_atomic(args.catalog, catalog)
    except Exception:
        destination.unlink(missing_ok=True)
        raise
    print(json.dumps({"artifact": str(destination), **record}))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
