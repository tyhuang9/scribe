#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
[[ "${1:-}" == --package && -n "${2:-}" && $# == 2 ]] || { echo 'usage: report-linux-worker-pack-sizes.sh --package <path.deb>' >&2; exit 2; }
package="$(realpath -e -- "$2")"; report="$package.sizes.json"
[[ -f "$report" && ! -L "$report" ]] || { echo 'deterministic package size report is missing.' >&2; exit 1; }
actual="$(stat -c %s -- "$package")"
python3 - "$report" "$actual" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); raw = path.read_bytes(); document = json.loads(raw)
canonical = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
if raw != canonical:
    raise SystemExit("package size report is not canonical JSON")
expected = {"schema_version", "target", "package_format", "installed_size_bytes", "compressed_size_bytes", "packs"}
if set(document) != expected or document["schema_version"] != 1 or document["target"] != "x86_64-unknown-linux-gnu" or document["package_format"] != "deb" or document["compressed_size_bytes"] != int(sys.argv[2]) or not isinstance(document["installed_size_bytes"], int) or isinstance(document["installed_size_bytes"], bool) or document["installed_size_bytes"] < 0 or document["packs"] != []:
    raise SystemExit("package size report is inconsistent")
print(raw.decode())
PY
