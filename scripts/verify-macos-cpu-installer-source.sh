#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

# Source tests only. Never run the production install command or write /Library.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="$repo_root/tools/macos-cpu-worker-installer/Cargo.toml"
unset SCRIBE_BUNDLED_WORKER_SHA256 SCRIBE_BUILD_REVISION
cargo fmt --manifest-path "$manifest" -- --check
cargo clippy --locked --offline --manifest-path "$manifest" --all-targets -- -D warnings
listing="$(cargo test --locked --offline --manifest-path "$manifest" -- --list)"
count="$(printf '%s\n' "$listing" | awk '/: test$/ { count++ } END { print count+0 }')"
case "$(uname -s)" in
  Darwin) expected=42 ;;
  *) expected=21 ;;
esac
[[ "$count" == "$expected" ]] || { echo "CPU writer test discovery mismatch: expected $expected, got $count." >&2; exit 1; }
cargo test --locked --offline --manifest-path "$manifest" -- --test-threads=1

# Compile checks only: no installer executable is linked or started. These
# fixed synthetic values are test identities, not an authenticated app image.
expect_build_rejection() {
  local expected="$1"
  shift
  local output
  if output="$(env "$@" cargo check --release --locked --offline --manifest-path "$manifest" 2>&1)"; then
    echo 'release installer unexpectedly accepted missing or malformed authority.' >&2
    exit 1
  fi
  grep -F -- "$expected" <<<"$output" >/dev/null || { echo "$output" >&2; exit 1; }
}
fixture_digest=0000000000000000000000000000000000000000000000000000000000000000
expect_build_rejection 'release installer requires compiled identity: SCRIBE_BUNDLED_WORKER_SHA256'
expect_build_rejection 'invalid compiled installer identity: SCRIBE_BUNDLED_WORKER_SHA256' \
  SCRIBE_BUNDLED_WORKER_SHA256=invalid SCRIBE_BUILD_REVISION=source-check-fixture
expect_build_rejection 'release installer requires compiled identity: SCRIBE_BUILD_REVISION' \
  SCRIBE_BUNDLED_WORKER_SHA256="$fixture_digest"
expect_build_rejection 'invalid compiled installer identity: SCRIBE_BUILD_REVISION' \
  SCRIBE_BUNDLED_WORKER_SHA256="$fixture_digest" SCRIBE_BUILD_REVISION=invalid
SCRIBE_BUNDLED_WORKER_SHA256="$fixture_digest" SCRIBE_BUILD_REVISION=source-check-fixture \
  cargo check --release --locked --offline --manifest-path "$manifest"
echo 'CPU writer release authority: four rejection cases and valid compile passed.'
