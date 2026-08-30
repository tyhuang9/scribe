#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ $# -eq 1 ]] || { echo 'Usage: report-macos-worker-pack-sizes.sh <Scribe.app>' >&2; exit 2; }
app="$1"
resources="$app/Contents/Resources"
catalog="$resources/worker-pack-catalog.json"
command -v jq >/dev/null || { echo 'jq is required.' >&2; exit 1; }
[[ -f "$catalog" && ! -L "$catalog" ]] || { echo 'worker-pack catalog is missing or unsafe.' >&2; exit 1; }
jq -e '.schema_version == 1 and (.packs | type == "array")' "$catalog" >/dev/null || { echo 'catalog is invalid.' >&2; exit 1; }
printf 'pack_id\tversion\tdigest\tinstalled_bytes\tcompressed_bytes\n'
jq -r '.packs[] | [.pack_id,.pack_version,.pack_digest,(.installed_size_bytes|tostring),(.compressed_size_bytes|tostring)] | @tsv' "$catalog"
