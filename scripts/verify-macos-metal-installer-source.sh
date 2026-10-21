#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

# Source fixtures only. Never install, elevate, sign, or touch a protected root.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
manifest="$repo_root/tools/macos-metal-pack-installer/Cargo.toml"
unset SCRIBE_BUILD_REVISION SCRIBE_GPU_PACK_CATALOG SCRIBE_GPU_PACK_RELEASE_AUTHORITY
unset SCRIBE_MACOS_METAL_PACKAGE_PACK SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256
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

# Feature tests exercise private unprivileged fixtures, NEVER production main.
cargo clippy --locked --offline --manifest-path "$manifest" --all-targets --features installer-package-entry -- -D warnings
package_listing="$(cargo test --locked --offline --manifest-path "$manifest" --features installer-package-entry -- --list)"
package_count="$(printf '%s\n' "$package_listing" | awk '/: test$/ { count++ } END { print count+0 }')"
context_count="$(printf '%s\n' "$package_listing" | awk '/^package_entry::invocation::tests::.*: test$/ { count++ } END { print count+0 }')"
snapshot_count="$(printf '%s\n' "$package_listing" | awk '/^package_snapshot::tests::.*: test$/ { count++ } END { print count+0 }')"
entry_count="$(printf '%s\n' "$package_listing" | awk '/^package_entry::tests::.*: test$/ { count++ } END { print count+0 }')"
case "$(uname -s)" in
  Darwin) package_expected=111 ;;
  *) package_expected=69 ;;
esac
[[ "$package_count" == "$package_expected" && "$context_count" == 4 && "$snapshot_count" == 7 && "$entry_count" == 1 ]] || {
  echo "Metal package discovery mismatch: $package_count total, $context_count context, $snapshot_count snapshot, $entry_count entry." >&2; exit 1;
}
cargo test --locked --offline --manifest-path "$manifest" --features installer-package-entry -- --test-threads=1

# Public ASCII shape fixtures have NO valid signature/release authority. This
# positive checks compilation only; runtime admission MUST reject these bytes.
fixture_root="$repo_root/tools/macos-metal-pack-installer/tests/fixtures"
native_build_path() {
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) cygpath -w "$1" ;;
    *) printf '%s\n' "$1" ;;
  esac
}
case "$(uname -m)" in
  arm64|aarch64) arch=aarch64; other_arch=x86_64
    manifest_sha=659a655de4353438a4d857def544bff73d65bea774794b336bfb8fe9d9b9ada4
    other_sha=57a1d1883b6caeb3b20af415d4b20632d20e6827bba2bfb63a28b3ed12017b34 ;;
  x86_64) arch=x86_64; other_arch=aarch64
    manifest_sha=57a1d1883b6caeb3b20af415d4b20632d20e6827bba2bfb63a28b3ed12017b34
    other_sha=659a655de4353438a4d857def544bff73d65bea774794b336bfb8fe9d9b9ada4 ;;
  *) echo 'Unsupported Metal package source-check architecture.' >&2; exit 1 ;;
esac
pack="$(native_build_path "$fixture_root/package-$arch")"
other_pack="$(native_build_path "$fixture_root/package-$other_arch")"
release_inputs=(SCRIBE_GPU_PACK_CATALOG="$(native_build_path "$fixture_root/catalog.json")" SCRIBE_GPU_PACK_RELEASE_AUTHORITY="$(native_build_path "$fixture_root/authority.json")")
expect_package_rejection() {
  local expected="$1" output
  shift
  if output="$(env SCRIBE_BUILD_REVISION=source-check-fixture "$@" cargo check --release --locked --offline --manifest-path "$manifest" --features installer-package-entry 2>&1)"; then
    echo 'release Metal package feature accepted invalid build inputs.' >&2; exit 1;
  fi
  grep -F -- "$expected" <<<"$output" >/dev/null || { echo "$output" >&2; exit 1; }
}
expect_package_rejection 'release Metal package entry requires SCRIBE_MACOS_METAL_PACKAGE_PACK'
expect_package_rejection 'Metal package entry requires explicit release inputs' SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack"
expect_package_rejection 'Metal package release input rejected' SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack" \
  "${release_inputs[@]}" SCRIBE_GPU_PACK_RELEASE_AUTHORITY="$(native_build_path "$repo_root/runtime-manifests/gpu-pack-release-authority-macos-empty.json")"
expect_package_rejection 'Metal package requires the frozen manifest SHA-256' SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack" "${release_inputs[@]}"
expect_package_rejection 'invalid Metal package build path' SCRIBE_MACOS_METAL_PACKAGE_PACK=relative "${release_inputs[@]}"
expect_package_rejection 'invalid Metal package build path' SCRIBE_MACOS_METAL_PACKAGE_PACK=$'/invalid\ncargo:rustc-env=bad' "${release_inputs[@]}"
expect_package_rejection 'Metal package snapshot rejected' SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack.absent" \
  SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256="$manifest_sha" "${release_inputs[@]}"
expect_package_rejection 'Metal package snapshot rejected' SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack" \
  SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256=invalid "${release_inputs[@]}"
expect_package_rejection 'Metal package snapshot rejected' SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack" \
  SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256="$other_sha" "${release_inputs[@]}"
expect_package_rejection 'Metal package snapshot rejected' SCRIBE_MACOS_METAL_PACKAGE_PACK="$other_pack" \
  SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256="$other_sha" "${release_inputs[@]}"
env SCRIBE_BUILD_REVISION=source-check-fixture SCRIBE_MACOS_METAL_PACKAGE_PACK="$pack" \
  SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256="$manifest_sha" "${release_inputs[@]}" \
  cargo check --release --locked --offline --manifest-path "$manifest" --features installer-package-entry
echo 'Metal package entry source checks passed; production entry was never run.'
