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
for command in cargo timeout sha256sum dpkg-deb python3 xz; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done
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
  "SHERPA_ONNX_ARCHIVE_DIR=$archive_root"
  "SCRIBE_BUILD_REVISION=$revision"
  "CARGO_TARGET_DIR=$native_target"
  CARGO_BUILD_JOBS=2
)
run_bounded 1800 "${native_env[@]}" SCRIBE_BUILDING_WORKER=1 \
  cargo build --locked --offline --release --manifest-path "$repo_root/Cargo.toml" \
    --bin scribe-inference-worker --features inference-worker
built_worker="$native_target/release/scribe-inference-worker"
[[ -f "$built_worker" && ! -L "$built_worker" ]] || { echo 'actual CPU worker build is missing.' >&2; exit 1; }
worker="$test_root/inputs/scribe-inference-worker"
# Copies avoid Cargo's hardlinked output aliases; the admitted payload is still
# exactly the built executable, never a synthetic protocol fixture.
cp -- "$built_worker" "$worker"
chmod 0755 "$worker"
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

test_name='onnx_worker::tests::linux_native_cpu_worker_authenticated_ready_health_shutdown_and_reap'
listing="$(run_bounded 1800 "${parent_env[@]}" \
  cargo test --locked --offline --release --manifest-path "$repo_root/Cargo.toml" --bin local-transcriber "$test_name" -- \
    --ignored --exact --list)"
test_count="$(printf '%s\n' "$listing" | awk -v expected="$test_name" '$0 == expected ": test" { count++ } END { print count+0 }')"
[[ "$test_count" == 1 ]] || { echo 'exact native CPU worker test was not discovered once.' >&2; exit 1; }
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
