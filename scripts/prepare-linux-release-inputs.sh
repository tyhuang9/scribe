#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

usage() { echo 'Usage: prepare-linux-release-inputs.sh --output-directory <new-directory>'; }
[[ $# == 2 && "$1" == --output-directory && -n "$2" ]] || { usage >&2; exit 2; }
[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || { echo 'Linux x86_64 is required.' >&2; exit 1; }

output_parent="$(dirname -- "$2")"
output_name="$(basename -- "$2")"
[[ -d "$output_parent" && ! -L "$output_parent" && "$output_name" != . && "$output_name" != .. ]] || {
  echo 'output must name a new directory below an existing real parent.' >&2
  exit 1
}
output_parent="$(cd "$output_parent" && pwd -P)"
output="$output_parent/$output_name"
[[ ! -e "$output" && ! -L "$output" ]] || { echo 'release-input output already exists.' >&2; exit 1; }
staging="$(mktemp -d "$output_parent/.scribe-linux-inputs.XXXXXX")"
cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "$staging" ]]; then
    [[ "$staging" == "$output_parent"/.scribe-linux-inputs.* && ! -L "$staging" ]] || {
      echo 'refusing to clean an unvalidated release-input staging root.' >&2
      exit 1
    }
    rm -rf -- "$staging" || status=1
  fi
  exit "$status"
}
trap cleanup EXIT

# Same reviewed input as vendor/sherpa-onnx-sys/src/archive_verifier.rs.
# The build script independently authenticates it again before extraction.
archive='sherpa-onnx-v1.13.5-linux-x64-static-lib.tar.bz2'
expected_size=22394054
expected_sha256='2ade8b7c62de66b9cf2e32bd7dbe077addaa4b18f422b49dc1bf3a1a0b1f762e'
partial="$staging/$archive.partial"
# Bound even chunked/lengthless responses before writing to disk. An oversize
# response fails the pipeline or the exact size check; it is never published.
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --connect-timeout 20 --max-time 300 \
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.5/$archive" \
  | head -c "$((expected_size + 1))" >"$partial"
[[ "$(stat -c %s -- "$partial")" == "$expected_size" ]] || { echo 'reviewed Linux archive size mismatch.' >&2; exit 1; }
[[ "$(sha256sum -- "$partial" | awk '{print $1}')" == "$expected_sha256" ]] || { echo 'reviewed Linux archive digest mismatch.' >&2; exit 1; }
mv -- "$partial" "$staging/$archive"
mv --no-clobber --no-target-directory -- "$staging" "$output"
[[ ! -e "$staging" && ! -L "$staging" ]] || { echo 'release-input publication collided with an existing output.' >&2; exit 1; }
staging=''
echo 'Prepared the reviewed Linux CPU native archive.'
