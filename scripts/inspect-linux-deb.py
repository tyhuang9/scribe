#!/usr/bin/env python3
import os
import pathlib
import subprocess
import sys

AR_MAGIC = b"!<arch>\n"
EXPECTED_MEMBERS = ("debian-binary", "control.tar.xz", "data.tar.xz")
CONTROL_COMPRESSED_LIMIT = 8 * 1024 * 1024
CONTROL_TAR_LIMIT = 2 * 1024 * 1024
DATA_COMPRESSED_LIMIT = 4 * 1024 * 1024 * 1024
DATA_TAR_LIMIT = DATA_COMPRESSED_LIMIT + 16 * 1024 * 1024


def fail(message: str) -> None:
    raise SystemExit(message)


def exact_decimal(field: bytes, label: str) -> int:
    value = field.rstrip(b" ")
    if not value or not value.isdigit() or field != value.ljust(len(field), b" "):
        fail(f"outer ar {label} is not canonical decimal")
    if len(value) > 1 and value.startswith(b"0"):
        fail(f"outer ar {label} has a leading zero")
    return int(value)


def copy_exact(source, output: pathlib.Path, size: int) -> None:
    remaining = size
    with output.open("xb") as target:
        while remaining:
            chunk = source.read(min(1024 * 1024, remaining))
            if not chunk:
                fail("outer ar member is truncated")
            target.write(chunk)
            remaining -= len(chunk)


def decompress_bounded(source: pathlib.Path, output: pathlib.Path, limit: int) -> None:
    environment = os.environ.copy()
    environment.pop("XZ_DEFAULTS", None)
    environment.pop("XZ_OPT", None)
    with open(os.devnull, "wb") as errors:
        process = subprocess.Popen(
            [
                "xz",
                "--decompress",
                "--stdout",
                "--threads=1",
                "--memlimit-decompress=256MiB",
                str(source),
            ],
            stdout=subprocess.PIPE,
            stderr=errors,
            env=environment,
        )
        total = 0
        try:
            with output.open("xb") as target:
                assert process.stdout is not None
                while True:
                    chunk = process.stdout.read(1024 * 1024)
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > limit:
                        process.kill()
                        fail("compressed Debian member exceeds its decompressed bound")
                    target.write(chunk)
            if process.wait() != 0:
                fail("Debian member is not one canonical complete XZ stream")
        finally:
            if process.poll() is None:
                process.kill()
            process.wait()
    if total == 0 or total % 512 != 0:
        fail("decompressed Debian tar member has an invalid length")


def inspect(package: pathlib.Path, output_root: pathlib.Path) -> None:
    compressed = {}
    with package.open("rb") as source:
        if source.read(len(AR_MAGIC)) != AR_MAGIC:
            fail("Debian package has an invalid outer ar magic")
        for expected in EXPECTED_MEMBERS:
            header = source.read(60)
            if len(header) != 60:
                fail("Debian package is missing an outer ar member")
            if header[58:60] != b"`\n":
                fail("outer ar member header terminator is invalid")
            if header[:16] != expected.encode("ascii").ljust(16, b" "):
                fail("outer ar members are not exact, unique, and ordered")
            exact_decimal(header[16:28], "timestamp")
            if header[28:34] != b"0     " or header[34:40] != b"0     ":
                fail("outer ar member ownership is not root/root")
            if header[40:48] != b"100644  ":
                fail("outer ar member mode is not canonical")
            size = exact_decimal(header[48:58], "member size")
            if expected == "debian-binary":
                if size != 4 or source.read(4) != b"2.0\n":
                    fail("debian-binary must be exactly version 2.0")
            else:
                limit = CONTROL_COMPRESSED_LIMIT if expected.startswith("control") else DATA_COMPRESSED_LIMIT
                if size == 0 or size > limit:
                    fail("compressed Debian member exceeds its bound")
                path = output_root / expected
                copy_exact(source, path, size)
                compressed[expected] = path
            if size % 2 and source.read(1) != b"\n":
                fail("outer ar member padding is invalid")
        if source.read(1):
            fail("outer ar archive contains trailing bytes or unexpected members")
    decompress_bounded(compressed["control.tar.xz"], output_root / "control.tar", CONTROL_TAR_LIMIT)
    decompress_bounded(compressed["data.tar.xz"], output_root / "data.tar", DATA_TAR_LIMIT)


if len(sys.argv) != 3:
    fail("usage: inspect-linux-deb.py <package.deb> <empty-output-directory>")
package_path = pathlib.Path(sys.argv[1])
output_path = pathlib.Path(sys.argv[2])
if not output_path.is_dir() or any(output_path.iterdir()):
    fail("Debian inspection output directory must exist and be empty")
inspect(package_path, output_path)
