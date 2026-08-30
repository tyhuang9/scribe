#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() {
  cat <<'EOF'
Usage: build-macos-metal-worker-pack.sh --target <aarch64-apple-darwin|x86_64-apple-darwin> --pack-version <version> --security-epoch <canonical-u64> --output-packs-root <directory> [--signing-mode <adhoc|developer-id>]

Builds one immutable, signed Metal worker pack. Production signing material is
read only from SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH and SCRIBE_PACK_SIGNING_KEY_ID.
The installed desktop must contain the separately reviewed matching public key.
EOF
}

target='' pack_version='' security_epoch='' output_packs_root='' signing_mode="${SCRIBE_MACOS_SIGNING_MODE:-developer-id}"
while (($#)); do
  case "$1" in
    --target) target="${2:-}"; shift 2 ;;
    --pack-version) pack_version="${2:-}"; shift 2 ;;
    --security-epoch) security_epoch="${2:-}"; shift 2 ;;
    --output-packs-root) output_packs_root="${2:-}"; shift 2 ;;
    --signing-mode) signing_mode="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ "$(uname -s)" == Darwin ]] || { echo 'Metal worker packs can only be built on macOS.' >&2; exit 1; }
[[ "$target" == aarch64-apple-darwin || "$target" == x86_64-apple-darwin ]] || { echo 'target must be a supported macOS architecture.' >&2; exit 2; }
[[ "$pack_version" =~ ^[a-z0-9]([a-z0-9._-]{0,94}[a-z0-9])?$ ]] || { echo 'pack version must be a canonical immutable-store component.' >&2; exit 2; }
[[ -n "$output_packs_root" ]] || { echo 'output packs root is required.' >&2; exit 2; }
[[ "$signing_mode" == adhoc || "$signing_mode" == developer-id ]] || { echo 'signing mode must be adhoc or developer-id.' >&2; exit 2; }
[[ "$security_epoch" =~ ^[1-9][0-9]{0,15}$ && ( ${#security_epoch} -lt 16 || "$security_epoch" < '9007199254740991' || "$security_epoch" == '9007199254740991' ) ]] || { echo 'security epoch must be a positive canonical exact JSON integer no greater than 9007199254740991.' >&2; exit 2; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
arch="${target%%-apple-darwin}"
manifest="$repo_root/runtime-manifests/gpu-worker-toolchain-macos-$arch.json"
[[ -f "$manifest" ]] || { echo 'pinned macOS toolchain manifest is missing.' >&2; exit 1; }
command -v jq >/dev/null || { echo 'jq is required to validate the pinned toolchain contract.' >&2; exit 1; }
jq -e --arg target "$target" --arg arch "$arch" '
  .schema_version == 1 and .target_triple == $target and .rust.release == "1.96.0" and
  .rust.host == $target and .macos.minimum_version == "13.0" and .macos.sdk == "macosx" and
  .macos.metal_provider == "transcribe-cpp-metal" and .build.profile == "release" and
  .build.dynamic_backends == false and .build.openmp == false
' "$manifest" >/dev/null || { echo 'pinned macOS Metal toolchain manifest is invalid.' >&2; exit 1; }
[[ "$(rustc --version)" == *'1.96.0'* ]] || { echo 'Rust 1.96.0 is required by the pinned Metal toolchain contract.' >&2; exit 1; }
[[ "${MACOSX_DEPLOYMENT_TARGET:-13.0}" == 13.0 ]] || { echo 'MACOSX_DEPLOYMENT_TARGET must be 13.0.' >&2; exit 1; }

case "$arch" in
  aarch64) [[ "$(uname -m)" == arm64 ]] || { echo 'aarch64 Metal pack builds require an Apple Silicon macOS runner.' >&2; exit 1; } ;;
  x86_64) [[ "$(uname -m)" == x86_64 ]] || { echo 'x86_64 Metal pack builds require an Intel macOS runner.' >&2; exit 1; } ;;
esac

if [[ "$signing_mode" == developer-id ]]; then
  : "${SCRIBE_MACOS_SIGNING_IDENTITY:?Developer-ID signing requires SCRIBE_MACOS_SIGNING_IDENTITY from the protected keychain.}"
  : "${SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH:?Metal pack signing requires SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH.}"
  : "${SCRIBE_PACK_SIGNING_KEY_ID:?Metal pack signing requires SCRIBE_PACK_SIGNING_KEY_ID.}"
  [[ "$SCRIBE_MACOS_SIGNING_IDENTITY" != '-' ]] || { echo 'Developer-ID signing cannot use an ad hoc identity.' >&2; exit 1; }
  [[ -f "$SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH" && ! -L "$SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH" ]] || { echo 'pack private key must be a regular non-symlink file.' >&2; exit 1; }
else
  echo 'Ad hoc mode validates the standalone Mach-O signing boundary only; it intentionally cannot author a production pack.' >&2
  exit 1
fi

mkdir -p "$(dirname "$output_packs_root")"
packs_root="$(cd "$(dirname "$output_packs_root")" && pwd -P)/$(basename "$output_packs_root")"
mkdir -p "$packs_root"
[[ ! -L "$packs_root" ]] || { echo 'pack root must not be a symlink.' >&2; exit 1; }
pack_id='metal'
parent="$packs_root/$pack_id/$pack_version"
mkdir -p "$parent"
[[ ! -L "$parent" ]] || { echo 'immutable pack parent must not be a symlink.' >&2; exit 1; }
stage="$(mktemp -d "$parent/.stage.XXXXXX")"
author_output="$(mktemp "$parent/.author.XXXXXX")"
cleanup() { rm -rf "$stage"; rm -f "$author_output"; }
trap cleanup EXIT
mkdir -p "$stage/worker"

target_dir="${SCRIBE_MACOS_CARGO_TARGET_DIR:-$repo_root/target-macos-metal-$arch}"
[[ ! -e "$target_dir" || ! -L "$target_dir" ]] || { echo 'Cargo target directory must not be a symlink.' >&2; exit 1; }
env -u SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP MACOSX_DEPLOYMENT_TARGET=13.0 SCRIBE_BUILDING_WORKER=1 CARGO_TARGET_DIR="$target_dir" \
  cargo build --locked --release --target "$target" --bin scribe-inference-worker --features metal-acceleration
worker="$target_dir/$target/release/scribe-inference-worker"
[[ -f "$worker" && ! -L "$worker" ]] || { echo 'Metal worker build did not produce a regular Mach-O.' >&2; exit 1; }
cp -p "$worker" "$stage/worker/scribe-inference-worker"
codesign --force --sign "$SCRIBE_MACOS_SIGNING_IDENTITY" --options runtime --timestamp --entitlements "$repo_root/installer/macos/Scribe.entitlements" "$stage/worker/scribe-inference-worker"
codesign --verify --strict --verbose=2 "$stage/worker/scribe-inference-worker"

if ! cargo run --locked --quiet --manifest-path "$repo_root/tools/worker-pack-author/Cargo.toml" -- \
  author --backend metal --target-os macos --target-arch "$arch" --pack-id "$pack_id" \
  --pack-version "$pack_version" --pack-root "$stage" --provider transcribe-cpp-metal \
  --security-epoch "$security_epoch" --worker-path worker/scribe-inference-worker --key-id "$SCRIBE_PACK_SIGNING_KEY_ID" \
  --private-key "$SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH" >"$author_output"; then
  echo 'Metal pack authoring failed. It requires the bounded author-tool extension for --backend metal, --target-os macos, and --target-arch.' >&2
  exit 1
fi
digest="$(jq -er '.pack_digest | select(test("^[0-9a-f]{64}$"))' "$author_output")"
final_root="$parent/$digest"
[[ ! -e "$final_root" ]] || { echo 'immutable pack destination already exists; refusing to overwrite it.' >&2; exit 1; }
find "$stage" -xdev \( -type l -o -type f -links +1 -o -name '._*' \) -print -quit | grep -q . && { echo 'pack staging tree contains a link, hardlink, or AppleDouble entry.' >&2; exit 1; }
mv "$stage" "$final_root"
rm -f "$author_output"
trap - EXIT
jq -cn --arg pack_root "$final_root" --arg pack_id "$pack_id" --arg pack_version "$pack_version" --arg pack_digest "$digest" --arg target_arch "$arch" --argjson security_epoch "$security_epoch" \
  '{pack_root:$pack_root,pack_id:$pack_id,pack_version:$pack_version,pack_digest:$pack_digest,security_epoch:$security_epoch,target_os:"macos",target_arch:$target_arch,backend:"metal",provider:"transcribe-cpp-metal",worker_relative_path:"worker/scribe-inference-worker"}'
