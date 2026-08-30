#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() { echo 'Usage: verify-macos-release-package.sh --app <Scribe.app> [--require-notarization]'; }
app='' require_notarization=false
while (($#)); do case "$1" in --app) app="${2:-}"; shift 2;; --require-notarization) require_notarization=true; shift;; -h|--help) usage; exit 0;; *) echo "unknown argument: $1" >&2; exit 2;; esac; done
[[ "$(uname -s)" == Darwin ]] || { echo 'macOS release verification requires macOS.' >&2; exit 1; }
[[ -d "$app" && ! -L "$app" ]] || { echo 'Scribe.app is missing or unsafe.' >&2; exit 1; }
command -v jq >/dev/null || { echo 'jq is required.' >&2; exit 1; }
resources="$app/Contents/Resources"; macos="$app/Contents/MacOS"; catalog="$resources/worker-pack-catalog.json"
for path in "$app/Contents/Info.plist" "$macos/Scribe" "$macos/scribe-inference-worker" "$catalog"; do [[ -f "$path" && ! -L "$path" ]] || { echo "required regular file missing: $path" >&2; exit 1; }; done
find "$app" -xdev \( -type l -o -type f -links +1 -o -name '._*' \) -print -quit | grep -q . && { echo 'application contains a symlink, hardlink, or AppleDouble entry.' >&2; exit 1; }
find "$resources/workers/packs" -type d -name '.stage.*' -print -quit | grep -q . && { echo 'application contains an interrupted worker-pack staging directory.' >&2; exit 1; }
if command -v xattr >/dev/null && xattr -lr "$app" 2>/dev/null | grep -E 'com\.apple\.ResourceFork|com\.apple\.FinderInfo' >/dev/null; then echo 'application contains resource-fork metadata.' >&2; exit 1; fi
for binary in "$macos/Scribe" "$macos/scribe-inference-worker"; do lipo -verify_arch arm64 "$binary"; lipo -verify_arch x86_64 "$binary"; otool -l "$binary" | grep -A3 'LC_BUILD_VERSION' | grep -q 'minos 13\.' || { echo "minimum macOS 13 load command missing: $binary" >&2; exit 1; }; codesign --verify --strict --verbose=2 "$binary"; done
codesign --verify --strict --verbose=2 "$app"
codesign -d --entitlements :- "$app" 2>/dev/null | plutil -extract com.apple.security.device.audio-input raw -o - - | grep -qx true || { echo 'microphone entitlement is missing.' >&2; exit 1; }
plutil -extract LSMinimumSystemVersion raw -o - "$app/Contents/Info.plist" | grep -qx '13.0' || { echo 'Info.plist must declare macOS 13.0.' >&2; exit 1; }
jq -e '.schema_version == 1 and (.packs | type == "array") and (.packs | length <= 8)' "$catalog" >/dev/null || { echo 'catalog is invalid.' >&2; exit 1; }
expected_file="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-expected.XXXXXX")"
actual_file="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-actual.XXXXXX")"
trap 'rm -f "$expected_file" "$actual_file"' EXIT
printf '%s\n' "Contents/Info.plist" "Contents/MacOS/Scribe" "Contents/MacOS/scribe-inference-worker" "Contents/Resources/worker-pack-catalog.json" >"$expected_file"
while IFS= read -r entry; do
  id="$(jq -r '.pack_id' <<<"$entry")"; version="$(jq -r '.pack_version' <<<"$entry")"; digest="$(jq -r '.pack_digest' <<<"$entry")"; root="workers/packs/$id/$version/$digest"
  [[ "$(jq -r '.root' <<<"$entry")" == "$root" ]] || { echo 'catalog root is not immutable.' >&2; exit 1; }
  [[ "$(jq -r '.target_os' <<<"$entry")" == macos && "$(jq -r '.target_arch' <<<"$entry")" =~ ^(aarch64|x86_64)$ && "$(jq -r '.backend' <<<"$entry")" == metal ]] || { echo 'catalog entry is not a macOS Metal pack.' >&2; exit 1; }
  pack_lipo_arch="$(jq -r '.target_arch' <<<"$entry")"
  case "$pack_lipo_arch" in aarch64) pack_lipo_arch=arm64 ;; x86_64) pack_lipo_arch=x86_64 ;; *) echo 'catalog pack architecture is unsupported.' >&2; exit 1 ;; esac
  files_file="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-files.XXXXXX")"; jq -r '.files[]' <<<"$entry" >"$files_file"
  files_count="$(wc -l <"$files_file" | tr -d ' ')"; (( files_count >= 3 )) || { echo 'catalog pack inventory is incomplete.' >&2; exit 1; }
  [[ "$(LC_ALL=C sort -u "$files_file" | wc -l | tr -d ' ')" == "$files_count" && "$(LC_ALL=C sort "$files_file")" == "$(cat "$files_file")" ]] || { echo 'catalog pack inventory is not sorted and unique.' >&2; exit 1; }
  installed=0
  while IFS= read -r file; do [[ "$file" == "$root/"* && -f "$resources/$file" && ! -L "$resources/$file" ]] || { echo 'catalog file escaped or is unsafe.' >&2; exit 1; }; printf '%s\n' "Contents/Resources/$file" >>"$expected_file"; installed=$((installed + $(stat -f %z "$resources/$file"))); done <"$files_file"
  rm -f "$files_file"
  [[ "$installed" == "$(jq -r '.installed_size_bytes' <<<"$entry")" ]] || { echo 'catalog installed size mismatch.' >&2; exit 1; }
  worker="$resources/$root/$(jq -r '.worker_relative_path' <<<"$entry")"; [[ -f "$worker" ]] || { echo 'catalog worker is absent.' >&2; exit 1; }
  lipo -verify_arch "$pack_lipo_arch" "$worker"
  [[ "$(lipo -archs "$worker")" == "$pack_lipo_arch" ]] || { echo 'catalog Metal worker contains an unexpected Mach-O slice.' >&2; exit 1; }
  codesign --verify --strict --verbose=2 "$worker"
done < <(jq -c '.packs[]' "$catalog")
(cd "$app" && find Contents -type f -print | LC_ALL=C sort) >"$actual_file"
LC_ALL=C sort -u "$expected_file" >"$expected_file.sorted"
cmp -s "$actual_file" "$expected_file.sorted" || { echo 'application inventory is not exact.' >&2; exit 1; }
rm -f "$expected_file.sorted"
worker_digest="$(shasum -a 256 "$macos/scribe-inference-worker" | awk '{print $1}')"
LC_ALL=C strings "$macos/Scribe" | grep -Fqx "$worker_digest" || { echo 'desktop does not embed the final CPU worker SHA-256 anchor.' >&2; exit 1; }
if "$require_notarization"; then xcrun stapler validate "$app"; fi
echo 'macOS release package verification passed.'
