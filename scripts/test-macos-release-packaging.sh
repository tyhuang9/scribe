#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
scripts=(build-macos-metal-worker-pack.sh build-macos-release.sh sign-notarize-macos-release.sh verify-macos-release-package.sh report-macos-worker-pack-sizes.sh)
for script in "${scripts[@]}"; do bash -n "$repo_root/scripts/$script"; done
for manifest in "$repo_root"/runtime-manifests/gpu-{worker-toolchain,auto-qualification}-macos-{aarch64,x86_64}.json; do jq -e . "$manifest" >/dev/null; done
jq -e '. == {schema_version:1,mode:"default_deny",target_os:"macos",target_arch:"aarch64",entries:[]}' "$repo_root/runtime-manifests/gpu-auto-qualification-macos-aarch64.json" >/dev/null
jq -e '. == {schema_version:1,mode:"default_deny",target_os:"macos",target_arch:"x86_64",entries:[]}' "$repo_root/runtime-manifests/gpu-auto-qualification-macos-x86_64.json" >/dev/null
if rg -n -- '--deep|Ed25519KeyPair|FIXTURE_SEED|SCRIBE_PACK_SIGNING_PRIVATE_KEY=' "$repo_root/scripts"/{build-macos-metal-worker-pack.sh,build-macos-release.sh,sign-notarize-macos-release.sh}; then echo 'macOS release scripts violate the signing-secret contract.' >&2; exit 1; fi
rg -F 'xcrun notarytool submit "$submission_archive"' "$repo_root/scripts/sign-notarize-macos-release.sh" >/dev/null || { echo 'notarization must submit a private ZIP archive, not an app directory.' >&2; exit 1; }
rg -F 'mv "$final_archive" "$requested_archive"' "$repo_root/scripts/sign-notarize-macos-release.sh" >/dev/null || { echo 'notarization must publish the requested ZIP only after stapling and verification.' >&2; exit 1; }
rg -F 'desktop does not embed the final signed CPU worker anchor' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must bind the desktop anchor to the signed CPU worker.' >&2; exit 1; }
rg -F 'SCRIBE_GPU_PACK_RELEASE_AUTHORITY="$release_authority"' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'both desktop slices must compile against the finalized release authority.' >&2; exit 1; }
rg -F 'desktop slice does not embed the exact pack-catalog authority' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must verify each desktop slice embeds the catalog authority.' >&2; exit 1; }
rg -F 'desktop does not embed the exact pack-catalog authority' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'release verifier must bind the installed catalog to desktop authority.' >&2; exit 1; }
rg -F 'lipo -verify_arch "$pack_lipo_arch" "$worker"' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'catalog Metal workers must have their declared single Mach-O slice verified.' >&2; exit 1; }
rg -F 'CPU/UI binary must not load Metal.framework' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'desktop and CPU worker Metal load-command rejection is missing.' >&2; exit 1; }
rg -F 'catalog Metal worker has no Metal load command' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'Metal worker load-command requirement is missing.' >&2; exit 1; }
rg -F 'macOS Metal packs must contain only the manifest, signature, and declared worker.' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'single-payload macOS Metal pack enforcement is missing.' >&2; exit 1; }
if [[ "$(uname -s)" == Darwin ]]; then
  lipo_temp="$(mktemp -d "${TMPDIR:-/tmp}/scribe-macos-lipo-test.XXXXXX")"; trap 'rm -rf "$lipo_temp"' EXIT
  printf 'int main(void) { return 0; }\n' >"$lipo_temp/thin.c"
  xcrun --sdk macosx clang -arch arm64 "$lipo_temp/thin.c" -o "$lipo_temp/arm64"
  xcrun --sdk macosx clang -arch x86_64 "$lipo_temp/thin.c" -o "$lipo_temp/x86_64"
  assert_single_arch() { lipo -verify_arch "$1" "$2" && [[ "$(lipo -archs "$2")" == "$1" ]]; }
  assert_single_arch arm64 "$lipo_temp/arm64" || { echo 'matching thin Mach-O slice was rejected.' >&2; exit 1; }
  if assert_single_arch x86_64 "$lipo_temp/arm64"; then echo 'wrong Mach-O slice was accepted.' >&2; exit 1; fi
  lipo -create "$lipo_temp/arm64" "$lipo_temp/x86_64" -output "$lipo_temp/universal"
  if assert_single_arch arm64 "$lipo_temp/universal"; then echo 'universal Mach-O was accepted for a single-architecture pack.' >&2; exit 1; fi
  rm -rf "$lipo_temp"; trap - EXIT
fi
if [[ -n "${SCRIBE_MACOS_TEST_BUNDLE:-}" ]]; then
  [[ "$(uname -s)" == Darwin ]] || { echo 'bundle mutation tests require macOS.' >&2; exit 1; }
  base="$SCRIBE_MACOS_TEST_BUNDLE"
  bash "$repo_root/scripts/verify-macos-release-package.sh" --app "$base"
  temp="$(mktemp -d "${TMPDIR:-/tmp}/scribe-macos-package-test.XXXXXX")"; trap 'rm -rf "$temp"' EXIT
  for attack in symlink hardlink case apple-double resource-fork tamper catalog downgrade interruption; do
    app="$temp/$attack/Scribe.app"; mkdir -p "$(dirname "$app")"; cp -R "$base" "$app"
    case "$attack" in
      symlink) ln -s /tmp "$app/Contents/Resources/escape" ;;
      hardlink) ln "$app/Contents/MacOS/Scribe" "$app/Contents/Resources/linked-Scribe" ;;
      case) cp "$app/Contents/Resources/worker-pack-catalog.json" "$app/Contents/Resources/Worker-pack-catalog.json" ;;
      apple-double) : >"$app/Contents/Resources/._worker-pack-catalog.json" ;;
      resource-fork) xattr -w com.apple.ResourceFork test "$app/Contents/Resources/worker-pack-catalog.json" ;;
      tamper) printf x >>"$app/Contents/MacOS/Scribe" ;;
      catalog) printf '%s' '{"schema_version":1,"packs":[{}]}' >"$app/Contents/Resources/worker-pack-catalog.json" ;;
      downgrade) printf '%s' '{"schema_version":1,"packs":[{"pack_id":"metal","pack_version":"old","pack_digest":"0000000000000000000000000000000000000000000000000000000000000000","security_epoch":0,"runtime_abi_version":1,"backend":"metal","provider":"transcribe-cpp-metal","target_os":"macos","target_arch":"aarch64","worker_relative_path":"worker/scribe-inference-worker","root":"workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000","installed_size_bytes":1,"compressed_size_bytes":1,"files":["workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000/pack-manifest.json","workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000/pack-manifest.sig","workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000/worker/scribe-inference-worker"]}]}' >"$app/Contents/Resources/worker-pack-catalog.json" ;;
      interruption) mkdir -p "$app/Contents/Resources/workers/packs/metal/test/.stage.interrupted" ;;
    esac
    if bash "$repo_root/scripts/verify-macos-release-package.sh" --app "$app" >/dev/null 2>&1; then echo "verifier accepted $attack fixture" >&2; exit 1; fi
  done
fi
echo 'macOS release packaging contract tests passed.'
