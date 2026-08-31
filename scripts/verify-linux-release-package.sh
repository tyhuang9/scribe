#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
trap 'status=$?; echo "Linux release package verification failed at line $LINENO (exit $status)." >&2; exit "$status"' ERR

[[ "${1:-}" == --package && -n "${2:-}" && $# == 2 ]] || { echo 'usage: verify-linux-release-package.sh --package <path.deb>' >&2; exit 2; }
[[ ! -L "$2" ]] || { echo 'package argument must not be a symlink.' >&2; exit 1; }
package="$(realpath -e -- "$2")"
[[ -f "$package" && ! -L "$package" ]] || { echo 'package must be a regular non-symlink file.' >&2; exit 1; }
[[ "$(stat -c %h -- "$package")" == 1 ]] || { echo 'package must not be hardlinked.' >&2; exit 1; }
[[ "$(stat -c %s -- "$package")" -le 4294967296 ]] || { echo 'compressed package exceeds the 4 GiB bound.' >&2; exit 1; }
for command in python3 sha256sum stat tar xz; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source "$repo_root/scripts/linux-release-package-common.sh"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/scribe-linux-verify.XXXXXX")"
trap 'status=$?; rm -rf -- "$temp_root"; exit "$status"' EXIT

mkdir "$temp_root/inspected"
python3 "$repo_root/scripts/inspect-linux-deb.py" "$package" "$temp_root/inspected"
mv "$temp_root/inspected/control.tar" "$temp_root/control.tar"
mv "$temp_root/inspected/data.tar" "$temp_root/data.tar"
printf '%s\n' ./ ./control >"$temp_root/expected-control-names"
tar -tf "$temp_root/control.tar" >"$temp_root/control-names"
cmp -s "$temp_root/expected-control-names" "$temp_root/control-names" || { echo 'package control archive contains unexpected metadata or maintainer scripts.' >&2; exit 1; }
if tar --numeric-owner -tvf "$temp_root/control.tar" | awk '$2 != "0/0" || (substr($1,1,1) != "d" && substr($1,1,1) != "-") { found=1 } END { exit(found ? 0 : 1) }'; then
  echo 'package control archive contains unsafe ownership or entry types.' >&2
  exit 1
fi
mkdir "$temp_root/control-root"
tar -xf "$temp_root/control.tar" -C "$temp_root/control-root" --no-same-owner --same-permissions
[[ "$(stat -c %a -- "$temp_root/control-root")" == 755 && "$(stat -c %a -- "$temp_root/control-root/control")" == 644 ]] || { echo 'package control archive modes are not canonical.' >&2; exit 1; }
python3 - "$temp_root/control-root/control" "$temp_root/control-installed-size" <<'PY'
import pathlib, re, sys

path, installed_output = map(pathlib.Path, sys.argv[1:])
raw = path.read_bytes()
try:
    text = raw.decode("utf-8")
except UnicodeDecodeError as error:
    raise SystemExit(f"package control is not UTF-8: {error}")
if "\r" in text or not text.endswith("\n"):
    raise SystemExit("package control must use canonical newline termination")
lines = text[:-1].split("\n")
expected_keys = ["Package", "Version", "Architecture", "Maintainer", "Installed-Size", "Section", "Priority", "Description"]
if len(lines) != len(expected_keys):
    raise SystemExit("package control field set is not exact")
fields = {}
for expected_key, line in zip(expected_keys, lines):
    if line.startswith((" ", "\t")) or ": " not in line:
        raise SystemExit("package control contains a continuation or malformed field")
    key, value = line.split(": ", 1)
    if key != expected_key or not value:
        raise SystemExit("package control fields are not exact and ordered")
    fields[key] = value
expected_values = {
    "Package": "scribe",
    "Architecture": "amd64",
    "Maintainer": "Scribe Release Engineering <noreply@example.invalid>",
    "Section": "sound",
    "Priority": "optional",
    "Description": "Scribe local transcription desktop and verified inference workers",
}
for key, value in expected_values.items():
    if fields[key] != value:
        raise SystemExit(f"package control {key} is unexpected")
if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[+~-][a-z0-9.+~-]+)?", fields["Version"]):
    raise SystemExit("package version is not canonical")
if not re.fullmatch(r"0|[1-9][0-9]*", fields["Installed-Size"]):
    raise SystemExit("package Installed-Size is not canonical")
installed_output.write_text(fields["Installed-Size"] + "\n", encoding="ascii")
PY
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
if tar --numeric-owner -tvf "$temp_root/data.tar" | awk 'substr($1,1,1) == "d" && $1 != "drwxr-xr-x" { found=1 } END { exit(found ? 0 : 1) }'; then
  echo 'package data archive contains an unsafe directory mode.' >&2
  exit 1
fi
if tar --numeric-owner -tvf "$temp_root/data.tar" | awk 'substr($1,1,1) == "-" { if ($3 > 2147483648) found=1; total += $3 } END { if (total > 4294967296) found=1; exit(found ? 0 : 1) }'; then
  echo 'package data archive exceeds file or aggregate size bounds.' >&2
  exit 1
fi
mkdir "$temp_root/root"
tar -xf "$temp_root/data.tar" -C "$temp_root/root" --no-same-owner --same-permissions
root="$temp_root/root"; authority="$root/usr/lib/scribe"; inventory="$authority/linux-release-inventory.json"
installed_bytes="$(linux_regular_file_bytes "$root/usr")"; installed_kib="$(((installed_bytes + 1023) / 1024))"
[[ "$(cat "$temp_root/control-installed-size")" == "$installed_kib" ]] || { echo 'package Installed-Size does not match the exact payload.' >&2; exit 1; }
[[ -d "$root/usr/bin" && ! -L "$root/usr/bin" && -d "$authority/workers/packs" && ! -L "$authority/workers/packs" ]] || { echo 'canonical Linux authority directories are missing or unsafe.' >&2; exit 1; }
expected_directories="$temp_root/expected-directories"; actual_directories="$temp_root/actual-directories"
printf '%s\n' usr usr/bin usr/lib usr/lib/scribe usr/lib/scribe/workers usr/lib/scribe/workers/packs >"$expected_directories"
(cd "$root" && find . -mindepth 1 -type d -printf '%P\n' | LC_ALL=C sort) >"$actual_directories"
cmp -s "$expected_directories" "$actual_directories" || { echo 'package directory tree is not exact.' >&2; exit 1; }
while IFS= read -r directory; do [[ "$(stat -c %a -- "$root/$directory")" == 755 ]] || { echo "directory mode mismatch: $directory" >&2; exit 1; }; done <"$expected_directories"
for path in "$root/usr/bin/local-transcriber" "$authority/scribe-inference-worker" "$authority/worker-pack-catalog.json" "$authority/linux-release-package.json" "$inventory"; do
  [[ -f "$path" && ! -L "$path" && "$(stat -c %h -- "$path")" == 1 ]] || { echo "required package file is missing or unsafe: $path" >&2; exit 1; }
done
expected_package_files="$temp_root/expected-package-files"; actual_package_files="$temp_root/actual-package-files"
printf '%s\n' \
  usr/bin/local-transcriber \
  usr/lib/scribe/linux-release-inventory.json \
  usr/lib/scribe/linux-release-package.json \
  usr/lib/scribe/scribe-inference-worker \
  usr/lib/scribe/worker-pack-catalog.json >"$expected_package_files"
(cd "$root" && find . -mindepth 1 -type f -printf '%P\n' | LC_ALL=C sort) >"$actual_package_files"
cmp -s "$expected_package_files" "$actual_package_files" || { echo 'CPU-only package file set is not independently authorized.' >&2; exit 1; }
[[ "$(stat -c %a -- "$root/usr/bin/local-transcriber")" == 755 ]] || { echo 'packaged desktop mode is not 0755.' >&2; exit 1; }
[[ "$(stat -c %a -- "$authority/scribe-inference-worker")" == 755 ]] || { echo 'packaged CPU worker mode is not 0755.' >&2; exit 1; }
for metadata in "$authority/worker-pack-catalog.json" "$authority/linux-release-package.json" "$inventory"; do
  [[ "$(stat -c %a -- "$metadata")" == 644 ]] || { echo "packaged metadata mode is not 0644: $metadata" >&2; exit 1; }
done
linux_require_x86_64_elf "$root/usr/bin/local-transcriber" 'packaged desktop'
linux_require_x86_64_elf "$authority/scribe-inference-worker" 'packaged CPU worker'
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
if type(document["schema_version"]) is not int or document["schema_version"] != 1 or document["target"] != "x86_64-unknown-linux-gnu":
    raise SystemExit("release inventory has an incompatible schema or target")
if not re.fullmatch(r"[0-9a-f]{40}", document["build_revision"]):
    raise SystemExit("release inventory build revision is invalid")
if not re.fullmatch(r"[0-9a-f]{64}", document["cpu_worker_sha256"]):
    raise SystemExit("release inventory CPU worker digest is invalid")
entries = document["entries"]
if not isinstance(entries, list) or not entries:
    raise SystemExit("release inventory entries are missing")
paths = []
casefolded_paths = set()
rows = []
for entry in entries:
    if set(entry) != {"path", "mode", "size_bytes", "sha256"}:
        raise SystemExit("release inventory entry has unknown or missing fields")
    if not isinstance(entry["size_bytes"], int) or isinstance(entry["size_bytes"], bool) or entry["size_bytes"] < 0:
        raise SystemExit("release inventory size is invalid")
    if not re.fullmatch(r"0[0-7]{3}", entry["mode"]) or not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"]):
        raise SystemExit("release inventory mode or digest is invalid")
    if not isinstance(entry["path"], str):
        raise SystemExit("release inventory path is invalid")
    folded = entry["path"].casefold()
    if folded in casefolded_paths:
        raise SystemExit("release inventory contains case-colliding paths")
    casefolded_paths.add(folded)
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
package_bytes="$(stat -c %s -- "$package")"
python3 - "$installed_bytes" "$package_bytes" <<'PY'
import json, sys
print(json.dumps({"schema_version": 1, "target": "x86_64-unknown-linux-gnu", "installed_size_bytes": int(sys.argv[1]), "compressed_size_bytes": int(sys.argv[2]), "packs": []}, sort_keys=True, separators=(",", ":")))
PY
