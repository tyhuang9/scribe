#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || { echo 'Linux x86_64 is required.' >&2; exit 1; }
: "${RUNNER_TEMP:?RUNNER_TEMP must identify the private temporary root.}"
: "${SHERPA_ONNX_ARCHIVE_DIR:?SHERPA_ONNX_ARCHIVE_DIR must identify the reviewed native archives.}"
[[ -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" && -d "$SHERPA_ONNX_ARCHIVE_DIR" && ! -L "$SHERPA_ONNX_ARCHIVE_DIR" ]] || {
  echo 'native test roots must be real directories.' >&2
  exit 1
}
[[ "$(rustc -vV | awk '/^host: / { print $2 }')" == x86_64-unknown-linux-gnu ]] || { echo 'native GNU x86_64 Rust host is required.' >&2; exit 1; }
if [[ -r /proc/sys/vm/memfd_noexec && "$(< /proc/sys/vm/memfd_noexec)" != 0 ]]; then
  echo 'native worker tests require executable memfd allowance; no host policy is changed.' >&2
  exit 1
fi
for command in cargo timeout sha256sum dpkg-deb python3 xz readelf; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
runner_root="$(cd "$RUNNER_TEMP" && pwd -P)"
archive_root="$(cd "$SHERPA_ONNX_ARCHIVE_DIR" && pwd -P)"
revision="${SCRIBE_BUILD_REVISION:-$(git -C "$repo_root" rev-parse --verify HEAD)}"
[[ "$revision" =~ ^[0-9a-f]{40}$ ]] || { echo 'native build revision must be a full lowercase Git commit.' >&2; exit 1; }
umask 077
test_root="$(mktemp -d "$runner_root/scribe-linux-native-cpu.XXXXXX")"
cleanup() {
  local status=$?
  trap - EXIT
  [[ "$test_root" == "$runner_root"/scribe-linux-native-cpu.* && ! -L "$test_root" ]] || {
    echo 'refusing to clean an unvalidated native CPU test root.' >&2
    exit 1
  }
  rm -rf -- "$test_root" || status=1
  exit "$status"
}
trap cleanup EXIT
run_bounded() {
  local seconds="$1"
  shift
  timeout --signal=TERM --kill-after=5s "${seconds}s" "$@"
}

native_target="$test_root/cargo-target"
mkdir -p "$test_root/inputs" "$test_root/tmp"
native_env=(env
  -u DOCS_RS -u SCRIBE_BUILDING_WORKER -u SCRIBE_BUNDLED_WORKER_SHA256
  -u CARGO_BUILD_TARGET -u SHERPA_ONNX_LIB_DIR -u SHERPA_ONNX_ALLOW_DEBUG_DOWNLOAD
  -u CMAKE_ARGS -u TRANSCRIBE_CMAKE_ARGS
  # The current CPU-only package owns no BLAS runtime. Do not let build-host
  # discovery add one implicitly; this is the upstream-supported static profile.
  TRANSCRIBE_CMAKE_ARGS=-DTRANSCRIBE_USE_SYSTEM_BLAS=OFF
  "SHERPA_ONNX_ARCHIVE_DIR=$archive_root"
  "SCRIBE_BUILD_REVISION=$revision"
  "CARGO_TARGET_DIR=$native_target"
  CARGO_BUILD_JOBS=2
)
check_native_cpu_profile() {
  shopt -s nullglob
  local caches=("$native_target"/release/build/transcribe-cpp-sys-*/out/build/CMakeCache.txt)
  local manifests=("$native_target"/release/build/transcribe-cpp-sys-*/out/lib/transcribe-link.json
    "$native_target"/release/build/transcribe-cpp-sys-*/out/lib64/transcribe-link.json)
  [[ ${#caches[@]} == 1 && ${#manifests[@]} == 1 ]] || { echo 'expected exactly one actual native CPU build profile.' >&2; exit 1; }
  grep -Fxq 'TRANSCRIBE_USE_SYSTEM_BLAS:BOOL=OFF' "${caches[0]}" || { echo 'actual native CPU build did not disable unowned system BLAS.' >&2; exit 1; }
  python3 - "${manifests[0]}" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
if path.stat().st_size > 4 * 1024 * 1024:
    raise SystemExit('native CPU link manifest exceeds its bound')
manifest = json.loads(path.read_bytes())
if manifest.get('shared') is not False or manifest.get('library_paths') != []:
    raise SystemExit('native CPU profile must not add shared native or absolute host libraries')
for key in ('libraries', 'system_libs', 'link_flags'):
    values = manifest.get(key)
    if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
        raise SystemExit('invalid native CPU link manifest inventory')
    if any(any(term in value.lower() for term in ('blas', 'blis', 'mkl')) for value in values):
        raise SystemExit('native CPU link manifest contains an unowned BLAS dependency')
PY
}
reject_blas_runtime() {
  local dynamic
  dynamic="$(readelf --dynamic "$1")"
  if grep -Ei '\(NEEDED\).*\[[^]]*(blas|blis|mkl)[^]]*\]' <<<"$dynamic" >/dev/null; then
    echo 'actual native CPU deliverable requires an unowned BLAS runtime.' >&2
    exit 1
  fi
}
run_bounded 1800 "${native_env[@]}" SCRIBE_BUILDING_WORKER=1 \
  cargo build --locked --offline --release --manifest-path "$repo_root/Cargo.toml" \
    --bin scribe-inference-worker --features inference-worker
built_worker="$native_target/release/scribe-inference-worker"
[[ -f "$built_worker" && ! -L "$built_worker" ]] || { echo 'actual CPU worker build is missing.' >&2; exit 1; }
check_native_cpu_profile
worker="$test_root/inputs/scribe-inference-worker"
# Copies avoid Cargo's hardlinked output aliases; the admitted payload is still
# exactly the built executable, never a synthetic protocol fixture.
cp -- "$built_worker" "$worker"
chmod 0755 "$worker"
reject_blas_runtime "$worker"
worker_digest="$(sha256sum -- "$worker" | awk '{print $1}')"
[[ "$worker_digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'CPU worker digest is invalid.' >&2; exit 1; }
parent_env=("${native_env[@]}"
  "SCRIBE_BUNDLED_WORKER_SHA256=$worker_digest"
  "SCRIBE_LINUX_NATIVE_CPU_WORKER_EXE=$worker"
  "TMPDIR=$test_root/tmp"
)
run_bounded 1800 "${parent_env[@]}" \
  cargo build --locked --offline --release --manifest-path "$repo_root/Cargo.toml" --bin local-transcriber
built_desktop="$native_target/release/local-transcriber"
[[ -f "$built_desktop" && ! -L "$built_desktop" ]] || { echo 'actual desktop build is missing.' >&2; exit 1; }
desktop="$test_root/inputs/local-transcriber"
cp -- "$built_desktop" "$desktop"
chmod 0755 "$desktop"
reject_blas_runtime "$desktop"

test_name='onnx_worker::tests::linux_native_cpu_worker_authenticated_ready_health_shutdown_and_reap'
listing="$(run_bounded 1800 "${parent_env[@]}" \
  cargo test --locked --offline --release --manifest-path "$repo_root/Cargo.toml" --bin local-transcriber "$test_name" -- \
    --ignored --exact --list)"
test_count="$(printf '%s\n' "$listing" | awk -v expected="$test_name" '$0 == expected ": test" { count++ } END { print count+0 }')"
[[ "$test_count" == 1 ]] || { echo 'exact native CPU worker test was not discovered once.' >&2; exit 1; }
check_native_cpu_profile
run_bounded 120 "${parent_env[@]}" \
  cargo test --locked --offline --release --manifest-path "$repo_root/Cargo.toml" --bin local-transcriber "$test_name" -- \
    --ignored --exact --nocapture --test-threads=1
[[ "$(sha256sum -- "$worker" | awk '{print $1}')" == "$worker_digest" ]] || { echo 'native test changed the worker input.' >&2; exit 1; }

# Exercise the existing six-file, default-deny assembler with actual linked
# executables. This creates and verifies a package; it never installs one.
package="$test_root/Scribe-native-cpu.deb"
epoch="$(git -C "$repo_root" show -s --format=%ct HEAD)"
run_bounded 300 env SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" \
  bash "$repo_root/scripts/build-linux-release-package.sh" \
    --desktop "$desktop" --cpu-worker "$worker" --output "$package" --version 0.1.0
run_bounded 120 bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$package"
run_bounded 120 bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$package"
echo 'Actual Linux CPU worker startup, authenticated IPC, reaping, and package verification passed.'
