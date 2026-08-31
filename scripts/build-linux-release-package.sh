#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
trap 'status=$?; echo "Linux release package assembly failed at line $LINENO (exit $status)." >&2; exit "$status"' ERR

usage() {
  echo 'usage: build-linux-release-package.sh --desktop <path> --cpu-worker <path> --output <path.deb> --version <semver> [--gpu-pack <signed-pack-root>]...' >&2
}

desktop=''; cpu_worker=''; output=''; version=''; gpu_packs=()
while (($#)); do
  case "$1" in
    --desktop) desktop="${2:-}"; shift 2 ;;
    --cpu-worker) cpu_worker="${2:-}"; shift 2 ;;
    --output) output="${2:-}"; shift 2 ;;
    --version) version="${2:-}"; shift 2 ;;
    --gpu-pack) gpu_packs+=("${2:-}"); shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

[[ "$(uname -s)" == Linux && "$(uname -m)" == x86_64 ]] || { echo 'Linux x86_64 is required.' >&2; exit 1; }
[[ -n "$desktop" && -n "$cpu_worker" && -n "$output" && -n "$version" ]] || { usage; exit 2; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+~-][a-z0-9.+~-]+)?$ ]] || { echo 'version must be a canonical lowercase Debian-compatible semantic version.' >&2; exit 2; }
for command in dpkg-deb python3 sha256sum stat tar; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
source "$repo_root/scripts/linux-release-package-common.sh"
[[ ! -L "$desktop" && ! -L "$cpu_worker" ]] || { echo 'release input arguments must not be symlinks.' >&2; exit 1; }
desktop="$(realpath -e -- "$desktop")"; cpu_worker="$(realpath -e -- "$cpu_worker")"
for input in "$desktop" "$cpu_worker"; do
  [[ -f "$input" && ! -L "$input" ]] || { echo "release input must be a regular non-symlink file: $input" >&2; exit 1; }
  [[ "$(stat -c %h -- "$input")" == 1 ]] || { echo "release input must not be hardlinked: $input" >&2; exit 1; }
  [[ "$(stat -c %s -- "$input")" -le 2147483648 ]] || { echo "release input exceeds the 2 GiB per-file bound: $input" >&2; exit 1; }
done
linux_require_x86_64_elf "$desktop" 'desktop input'
linux_require_x86_64_elf "$cpu_worker" 'CPU worker input'
output_parent="$(dirname -- "$output")"; mkdir -p -- "$output_parent"
output="$(cd "$output_parent" && pwd -P)/$(basename -- "$output")"
[[ "$output" == *.deb ]] || { echo 'output must have a .deb suffix.' >&2; exit 2; }
[[ ! -e "$output" && ! -e "$output.sizes.json" ]] || { echo 'release output already exists; refusing to overwrite it.' >&2; exit 1; }

source_date_epoch="${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" show -s --format=%ct HEAD)}"
[[ "$source_date_epoch" =~ ^[1-9][0-9]{8,10}$ ]] || { echo 'SOURCE_DATE_EPOCH must be a canonical positive Unix timestamp.' >&2; exit 2; }
build_revision="${SCRIBE_BUILD_REVISION:-$(git -C "$repo_root" rev-parse --verify HEAD)}"
[[ "$build_revision" =~ ^[0-9a-f]{40}$ ]] || { echo 'SCRIBE_BUILD_REVISION must be a full lowercase Git commit.' >&2; exit 2; }

temp_root="$(mktemp -d "${TMPDIR:-/tmp}/scribe-linux-release.XXXXXX")"
temporary_deb=''; temporary_report=''
cleanup() {
  local status=$?
  rm -rf -- "$temp_root"
  [[ -z "$temporary_deb" ]] || rm -f -- "$temporary_deb"
  [[ -z "$temporary_report" ]] || rm -f -- "$temporary_report"
  if ((status)); then echo "Linux release package assembly failed (exit $status)." >&2; fi
  exit "$status"
}
trap cleanup EXIT
package_root="$temp_root/root"; authority_root="$package_root/usr/lib/scribe"; packs_root="$authority_root/workers"
umask 022
mkdir -p -- "$package_root/DEBIAN" "$package_root/usr/bin" "$packs_root/packs" "$temp_root/pack-state"
install -m 0755 -- "$desktop" "$package_root/usr/bin/local-transcriber"
install -m 0755 -- "$cpu_worker" "$authority_root/scribe-inference-worker"

worker_sha256="$(sha256sum "$authority_root/scribe-inference-worker" | awk '{print $1}')"
LC_ALL=C grep -aF -- "$worker_sha256" "$package_root/usr/bin/local-transcriber" >/dev/null || {
  echo 'desktop does not embed the exact packaged CPU worker SHA-256 anchor.' >&2
  exit 1
}

printf '%s' '{"schema_version":1,"packs":[]}' >"$authority_root/worker-pack-catalog.json"
chmod 0644 "$authority_root/worker-pack-catalog.json"
install -m 0644 -- "$repo_root/runtime-manifests/linux-release-package-x86_64.json" "$authority_root/linux-release-package.json"

for pack in "${gpu_packs[@]}"; do
  [[ ! -L "$pack" ]] || { echo 'GPU pack input argument must not be a symlink.' >&2; exit 1; }
done
if ((${#gpu_packs[@]})); then
  command -v cargo >/dev/null || { echo 'cargo is required to verify a requested GPU pack.' >&2; exit 1; }
  export SCRIBE_BUILD_REVISION="$build_revision"
  cargo build --locked --release --manifest-path "$repo_root/tools/worker-pack-author/Cargo.toml"
  pack_tool="$repo_root/tools/worker-pack-author/target/release/scribe-worker-pack-tool"
  for pack in "${gpu_packs[@]}"; do
    "$pack_tool" install-production-linux --pack-root "$pack" --packs-root "$packs_root" --state-root "$temp_root/pack-state"
  done
  echo 'internal error: a non-empty Linux GPU pack set was accepted while production trust is empty.' >&2
  exit 1
fi

inventory_entries="$temp_root/inventory.entries"
: >"$inventory_entries"
while IFS= read -r relative; do
  path="$package_root/$relative"
  size="$(stat -c %s -- "$path")"; mode="$(stat -c %a -- "$path")"; digest="$(sha256sum "$path" | awk '{print $1}')"
  printf '%s\t%s\t%s\t%s\n' "$relative" "0$mode" "$size" "$digest" >>"$inventory_entries"
done < <(cd "$package_root" && find usr -type f ! -path 'usr/lib/scribe/linux-release-inventory.json' -printf '%p\n' | LC_ALL=C sort)
python3 - "$inventory_entries" "$authority_root/linux-release-inventory.json" "$worker_sha256" "$build_revision" <<'PY'
import json, pathlib, sys
source, output, worker_sha256, build_revision = sys.argv[1:]
entries = []
for line in pathlib.Path(source).read_text(encoding="utf-8").splitlines():
    path, mode, size, digest = line.split("\t")
    entries.append({"path": path, "mode": mode, "size_bytes": int(size), "sha256": digest})
document = {"schema_version": 1, "target": "x86_64-unknown-linux-gnu", "build_revision": build_revision, "cpu_worker_sha256": worker_sha256, "entries": entries}
pathlib.Path(output).write_text(json.dumps(document, sort_keys=True, separators=(",", ":")), encoding="utf-8")
PY
chmod 0644 "$authority_root/linux-release-inventory.json"

installed_bytes="$(linux_regular_file_bytes "$package_root/usr")"
[[ "$installed_bytes" -le 4294967296 ]] || { echo 'release package exceeds the 4 GiB aggregate bound.' >&2; exit 1; }
installed_kib="$(((installed_bytes + 1023) / 1024))"
cat >"$package_root/DEBIAN/control" <<EOF
Package: scribe
Version: $version
Architecture: amd64
Maintainer: Scribe Release Engineering <noreply@example.invalid>
Installed-Size: $installed_kib
Section: sound
Priority: optional
Description: Scribe local transcription desktop and verified inference workers
EOF
chmod 0644 "$package_root/DEBIAN/control"
find "$package_root" -type d -exec chmod 0755 -- {} +
chmod 0755 "$package_root/usr/bin/local-transcriber" "$authority_root/scribe-inference-worker"
chmod 0644 "$authority_root/worker-pack-catalog.json" "$authority_root/linux-release-package.json" "$authority_root/linux-release-inventory.json" "$package_root/DEBIAN/control"

find "$package_root" -exec touch --no-dereference --date="@$source_date_epoch" -- {} +
temporary_deb="$(mktemp "$output_parent/.scribe-linux-release.XXXXXX.deb")"
rm -f -- "$temporary_deb"
SOURCE_DATE_EPOCH="$source_date_epoch" dpkg-deb --root-owner-group --build -Zxz -z9 --uniform-compression "$package_root" "$temporary_deb" >/dev/null
chmod 0644 "$temporary_deb"
mv --no-clobber -- "$temporary_deb" "$output"
[[ ! -e "$temporary_deb" ]] || { echo 'release output appeared before atomic publication; refusing to overwrite it.' >&2; exit 1; }
package_bytes="$(stat -c %s -- "$output")"
temporary_report="$(mktemp "$output_parent/.scribe-linux-release-size.XXXXXX.json")"
python3 - "$temporary_report" "$installed_bytes" "$package_bytes" <<'PY'
import json, pathlib, sys
document = {"schema_version": 1, "target": "x86_64-unknown-linux-gnu", "package_format": "deb", "installed_size_bytes": int(sys.argv[2]), "compressed_size_bytes": int(sys.argv[3]), "packs": []}
pathlib.Path(sys.argv[1]).write_text(json.dumps(document, sort_keys=True, separators=(",", ":")), encoding="utf-8")
PY
chmod 0644 "$temporary_report"
mv --no-clobber -- "$temporary_report" "$output.sizes.json"
[[ ! -e "$temporary_report" ]] || { echo 'size report output appeared before atomic publication; refusing to overwrite it.' >&2; exit 1; }
echo "$output"
