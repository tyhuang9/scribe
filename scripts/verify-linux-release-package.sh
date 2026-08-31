#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
trap 'status=$?; echo "Linux release package verification failed at line $LINENO (exit $status)." >&2; exit "$status"' ERR

[[ "${1:-}" == --package && -n "${2:-}" && $# == 2 ]] || { echo 'usage: verify-linux-release-package.sh --package <path.deb>' >&2; exit 2; }
package="$(realpath -e -- "$2")"
[[ -f "$package" && ! -L "$package" ]] || { echo 'package must be a regular non-symlink file.' >&2; exit 1; }
for command in dpkg-deb python3 sha256sum stat tar; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/scribe-linux-verify.XXXXXX")"
trap 'status=$?; rm -rf -- "$temp_root"; exit "$status"' EXIT

dpkg-deb --info "$package" >/dev/null
[[ "$(dpkg-deb -f "$package" Package)" == scribe ]] || { echo 'package name is not scribe.' >&2; exit 1; }
[[ "$(dpkg-deb -f "$package" Architecture)" == amd64 ]] || { echo 'package architecture is not amd64.' >&2; exit 1; }
dpkg-deb --fsys-tarfile "$package" >"$temp_root/data.tar"
tar -tf "$temp_root/data.tar" >"$temp_root/names"
[[ -z "$(LC_ALL=C sort "$temp_root/names" | uniq -d)" ]] || { echo 'package contains duplicate archive names.' >&2; exit 1; }
while IFS= read -r name; do
  [[ "$name" == ./* && "$name" != *//* && "$name" != *'/../'* && "$name" != *'/./'* && "$name" != *'/..' ]] || { echo "unsafe package archive path: $name" >&2; exit 1; }
done <"$temp_root/names"
if tar -tvf "$temp_root/data.tar" | awk 'substr($1,1,1) != "d" && substr($1,1,1) != "-" { found=1 } END { exit(found ? 0 : 1) }'; then
  echo 'package contains a link or nonregular archive entry.' >&2
  exit 1
fi
if tar --numeric-owner -tvf "$temp_root/data.tar" | awk '$2 != "0/0" { found=1 } END { exit(found ? 0 : 1) }'; then
  echo 'package data archive contains a non-root owner or group.' >&2
  exit 1
fi
mkdir "$temp_root/root"
tar -xf "$temp_root/data.tar" -C "$temp_root/root" --no-same-owner --no-same-permissions
root="$temp_root/root"; authority="$root/usr/lib/scribe"; inventory="$authority/linux-release-inventory.json"
[[ -d "$root/usr/bin" && ! -L "$root/usr/bin" && -d "$authority/workers/packs" && ! -L "$authority/workers/packs" ]] || { echo 'canonical Linux authority directories are missing or unsafe.' >&2; exit 1; }
expected_directories="$temp_root/expected-directories"; actual_directories="$temp_root/actual-directories"
printf '%s\n' usr usr/bin usr/lib usr/lib/scribe usr/lib/scribe/workers usr/lib/scribe/workers/packs >"$expected_directories"
(cd "$root" && find usr -type d -printf '%p\n' | LC_ALL=C sort) >"$actual_directories"
cmp -s "$expected_directories" "$actual_directories" || { echo 'package directory tree is not exact.' >&2; exit 1; }
while IFS= read -r directory; do [[ "$(stat -c %a -- "$root/$directory")" == 755 ]] || { echo "directory mode mismatch: $directory" >&2; exit 1; }; done <"$expected_directories"
for path in "$root/usr/bin/local-transcriber" "$authority/scribe-inference-worker" "$authority/worker-pack-catalog.json" "$authority/linux-release-package.json" "$inventory"; do
  [[ -f "$path" && ! -L "$path" && "$(stat -c %h -- "$path")" == 1 ]] || { echo "required package file is missing or unsafe: $path" >&2; exit 1; }
done
[[ "$(stat -c %a -- "$inventory")" == 644 ]] || { echo 'release inventory mode is not 0644.' >&2; exit 1; }
cmp -s "$authority/linux-release-package.json" "$repo_root/runtime-manifests/linux-release-package-x86_64.json" || { echo 'package release contract differs from the reviewed manifest.' >&2; exit 1; }
[[ "$(cat "$authority/worker-pack-catalog.json")" == '{"schema_version":1,"packs":[]}' ]] || { echo 'production Linux pack catalog must remain canonical and empty.' >&2; exit 1; }
[[ -z "$(find "$authority/workers/packs" -mindepth 1 -print -quit)" ]] || { echo 'production Linux package contains an untrusted GPU pack.' >&2; exit 1; }

expected_paths="$temp_root/expected"; actual_paths="$temp_root/actual"
inventory_rows="$temp_root/inventory.rows"
python3 - "$inventory" "$expected_paths" "$inventory_rows" <<'PY'
import json, pathlib, re, sys
inventory, paths_output, rows_output = map(pathlib.Path, sys.argv[1:])
raw = inventory.read_bytes()
try:
    document = json.loads(raw)
except Exception as error:
    raise SystemExit(f"release inventory is invalid JSON: {error}")
canonical = json.dumps(document, sort_keys=True, separators=(",", ":")).encode()
if canonical != raw:
    raise SystemExit("release inventory is not canonical JSON")
if set(document) != {"schema_version", "target", "build_revision", "cpu_worker_sha256", "entries"}:
    raise SystemExit("release inventory has unknown or missing fields")
if document["schema_version"] != 1 or document["target"] != "x86_64-unknown-linux-gnu":
    raise SystemExit("release inventory has an incompatible schema or target")
if not re.fullmatch(r"[0-9a-f]{40}", document["build_revision"]):
    raise SystemExit("release inventory build revision is invalid")
if not re.fullmatch(r"[0-9a-f]{64}", document["cpu_worker_sha256"]):
    raise SystemExit("release inventory CPU worker digest is invalid")
entries = document["entries"]
if not isinstance(entries, list) or not entries:
    raise SystemExit("release inventory entries are missing")
paths = []
rows = []
for entry in entries:
    if set(entry) != {"path", "mode", "size_bytes", "sha256"}:
        raise SystemExit("release inventory entry has unknown or missing fields")
    if not isinstance(entry["size_bytes"], int) or isinstance(entry["size_bytes"], bool) or entry["size_bytes"] < 0:
        raise SystemExit("release inventory size is invalid")
    if not re.fullmatch(r"0[0-7]{3}", entry["mode"]) or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]):
        raise SystemExit("release inventory mode or digest is invalid")
    paths.append(entry["path"])
    rows.append("\t".join((entry["path"], entry["mode"], str(entry["size_bytes"]), entry["sha256"])))
paths_output.write_text("\n".join(paths) + "\n", encoding="utf-8")
rows_output.write_text("\n".join(rows) + "\n", encoding="utf-8")
PY
LC_ALL=C sort -c "$expected_paths" || { echo 'release inventory paths are not strictly sorted.' >&2; exit 1; }
[[ -z "$(uniq -d "$expected_paths")" ]] || { echo 'release inventory contains duplicate paths.' >&2; exit 1; }
(cd "$root" && find usr -type f ! -path 'usr/lib/scribe/linux-release-inventory.json' -printf '%p\n' | LC_ALL=C sort) >"$actual_paths"
cmp -s "$expected_paths" "$actual_paths" || { echo 'release inventory does not exactly match the package file tree.' >&2; exit 1; }
while IFS=$'\t' read -r relative mode size digest; do
  [[ "$relative" =~ ^usr/([A-Za-z0-9._+-]+/)*[A-Za-z0-9._+-]+$ && "$relative" != *..* ]] || { echo "invalid inventory path: $relative" >&2; exit 1; }
  path="$root/$relative"
  [[ "0$(stat -c %a -- "$path")" == "$mode" ]] || { echo "mode mismatch: $relative" >&2; exit 1; }
  [[ "$(stat -c %s -- "$path")" == "$size" ]] || { echo "size mismatch: $relative" >&2; exit 1; }
  [[ "$(sha256sum "$path" | awk '{print $1}')" == "$digest" ]] || { echo "digest mismatch: $relative" >&2; exit 1; }
done <"$inventory_rows"
worker_digest="$(sha256sum "$authority/scribe-inference-worker" | awk '{print $1}')"
inventory_worker_digest="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["cpu_worker_sha256"])' "$inventory")"
[[ "$worker_digest" == "$inventory_worker_digest" ]] || { echo 'CPU worker inventory anchor differs.' >&2; exit 1; }
LC_ALL=C grep -aF -- "$worker_digest" "$root/usr/bin/local-transcriber" >/dev/null || { echo 'desktop does not embed the packaged CPU worker anchor.' >&2; exit 1; }
echo 'Linux release package verification passed.'
