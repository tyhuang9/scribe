#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

if [[ "${1:-}" == --package && -n "${2:-}" && $# == 2 ]]; then
  [[ ! -L "$2" ]] || { echo 'package argument must not be a symlink.' >&2; exit 1; }
  package="$(realpath -e -- "$2")"; report="$package.sizes.json"
  [[ -f "$report" && ! -L "$report" ]] || { echo 'deterministic package size report is missing.' >&2; exit 1; }
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
  verified="$(bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$package")"
  python3 - "$report" "$verified" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); raw = path.read_bytes(); document = json.loads(raw)
canonical = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
if raw != canonical:
    raise SystemExit("package size report is not canonical JSON")
verified = json.loads(sys.argv[2])
expected = {"schema_version", "target", "package_format", "installed_size_bytes", "compressed_size_bytes", "packs"}
is_nonnegative_integer = lambda value: type(value) is int and value >= 0
if set(document) != expected or type(document["schema_version"]) is not int or document["schema_version"] != 1 or document["target"] != "x86_64-unknown-linux-gnu" or document["package_format"] != "deb" or not is_nonnegative_integer(document["compressed_size_bytes"]) or not is_nonnegative_integer(document["installed_size_bytes"]) or document["compressed_size_bytes"] != verified["compressed_size_bytes"] or document["installed_size_bytes"] != verified["installed_size_bytes"] or document["packs"] != [] or verified["packs"] != []:
    raise SystemExit("package size report is inconsistent")
print(raw.decode())
PY
  exit 0
fi

[[ ("${1:-}" == --fixture-pack || "${1:-}" == --production-pack) && -n "${2:-}" && "${3:-}" == --tool && -n "${4:-}" && $# == 4 ]] || {
  echo 'usage: report-linux-worker-pack-sizes.sh --package <path.deb>' >&2
  echo '   or: report-linux-worker-pack-sizes.sh <--fixture-pack|--production-pack> <pack-root> --tool <scribe-worker-pack-tool>' >&2
  exit 2
}
[[ ! -L "$2" && ! -L "$4" ]] || { echo 'pack-root and tool arguments must not be symlinks.' >&2; exit 1; }
mode="$1"; pack_root="$(realpath -e -- "$2")"; tool="$(realpath -e -- "$4")"
[[ -d "$pack_root" && ! -L "$pack_root" && -x "$tool" ]] || { echo 'pack root or verification tool is unsafe.' >&2; exit 1; }
if [[ "$mode" == --fixture-pack ]]; then verification=verify-fixture; else verification=verify-production-linux; fi
"$tool" "$verification" --pack-root "$pack_root" >/dev/null
if find "$pack_root" -mindepth 1 ! -type d ! -type f -print -quit | grep . >/dev/null; then echo 'pack size report rejects links and nonregular entries.' >&2; exit 1; fi
if python3 - "$pack_root" <<'PY'
import os, pathlib, sys
root = pathlib.Path(sys.argv[1])
for directory, directories, files in os.walk(root):
    relative = pathlib.Path(directory).relative_to(root)
    for name in directories + files:
        parts = (*relative.parts, name)
        if any(part.startswith("-") for part in parts):
            raise SystemExit(0)
raise SystemExit(1)
PY
then
  echo 'pack size report rejects path components beginning with a dash.' >&2
  exit 1
fi
installed="$(find "$pack_root" -type f -printf '%s\n' | awk '{ total += $1 } END { print total + 0 }')"
compressed="$(cd "$pack_root" && LC_ALL=C find . -type f -printf '%P\0' | LC_ALL=C sort -z | env -u TAR_OPTIONS tar --create --null --verbatim-files-from --files-from=- --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner --format=gnu | env -u GZIP gzip -n -9 | wc -c)"
python3 - "$pack_root/pack-manifest.json" "$installed" "$compressed" "$mode" <<'PY'
import json, pathlib, sys
manifest = json.loads(pathlib.Path(sys.argv[1]).read_bytes())
document = {
    "schema_version": 1,
    "pack_id": manifest["pack_id"],
    "pack_version": manifest["pack_version"],
    "pack_digest": manifest["pack_digest"],
    "backend": manifest["backend"],
    "target_os": manifest["target_os"],
    "target_arch": manifest["target_arch"],
    "installed_size_bytes": int(sys.argv[2]),
    "compressed_size_bytes": int(sys.argv[3]),
    "verification": "fixture-only" if sys.argv[4] == "--fixture-pack" else "production",
}
print(json.dumps(document, sort_keys=True, separators=(",", ":")))
PY
