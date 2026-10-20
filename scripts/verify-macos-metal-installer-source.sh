#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

# Admission source only. Never install, elevate, sign, or touch a protected root.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="$repo_root/tools/macos-metal-pack-installer/Cargo.toml"
unset SCRIBE_BUILD_REVISION SCRIBE_GPU_PACK_CATALOG SCRIBE_GPU_PACK_RELEASE_AUTHORITY
cargo fmt --manifest-path "$manifest" -- --check
cargo clippy --locked --offline --manifest-path "$manifest" --all-targets -- -D warnings
listing="$(cargo test --locked --offline --manifest-path "$manifest" -- --list)"
count="$(printf '%s\n' "$listing" | awk '/: test$/ { count++ } END { print count+0 }')"
admission_count="$(printf '%s\n' "$listing" | awk '/^admission::tests::metal_admission_.*: test$/ { count++ } END { print count+0 }')"
case "$(uname -s)" in
  Darwin) expected=51; admission_expected=16 ;;
  Linux) expected=37; admission_expected=14 ;;
  MINGW*|MSYS*|CYGWIN*) expected=37; admission_expected=10 ;;
  *) echo 'Unsupported Metal admission source-check host.' >&2; exit 1 ;;
esac
[[ "$count" == "$expected" && "$admission_count" == "$admission_expected" ]] || {
  echo "Metal admission test discovery mismatch: $count total, $admission_count admission." >&2
  exit 1
}
cargo test --locked --offline --manifest-path "$manifest" -- --test-threads=1

expect_build_rejection() {
  local output
  if output="$(env "$@" cargo check --release --locked --offline --manifest-path "$manifest" 2>&1)"; then
    echo 'release admission tool accepted missing/malformed build identity.' >&2
    exit 1
  fi
  grep -F 'Metal installer requires a valid compiled SCRIBE_BUILD_REVISION' <<<"$output" >/dev/null || {
    echo "$output" >&2; exit 1;
  }
}
expect_build_rejection
expect_build_rejection SCRIBE_BUILD_REVISION=invalid
SCRIBE_BUILD_REVISION=source-check-fixture cargo check --release --locked --offline --manifest-path "$manifest"
echo 'Metal admission source checks passed; no installer or worker was run.'
