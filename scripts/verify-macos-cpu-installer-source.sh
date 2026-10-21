#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

# Source tests only. Never run the production install command or write /Library.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="$repo_root/tools/macos-cpu-worker-installer/Cargo.toml"
unset SCRIBE_BUNDLED_WORKER_SHA256 SCRIBE_BUILD_REVISION SCRIBE_MACOS_CPU_PACKAGE_WORKER
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

# Dedicated package mode is source-tested only. Never launch its production main.
cargo clippy --locked --offline --manifest-path "$manifest" --all-targets --features installer-package-entry -- -D warnings
package_listing="$(cargo test --locked --offline --manifest-path "$manifest" --features installer-package-entry -- --list)"
package_count="$(printf '%s\n' "$package_listing" | awk '/: test$/ { count++ } END { print count+0 }')"
entry_count="$(printf '%s\n' "$package_listing" | awk '/^package_entry::invocation::tests::.*: test$/ { count++ } END { print count+0 }')"
native_count="$(printf '%s\n' "$package_listing" | awk '/^native::tests::package_native_.*: test$/ { count++ } END { print count+0 }')"
native_expected=0
case "$(uname -s)" in
  Darwin) package_expected=57; native_expected=3 ;;
  Linux) package_expected=33 ;;
  MINGW*|MSYS*|CYGWIN*) package_expected=32 ;;
  *) echo 'Unsupported CPU package source-check host.' >&2; exit 1 ;;
esac
[[ "$package_count" == "$package_expected" && "$entry_count" == 4 && "$native_count" == "$native_expected" ]] || {
  echo "Package entry discovery mismatch: $package_count total, $entry_count entry, $native_count native." >&2; exit 1;
}
cargo test --locked --offline --manifest-path "$manifest" --features installer-package-entry -- --test-threads=1

# Public ASCII source fixture, not a Mach-O worker or installer credential.
payload="$repo_root/tools/macos-cpu-worker-installer/tests/fixtures/package-worker.txt"
payload_digest=60810cd141faf93eacfcdc51ae6ec2974ab3280bc9d9cd322bfefcbae1255e2c
expect_package_rejection() {
  local expected="$1" output
  shift
  if output="$(env SCRIBE_BUNDLED_WORKER_SHA256="$payload_digest" SCRIBE_BUILD_REVISION=source-check-fixture "$@" \
      cargo check --release --locked --offline --manifest-path "$manifest" --features installer-package-entry 2>&1)"; then
    echo 'release package entry accepted invalid embedded authority.' >&2; exit 1;
  fi
  grep -F -- "$expected" <<<"$output" >/dev/null || { echo "$output" >&2; exit 1; }
}
expect_package_rejection 'release package entry requires SCRIBE_MACOS_CPU_PACKAGE_WORKER'
expect_package_rejection 'invalid package worker build path' SCRIBE_MACOS_CPU_PACKAGE_WORKER=relative
expect_package_rejection 'invalid package worker build path' SCRIBE_MACOS_CPU_PACKAGE_WORKER=$'/invalid\ncargo:rustc-env=bad'
expect_package_rejection 'CPU package worker snapshot rejected' SCRIBE_MACOS_CPU_PACKAGE_WORKER="$payload.absent"
expect_package_rejection 'CPU package worker snapshot rejected' SCRIBE_MACOS_CPU_PACKAGE_WORKER="$(dirname "$payload")"
expect_package_rejection 'CPU package worker snapshot rejected' SCRIBE_MACOS_CPU_PACKAGE_WORKER="$payload" SCRIBE_BUNDLED_WORKER_SHA256="$fixture_digest"
SCRIBE_MACOS_CPU_PACKAGE_WORKER="$payload" SCRIBE_BUNDLED_WORKER_SHA256="$payload_digest" SCRIBE_BUILD_REVISION=source-check-fixture \
  cargo check --release --locked --offline --manifest-path "$manifest" --features installer-package-entry
echo 'CPU package entry source checks passed; production entry was never run.'
