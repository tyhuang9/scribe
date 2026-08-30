#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() { echo 'Usage: verify-macos-release-package.sh --app <Scribe.app> [--require-notarization]'; }
app='' require_notarization=false
while (($#)); do case "$1" in --app) app="${2:-}"; shift 2;; --require-notarization) require_notarization=true; shift;; -h|--help) usage; exit 0;; *) echo "unknown argument: $1" >&2; exit 2;; esac; done
[[ "$(uname -s)" == Darwin ]] || { echo 'macOS release verification requires macOS.' >&2; exit 1; }
[[ -d "$app" && ! -L "$app" ]] || { echo 'Scribe.app is missing or unsafe.' >&2; exit 1; }
command -v jq >/dev/null || { echo 'jq is required.' >&2; exit 1; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
resources="$app/Contents/Resources"; macos="$app/Contents/MacOS"; catalog="$resources/worker-pack-catalog.json"
authority="$resources/gpu-pack-release-authority.json"
assert_no_keychain_group() {
  local target="$1" entitlements
  entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-worker-entitlements.XXXXXX")"
  if ! codesign -d --entitlements :- "$target" 2>/dev/null >"$entitlements"; then
    rm -f "$entitlements"
    echo "could not inspect worker entitlements: $target" >&2
    return 1
  fi
  if [[ -s "$entitlements" ]] && ! plutil -lint "$entitlements" >/dev/null 2>&1; then
    rm -f "$entitlements"
    echo "worker exposes malformed entitlement data: $target" >&2
    return 1
  fi
  if plutil -extract keychain-access-groups json -o - "$entitlements" >/dev/null 2>&1; then
    rm -f "$entitlements"
    echo "worker must not expose the desktop Keychain group: $target" >&2
    return 1
  fi
  rm -f "$entitlements"
}
assert_exact_protected_entitlements() {
  local target="$1" expected_group="$2" expected_team="${2%%.*}" entitlements
  entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-protected-entitlements.XXXXXX")"
  codesign -d --entitlements :- "$target" 2>/dev/null >"$entitlements" || { rm -f "$entitlements"; echo "could not inspect protected target entitlements: $target" >&2; return 1; }
  plutil -convert json -o - "$entitlements" |
    jq -e --arg group "$expected_group" --arg team "$expected_team" '
      .["keychain-access-groups"] == [$group] and
      .["com.apple.application-identifier"] == $group and
      .["com.apple.developer.team-identifier"] == $team
    ' >/dev/null || { rm -f "$entitlements"; echo "protected target application, team, or Keychain identifier is not exact: $target" >&2; return 1; }
  rm -f "$entitlements"
}
assert_microphone_entitlement() {
  local target="$1" entitlements
  entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-microphone-entitlements.XXXXXX")"
  if ! codesign -d --entitlements :- "$target" 2>/dev/null >"$entitlements"; then
    rm -f "$entitlements"
    echo "could not inspect desktop entitlements: $target" >&2
    return 1
  fi
  if ! plutil -convert json -o - "$entitlements" |
      jq -e '.["com.apple.security.device.audio-input"] == true' >/dev/null; then
    rm -f "$entitlements"
    echo "microphone entitlement is missing from signed target: $target" >&2
    return 1
  fi
  rm -f "$entitlements"
}
for path in "$app/Contents/Info.plist" "$app/Contents/_CodeSignature/CodeResources" "$macos/Scribe" "$macos/scribe-inference-worker" "$catalog" "$authority"; do [[ -f "$path" && ! -L "$path" ]] || { echo "required regular file missing: $path" >&2; exit 1; }; done
[[ -z "$(find "$app" -xdev \( -type l -o -type f -links +1 -o -name '._*' \) -print -quit)" ]] || { echo 'application contains a symlink, hardlink, or AppleDouble entry.' >&2; exit 1; }
[[ -z "$(find "$resources/workers/packs" -type d -name '.stage.*' -print -quit)" ]] || { echo 'application contains an interrupted worker-pack staging directory.' >&2; exit 1; }
if command -v xattr >/dev/null && xattr -lr "$app" 2>/dev/null | grep -E 'com\.apple\.ResourceFork|com\.apple\.FinderInfo' >/dev/null; then echo 'application contains resource-fork metadata.' >&2; exit 1; fi
for binary in "$macos/Scribe" "$macos/scribe-inference-worker"; do
  lipo "$binary" -verify_arch arm64; lipo "$binary" -verify_arch x86_64
  otool -l "$binary" | grep -A3 'LC_BUILD_VERSION' | grep -F 'minos 13.' >/dev/null || { echo "minimum macOS 13 load command missing: $binary" >&2; exit 1; }
  if otool -L "$binary" | grep -F '/Metal.framework/' >/dev/null || otool -l "$binary" | grep -F '/Metal.framework/' >/dev/null; then echo "CPU/UI binary must not load Metal.framework: $binary" >&2; exit 1; fi
  codesign --verify --strict --verbose=2 "$binary"
done
assert_no_keychain_group "$macos/scribe-inference-worker"
codesign --verify --strict --verbose=2 "$app"
assert_microphone_entitlement "$macos/Scribe"
[[ "$(plutil -extract LSMinimumSystemVersion raw -o - "$app/Contents/Info.plist")" == '13.0' ]] || { echo 'Info.plist must declare macOS 13.0.' >&2; exit 1; }
jq -e '.schema_version == 1 and (.packs | type == "array") and (.packs | length <= 8)' "$catalog" >/dev/null || { echo 'catalog is invalid.' >&2; exit 1; }
catalog_digest="$(shasum -a 256 "$catalog" | awk '{print $1}')"
[[ "$catalog_digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'catalog digest is invalid.' >&2; exit 1; }
LC_ALL=C grep -aF "$catalog_digest" "$macos/Scribe" >/dev/null || { echo 'desktop does not embed the exact pack-catalog authority.' >&2; exit 1; }
authority_json="$(jq -c . "$authority")"
release_security_epoch="$(jq -r '.release_security_epoch' "$authority")"
keychain_access_group="$(jq -r '.keychain_access_group' "$authority")"
embedded_profile_required=false
[[ "$release_security_epoch" =~ ^(0|[1-9][0-9]{0,15})$ && ( ${#release_security_epoch} -lt 16 || "$release_security_epoch" < '9007199254740991' || "$release_security_epoch" == '9007199254740991' ) ]] || { echo 'release authority epoch is not a canonical exact JSON integer from 0 through 9007199254740991.' >&2; exit 1; }
jq -e --arg digest "$catalog_digest" --slurpfile catalog "$catalog" '
  .schema_version == 2 and .catalog_sha256 == $digest and
  (.entries | type == "array") and
  [.entries[] | {
    pack_id, pack_version, pack_digest, security_epoch, runtime_abi_version,
    backend, provider, target_os, target_arch, worker_relative_path, root,
    installed_size_bytes, compressed_size_bytes, files
  }] == [$catalog[0].packs[] | {
    pack_id, pack_version, pack_digest, security_epoch, runtime_abi_version,
    backend, provider, target_os, target_arch, worker_relative_path, root,
    installed_size_bytes, compressed_size_bytes, files
  }]
' "$authority" >/dev/null || { echo 'release authority does not exactly bind the installed catalog.' >&2; exit 1; }
if [[ "$release_security_epoch" == 0 ]]; then
  [[ "$keychain_access_group" == '' ]] || { echo 'epoch-zero authority must not carry a Keychain group.' >&2; exit 1; }
  [[ "$(jq '.packs | length' "$catalog")" == 0 && "$(jq '.entries | length' "$authority")" == 0 ]] || { echo 'epoch-zero authority must be the empty default-deny authority.' >&2; exit 1; }
  [[ "$authority_json" == "$(cat "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)/runtime-manifests/gpu-pack-release-authority-macos-empty.json")" ]] || { echo 'epoch-zero authority is not canonical.' >&2; exit 1; }
else
  [[ "$keychain_access_group" =~ ^[A-Z0-9]{10}\.com\.scribe\.local-transcriber$ ]] || { echo 'positive release authority has an invalid Keychain group.' >&2; exit 1; }
  reviewed_namespace="$repo_root/runtime-manifests/gpu-keychain-namespace-macos-release.json"
  [[ "$(jq -c . "$reviewed_namespace")" == "$(cat "$reviewed_namespace")" ]] || { echo 'reviewed macOS Keychain namespace manifest is not canonical.' >&2; exit 1; }
  reviewed_group="$(jq -r 'select(.schema_version == 1) | .keychain_access_group' "$reviewed_namespace")"
  [[ -n "$reviewed_group" && "$keychain_access_group" == "$reviewed_group" ]] || { echo 'positive release does not use the exact non-empty source-reviewed Keychain namespace.' >&2; exit 1; }
  jq -e --argjson epoch "$release_security_epoch" 'all(.entries[]; .security_epoch == $epoch)' "$authority" >/dev/null || { echo 'release authority pack epoch differs from release epoch.' >&2; exit 1; }
fi
LC_ALL=C grep -aFf "$authority" "$macos/Scribe" >/dev/null || { echo 'desktop does not embed the exact release authority.' >&2; exit 1; }
desktop_entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-desktop-entitlements.XXXXXX")"
trap 'rm -f "$desktop_entitlements"' EXIT
codesign -d --entitlements :- "$app" 2>/dev/null >"$desktop_entitlements"
if [[ "$release_security_epoch" == 0 ]]; then
  ! plutil -extract keychain-access-groups json -o - "$desktop_entitlements" >/dev/null 2>&1 || { echo 'epoch-zero desktop must not expose a Keychain group.' >&2; exit 1; }
  codesign -d --entitlements :- "$macos/Scribe" 2>/dev/null >"$desktop_entitlements"
  ! plutil -extract keychain-access-groups json -o - "$desktop_entitlements" >/dev/null 2>&1 || { echo 'epoch-zero desktop binary must not expose a Keychain group.' >&2; exit 1; }
else
  assert_exact_protected_entitlements "$app" "$keychain_access_group"
  assert_exact_protected_entitlements "$macos/Scribe" "$keychain_access_group"
  profile="$app/Contents/embedded.provisionprofile"
  [[ -f "$profile" && ! -L "$profile" ]] || { echo 'positive release is missing a safe embedded provisioning profile.' >&2; exit 1; }
  decoded_profile="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-profile.XXXXXX")"
  profile_entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-profile-entitlements.XXXXXX")"
  security cms -D -i "$profile" >"$decoded_profile" || { echo 'embedded provisioning profile could not be decoded.' >&2; exit 1; }
  plutil -extract Entitlements xml1 -o "$profile_entitlements" "$decoded_profile" || { echo 'embedded provisioning profile has no entitlement dictionary.' >&2; exit 1; }
  profile_application_identifier="$(plutil -extract application-identifier raw -o - "$profile_entitlements" 2>/dev/null)" || { echo 'embedded provisioning profile has no application identifier.' >&2; exit 1; }
  profile_team_identifier="$(plutil -convert json -o - "$profile_entitlements" | jq -er '.["com.apple.developer.team-identifier"] | select(type == "string")')" || { echo 'embedded provisioning profile has no team identifier.' >&2; exit 1; }
  [[ "$profile_application_identifier" == "$keychain_access_group" ]] || { echo 'embedded provisioning profile application identifier does not authorize the authority group.' >&2; exit 1; }
  [[ "$profile_team_identifier" == "${keychain_access_group%%.*}" ]] || { echo 'embedded provisioning profile team identifier does not authorize the authority group.' >&2; exit 1; }
  plutil -extract keychain-access-groups json -o - "$profile_entitlements" |
    jq -e --arg group "$keychain_access_group" 'type == "array" and . == [$group]' >/dev/null || { echo 'embedded provisioning profile keychain groups are not exact.' >&2; exit 1; }
  rm -f "$decoded_profile" "$profile_entitlements"
  embedded_profile_required=true
fi
expected_file="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-expected.XXXXXX")"
actual_file="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-actual.XXXXXX")"
trap 'rm -f "$expected_file" "$actual_file"' EXIT
printf '%s\n' "Contents/Info.plist" "Contents/MacOS/Scribe" "Contents/MacOS/scribe-inference-worker" "Contents/Resources/worker-pack-catalog.json" "Contents/Resources/gpu-pack-release-authority.json" "Contents/_CodeSignature/CodeResources" >"$expected_file"
if "$embedded_profile_required"; then printf '%s\n' 'Contents/embedded.provisionprofile' >>"$expected_file"; fi
while IFS= read -r entry; do
  id="$(jq -r '.pack_id' <<<"$entry")"; version="$(jq -r '.pack_version' <<<"$entry")"; digest="$(jq -r '.pack_digest' <<<"$entry")"; root="workers/packs/$id/$version/$digest"
  [[ "$(jq -r '.root' <<<"$entry")" == "$root" ]] || { echo 'catalog root is not immutable.' >&2; exit 1; }
  [[ "$(jq -r '.target_os' <<<"$entry")" == macos && "$(jq -r '.target_arch' <<<"$entry")" =~ ^(aarch64|x86_64)$ && "$(jq -r '.backend' <<<"$entry")" == metal ]] || { echo 'catalog entry is not a macOS Metal pack.' >&2; exit 1; }
  pack_lipo_arch="$(jq -r '.target_arch' <<<"$entry")"
  case "$pack_lipo_arch" in aarch64) pack_lipo_arch=arm64 ;; x86_64) pack_lipo_arch=x86_64 ;; *) echo 'catalog pack architecture is unsupported.' >&2; exit 1 ;; esac
  files_file="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-files.XXXXXX")"; jq -r '.files[]' <<<"$entry" >"$files_file"
  files_count="$(wc -l <"$files_file" | tr -d ' ')"; (( files_count == 3 )) || { echo 'macOS Metal packs must contain only the manifest, signature, and declared worker.' >&2; exit 1; }
  [[ "$(LC_ALL=C sort -u "$files_file" | wc -l | tr -d ' ')" == "$files_count" && "$(LC_ALL=C sort "$files_file")" == "$(cat "$files_file")" ]] || { echo 'catalog pack inventory is not sorted and unique.' >&2; exit 1; }
  installed=0
  while IFS= read -r file; do [[ "$file" == "$root/"* && -f "$resources/$file" && ! -L "$resources/$file" ]] || { echo 'catalog file escaped or is unsafe.' >&2; exit 1; }; printf '%s\n' "Contents/Resources/$file" >>"$expected_file"; installed=$((installed + $(stat -f %z "$resources/$file"))); done <"$files_file"
  [[ "$installed" == "$(jq -r '.installed_size_bytes' <<<"$entry")" ]] || { echo 'catalog installed size mismatch.' >&2; exit 1; }
  worker_relative="$(jq -r '.worker_relative_path' <<<"$entry")"
  expected_pack_files="$(printf '%s\n' "$root/pack-manifest.json" "$root/pack-manifest.sig" "$root/$worker_relative" | LC_ALL=C sort)"
  [[ "$(cat "$files_file")" == "$expected_pack_files" ]] || { echo 'macOS Metal pack contains auxiliary payload.' >&2; exit 1; }
  rm -f "$files_file"
  worker="$resources/$root/$worker_relative"; [[ -f "$worker" ]] || { echo 'catalog worker is absent.' >&2; exit 1; }
  lipo "$worker" -verify_arch "$pack_lipo_arch"
  [[ "$(lipo "$worker" -archs)" == "$pack_lipo_arch" ]] || { echo 'catalog Metal worker contains an unexpected Mach-O slice.' >&2; exit 1; }
  otool -L "$worker" | grep -F '/Metal.framework/' >/dev/null || { echo 'catalog Metal worker does not link Metal.framework.' >&2; exit 1; }
  otool -l "$worker" | grep -F '/Metal.framework/' >/dev/null || { echo 'catalog Metal worker has no Metal load command.' >&2; exit 1; }
  codesign --verify --strict --verbose=2 "$worker"
  assert_no_keychain_group "$worker"
done < <(jq -c '.packs[]' "$catalog")
(cd "$app" && find Contents -type f -print | LC_ALL=C sort) >"$actual_file"
LC_ALL=C sort -u "$expected_file" >"$expected_file.sorted"
cmp -s "$actual_file" "$expected_file.sorted" || { echo 'application inventory is not exact.' >&2; exit 1; }
rm -f "$expected_file.sorted"
worker_digest="$(shasum -a 256 "$macos/scribe-inference-worker" | awk '{print $1}')"
LC_ALL=C grep -aF "$worker_digest" "$macos/Scribe" >/dev/null || { echo 'desktop does not embed the final CPU worker SHA-256 anchor.' >&2; exit 1; }
if "$require_notarization"; then xcrun stapler validate "$app"; fi
echo 'macOS release package verification passed.'
