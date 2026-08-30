#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
trap 'status=$?; echo "macOS release packaging contract check failed at line $LINENO (exit $status)." >&2; exit "$status"' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
scripts=(build-macos-metal-worker-pack.sh build-macos-release.sh prepare-macos-release-inputs.sh sign-notarize-macos-release.sh verify-macos-release-package.sh report-macos-worker-pack-sizes.sh)
for script in "${scripts[@]}"; do bash -n "$repo_root/scripts/$script"; done
for manifest in "$repo_root"/runtime-manifests/gpu-{worker-toolchain,auto-qualification}-macos-{aarch64,x86_64}.json; do jq -e . "$manifest" >/dev/null; done
jq -e '. == {schema_version:2,catalog_sha256:"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc",release_security_epoch:0,keychain_access_group:"",entries:[]}' "$repo_root/runtime-manifests/gpu-pack-release-authority-macos-empty.json" >/dev/null || { echo 'default macOS release authority must remain canonical default-deny.' >&2; exit 1; }
jq -e '. == {schema_version:1,keychain_access_group:""}' "$repo_root/runtime-manifests/gpu-keychain-namespace-macos-release.json" >/dev/null || { echo 'production Keychain namespace must remain explicitly unprovisioned until reviewed.' >&2; exit 1; }
jq -e '. == {schema_version:1,mode:"default_deny",target_os:"macos",target_arch:"aarch64",entries:[]}' "$repo_root/runtime-manifests/gpu-auto-qualification-macos-aarch64.json" >/dev/null
jq -e '. == {schema_version:1,mode:"default_deny",target_os:"macos",target_arch:"x86_64",entries:[]}' "$repo_root/runtime-manifests/gpu-auto-qualification-macos-x86_64.json" >/dev/null
grep -F '339c8fc19bb4b26e118c80792bbc4546eb263040fac36ef0cc027ec29c756b44' "$repo_root/scripts/prepare-macos-release-inputs.sh" >/dev/null || { echo 'reviewed arm64 sherpa-onnx archive digest is missing.' >&2; exit 1; }
grep -F '689f8167a52dc4dbaf05369705e26c8f203c748a8c342750fdfdcd8ca6bb8699' "$repo_root/scripts/prepare-macos-release-inputs.sh" >/dev/null || { echo 'reviewed x86_64 sherpa-onnx archive digest is missing.' >&2; exit 1; }
if grep -En -- '--deep|Ed25519KeyPair|FIXTURE_SEED|SCRIBE_PACK_SIGNING_PRIVATE_KEY=' "$repo_root/scripts"/{build-macos-metal-worker-pack.sh,build-macos-release.sh,sign-notarize-macos-release.sh}; then echo 'macOS release scripts violate the signing-secret contract.' >&2; exit 1; fi
if grep -En 'strings .* \| grep|LC_ALL=C grep -[^ ]*q|LC_ALL=C grep .* -q' "$repo_root/scripts"/{build-macos-release.sh,verify-macos-release-package.sh}; then echo 'binary authority checks must scan complete binaries without early-exit pipelines.' >&2; exit 1; fi
grep -F 'grep -aFf "$release_authority" "$desktop_path"' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must search each desktop slice for the complete authority bytes.' >&2; exit 1; }
grep -F 'grep -aFf "$authority" "$macos/Scribe"' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'release verifier must search the final desktop for the complete authority bytes.' >&2; exit 1; }
grep -F 'xcrun notarytool submit "$submission_archive"' "$repo_root/scripts/sign-notarize-macos-release.sh" >/dev/null || { echo 'notarization must submit a private ZIP archive, not an app directory.' >&2; exit 1; }
grep -F 'mv "$final_archive" "$requested_archive"' "$repo_root/scripts/sign-notarize-macos-release.sh" >/dev/null || { echo 'notarization must publish the requested ZIP only after stapling and verification.' >&2; exit 1; }
grep -F 'desktop does not embed the final signed CPU worker anchor' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must bind the desktop anchor to the signed CPU worker.' >&2; exit 1; }
grep -F 'SCRIBE_GPU_PACK_RELEASE_AUTHORITY="$release_authority"' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'both desktop slices must compile against the finalized release authority.' >&2; exit 1; }
grep -F 'desktop slice does not embed the exact pack-catalog authority' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must verify each desktop slice embeds the catalog authority.' >&2; exit 1; }
grep -F 'desktop does not embed the exact pack-catalog authority' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'release verifier must bind the installed catalog to desktop authority.' >&2; exit 1; }
grep -F 'desktop does not embed the exact release authority' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'release verifier must bind the desktop to the exact release authority.' >&2; exit 1; }
grep -F 'release_security_epoch:$release_security_epoch' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must write the requested release epoch into the authority.' >&2; exit 1; }
grep -F 'keychain_access_group:$keychain_access_group' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must bind the selected Keychain group into the authority.' >&2; exit 1; }
grep -F -- '--security-epoch "$release_security_epoch"' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must pass the release epoch to every Metal pack author invocation.' >&2; exit 1; }
grep -F -- '--security-epoch "$security_epoch"' "$repo_root/scripts/build-macos-metal-worker-pack.sh" >/dev/null || { echo 'Metal pack author must use the supplied security epoch.' >&2; exit 1; }
! grep -F -- '--security-epoch 1' "$repo_root/scripts/build-macos-metal-worker-pack.sh" "$repo_root/scripts/build-macos-release.sh" || { echo 'hard-coded Metal security epoch regression detected.' >&2; exit 1; }
grep -F 'Metal packs require an explicit positive SCRIBE_MACOS_GPU_RELEASE_SECURITY_EPOCH.' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'epoch-zero Metal catalog rejection is missing.' >&2; exit 1; }
grep -F '9007199254740991' "$repo_root/scripts/build-macos-release.sh" "$repo_root/scripts/build-macos-metal-worker-pack.sh" "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'exact JSON epoch bound is missing.' >&2; exit 1; }
grep -F 'SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'protected release Keychain group requirement is missing.' >&2; exit 1; }
grep -F '^[A-Z0-9]{10}\.com\.scribe\.local-transcriber$' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'protected release must reject wildcard or unexpected Keychain groups.' >&2; exit 1; }
grep -F 'provisioning profile must be a regular non-symlink file.' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'protected release must reject unsafe provisioning-profile paths.' >&2; exit 1; }
grep -F 'provisioning profile keychain groups do not authorize exactly the selected group.' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'protected release must reject profile/group mismatch.' >&2; exit 1; }
grep -F 'profile_application_identifier' "$repo_root/scripts/build-macos-release.sh" "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'profile application-identifier authorization check is missing.' >&2; exit 1; }
grep -F 'profile_team_identifier' "$repo_root/scripts/build-macos-release.sh" "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'profile team-identifier authorization check is missing.' >&2; exit 1; }
grep -F 'signed target does not expose the exact reviewed application, team, and Keychain identifiers' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'protected release must verify final effective desktop entitlements.' >&2; exit 1; }
grep -F 'source-reviewed Keychain namespace' "$repo_root/scripts/build-macos-release.sh" "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'protected release must bind the source-reviewed Keychain namespace.' >&2; exit 1; }
! grep -F 'Entitlements:' "$repo_root/scripts/build-macos-release.sh" "$repo_root/scripts/verify-macos-release-package.sh" || { echo 'plutil profile extraction must use key paths, not PlistBuddy colon syntax.' >&2; exit 1; }
! grep -E 'plutil -extract com\.' "$repo_root/scripts/build-macos-release.sh" "$repo_root/scripts/verify-macos-release-package.sh" || { echo 'dotted entitlement names must use exact JSON object-key lookup.' >&2; exit 1; }
grep -F '.["com.apple.security.device.audio-input"] == true' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'release verifier must require the exact microphone entitlement key.' >&2; exit 1; }
grep -F 'worker target must not expose the desktop Keychain access group' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must keep workers free of the desktop Keychain entitlement.' >&2; exit 1; }
grep -F 'worker target exposes malformed entitlement data' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release builder must reject malformed worker entitlement data.' >&2; exit 1; }
grep -F 'embedded provisioning profile keychain groups are not exact.' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'package verifier must reject profile group mismatch.' >&2; exit 1; }
grep -F 'application inventory is not exact.' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'package verifier must enforce profile-aware exact inventory.' >&2; exit 1; }
grep -F 'worker must not expose the desktop Keychain group' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'package verifier must reject Keychain-entitled workers.' >&2; exit 1; }
grep -F 'worker exposes malformed entitlement data' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'package verifier must reject malformed worker entitlement data.' >&2; exit 1; }
[[ -f "$repo_root/installer/macos/Scribe.protected.entitlements.template" ]] || { echo 'protected desktop entitlement template is missing.' >&2; exit 1; }
grep -F '<string>${SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP}</string>' "$repo_root/installer/macos/Scribe.protected.entitlements.template" >/dev/null || { echo 'protected entitlement template must contain only the generated group placeholder.' >&2; exit 1; }
grep -F '<key>com.apple.application-identifier</key>' "$repo_root/installer/macos/Scribe.protected.entitlements.template" >/dev/null || { echo 'protected entitlement template must bind the application identifier.' >&2; exit 1; }
grep -F '<string>${SCRIBE_MACOS_GPU_ROLLBACK_TEAM_IDENTIFIER}</string>' "$repo_root/installer/macos/Scribe.protected.entitlements.template" >/dev/null || { echo 'protected entitlement template must bind the team identifier.' >&2; exit 1; }
embed_line="$(grep -En 'embedded\.provisionprofile' "$repo_root/scripts/build-macos-release.sh" | head -n 1 | cut -d: -f1)"
grep -F 'codesign_args=(--force --sign "$identity" --options runtime)' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'release signing arguments must always include the hardened runtime options.' >&2; exit 1; }
grep -F 'if [[ "$signing_mode" == developer-id ]]; then codesign_args+=(--timestamp); fi' "$repo_root/scripts/build-macos-release.sh" >/dev/null || { echo 'Developer ID signing must request a trusted timestamp.' >&2; exit 1; }
sign_line="$(grep -Fn 'codesign "${codesign_args[@]}" --entitlements "$desktop_entitlements" "$app"' "$repo_root/scripts/build-macos-release.sh" | tail -n 1 | cut -d: -f1)"
[[ "$embed_line" =~ ^[0-9]+$ && "$sign_line" =~ ^[0-9]+$ && "$embed_line" -lt "$sign_line" ]] || { echo 'provisioning profile must be embedded before final app signing.' >&2; exit 1; }
structural_job="$(sed -n '/^  structural:/,/^  official-sign-notarize:/p' "$repo_root/.github/workflows/macos-release.yml")"
! grep -F 'secrets.' <<<"$structural_job" >/dev/null || { echo 'pull-request structural job must not receive production secrets.' >&2; exit 1; }
grep -F 'lipo "$worker" -verify_arch "$pack_lipo_arch"' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'catalog Metal workers must have their declared single Mach-O slice verified.' >&2; exit 1; }
grep -F 'CPU/UI binary must not load Metal.framework' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'desktop and CPU worker Metal load-command rejection is missing.' >&2; exit 1; }
grep -F 'catalog Metal worker has no Metal load command' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'Metal worker load-command requirement is missing.' >&2; exit 1; }
grep -F 'macOS Metal packs must contain only the manifest, signature, and declared worker.' "$repo_root/scripts/verify-macos-release-package.sh" >/dev/null || { echo 'single-payload macOS Metal pack enforcement is missing.' >&2; exit 1; }
if [[ "$(uname -s)" == Darwin ]]; then
  lipo_temp="$(mktemp -d "${TMPDIR:-/tmp}/scribe-macos-lipo-test.XXXXXX")"; trap 'rm -rf "$lipo_temp"' EXIT
  printf 'int main(void) { return 0; }\n' >"$lipo_temp/thin.c"
  xcrun --sdk macosx clang -arch arm64 "$lipo_temp/thin.c" -o "$lipo_temp/arm64"
  xcrun --sdk macosx clang -arch x86_64 "$lipo_temp/thin.c" -o "$lipo_temp/x86_64"
  assert_single_arch() { lipo "$2" -verify_arch "$1" && [[ "$(lipo "$2" -archs)" == "$1" ]]; }
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
  for attack in symlink hardlink case apple-double resource-fork tamper catalog authority downgrade interruption; do
    app="$temp/$attack/Scribe.app"; mkdir -p "$(dirname "$app")"; cp -R "$base" "$app"
    case "$attack" in
      symlink) ln -s /tmp "$app/Contents/Resources/escape" ;;
      hardlink) ln "$app/Contents/MacOS/Scribe" "$app/Contents/Resources/linked-Scribe" ;;
      case) cp "$app/Contents/Resources/worker-pack-catalog.json" "$app/Contents/Resources/Worker-pack-catalog.json" ;;
      apple-double) : >"$app/Contents/Resources/._worker-pack-catalog.json" ;;
      resource-fork) xattr -w com.apple.ResourceFork test "$app/Contents/Resources/worker-pack-catalog.json" ;;
      tamper) printf x >>"$app/Contents/MacOS/Scribe" ;;
      catalog) printf '%s' '{"schema_version":1,"packs":[{}]}' >"$app/Contents/Resources/worker-pack-catalog.json" ;;
      authority) printf '%s' '{"schema_version":2,"catalog_sha256":"0000000000000000000000000000000000000000000000000000000000000000","release_security_epoch":0,"keychain_access_group":"","entries":[]}' >"$app/Contents/Resources/gpu-pack-release-authority.json" ;;
      downgrade) printf '%s' '{"schema_version":1,"packs":[{"pack_id":"metal","pack_version":"old","pack_digest":"0000000000000000000000000000000000000000000000000000000000000000","security_epoch":0,"runtime_abi_version":1,"backend":"metal","provider":"transcribe-cpp-metal","target_os":"macos","target_arch":"aarch64","worker_relative_path":"worker/scribe-inference-worker","root":"workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000","installed_size_bytes":1,"compressed_size_bytes":1,"files":["workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000/pack-manifest.json","workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000/pack-manifest.sig","workers/packs/metal/old/0000000000000000000000000000000000000000000000000000000000000000/worker/scribe-inference-worker"]}]}' >"$app/Contents/Resources/worker-pack-catalog.json" ;;
      interruption) mkdir -p "$app/Contents/Resources/workers/packs/metal/test/.stage.interrupted" ;;
    esac
    if bash "$repo_root/scripts/verify-macos-release-package.sh" --app "$app" >/dev/null 2>&1; then echo "verifier accepted $attack fixture" >&2; exit 1; fi
  done
fi
echo 'macOS release packaging contract tests passed.'
