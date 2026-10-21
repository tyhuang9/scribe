#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

# Source fixtures only. Never install, elevate, sign, or touch a protected root.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="$repo_root/tools/macos-metal-pack-installer/Cargo.toml"
unset SCRIBE_BUILD_REVISION SCRIBE_GPU_PACK_CATALOG SCRIBE_GPU_PACK_RELEASE_AUTHORITY
cargo fmt --manifest-path "$manifest" -- --check
cargo clippy --locked --offline --manifest-path "$manifest" --all-targets -- -D warnings
listing="$(cargo test --locked --offline --manifest-path "$manifest" -- --list)"
count="$(printf '%s\n' "$listing" | awk '/: test$/ { count++ } END { print count+0 }')"
admission_count="$(printf '%s\n' "$listing" | awk '/^admission::tests::metal_admission_.*: test$/ { count++ } END { print count+0 }')"
native_count="$(printf '%s\n' "$listing" | awk '/^native::tests::metal_native_.*: test$/ { count++ } END { print count+0 }')"
embedded_count="$(printf '%s\n' "$listing" | awk '/^admission::tests::metal_embedded_.*: test$/ { count++ } END { print count+0 }')"
bootstrap_count="$(printf '%s\n' "$listing" | awk '/^installer_bootstrap::tests::.*: test$/ { count++ } END { print count+0 }')"
native_expected=0; bootstrap_expected=0
case "$(uname -s)" in
  Darwin) expected=100; admission_expected=19; native_expected=20; bootstrap_expected=8 ;;
  Linux) expected=58; admission_expected=17 ;;
  MINGW*|MSYS*|CYGWIN*) expected=58; admission_expected=13 ;;
  *) echo 'Unsupported Metal writer source-check host.' >&2; exit 1 ;;
esac
[[ "$count" == "$expected" && "$admission_count" == "$admission_expected" && "$embedded_count" == 6 ]] || {
  echo "Metal writer test discovery mismatch: $count total, $admission_count admission." >&2
  exit 1
}
[[ "$native_count" == "$native_expected" && "$bootstrap_count" == "$bootstrap_expected" ]] || {
  echo "Metal native discovery mismatch: $native_count copy, $bootstrap_count bootstrap." >&2; exit 1;
}
cargo test --locked --offline --manifest-path "$manifest" -- --test-threads=1

expect_build_rejection() {
  local output
  if output="$(env "$@" cargo check --release --locked --offline --manifest-path "$manifest" 2>&1)"; then
    echo 'release writer tool accepted missing/malformed build identity.' >&2
    exit 1
  fi
  grep -F 'Metal installer requires a valid compiled SCRIBE_BUILD_REVISION' <<<"$output" >/dev/null || {
    echo "$output" >&2; exit 1;
  }
}
expect_build_rejection
expect_build_rejection SCRIBE_BUILD_REVISION=invalid
SCRIBE_BUILD_REVISION=source-check-fixture cargo check --release --locked --offline --manifest-path "$manifest"
echo 'Metal writer source checks passed; no production installer or worker was run.'
