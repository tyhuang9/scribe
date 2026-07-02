#!/usr/bin/env python3
"""Check pinned runtime dependency defaults against PyPI.

This is intentionally a maintainer/release check. The desktop app should update
managed runtimes from signed/bundled artifacts, not install arbitrary latest
packages from PyPI on user machines.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from urllib.error import URLError
from urllib.request import urlopen


SCRIPT_DIR = Path(__file__).resolve().parent
DEFAULT_ENV = SCRIPT_DIR / "runtime-dependencies.env"

PINNED_PACKAGES = {
    "SCRIBE_PIP_VERSION_DEFAULT": "pip",
    "SCRIBE_SETUPTOOLS_VERSION_DEFAULT": "setuptools",
    "SCRIBE_WHEEL_VERSION_DEFAULT": "wheel",
    "SCRIBE_FASTER_WHISPER_VERSION_DEFAULT": "faster-whisper",
    "SCRIBE_NVIDIA_CUBLAS_CU12_VERSION_DEFAULT": "nvidia-cublas-cu12",
    "SCRIBE_NVIDIA_CUDNN_CU12_VERSION_DEFAULT": "nvidia-cudnn-cu12",
    "SCRIBE_VOSK_VERSION_DEFAULT": "vosk",
    "SCRIBE_SHERPA_ONNX_VERSION_DEFAULT": "sherpa-onnx",
    "SCRIBE_SHERPA_ONNX_BIN_VERSION_DEFAULT": "sherpa-onnx-bin",
    "SCRIBE_NUMPY_VERSION_DEFAULT": "numpy",
}


def parse_env(path: Path) -> dict[str, str]:
    pins: dict[str, str] = {}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValueError(f"invalid dependency pin line: {raw_line}")
        key, value = line.split("=", 1)
        pins[key.strip()] = value.strip().strip('"').strip("'")
    return pins


def pypi_version(package: str, timeout: float) -> str:
    url = f"https://pypi.org/pypi/{package}/json"
    with urlopen(url, timeout=timeout) as response:  # nosec: release check URL is fixed.
        payload = json.load(response)
    return str(payload["info"]["version"])


def collect_updates(env_path: Path, timeout: float) -> list[dict[str, str | bool]]:
    pins = parse_env(env_path)
    rows: list[dict[str, str | bool]] = []
    for key, package in PINNED_PACKAGES.items():
        pinned = pins.get(key)
        if not pinned:
            rows.append(
                {
                    "package": package,
                    "key": key,
                    "pinned": "",
                    "latest": "",
                    "update_available": True,
                    "error": "missing pin",
                }
            )
            continue
        try:
            latest = pypi_version(package, timeout)
            error = ""
        except (OSError, URLError, KeyError, json.JSONDecodeError) as exc:
            latest = ""
            error = str(exc)
        rows.append(
            {
                "package": package,
                "key": key,
                "pinned": pinned,
                "latest": latest,
                "update_available": bool(latest and latest != pinned),
                "error": error,
            }
        )
    return rows


def print_table(rows: list[dict[str, str | bool]]) -> None:
    headers = ("package", "pinned", "latest", "status")
    widths = {
        "package": max(len(headers[0]), *(len(str(row["package"])) for row in rows)),
        "pinned": max(len(headers[1]), *(len(str(row["pinned"])) for row in rows)),
        "latest": max(len(headers[2]), *(len(str(row["latest"])) for row in rows)),
    }
    print(
        f"{headers[0]:<{widths['package']}}  "
        f"{headers[1]:<{widths['pinned']}}  "
        f"{headers[2]:<{widths['latest']}}  {headers[3]}"
    )
    print(
        f"{'-' * widths['package']}  "
        f"{'-' * widths['pinned']}  "
        f"{'-' * widths['latest']}  ------"
    )
    for row in rows:
        if row["error"]:
            status = f"error: {row['error']}"
        elif row["update_available"]:
            status = "update available"
        else:
            status = "current"
        print(
            f"{str(row['package']):<{widths['package']}}  "
            f"{str(row['pinned']):<{widths['pinned']}}  "
            f"{str(row['latest']):<{widths['latest']}}  {status}"
        )


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Check Scribe runtime dependency pins against PyPI."
    )
    parser.add_argument("--env", type=Path, default=DEFAULT_ENV)
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--json", action="store_true", help="print JSON rows")
    parser.add_argument(
        "--fail-on-updates",
        action="store_true",
        help="exit non-zero when a newer PyPI version is available",
    )
    args = parser.parse_args()

    rows = collect_updates(args.env, args.timeout)
    if args.json:
        print(json.dumps(rows, indent=2))
    else:
        print_table(rows)

    has_errors = any(row["error"] for row in rows)
    has_updates = any(row["update_available"] for row in rows if not row["error"])
    if has_errors:
        return 2
    if args.fail_on_updates and has_updates:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
