#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() { echo 'Usage: prepare-macos-release-inputs.sh --output-directory <directory>'; }

output=''
while (($#)); do case "$1" in
  --output-directory) output="${2:-}"; shift 2 ;;
  -h|--help) usage; exit 0 ;;
  *) echo "unknown argument: $1" >&2; exit 2 ;;
esac; done
[[ -n "$output" ]] || { usage >&2; exit 2; }

output_parent="$(dirname "$output")"
mkdir -p "$output_parent"
output="$(cd "$output_parent" && pwd -P)/$(basename "$output")"
[[ ! -e "$output" ]] || { echo 'release-input output already exists; refusing to overwrite it.' >&2; exit 1; }
mkdir "$output"

partial=''
trap '[[ -z "$partial" ]] || rm -f -- "$partial"' EXIT
fetch_archive() {
  local name="$1" expected_size="$2" expected_sha256="$3" path actual_size actual_sha256
  path="$output/$name"
  [[ ! -e "$path" ]] || { echo "refusing to overwrite release input: $path" >&2; return 1; }
  partial="$path.partial"
  curl --fail --location --proto '=https' --tlsv1.2 --retry 3 --retry-delay 2 \
    --output "$partial" "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.5/$name"
  actual_size="$(wc -c <"$partial" | tr -d '[:space:]')"
  [[ "$actual_size" == "$expected_size" ]] || { echo "unexpected size for $name" >&2; return 1; }
  actual_sha256="$(shasum -a 256 "$partial" | awk '{print $1}')"
  [[ "$actual_sha256" == "$expected_sha256" ]] || { echo "unexpected SHA-256 for $name" >&2; return 1; }
  mv "$partial" "$path"
  partial=''
}

fetch_archive \
  sherpa-onnx-v1.13.5-osx-arm64-static-lib.tar.bz2 \
  19862746 \
  339c8fc19bb4b26e118c80792bbc4546eb263040fac36ef0cc027ec29c756b44
fetch_archive \
  sherpa-onnx-v1.13.5-osx-x64-static-lib.tar.bz2 \
  19623101 \
  689f8167a52dc4dbaf05369705e26c8f203c748a8c342750fdfdcd8ca6bb8699

echo "Prepared reviewed macOS release inputs in $output"
