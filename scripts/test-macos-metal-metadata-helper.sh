#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ "$(uname -s)" == Darwin ]] || { echo 'native Metal metadata helper tests require macOS.' >&2; exit 1; }
: "${RUNNER_TEMP:?RUNNER_TEMP must identify the private CI temporary root.}"
: "${SHERPA_ONNX_ARCHIVE_DIR:?SHERPA_ONNX_ARCHIVE_DIR must identify the reviewed native archives.}"
: "${SCRIBE_BUILD_REVISION:?SCRIBE_BUILD_REVISION must bind the native worker and test app.}"
[[ -d "$RUNNER_TEMP" && ! -L "$RUNNER_TEMP" ]] || { echo 'RUNNER_TEMP must be a real directory.' >&2; exit 1; }
[[ -d "$SHERPA_ONNX_ARCHIVE_DIR" && ! -L "$SHERPA_ONNX_ARCHIVE_DIR" ]] || {
  echo 'SHERPA_ONNX_ARCHIVE_DIR must be a real directory.' >&2
  exit 1
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
runner_root="$(cd "$RUNNER_TEMP" && pwd -P)"
test_root="$(mktemp -d "$runner_root/scribe-metal-metadata-helper.XXXXXX")"

cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$test_root" != "$runner_root"/scribe-metal-metadata-helper.* || -L "$test_root" ]]; then
    echo 'refusing to clean an unvalidated Metal metadata helper root.' >&2
    exit 1
  fi
  if ! rm -rf -- "$test_root"; then
    echo 'could not clean the Metal metadata helper test root.' >&2
    status=1
  fi
  exit "$status"
}
trap cleanup EXIT

test_root="$(cd "$test_root" && pwd -P)"
[[ "$test_root" == "$runner_root"/scribe-metal-metadata-helper.* && ! -L "$test_root" ]] || {
  echo 'temporary Metal metadata helper root escaped RUNNER_TEMP.' >&2
  exit 1
}

run_bounded() {
  local seconds="$1"
  shift
  python3 - "$seconds" "$@" <<'PY'
import os
import signal
import subprocess
import sys

timeout = int(sys.argv[1])
command = sys.argv[2:]
process = subprocess.Popen(command, start_new_session=True)
try:
    status = process.wait(timeout=timeout)
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    print(f"command exceeded {timeout} seconds: {command[0]}", file=sys.stderr)
    sys.exit(124)
sys.exit(status)
PY
}

rust_target="$(rustc -vV | awk '/^host: / { print $2 }')"
case "$rust_target" in
  aarch64-apple-darwin) target_arch='aarch64' ;;
  x86_64-apple-darwin) target_arch='x86_64' ;;
  *) echo "unsupported native macOS Rust target: $rust_target" >&2; exit 1 ;;
esac

contract_binary="$test_root/macos-metal-metadata-contract"
run_bounded 120 xcrun --sdk macosx clang \
  -Wall -Wextra -Werror -mmacosx-version-min=13.0 \
  -I "$repo_root/native" \
  "$repo_root/native/test_scribe_macos_gpu_metadata.m" \
  -framework Foundation -framework Metal -o "$contract_binary"
run_bounded 30 "$contract_binary"

worker_target="$test_root/worker-target"
run_bounded 1800 env \
  -u DOCS_RS \
  -u SCRIBE_BUNDLED_WORKER_SHA256 \
  -u SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP \
  MACOSX_DEPLOYMENT_TARGET=13.0 \
  SCRIBE_BUILDING_WORKER=1 \
  CARGO_TARGET_DIR="$worker_target" \
  cargo build --locked --release --target "$rust_target" \
    --bin scribe-inference-worker --features metal-acceleration \
    --manifest-path "$repo_root/Cargo.toml"

worker="$worker_target/$rust_target/release/scribe-inference-worker"
[[ -f "$worker" && ! -L "$worker" ]] || { echo 'native Metal worker build is missing.' >&2; exit 1; }
pack_root="$test_root/fixture-pack"
mkdir -p "$pack_root/worker"
cp -p "$worker" "$pack_root/worker/scribe-inference-worker"
run_bounded 60 codesign --force --sign - "$pack_root/worker/scribe-inference-worker"
run_bounded 60 codesign --verify --strict --verbose=2 "$pack_root/worker/scribe-inference-worker"

# Fixture authoring happens only after the final ad-hoc signature has changed
# the Mach-O bytes. No project signing key or Keychain identity is involved.
run_bounded 900 cargo run --locked --quiet \
  --manifest-path "$repo_root/tools/worker-pack-author/Cargo.toml" -- \
  author --backend metal --target-os macos --target-arch "$target_arch" \
  --pack-id metal-native-metadata-helper --pack-version native-ci-v1 \
  --pack-root "$pack_root" --provider transcribe-cpp-metal --security-epoch 1 \
  --worker-path worker/scribe-inference-worker --fixture-signing
run_bounded 300 cargo run --locked --quiet \
  --manifest-path "$repo_root/tools/worker-pack-author/Cargo.toml" -- \
  verify-fixture --pack-root "$pack_root"

mkdir -p "$test_root/tmp"
test_name='onnx_worker::tests::macos_native_metal_metadata_helper_launches_authenticated_worker_and_reaps'
test_listing="$(
  run_bounded 1200 env \
    TMPDIR="$test_root/tmp" \
    SCRIBE_MACOS_NATIVE_METAL_METADATA_FIXTURE_PACK="$pack_root" \
    cargo test --locked --bin local-transcriber "$test_name" -- \
      --ignored --exact --list
)"
test_count="$(printf '%s\n' "$test_listing" | awk -v expected="$test_name" '$0 == expected ": test" { count++ } END { print count+0 }')"
[[ "$test_count" == 1 ]] || { echo 'exact native Metal metadata helper test was not discovered once.' >&2; exit 1; }

run_bounded 1200 env \
  TMPDIR="$test_root/tmp" \
  SCRIBE_MACOS_NATIVE_METAL_METADATA_FIXTURE_PACK="$pack_root" \
  cargo test --locked --bin local-transcriber "$test_name" -- \
    --ignored --exact --nocapture --test-threads=1
