#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() {
  cat <<'EOF'
Usage: build-macos-release.sh --output-directory <directory> --pack-version <version> [--signing-mode <adhoc|developer-id>] [--include-metal-packs]

Creates a universal Scribe.app with a universal CPU worker and a default-empty
Metal pack catalog. --include-metal-packs is only for protected builds with the
reviewed Ed25519 signing key and Developer-ID keychain identity.
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
if [[ "$signing_mode" == developer-id ]]; then : "${SCRIBE_MACOS_SIGNING_IDENTITY:?Developer-ID mode requires a protected keychain identity.}"; [[ "$SCRIBE_MACOS_SIGNING_IDENTITY" != '-' ]] || { echo 'Developer-ID mode cannot use ad hoc signing.' >&2; exit 1; }; fi
if "$include_metal_packs" && [[ "$signing_mode" != developer-id ]]; then echo 'Metal packs require Developer-ID signing and protected production signing material.' >&2; exit 1; fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
output_parent="$(dirname "$output")"
mkdir -p "$output_parent"
output="$(cd "$output_parent" && pwd -P)/$(basename "$output")"
[[ ! -e "$output" ]] || { echo 'release output already exists; refusing to overwrite it.' >&2; exit 1; }
mkdir -p "$output"
trap 'rm -rf "$output"' ERR
app="$output/Scribe.app"; resources="$app/Contents/Resources"; macos="$app/Contents/MacOS"
mkdir -p "$resources/workers/packs" "$macos"
version="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$repo_root/Cargo.toml")"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'Cargo.toml version must be an exact semantic version.' >&2; exit 1; }
sed "s/\${SCRIBE_APP_VERSION}/$version/g" "$repo_root/installer/macos/Info.plist" >"$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  arch="${target%%-apple-darwin}"
  target_dir="$output/cargo-cpu-$arch"
  MACOSX_DEPLOYMENT_TARGET=13.0 SCRIBE_BUILDING_WORKER=1 CARGO_TARGET_DIR="$target_dir" \
    cargo build --locked --release --target "$target" --bin scribe-inference-worker --features inference-worker
  worker_path="$target_dir/$target/release/scribe-inference-worker"
  [[ -f "$worker_path" && ! -L "$worker_path" ]] || { echo 'CPU worker build is missing.' >&2; exit 1; }
  if [[ "$arch" == aarch64 ]]; then cpu_worker_arm="$worker_path"; else cpu_worker_x86="$worker_path"; fi
done
lipo -create -output "$macos/scribe-inference-worker" "$cpu_worker_arm" "$cpu_worker_x86"
worker_digest="$(shasum -a 256 "$macos/scribe-inference-worker" | awk '{print $1}')"

for target in aarch64-apple-darwin x86_64-apple-darwin; do
  arch="${target%%-apple-darwin}"
  target_dir="$output/cargo-desktop-$arch"
  MACOSX_DEPLOYMENT_TARGET=13.0 SCRIBE_BUNDLED_WORKER_SHA256="$worker_digest" CARGO_TARGET_DIR="$target_dir" \
    cargo build --locked --release --target "$target" --bin local-transcriber
  desktop_path="$target_dir/$target/release/local-transcriber"
  [[ -f "$desktop_path" && ! -L "$desktop_path" ]] || { echo 'desktop build is missing.' >&2; exit 1; }
  if [[ "$arch" == aarch64 ]]; then desktop_arm="$desktop_path"; else desktop_x86="$desktop_path"; fi
done
lipo -create -output "$macos/Scribe" "$desktop_arm" "$desktop_x86"

if "$include_metal_packs"; then
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    bash "$repo_root/scripts/build-macos-metal-worker-pack.sh" --target "$target" --pack-version "$pack_version" --output-packs-root "$resources/workers/packs" --signing-mode developer-id >"$output/metal-${target%%-apple-darwin}.json"
  done
  jq -n '{schema_version:1,packs:[]}' >"$resources/worker-pack-catalog.json"
  while IFS= read -r descriptor; do
    root="$(jq -r '.pack_root' <<<"$descriptor")"; [[ "$root" == "$resources/"* ]] || { echo 'pack output escaped app resources.' >&2; exit 1; }; rel="${root#"$resources/"}"
    [[ "$rel" == workers/packs/* ]] || { echo 'pack output does not use the immutable workers/packs layout.' >&2; exit 1; }
    files_tmp="$(mktemp "$output/.files.XXXXXX")"; (cd "$resources" && find "$rel" -type f -print | LC_ALL=C sort) >"$files_tmp"
    installed=0; while IFS= read -r file; do installed=$((installed + $(stat -f %z "$resources/$file"))); done <"$files_tmp"
    compressed_tmp="$(mktemp "$output/.pack.XXXXXX.zip")"; rm -f "$compressed_tmp"; (cd "$resources" && ditto -c -k "$rel" "$compressed_tmp"); compressed="$(stat -f %z "$compressed_tmp")"; rm -f "$compressed_tmp"
    jq --argjson descriptor "$descriptor" --arg root "$rel" --argjson installed "$installed" --argjson compressed "$compressed" --argjson files "$(jq -R . "$files_tmp" | jq -s .)" '
      .packs += [{pack_id:$descriptor.pack_id,pack_version:$descriptor.pack_version,pack_digest:$descriptor.pack_digest,security_epoch:1,runtime_abi_version:1,backend:$descriptor.backend,provider:$descriptor.provider,target_os:$descriptor.target_os,target_arch:$descriptor.target_arch,worker_relative_path:$descriptor.worker_relative_path,root:$root,installed_size_bytes:$installed,compressed_size_bytes:$compressed,files:$files}]
    ' "$resources/worker-pack-catalog.json" >"$resources/worker-pack-catalog.json.next"
    mv "$resources/worker-pack-catalog.json.next" "$resources/worker-pack-catalog.json"
    rm -f "$files_tmp"
  done < <(cat "$output"/metal-*.json)
else
  printf '%s\n' '{"schema_version":1,"packs":[]}' >"$resources/worker-pack-catalog.json"
fi

identity='-'; timestamp=()
if [[ "$signing_mode" == developer-id ]]; then identity="$SCRIBE_MACOS_SIGNING_IDENTITY"; timestamp=(--timestamp); fi
for binary in "$macos/scribe-inference-worker" "$macos/Scribe"; do codesign --force --sign "$identity" --options runtime "${timestamp[@]}" --entitlements "$repo_root/installer/macos/Scribe.entitlements" "$binary"; codesign --verify --strict --verbose=2 "$binary"; done
codesign --force --sign "$identity" --options runtime "${timestamp[@]}" --entitlements "$repo_root/installer/macos/Scribe.entitlements" "$app"
codesign --verify --strict --verbose=2 "$app"
rm -rf "$output"/cargo-cpu-* "$output"/cargo-desktop-* "$output"/metal-*.json
ditto -c -k --sequesterRsrc --keepParent "$app" "$output/Scribe-macos-universal.zip"
echo "$app"
