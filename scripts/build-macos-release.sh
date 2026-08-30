#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() {
  cat <<'EOF'
Usage: build-macos-release.sh --output-directory <directory> --pack-version <version> [--signing-mode <adhoc|developer-id>] [--include-metal-packs]

Creates a universal Scribe.app with a universal CPU worker and a default-empty
Metal pack catalog. A positive SCRIBE_MACOS_GPU_RELEASE_SECURITY_EPOCH creates
a protected release and requires the reviewed Developer-ID profile and stable
Keychain access group. --include-metal-packs is only for protected builds with
the reviewed Ed25519 signing key and Developer-ID keychain identity.
EOF
}

output='' pack_version='' signing_mode="${SCRIBE_MACOS_SIGNING_MODE:-adhoc}" include_metal_packs=false
while (($#)); do case "$1" in
  --output-directory) output="${2:-}"; shift 2 ;;
  --pack-version) pack_version="${2:-}"; shift 2 ;;
  --signing-mode) signing_mode="${2:-}"; shift 2 ;;
  --include-metal-packs) include_metal_packs=true; shift ;;
  -h|--help) usage; exit 0 ;;
  *) echo "unknown argument: $1" >&2; exit 2 ;;
esac; done
[[ "$(uname -s)" == Darwin ]] || { echo 'macOS releases can only be built on macOS.' >&2; exit 1; }
[[ -n "$output" && -n "$pack_version" ]] || { usage >&2; exit 2; }
[[ "$pack_version" =~ ^[a-z0-9]([a-z0-9._-]{0,94}[a-z0-9])?$ ]] || { echo 'pack version must be canonical.' >&2; exit 2; }
[[ "$signing_mode" == adhoc || "$signing_mode" == developer-id ]] || { echo 'signing mode must be adhoc or developer-id.' >&2; exit 2; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
command -v jq >/dev/null || { echo 'jq is required to build a macOS release.' >&2; exit 1; }

is_canonical_json_epoch() {
  local value="$1"
  [[ "$value" =~ ^(0|[1-9][0-9]{0,15})$ ]] || return 1
  (( ${#value} < 16 )) || [[ "$value" < '9007199254740991' || "$value" == '9007199254740991' ]]
}

verify_exact_keychain_group() {
  local target="$1" expected="$2" expected_team="${2%%.*}" entitlements
  entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-entitlements.XXXXXX")"
  if ! codesign -d --entitlements :- "$target" 2>/dev/null >"$entitlements"; then
    rm -f "$entitlements"
    echo "could not inspect signed target entitlements: $target" >&2
    return 1
  fi
  if ! plutil -extract keychain-access-groups json -o - "$entitlements" |
      jq -e --arg group "$expected" 'type == "array" and . == [$group]' >/dev/null ||
    [[ "$(plutil -extract com.apple.application-identifier raw -o - "$entitlements" 2>/dev/null)" != "$expected" ]] ||
    [[ "$(plutil -extract com.apple.developer.team-identifier raw -o - "$entitlements" 2>/dev/null)" != "$expected_team" ]]; then
    rm -f "$entitlements"
    echo "signed target does not expose the exact reviewed application, team, and Keychain identifiers: $target" >&2
    return 1
  fi
  rm -f "$entitlements"
}

verify_no_keychain_group() {
  local target="$1" entitlements
  entitlements="$(mktemp "${TMPDIR:-/tmp}/scribe-macos-entitlements.XXXXXX")"
  if ! codesign -d --entitlements :- "$target" 2>/dev/null >"$entitlements"; then
    rm -f "$entitlements"
    echo "could not inspect signed target entitlements: $target" >&2
    return 1
  fi
  if plutil -extract keychain-access-groups json -o - "$entitlements" >/dev/null 2>&1; then
    rm -f "$entitlements"
    echo "worker target must not expose the desktop Keychain access group: $target" >&2
    return 1
  fi
  rm -f "$entitlements"
}

release_security_epoch="${SCRIBE_MACOS_GPU_RELEASE_SECURITY_EPOCH:-0}"
is_canonical_json_epoch "$release_security_epoch" || { echo 'SCRIBE_MACOS_GPU_RELEASE_SECURITY_EPOCH must be a canonical exact JSON integer from 0 through 9007199254740991.' >&2; exit 2; }
reviewed_namespace="$repo_root/runtime-manifests/gpu-keychain-namespace-macos-release.json"
reviewed_namespace_json="$(jq -c . "$reviewed_namespace")"
[[ "$reviewed_namespace_json" == "$(cat "$reviewed_namespace")" ]] || { echo 'reviewed macOS Keychain namespace manifest is not canonical.' >&2; exit 1; }
jq -e '.schema_version == 1 and (.keychain_access_group | type == "string")' "$reviewed_namespace" >/dev/null || { echo 'reviewed macOS Keychain namespace manifest is invalid.' >&2; exit 1; }
reviewed_keychain_access_group="$(jq -r '.keychain_access_group' "$reviewed_namespace")"
[[ -z "$reviewed_keychain_access_group" || "$reviewed_keychain_access_group" =~ ^[A-Z0-9]{10}\.com\.scribe\.local-transcriber$ ]] || { echo 'reviewed macOS Keychain namespace is invalid.' >&2; exit 1; }
protected_release=false
if "$include_metal_packs" || [[ "$release_security_epoch" != 0 ]]; then protected_release=true; fi
if [[ "$signing_mode" == developer-id ]]; then
  : "${SCRIBE_MACOS_SIGNING_IDENTITY:?Developer-ID mode requires a protected keychain identity.}"
  [[ "$SCRIBE_MACOS_SIGNING_IDENTITY" != '-' ]] || { echo 'Developer-ID mode cannot use ad hoc signing.' >&2; exit 1; }
fi
if "$include_metal_packs" && [[ "$release_security_epoch" == 0 ]]; then
  echo 'Metal packs require an explicit positive SCRIBE_MACOS_GPU_RELEASE_SECURITY_EPOCH.' >&2
  exit 1
fi
if "$protected_release"; then
  [[ "$signing_mode" == developer-id ]] || { echo 'a positive release epoch or Metal packs require Developer-ID signing.' >&2; exit 1; }
  : "${SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP:?protected releases require SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP.}"
  keychain_access_group="$SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP"
  [[ "$keychain_access_group" =~ ^[A-Z0-9]{10}\.com\.scribe\.local-transcriber$ ]] || { echo 'Keychain access group must be the exact stable Scribe group.' >&2; exit 2; }
  [[ -n "$reviewed_keychain_access_group" && "$keychain_access_group" == "$reviewed_keychain_access_group" ]] || { echo 'protected releases require the exact non-empty source-reviewed Keychain namespace.' >&2; exit 1; }
  : "${SCRIBE_MACOS_PROVISIONING_PROFILE:?protected releases require SCRIBE_MACOS_PROVISIONING_PROFILE.}"
  [[ -f "$SCRIBE_MACOS_PROVISIONING_PROFILE" && ! -L "$SCRIBE_MACOS_PROVISIONING_PROFILE" ]] || { echo 'provisioning profile must be a regular non-symlink file.' >&2; exit 1; }
else
  keychain_access_group=''
fi

output_parent="$(dirname "$output")"
mkdir -p "$output_parent"
output="$(cd "$output_parent" && pwd -P)/$(basename "$output")"
[[ ! -e "$output" ]] || { echo 'release output already exists; refusing to overwrite it.' >&2; exit 1; }
mkdir -p "$output"
trap 'rm -rf "$output"' ERR
app="$output/Scribe.app"; resources="$app/Contents/Resources"; macos="$app/Contents/MacOS"
mkdir -p "$resources/workers/packs" "$macos"
desktop_entitlements="$repo_root/installer/macos/Scribe.entitlements"
if "$protected_release"; then
  decoded_profile="$(mktemp "$output/.provisionprofile.XXXXXX")"
  profile_entitlements="$(mktemp "$output/.provisionprofile-entitlements.XXXXXX")"
  security cms -D -i "$SCRIBE_MACOS_PROVISIONING_PROFILE" >"$decoded_profile" || { echo 'provisioning profile could not be decoded.' >&2; exit 1; }
  plutil -extract Entitlements xml1 -o "$profile_entitlements" "$decoded_profile" || { echo 'provisioning profile has no entitlement dictionary.' >&2; exit 1; }
  profile_application_identifier="$(plutil -extract application-identifier raw -o - "$profile_entitlements" 2>/dev/null)" || { echo 'provisioning profile has no application identifier entitlement.' >&2; exit 1; }
  profile_team_identifier="$(plutil -extract com.apple.developer.team-identifier raw -o - "$profile_entitlements" 2>/dev/null)" || { echo 'provisioning profile has no team identifier entitlement.' >&2; exit 1; }
  team_identifier="${keychain_access_group%%.*}"
  [[ "$profile_application_identifier" == "$keychain_access_group" ]] || { echo 'provisioning profile application identifier does not authorize the selected Keychain group.' >&2; exit 1; }
  [[ "$profile_team_identifier" == "$team_identifier" ]] || { echo 'provisioning profile team identifier does not authorize the selected Keychain group.' >&2; exit 1; }
  plutil -extract keychain-access-groups json -o - "$profile_entitlements" |
    jq -e --arg group "$keychain_access_group" 'type == "array" and . == [$group]' >/dev/null || {
      echo 'provisioning profile keychain groups do not authorize exactly the selected group.' >&2
      exit 1
    }
  rm -f "$decoded_profile" "$profile_entitlements"
  desktop_entitlements="$output/Scribe.protected.entitlements"
  sed -e "s/\${SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP}/$keychain_access_group/g" \
    -e "s/\${SCRIBE_MACOS_GPU_ROLLBACK_TEAM_IDENTIFIER}/$team_identifier/g" \
    "$repo_root/installer/macos/Scribe.protected.entitlements.template" >"$desktop_entitlements"
  plutil -lint "$desktop_entitlements" >/dev/null
  cp -p "$SCRIBE_MACOS_PROVISIONING_PROFILE" "$app/Contents/embedded.provisionprofile"
  [[ -f "$app/Contents/embedded.provisionprofile" && ! -L "$app/Contents/embedded.provisionprofile" ]] || { echo 'embedded provisioning profile is unsafe.' >&2; exit 1; }
fi
version="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$repo_root/Cargo.toml")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Cargo.toml version must be an exact semantic version.' >&2; exit 1; }
build_revision="${SCRIBE_BUILD_REVISION:-$(git -C "$repo_root" rev-parse --verify HEAD)}"
[[ "$build_revision" =~ ^[[:graph:]]{12,96}$ ]] || { echo 'build revision must be 12-96 printable non-space ASCII characters.' >&2; exit 1; }
sed "s/\${SCRIBE_APP_VERSION}/$version/g" "$repo_root/installer/macos/Info.plist" >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
identity='-'; timestamp=()
if [[ "$signing_mode" == developer-id ]]; then identity="$SCRIBE_MACOS_SIGNING_IDENTITY"; timestamp=(--timestamp); fi

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  arch="${target%%-apple-darwin}"
  target_dir="$output/cargo-cpu-$arch"
  env -u SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP MACOSX_DEPLOYMENT_TARGET=13.0 SCRIBE_BUILD_REVISION="$build_revision" SCRIBE_BUILDING_WORKER=1 CARGO_TARGET_DIR="$target_dir" \
    cargo build --locked --release --target "$target" --bin scribe-inference-worker --features inference-worker
  worker_path="$target_dir/$target/release/scribe-inference-worker"
  [[ -f "$worker_path" && ! -L "$worker_path" ]] || { echo 'CPU worker build is missing.' >&2; exit 1; }
  if [[ "$arch" == aarch64 ]]; then cpu_worker_arm="$worker_path"; else cpu_worker_x86="$worker_path"; fi
done
lipo -create -output "$macos/scribe-inference-worker" "$cpu_worker_arm" "$cpu_worker_x86"
codesign --force --sign "$identity" --options runtime "${timestamp[@]}" --entitlements "$repo_root/installer/macos/Scribe.entitlements" "$macos/scribe-inference-worker"
codesign --verify --strict --verbose=2 "$macos/scribe-inference-worker"
verify_no_keychain_group "$macos/scribe-inference-worker"
worker_digest="$(shasum -a 256 "$macos/scribe-inference-worker" | awk '{print $1}')"
[[ "$worker_digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'signed universal CPU worker digest is invalid.' >&2; exit 1; }

if "$include_metal_packs"; then
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    SCRIBE_BUILD_REVISION="$build_revision" bash "$repo_root/scripts/build-macos-metal-worker-pack.sh" --target "$target" --pack-version "$pack_version" --security-epoch "$release_security_epoch" --output-packs-root "$resources/workers/packs" --signing-mode developer-id >"$output/metal-${target%%-apple-darwin}.json"
  done
  jq -n '{schema_version:1,packs:[]}' >"$resources/worker-pack-catalog.json"
  while IFS= read -r descriptor; do
    root="$(jq -r '.pack_root' <<<"$descriptor")"; [[ "$root" == "$resources/"* ]] || { echo 'pack output escaped app resources.' >&2; exit 1; }; rel="${root#"$resources/"}"
    [[ "$rel" == workers/packs/* ]] || { echo 'pack output does not use the immutable workers/packs layout.' >&2; exit 1; }
    files_tmp="$(mktemp "$output/.files.XXXXXX")"; (cd "$resources" && find "$rel" -type f -print | LC_ALL=C sort) >"$files_tmp"
    installed=0; while IFS= read -r file; do installed=$((installed + $(stat -f %z "$resources/$file"))); done <"$files_tmp"
    compressed_tmp="$(mktemp "$output/.pack.XXXXXX.zip")"; rm -f "$compressed_tmp"; (cd "$resources" && ditto -c -k "$rel" "$compressed_tmp"); compressed="$(stat -f %z "$compressed_tmp")"; rm -f "$compressed_tmp"
    [[ "$(jq -r '.security_epoch' <<<"$descriptor")" == "$release_security_epoch" ]] || { echo 'Metal pack descriptor epoch differs from its release epoch.' >&2; exit 1; }
    jq --argjson descriptor "$descriptor" --arg root "$rel" --argjson installed "$installed" --argjson compressed "$compressed" --argjson files "$(jq -R . "$files_tmp" | jq -s .)" '
      .packs += [{pack_id:$descriptor.pack_id,pack_version:$descriptor.pack_version,pack_digest:$descriptor.pack_digest,security_epoch:$descriptor.security_epoch,runtime_abi_version:1,backend:$descriptor.backend,provider:$descriptor.provider,target_os:$descriptor.target_os,target_arch:$descriptor.target_arch,worker_relative_path:$descriptor.worker_relative_path,root:$root,installed_size_bytes:$installed,compressed_size_bytes:$compressed,files:$files}]
    ' "$resources/worker-pack-catalog.json" >"$resources/worker-pack-catalog.json.next"
    mv "$resources/worker-pack-catalog.json.next" "$resources/worker-pack-catalog.json"
    rm -f "$files_tmp"
  done < <(cat "$output"/metal-*.json)
else
  printf '%s' '{"schema_version":1,"packs":[]}' >"$resources/worker-pack-catalog.json"
fi

catalog_json="$(jq -c . "$resources/worker-pack-catalog.json")"
printf '%s' "$catalog_json" >"$resources/worker-pack-catalog.json"
catalog_digest="$(shasum -a 256 "$resources/worker-pack-catalog.json" | awk '{print $1}')"
[[ "$catalog_digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'catalog digest is invalid.' >&2; exit 1; }
release_authority="$resources/gpu-pack-release-authority.json"
authority_json="$(jq -c --arg app_version "$version" --arg build_revision "$build_revision" --arg catalog_sha256 "$catalog_digest" --argjson release_security_epoch "$release_security_epoch" --arg keychain_access_group "$keychain_access_group" '
  {schema_version:2,catalog_sha256:$catalog_sha256,release_security_epoch:$release_security_epoch,keychain_access_group:$keychain_access_group,entries:[.packs[] | {
    app_version:$app_version,build_revision:$build_revision,app_protocol_version:5,
    pack_id:.pack_id,pack_version:.pack_version,pack_digest:.pack_digest,security_epoch:.security_epoch,
    runtime_abi_version:.runtime_abi_version,backend:.backend,provider:.provider,target_os:.target_os,
    target_arch:.target_arch,worker_relative_path:.worker_relative_path,root:.root,
    installed_size_bytes:.installed_size_bytes,compressed_size_bytes:.compressed_size_bytes,files:.files
  }]}
' "$resources/worker-pack-catalog.json")"
printf '%s' "$authority_json" >"$release_authority"
if [[ "$release_security_epoch" == 0 ]]; then
  [[ "$authority_json" == "$(cat "$repo_root/runtime-manifests/gpu-pack-release-authority-macos-empty.json")" ]] || {
    echo 'epoch-zero authority must be the canonical empty default-deny document.' >&2
    exit 1
  }
else
  jq -e --argjson epoch "$release_security_epoch" --arg group "$keychain_access_group" '
    .schema_version == 2 and .release_security_epoch == $epoch and .keychain_access_group == $group and
    ((.entries | length) == 0 or all(.entries[]; .security_epoch == $epoch))
  ' "$release_authority" >/dev/null || { echo 'release authority does not bind every pack to its release epoch and group.' >&2; exit 1; }
fi

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  arch="${target%%-apple-darwin}"
  target_dir="$output/cargo-desktop-$arch"
  if "$protected_release"; then
    MACOSX_DEPLOYMENT_TARGET=13.0 SCRIBE_BUILD_REVISION="$build_revision" SCRIBE_BUNDLED_WORKER_SHA256="$worker_digest" SCRIBE_GPU_PACK_RELEASE_AUTHORITY="$release_authority" SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP="$keychain_access_group" CARGO_TARGET_DIR="$target_dir" \
      cargo build --locked --release --target "$target" --bin local-transcriber
  else
    env -u SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP MACOSX_DEPLOYMENT_TARGET=13.0 SCRIBE_BUILD_REVISION="$build_revision" SCRIBE_BUNDLED_WORKER_SHA256="$worker_digest" SCRIBE_GPU_PACK_RELEASE_AUTHORITY="$release_authority" CARGO_TARGET_DIR="$target_dir" \
      cargo build --locked --release --target "$target" --bin local-transcriber
  fi
  desktop_path="$target_dir/$target/release/local-transcriber"
  [[ -f "$desktop_path" && ! -L "$desktop_path" ]] || { echo 'desktop build is missing.' >&2; exit 1; }
  LC_ALL=C strings "$desktop_path" | grep -F "$catalog_digest" >/dev/null || { echo 'desktop slice does not embed the exact pack-catalog authority.' >&2; exit 1; }
  LC_ALL=C strings "$desktop_path" | grep -Fqx "$authority_json" >/dev/null || { echo 'desktop slice does not embed the exact release authority.' >&2; exit 1; }
  if [[ "$arch" == aarch64 ]]; then desktop_arm="$desktop_path"; else desktop_x86="$desktop_path"; fi
done
lipo -create -output "$macos/Scribe" "$desktop_arm" "$desktop_x86"
LC_ALL=C strings "$macos/Scribe" | grep -Fqx "$worker_digest" || { echo 'desktop does not embed the final signed CPU worker anchor.' >&2; exit 1; }
LC_ALL=C strings "$macos/Scribe" | grep -F "$catalog_digest" >/dev/null || { echo 'desktop does not embed the exact pack-catalog authority.' >&2; exit 1; }
LC_ALL=C strings "$macos/Scribe" | grep -Fqx "$authority_json" >/dev/null || { echo 'desktop does not embed the exact release authority.' >&2; exit 1; }

codesign --force --sign "$identity" --options runtime "${timestamp[@]}" --entitlements "$desktop_entitlements" "$macos/Scribe"
codesign --verify --strict --verbose=2 "$macos/Scribe"
if "$protected_release"; then verify_exact_keychain_group "$macos/Scribe" "$keychain_access_group"; else verify_no_keychain_group "$macos/Scribe"; fi
codesign --force --sign "$identity" --options runtime "${timestamp[@]}" --entitlements "$desktop_entitlements" "$app"
codesign --verify --strict --verbose=2 "$app"
if "$protected_release"; then verify_exact_keychain_group "$app" "$keychain_access_group"; else verify_no_keychain_group "$app"; fi
[[ "$(shasum -a 256 "$macos/scribe-inference-worker" | awk '{print $1}')" == "$worker_digest" ]] || { echo 'outer application signing changed the anchored CPU worker.' >&2; exit 1; }
rm -rf "$output"/cargo-cpu-* "$output"/cargo-desktop-* "$output"/metal-*.json "$output"/Scribe.protected.entitlements
ditto -c -k --sequesterRsrc --keepParent "$app" "$output/Scribe-macos-universal.zip"
echo "$app"
