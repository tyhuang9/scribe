"""Dedicated hosted-Mac construction contract; NEVER execute the resulting entry.

This is a test driver, not a release/installer CLI. The completed CI app and
reviewed toolchain are trusted inputs. No PKG, signing or output upload occurs.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys
import tempfile

import macos_cpu_component as component
import macos_cpu_pkg_entry as entry

INPUTS = component.CPU_APP_INPUTS


def input_snapshot(app: Path) -> dict:
    return component.cpu_app_input_snapshot(app)


def run_contract(app: Path, digest: str, revision: str) -> dict:
    component.require(sys.platform == "darwin", "native CPU construction contract requires macOS")
    component.require(os.geteuid() != 0, "native CPU construction contract must be unprivileged")
    component.validate_identity(digest, revision)
    component.require(app.is_dir() and not app.is_symlink(), "unsafe native contract app")
    app = app.resolve(strict=True)
    component.verify_cpu_app(app)
    before = input_snapshot(app)
    component.require(before[INPUTS[0]][0] == digest, "native contract worker differs from frozen anchor")
    with component.private_workspace(Path(tempfile.gettempdir()), "scribe-native-cpu-entry-") as workspace:
        output, returned_digest, returned_size = entry.build_entry(app, workspace, digest, revision)
        component.require(output == workspace / "postinstall", "native entry published at the wrong path")
        component.require(sorted(path.name for path in workspace.iterdir()) == ["postinstall"],
                          "native construction left unexpected output")
        measured = component.hash_file(output, entry.MAX_ENTRY)
        component.require(measured == (returned_digest, returned_size), "native entry identity mismatch")
        component.require(os.access(output, os.X_OK), "native entry is not executable")
        identity = component.fingerprint(output.lstat())
        architectures = component.capture(["/usr/bin/lipo", str(output), "-archs"]).decode("ascii").split()
        component.require(sorted(architectures) == ["arm64", "x86_64"], "native entry is not exactly universal")
        try:
            entry.build_entry(app, workspace, digest, revision)
        except component.CleanupUncertain:
            raise
        except component.ComponentError as error:
            component.require(str(error) == "CPU entry output already exists", "native repeat rejected for wrong reason")
        else:
            raise component.ComponentError("native construction replaced an existing entry")
        component.require(component.hash_file(output, entry.MAX_ENTRY) == measured
                          and component.fingerprint(output.lstat()) == identity,
                          "native repeated construction changed the entry")
        component.verify_cpu_app(app)
        component.require(input_snapshot(app) == before, "native construction changed application inputs")
        return {"constructed": True, "executed": False, "architectures": sorted(architectures),
                "entry_size_bytes": returned_size, "worker_size_bytes": before[INPUTS[0]][1]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--worker-sha256", required=True)
    parser.add_argument("--build-revision", required=True)
    args = parser.parse_args()
    try:
        report = run_contract(args.app, args.worker_sha256, args.build_revision)
    except component.ComponentError as error:
        print(f"native CPU construction contract failed: {error}", file=sys.stderr)
        return 1
    except OSError:
        # Do not publish local filenames or arbitrary tool diagnostics.
        print("native CPU construction contract failed: filesystem/tool unavailable", file=sys.stderr)
        return 1
    print("SCRIBE_NATIVE_CPU_ENTRY_CONSTRUCTION " + json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
