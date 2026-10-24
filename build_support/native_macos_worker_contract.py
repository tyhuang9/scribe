"""Fixed unprivileged CPU/Metal CI contracts; never an installer/release CLI.

Cargo's group excludes the separately grouped worker. An abnormal actual test
run retains its entire fixture even after Cargo exits. Only the exact successful
Rust contract establishes worker shutdown/reaping. This is not arbitrary child
containment or a promise of cleanup after SIGKILL.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import signal
import sys

import macos_cpu_component as component
from macos_full_release import MAX_METAL_WORKER

CPU_TESTS = (
    "onnx_worker::tests::macos_native_protected_cpu_worker_authenticates_health_shutdown_and_reap",
    "onnx_worker::tests::macos_native_protected_cpu_vad_authenticates_health_shutdown_and_reap",
)
METAL_TEST = "onnx_worker::tests::macos_native_metal_metadata_helper_launches_authenticated_worker_and_reaps"
MAX_OUTPUT = 8 * 1024 * 1024


def _tool(name: str) -> str:
    path = shutil.which(name)
    component.require(path is not None and Path(path).is_absolute(), "reviewed native toolchain is required")
    # Resolving a rustup shim symlink changes its invocation name.
    return path


def _run(arguments: list[str], seconds: int, environment: dict) -> bytes:
    return component.capture(arguments, timeout=seconds, limit=MAX_OUTPUT,
                             error_limit=MAX_OUTPUT, environment=environment, cwd=component.REPO)


def _execute_contract(name: str, environment: dict, *, offline: bool) -> None:
    component.require(name in (*CPU_TESTS, METAL_TEST), "unknown native worker contract")
    command = [_tool("cargo"), "test", "--locked"]
    if offline:
        command.append("--offline")
    command += ["--release", "--bin", "local-transcriber", name,
                "--manifest-path", str(component.REPO / "Cargo.toml"), "--", "--ignored", "--exact"]
    listing = _run([*command, "--list"], 1200, environment).decode("utf-8").splitlines()
    component.require([line for line in listing if line.endswith(": test")] == [name + ": test"],
                      "native contract discovery differs")
    try:
        # Include both invocation and result validation in the retention boundary.
        result = _run([*command, "--nocapture", "--test-threads=1"], 1200, environment)
        summaries = [line for line in result.decode("utf-8").splitlines() if line.startswith("test result:")]
        component.require(len(summaries) == 1 and re.fullmatch(
            r"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [^\r\n]+",
            summaries[0]) is not None, "native contract success was not established")
    except BaseException:
        # Never signal a stale worker PID or infer its exit from Cargo's group.
        raise component.CleanupUncertain("native worker retirement unconfirmed; retaining fixture") from None


def _environment() -> tuple[Path, dict]:
    component.require(sys.platform == "darwin" and os.geteuid() != 0, "native contracts require an unprivileged Mac")
    for key in ("RUNNER_TEMP", "SHERPA_ONNX_ARCHIVE_DIR", "SCRIBE_BUILD_REVISION"):
        component.require(bool(os.environ.get(key)), "native contract prerequisite is missing")
    for key in ("RUNNER_TEMP", "SHERPA_ONNX_ARCHIVE_DIR"):
        path = Path(os.environ[key])
        component.require(path.is_absolute() and path.is_dir() and not path.is_symlink(),
                          "unsafe native contract directory")
    environment = dict(os.environ)
    for key in ("DOCS_RS", "SCRIBE_BUILDING_WORKER", "SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP"):
        environment.pop(key, None)
    return Path(environment["RUNNER_TEMP"]).resolve(strict=True), environment


def _cpu(app: Path, root: Path, environment: dict) -> None:
    component.require(app.is_absolute() and app.is_dir() and not app.is_symlink(), "unsafe native CPU app")
    app = app.resolve(strict=True)
    # Preserve the original complete app verifier, including full GPU releases.
    # CPU-only development-component admission is not a substitute here.
    _run(["/bin/bash", str(component.REPO / "scripts/verify-macos-release-package.sh"),
          "--app", str(app)], 60, environment)
    worker = app / component.CPU_APP_INPUTS[0]
    digest, size = component.hash_file(worker, component.MAX_WORKER)
    image = root / "image"
    image.mkdir(mode=0o700)
    copy = image / "scribe-inference-worker"
    component.require(component.copy_new(worker, copy, component.MAX_WORKER) == (digest, size),
                      "native CPU fixture copy differs")
    _run(["/usr/bin/codesign", "--verify", "--strict", str(copy)], 60, environment)
    environment = {**environment, "SCRIBE_BUNDLED_WORKER_SHA256": digest,
                   "SCRIBE_MACOS_NATIVE_CPU_FIXTURE_ROOT": str(image)}
    for name in CPU_TESTS:
        _execute_contract(name, environment, offline=True)
    component.require(component.hash_file(worker, component.MAX_WORKER) == (digest, size)
                      and component.hash_file(copy, component.MAX_WORKER) == (digest, size),
                      "native CPU contract changed worker bytes")


def _metal(root: Path, environment: dict) -> None:
    hosts = [line.removeprefix("host: ") for line in
             _run([_tool("rustc"), "-vV"], 30, environment).decode("ascii").splitlines()
             if line.startswith("host: ")]
    component.require(len(hosts) == 1 and hosts[0] in component.TARGETS, "unsupported native Rust host")
    target = hosts[0]
    arch = "aarch64" if target == "aarch64-apple-darwin" else "x86_64"
    contract = root / "macos-metal-metadata-contract"
    _run(["/usr/bin/xcrun", "--sdk", "macosx", "clang", "-Wall", "-Wextra", "-Werror",
          "-mmacosx-version-min=13.0", "-I", str(component.REPO / "native"),
          str(component.REPO / "native/test_scribe_macos_gpu_metadata.m"),
          "-framework", "Foundation", "-framework", "Metal", "-o", str(contract)], 120, environment)
    _run([str(contract)], 30, environment)
    target_root = root / "worker-target"
    build_environment = {**environment, "MACOSX_DEPLOYMENT_TARGET": "13.0", "SCRIBE_BUILDING_WORKER": "1",
                         "CARGO_TARGET_DIR": str(target_root)}
    build_environment.pop("SCRIBE_BUNDLED_WORKER_SHA256", None)
    cargo = _tool("cargo")
    _run([cargo, "build", "--locked", "--release", "--target", target, "--bin", "scribe-inference-worker",
          "--features", "metal-acceleration", "--manifest-path", str(component.REPO / "Cargo.toml")],
         1800, build_environment)
    pack = root / "fixture-pack"
    (pack / "worker").mkdir(parents=True, mode=0o700)
    worker = pack / "worker/scribe-inference-worker"
    component.copy_new(target_root / target / "release/scribe-inference-worker", worker, MAX_METAL_WORKER)
    # Existing ad-hoc/fixture signing only; no publisher key or Keychain identity.
    _run(["/usr/bin/codesign", "--force", "--sign", "-", str(worker)], 60, environment)
    _run(["/usr/bin/codesign", "--verify", "--strict", "--verbose=2", str(worker)], 60, environment)
    digest, size = component.hash_file(worker, MAX_METAL_WORKER)
    author = [cargo, "run", "--locked", "--quiet", "--manifest-path",
              str(component.REPO / "tools/worker-pack-author/Cargo.toml"), "--"]
    _run([*author, "author", "--backend", "metal", "--target-os", "macos", "--target-arch", arch,
          "--pack-id", "metal-native-metadata-helper", "--pack-version", "native-ci-v1", "--pack-root", str(pack),
          "--provider", "transcribe-cpp-metal", "--security-epoch", "1",
          "--worker-path", "worker/scribe-inference-worker", "--fixture-signing"], 900, environment)
    _run([*author, "verify-fixture", "--pack-root", str(pack)], 300, environment)
    _execute_contract(METAL_TEST, {**environment, "SCRIBE_BUNDLED_WORKER_SHA256": digest,
                                 "SCRIBE_MACOS_NATIVE_METAL_METADATA_FIXTURE_PACK": str(pack)}, offline=False)
    component.require(component.hash_file(worker, MAX_METAL_WORKER) == (digest, size),
                      "native Metal contract changed worker bytes")


def run_contract(kind: str, app: Path | None) -> None:
    component.require(kind in ("cpu", "metal") and (app is not None) == (kind == "cpu"),
                      "invalid native contract mode")
    parent, environment = _environment()
    with component.private_workspace(parent, "scribe-native-" + kind + "-") as root:
        try:
            temporary = root / "tmp"
            temporary.mkdir(mode=0o700)
            environment = {**environment, "TMPDIR": str(temporary)}
            if kind == "cpu":
                _cpu(app, root, environment)
            else:
                _metal(root, environment)
        except BaseException:
            # Conservatively retain preparation failures too. In particular, a
            # second cancellation may interrupt capture's retirement finally.
            raise component.CleanupUncertain("native contract failed; retaining fixture") from None
    # Scope cleanup must succeed before main emits success.


def _cancel(_number, _frame):
    raise KeyboardInterrupt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("cpu", "metal"))
    parser.add_argument("--app", type=Path)
    args = parser.parse_args()
    previous = signal.signal(signal.SIGTERM, _cancel)
    try:
        try:
            run_contract(args.kind, args.app)
        except (Exception, KeyboardInterrupt):
            print("native Mac worker contract rejected; no success or cleanup claim", file=sys.stderr)
            return 1
        print("SCRIBE_NATIVE_" + args.kind.upper() + "_CONTRACT_PASS")
        return 0
    finally:
        signal.signal(signal.SIGTERM, previous)


if __name__ == "__main__":
    raise SystemExit(main())
