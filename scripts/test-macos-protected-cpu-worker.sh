#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ "$(uname -s)" == Darwin ]] || { echo 'protected CPU contract requires native macOS.' >&2; exit 1; }
[[ "$#" == 2 && "$1" == --app ]] || { echo 'Usage: test-macos-protected-cpu-worker.sh --app <verified Scribe.app>' >&2; exit 2; }
: "${RUNNER_TEMP:?private CI temporary root is required.}"
: "${SHERPA_ONNX_ARCHIVE_DIR:?reviewed native archives are required.}"
: "${SCRIBE_BUILD_REVISION:?must match the supplied release app CPU worker build.}"
[[ -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" ]] || { echo 'unsafe CI temporary root.' >&2; exit 1; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
app="$2"
bash "$repo_root/scripts/verify-macos-release-package.sh" --app "$app"
worker="$app/Contents/MacOS/scribe-inference-worker"
digest="$(shasum -a 256 "$worker" | awk '{print $1}')"
[[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'invalid final CPU worker digest.' >&2; exit 1; }

runner_root="$(cd "$RUNNER_TEMP" && pwd -P)"
test_root="$(mktemp -d "$runner_root/scribe-protected-cpu.XXXXXX")"
cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$test_root" != "$runner_root"/scribe-protected-cpu.* || -L "$test_root" ]]; then
    echo 'refusing cleanup outside the private CPU fixture root.' >&2
    exit 1
  fi
  if ! rm -rf -- "$test_root"; then status=1; fi
  exit "$status"
}
trap cleanup EXIT
test_root="$(cd "$test_root" && pwd -P)"
[[ "$test_root" == "$runner_root"/scribe-protected-cpu.* && ! -L "$test_root" ]] || exit 1
mkdir -m 700 "$test_root/image" "$test_root/tmp"
# Copy bytes into a fresh unprivileged fixture, never the real protected root.
# Preserve the existing final signature; no signing key or installer is used.
cp -p "$worker" "$test_root/image/scribe-inference-worker"
[[ "$(shasum -a 256 "$test_root/image/scribe-inference-worker" | awk '{print $1}')" == "$digest" ]] || exit 1
codesign --verify --strict "$test_root/image/scribe-inference-worker"

run_bounded() {
  python3 - "$@" <<'PY'
import os
import signal
import subprocess
import sys

timeout = int(sys.argv[1])
process = subprocess.Popen(sys.argv[2:], start_new_session=True)
try:
    status = process.wait(timeout=timeout)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    print("native CPU contract command exceeded its deadline", file=sys.stderr)
    sys.exit(124)
sys.exit(status)
PY
}

test_name='onnx_worker::tests::macos_native_protected_cpu_worker_authenticates_health_shutdown_and_reap'
run_test() {
  run_bounded 1200 env -u DOCS_RS -u SCRIBE_BUILDING_WORKER \
    -u SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP \
    SCRIBE_BUNDLED_WORKER_SHA256="$digest" \
    SCRIBE_MACOS_NATIVE_CPU_FIXTURE_ROOT="$test_root/image" \
    TMPDIR="$test_root/tmp" \
    cargo test --locked --offline --release --bin local-transcriber "$test_name" \
      --manifest-path "$repo_root/Cargo.toml" -- --ignored --exact "$@"
}
listing="$(run_test --list)"
count="$(printf '%s\n' "$listing" | awk -v expected="$test_name" '$0 == expected ": test" { count++ } END { print count+0 }')"
[[ "$count" == 1 ]] || { echo 'native CPU contract was not discovered exactly once.' >&2; exit 1; }
run_test --nocapture --test-threads=1
