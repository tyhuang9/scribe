#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
trap 'status=$?; echo "Linux release packaging contract check failed at line $LINENO (exit $status)." >&2; exit "$status"' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
for command in ar cc dpkg-deb python3 xz; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done
for script in build-linux-release-package.sh verify-linux-release-package.sh report-linux-worker-pack-sizes.sh linux-release-package-common.sh; do bash -n "$repo_root/scripts/$script"; done
python3 - "$repo_root/runtime-manifests/linux-release-package-x86_64.json" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); raw = path.read_bytes(); document = json.loads(raw)
expected = {"schema_version":1,"target":"x86_64-unknown-linux-gnu","package_format":"deb","package_name":"scribe","desktop_path":"usr/bin/local-transcriber","authority_root":"usr/lib/scribe","cpu_worker_path":"usr/lib/scribe/scribe-inference-worker","pack_root":"usr/lib/scribe/workers/packs","catalog_path":"usr/lib/scribe/worker-pack-catalog.json","release_authority_path":"usr/lib/scribe/gpu-pack-release-authority.json","inventory_path":"usr/lib/scribe/linux-release-inventory.json","production_trust":"empty","gpu_packs":[]}
if document != expected or raw != (json.dumps(expected, separators=(",", ":")) + "\n").encode():
    raise SystemExit("Linux release package contract is not canonical default-deny")
PY
[[ "$(cat "$repo_root/runtime-manifests/gpu-auto-qualification-linux-x86_64.json")" == '{"schema_version":2,"mode":"default_deny","target_os":"linux","target_arch":"x86_64","entries":[]}' ]] || { echo 'Linux Auto qualification must remain canonical and empty.' >&2; exit 1; }

test_root="$(mktemp -d "${TMPDIR:-/tmp}/scribe-linux-package-test.XXXXXX")"
trap 'status=$?; rm -rf -- "$test_root"; exit "$status"' EXIT
worker="$test_root/scribe-inference-worker"; desktop="$test_root/local-transcriber"
cat >"$test_root/worker.c" <<'C'
int main(void) { return 0; }
C
cc -Os -Wl,--build-id=none -o "$worker" "$test_root/worker.c"
worker_sha="$(sha256sum "$worker" | awk '{print $1}')"
cat >"$test_root/desktop.c" <<C
#include <stdio.h>
static const char packaged_worker_anchor[] = "$worker_sha";
int main(void) { return puts(packaged_worker_anchor) < 0; }
C
cc -Os -Wl,--build-id=none -o "$desktop" "$test_root/desktop.c"
source "$repo_root/scripts/linux-release-package-common.sh"
python3 - "$desktop" <<'PY'
import pathlib, sys
if pathlib.Path(sys.argv[1]).read_bytes()[7] != 0:
    raise SystemExit("compiler fixture did not provide System V ELF OSABI 0")
PY
cp "$desktop" "$test_root/linux-osabi"
cp "$desktop" "$test_root/invalid-osabi"
python3 - "$test_root/linux-osabi" "$test_root/invalid-osabi" <<'PY'
import pathlib, sys
for name, osabi in ((sys.argv[1], 3), (sys.argv[2], 9)):
    path = pathlib.Path(name); data = bytearray(path.read_bytes()); data[7] = osabi; path.write_bytes(data)
PY
linux_require_x86_64_elf "$desktop" 'System V OSABI fixture'
linux_require_x86_64_elf "$test_root/linux-osabi" 'Linux OSABI fixture'
if linux_require_x86_64_elf "$test_root/invalid-osabi" 'invalid OSABI fixture' >/dev/null 2>&1; then echo 'ELF validator accepted an unsupported OSABI.' >&2; exit 1; fi
if [[ -n "${SCRIBE_BUILD_REVISION:-}" ]]; then
  revision="$SCRIBE_BUILD_REVISION"
elif revision="$(git -C "$repo_root" rev-parse --verify HEAD 2>/dev/null)"; then
  :
else
  revision='0000000000000000000000000000000000000001'
fi
[[ "$revision" =~ ^[0-9a-f]{40}$ ]] || { echo 'test build revision is invalid.' >&2; exit 1; }
epoch=1700000000

expect_build_rejection() {
  local label="$1" desktop_input="$2" worker_input="$3"
  if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop_input" --cpu-worker "$worker_input" --output "$test_root/rejected-$label.deb" --version 0.1.0 >/dev/null 2>&1; then
    echo "release builder accepted $label input." >&2; exit 1
  fi
  [[ ! -e "$test_root/rejected-$label.deb" && ! -L "$test_root/rejected-$label.deb" && ! -e "$test_root/rejected-$label.deb.sizes.json" && ! -L "$test_root/rejected-$label.deb.sizes.json" ]] || { echo "rejected $label input published an output." >&2; exit 1; }
}
printf 'not an ELF\n' >"$test_root/not-elf"
cp "$worker" "$test_root/wrong-arch"
python3 - "$test_root/wrong-arch" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1]); data = bytearray(path.read_bytes()); data[18:20] = (183).to_bytes(2, "little"); path.write_bytes(data)
PY
ln -s "$desktop" "$test_root/desktop-link"
ln -s "$worker" "$test_root/worker-link"
expect_build_rejection non-elf-desktop "$test_root/not-elf" "$worker"
expect_build_rejection non-elf-worker "$desktop" "$test_root/not-elf"
expect_build_rejection wrong-arch-worker "$desktop" "$test_root/wrong-arch"
expect_build_rejection symlink-desktop "$test_root/desktop-link" "$worker"
expect_build_rejection symlink-worker "$desktop" "$test_root/worker-link"

expect_source_authority_rejection() {
  local label="$1" mini_root="$test_root/source-authority-$1" output="$test_root/source-authority-$1.deb"
  mkdir -p "$mini_root/scripts" "$mini_root/runtime-manifests"
  cp -- "$repo_root/scripts/build-linux-release-package.sh" "$mini_root/scripts/"
  cp -- "$repo_root/scripts/linux-release-package-common.sh" "$mini_root/scripts/"
  cp -- "$repo_root/runtime-manifests/linux-release-package-x86_64.json" "$mini_root/runtime-manifests/"
  cp -- "$repo_root/runtime-manifests/gpu-pack-release-authority-linux-empty.json" "$mini_root/runtime-manifests/"
  python3 - "$mini_root/runtime-manifests/gpu-pack-release-authority-linux-empty.json" "$label" <<'PY'
import pathlib, sys

path = pathlib.Path(sys.argv[1])
kind = sys.argv[2]
raw = path.read_bytes()

def replace_once(before, after):
    global raw
    if raw.count(before) != 1:
        raise SystemExit(f"expected one {before!r} mutation target")
    raw = raw.replace(before, after)

if kind == "missing":
    path.unlink()
    raise SystemExit(0)
if kind == "tampered":
    replace_once(b'"entries":[]}', b'"entries":[],"reviewed":false}')
elif kind == "wrong-target":
    replace_once(b'x86_64-unknown-linux-gnu', b'x86_64-unknown-linux-musl')
elif kind == "schema":
    replace_once(b'"schema_version":1', b'"schema_version":2')
elif kind == "digest":
    replace_once(b'c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc', b'0' * 64)
elif kind == "positive-empty":
    replace_once(b'"release_security_epoch":0', b'"release_security_epoch":1')
elif kind == "nonempty":
    replace_once(b'"entries":[]', b'"entries":[{}]')
elif kind == "bom":
    raw = b'\xef\xbb\xbf' + raw
elif kind == "trailing-lf":
    raw += b'\n'
elif kind == "whitespace":
    replace_once(b'{"schema_version"', b'{ "schema_version"')
else:
    raise SystemExit(f"unknown source authority mutation: {kind}")
path.write_bytes(raw)
PY
  if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$mini_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$output" --version 0.1.0 >/dev/null 2>&1; then
    echo "release builder accepted source authority mutation: $label." >&2; exit 1
  fi
  [[ ! -e "$output" && ! -L "$output" && ! -e "$output.sizes.json" && ! -L "$output.sizes.json" ]] || { echo "source authority mutation published an output: $label." >&2; exit 1; }
}

for source_authority_mutation in missing tampered wrong-target schema digest positive-empty nonempty bom trailing-lf whitespace; do
  expect_source_authority_rejection "$source_authority_mutation"
done

existing_output="$test_root/existing-output.deb"
printf 'existing-package-sentinel\n' >"$existing_output"
if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$existing_output" --version 0.1.0 >/dev/null 2>&1; then
  echo 'release builder overwrote an existing package sentinel.' >&2; exit 1
fi
cmp -s "$existing_output" <(printf 'existing-package-sentinel\n') || { echo 'existing package sentinel changed.' >&2; exit 1; }

regular_sidecar_output="$test_root/regular-sidecar-sentinel.deb"
printf 'existing-sidecar-sentinel\n' >"$regular_sidecar_output.sizes.json"
if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$regular_sidecar_output" --version 0.1.0 >/dev/null 2>&1; then
  echo 'release builder overwrote an existing size-report sentinel.' >&2; exit 1
fi
[[ ! -e "$regular_sidecar_output" && ! -L "$regular_sidecar_output" ]] || { echo 'size-report collision published a package.' >&2; exit 1; }
cmp -s "$regular_sidecar_output.sizes.json" <(printf 'existing-sidecar-sentinel\n') || { echo 'existing size-report sentinel changed.' >&2; exit 1; }

dangling_output="$test_root/dangling-output.deb"
ln -s "$test_root/missing-output-target" "$dangling_output"
if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$dangling_output" --version 0.1.0 >/dev/null 2>&1; then
  echo 'release builder accepted a dangling package output symlink.' >&2; exit 1
fi
[[ -L "$dangling_output" && "$(readlink "$dangling_output")" == "$test_root/missing-output-target" ]] || { echo 'dangling package output link changed.' >&2; exit 1; }

dangling_sidecar_output="$test_root/dangling-sidecar.deb"
ln -s "$test_root/missing-sidecar-target" "$dangling_sidecar_output.sizes.json"
if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$dangling_sidecar_output" --version 0.1.0 >/dev/null 2>&1; then
  echo 'release builder accepted a dangling size-report output symlink.' >&2; exit 1
fi
[[ ! -e "$dangling_sidecar_output" && ! -L "$dangling_sidecar_output" ]] || { echo 'dangling size-report collision published a package.' >&2; exit 1; }
[[ -L "$dangling_sidecar_output.sizes.json" && "$(readlink "$dangling_sidecar_output.sizes.json")" == "$test_root/missing-sidecar-target" ]] || { echo 'dangling size-report link changed.' >&2; exit 1; }

for build in first:077 second:022 third:002; do
  name="${build%%:*}"; caller_umask="${build##*:}"
  (umask "$caller_umask"; SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" \
    --desktop "$desktop" --cpu-worker "$worker" --output "$test_root/$name.deb" --version 0.1.0 >/dev/null)
  bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$test_root/$name.deb" >/dev/null
  bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$test_root/$name.deb" >/dev/null
  [[ "$(stat -c %a "$test_root/$name.deb")" == 644 && "$(stat -c %a "$test_root/$name.deb.sizes.json")" == 644 ]] || { echo 'release artifact modes are not normalized to 0644.' >&2; exit 1; }
done
cmp -s "$test_root/first.deb" "$test_root/second.deb" || { echo 'identical release inputs did not produce an identical .deb.' >&2; exit 1; }
cmp -s "$test_root/first.deb" "$test_root/third.deb" || { echo 'caller umask changed the assembled .deb.' >&2; exit 1; }
cmp -s "$test_root/first.deb.sizes.json" "$test_root/second.deb.sizes.json" || { echo 'identical release inputs did not produce an identical size report.' >&2; exit 1; }
cmp -s "$test_root/first.deb.sizes.json" "$test_root/third.deb.sizes.json" || { echo 'caller umask changed the size report.' >&2; exit 1; }
dpkg-deb -R "$test_root/first.deb" "$test_root/first-expanded" >/dev/null
python3 - "$test_root/first-expanded" <<'PY'
import json, pathlib, sys

root = pathlib.Path(sys.argv[1])
files = sorted(str(path.relative_to(root)) for path in (root / "usr").rglob("*") if path.is_file())
expected_files = [
    "usr/bin/local-transcriber",
    "usr/lib/scribe/gpu-pack-release-authority.json",
    "usr/lib/scribe/linux-release-inventory.json",
    "usr/lib/scribe/linux-release-package.json",
    "usr/lib/scribe/scribe-inference-worker",
    "usr/lib/scribe/worker-pack-catalog.json",
]
if files != expected_files or len(files) != 6:
    raise SystemExit("CPU-only package payload must contain exactly six reviewed files")
inventory = json.loads((root / "usr/lib/scribe/linux-release-inventory.json").read_bytes())
if len(inventory.get("entries", [])) != 5:
    raise SystemExit("CPU-only package inventory must contain exactly five payload rows")
authority = (root / "usr/lib/scribe/gpu-pack-release-authority.json").read_bytes()
reviewed_authority = b'{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[]}'
if authority != reviewed_authority:
    raise SystemExit("packaged release authority bytes differ from the reviewed empty authority")
if (root / "usr/lib/scribe/worker-pack-catalog.json").read_bytes() != b'{"schema_version":1,"packs":[]}':
    raise SystemExit("packaged empty catalog bytes are not exact")
PY
ln -s "$test_root/first.deb" "$test_root/package-link.deb"
if bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$test_root/package-link.deb" >/dev/null 2>&1; then echo 'release verifier accepted a symlink package argument.' >&2; exit 1; fi
if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$test_root/package-link.deb" >/dev/null 2>&1; then echo 'size reporter accepted a symlink package argument.' >&2; exit 1; fi
cp "$test_root/first.deb.sizes.json" "$test_root/first.deb.sizes.backup"
for sidecar_attack in stale-installed bool-schema float-compressed; do
  cp "$test_root/first.deb.sizes.backup" "$test_root/first.deb.sizes.json"
  python3 - "$test_root/first.deb.sizes.json" "$sidecar_attack" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); value = json.loads(path.read_bytes())
if sys.argv[2] == "stale-installed": value["installed_size_bytes"] += 1
elif sys.argv[2] == "bool-schema": value["schema_version"] = True
elif sys.argv[2] == "float-compressed": value["compressed_size_bytes"] = float(value["compressed_size_bytes"])
path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")), encoding="utf-8")
PY
  if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$test_root/first.deb" >/dev/null 2>&1; then echo "size reporter accepted $sidecar_attack sidecar." >&2; exit 1; fi
done
mv "$test_root/first.deb.sizes.backup" "$test_root/first.deb.sizes.json"
if SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$test_root/first.deb" --version 0.1.0 >/dev/null 2>&1; then
  echo 'release builder overwrote an existing package.' >&2; exit 1
fi

attack_package() {
  local name="$1"; shift
  local root="$test_root/$name-root" package="$test_root/$name.deb"
  dpkg-deb -R "$test_root/first.deb" "$root" >/dev/null
  "$@" "$root"
  find "$root" -exec touch --no-dereference --date="@$epoch" -- {} +
  SOURCE_DATE_EPOCH="$epoch" dpkg-deb --root-owner-group --build -Zxz -z9 --uniform-compression "$root" "$package" >/dev/null
  if bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$package" >/dev/null 2>&1; then
    echo "release verifier accepted $name attack package." >&2; exit 1
  fi
}
refresh_inventory_modes() {
  python3 - "$1" <<'PY'
import json, pathlib, stat, sys
root = pathlib.Path(sys.argv[1]); inventory = root / "usr/lib/scribe/linux-release-inventory.json"
document = json.loads(inventory.read_bytes())
for entry in document["entries"]:
    entry["mode"] = f"{stat.S_IMODE((root / entry['path']).stat().st_mode):04o}"
inventory.write_text(json.dumps(document, sort_keys=True, separators=(",", ":")), encoding="utf-8")
PY
}
refresh_inventory_entries_and_installed_size() {
  python3 - "$1" <<'PY'
import hashlib, json, pathlib, stat, sys

root = pathlib.Path(sys.argv[1])
inventory = root / "usr/lib/scribe/linux-release-inventory.json"
document = json.loads(inventory.read_bytes())
for entry in document["entries"]:
    path = root / entry["path"]
    entry["mode"] = f"{stat.S_IMODE(path.stat().st_mode):04o}"
    entry["size_bytes"] = path.stat().st_size
    entry["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
inventory.write_text(json.dumps(document, sort_keys=True, separators=(",", ":")), encoding="utf-8")
installed = sum(path.stat().st_size for path in (root / "usr").rglob("*") if path.is_file())
control = root / "DEBIAN/control"
lines = control.read_text(encoding="utf-8").splitlines()
lines = [f"Installed-Size: {(installed + 1023) // 1024}" if line.startswith("Installed-Size: ") else line for line in lines]
control.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
}
add_file_with_consistent_inventory() {
  local root="$1" relative="$2" source="$3"
  install -m 0755 "$source" "$root/$relative"
  python3 - "$root" "$relative" <<'PY'
import hashlib, json, pathlib, stat, sys
root = pathlib.Path(sys.argv[1]); relative = sys.argv[2]
inventory = root / "usr/lib/scribe/linux-release-inventory.json"
document = json.loads(inventory.read_bytes())
path = root / relative
document["entries"].append({
    "path": relative,
    "mode": f"{stat.S_IMODE(path.stat().st_mode):04o}",
    "size_bytes": path.stat().st_size,
    "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
})
document["entries"].sort(key=lambda entry: entry["path"])
inventory.write_text(json.dumps(document, sort_keys=True, separators=(",", ":")), encoding="utf-8")
installed = sum(path.stat().st_size for path in (root / "usr").rglob("*") if path.is_file())
control = root / "DEBIAN/control"
lines = control.read_text(encoding="utf-8").splitlines()
lines = [f"Installed-Size: {(installed + 1023) // 1024}" if line.startswith("Installed-Size: ") else line for line in lines]
control.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
}
tamper_worker() { printf 'tampered\n' >>"$1/usr/lib/scribe/scribe-inference-worker"; }
add_unexpected() { printf 'unexpected\n' >"$1/usr/lib/scribe/unexpected"; }
add_unexpected_top_level() { mkdir "$1/opt"; printf 'unexpected\n' >"$1/opt/extra-tool"; chmod 0755 "$1/opt/extra-tool"; }
add_unexpected_directory() { mkdir "$1/usr/lib/scribe/unexpected-directory"; }
replace_catalog() { printf '%s' '{"schema_version":1,"packs":[{}]}' >"$1/usr/lib/scribe/worker-pack-catalog.json"; refresh_inventory_entries_and_installed_size "$1"; }
catalog_trailing_lf() { printf '\n' >>"$1/usr/lib/scribe/worker-pack-catalog.json"; refresh_inventory_entries_and_installed_size "$1"; }
replace_contract() { printf '%s\n' '{}' >"$1/usr/lib/scribe/linux-release-package.json"; }
remove_release_authority() { rm -- "$1/usr/lib/scribe/gpu-pack-release-authority.json"; }
replace_release_authority() {
  printf '%s' "$2" >"$1/usr/lib/scribe/gpu-pack-release-authority.json"
  refresh_inventory_entries_and_installed_size "$1"
}
tamper_release_authority() { replace_release_authority "$1" '{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[],"reviewed":false}'; }
wrong_target_release_authority() { replace_release_authority "$1" '{"schema_version":1,"target":"x86_64-unknown-linux-musl","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[]}'; }
wrong_schema_release_authority() { replace_release_authority "$1" '{"schema_version":2,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[]}'; }
wrong_digest_release_authority() { replace_release_authority "$1" '{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"0000000000000000000000000000000000000000000000000000000000000000","release_security_epoch":0,"entries":[]}'; }
positive_empty_release_authority() { replace_release_authority "$1" '{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":1,"entries":[]}'; }
nonempty_release_authority() { replace_release_authority "$1" '{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":1,"entries":[{}]}'; }
release_authority_bom() { python3 - "$1" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1]) / "usr/lib/scribe/gpu-pack-release-authority.json"
path.write_bytes(b'\xef\xbb\xbf' + path.read_bytes())
PY
  refresh_inventory_entries_and_installed_size "$1"
}
release_authority_trailing_lf() { printf '\n' >>"$1/usr/lib/scribe/gpu-pack-release-authority.json"; refresh_inventory_entries_and_installed_size "$1"; }
release_authority_whitespace() { python3 - "$1" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1]) / "usr/lib/scribe/gpu-pack-release-authority.json"
raw = path.read_bytes()
if raw.count(b'{"schema_version"') != 1:
    raise SystemExit("release authority whitespace fixture target is ambiguous")
path.write_bytes(raw.replace(b'{"schema_version"', b'{ "schema_version"'))
PY
  refresh_inventory_entries_and_installed_size "$1"
}
replace_release_authority_with_link() { rm -- "$1/usr/lib/scribe/gpu-pack-release-authority.json"; ln -s scribe-inference-worker "$1/usr/lib/scribe/gpu-pack-release-authority.json"; }
replace_release_authority_with_hardlink() { rm -- "$1/usr/lib/scribe/gpu-pack-release-authority.json"; ln "$1/usr/lib/scribe/scribe-inference-worker" "$1/usr/lib/scribe/gpu-pack-release-authority.json"; }
replace_worker_with_link() { rm "$1/usr/lib/scribe/scribe-inference-worker"; ln -s /bin/true "$1/usr/lib/scribe/scribe-inference-worker"; }
add_maintainer_script() { printf '#!/bin/sh\nexit 0\n' >"$1/DEBIAN/postinst"; chmod 0755 "$1/DEBIAN/postinst"; }
make_authority_world_writable() { chmod 0777 "$1/usr/lib/scribe"; }
make_pack_root_group_writable() { chmod 0775 "$1/usr/lib/scribe/workers/packs"; }
replace_desktop_with_text() { printf 'not an ELF\n' >"$1/usr/bin/local-transcriber"; chmod 0755 "$1/usr/bin/local-transcriber"; }
replace_worker_with_wrong_arch() { cp "$test_root/wrong-arch" "$1/usr/lib/scribe/scribe-inference-worker"; chmod 0755 "$1/usr/lib/scribe/scribe-inference-worker"; }
make_desktop_group_writable_with_consistent_inventory() { chmod 0775 "$1/usr/bin/local-transcriber"; refresh_inventory_modes "$1"; }
make_catalog_group_writable_with_consistent_inventory() { chmod 0664 "$1/usr/lib/scribe/worker-pack-catalog.json"; refresh_inventory_modes "$1"; }
make_release_authority_group_writable_with_consistent_inventory() { chmod 0664 "$1/usr/lib/scribe/gpu-pack-release-authority.json"; refresh_inventory_modes "$1"; }
make_inventory_schema_boolean() { python3 -c 'import json,pathlib,sys; p=pathlib.Path(sys.argv[1])/"usr/lib/scribe/linux-release-inventory.json"; d=json.loads(p.read_bytes()); d["schema_version"]=True; p.write_text(json.dumps(d,sort_keys=True,separators=(",",":")),encoding="utf-8")' "$1"; }
add_replaces_control_field() { printf 'Replaces: unsafe-package\n' >>"$1/DEBIAN/control"; }
add_consistent_extra_executable() { add_file_with_consistent_inventory "$1" usr/bin/extra-tool "$worker"; }
add_consistent_case_collision() { add_file_with_consistent_inventory "$1" usr/bin/Local-Transcriber "$desktop"; }
attack_package tampered-worker tamper_worker
attack_package unexpected-file add_unexpected
attack_package unexpected-top-level-file add_unexpected_top_level
attack_package unexpected-directory add_unexpected_directory
attack_package nonempty-catalog replace_catalog
attack_package trailing-lf-catalog catalog_trailing_lf
attack_package wrong-contract replace_contract
attack_package missing-release-authority remove_release_authority
attack_package tampered-release-authority tamper_release_authority
attack_package wrong-target-release-authority wrong_target_release_authority
attack_package wrong-schema-release-authority wrong_schema_release_authority
attack_package wrong-digest-release-authority wrong_digest_release_authority
attack_package positive-empty-release-authority positive_empty_release_authority
attack_package nonempty-release-authority nonempty_release_authority
attack_package bom-release-authority release_authority_bom
attack_package trailing-lf-release-authority release_authority_trailing_lf
attack_package whitespace-release-authority release_authority_whitespace
attack_package release-authority-symlink replace_release_authority_with_link
attack_package release-authority-hardlink replace_release_authority_with_hardlink
attack_package worker-symlink replace_worker_with_link
attack_package maintainer-script add_maintainer_script
attack_package world-writable-authority make_authority_world_writable
attack_package group-writable-pack-root make_pack_root_group_writable
attack_package non-elf-desktop replace_desktop_with_text
attack_package wrong-arch-worker replace_worker_with_wrong_arch
attack_package consistent-inventory-executable-mode make_desktop_group_writable_with_consistent_inventory
attack_package consistent-inventory-metadata-mode make_catalog_group_writable_with_consistent_inventory
attack_package consistent-inventory-release-authority-mode make_release_authority_group_writable_with_consistent_inventory
attack_package boolean-inventory-schema make_inventory_schema_boolean
attack_package relationship-control-field add_replaces_control_field
attack_package consistent-inventory-extra-executable add_consistent_extra_executable
attack_package consistent-inventory-case-collision add_consistent_case_collision

control_attack_root="$test_root/control-mode-components"; mkdir "$control_attack_root"
(cd "$control_attack_root" && ar x "$test_root/first.deb")
mkdir "$control_attack_root/control-root"
tar -xf "$control_attack_root/control.tar.xz" -C "$control_attack_root/control-root"
chmod 0664 "$control_attack_root/control-root/control"
tar -C "$control_attack_root/control-root" --create --xz --no-recursion --owner=0 --group=0 --numeric-owner --mtime="@$epoch" -f "$control_attack_root/control.tar.xz" ./ ./control
(cd "$control_attack_root" && ar rcD "$test_root/control-mode.deb" debian-binary control.tar.xz data.tar.xz)
chmod 0644 "$test_root/control-mode.deb"
if bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$test_root/control-mode.deb" >/dev/null 2>&1; then echo 'release verifier accepted non-0644 DEBIAN/control.' >&2; exit 1; fi

python3 - "$test_root/first.deb" "$test_root" "$epoch" <<'PY'
import copy, io, lzma, pathlib, sys, tarfile

source = pathlib.Path(sys.argv[1]); root = pathlib.Path(sys.argv[2]); epoch = int(sys.argv[3])
raw = source.read_bytes()
if not raw.startswith(b"!<arch>\n"):
    raise SystemExit("fixture package is not ar")
offset = 8
members = []
while offset < len(raw):
    header = raw[offset:offset + 60]
    if len(header) != 60:
        raise SystemExit("fixture package has a truncated ar header")
    size = int(header[48:58].decode("ascii").strip())
    name = header[:16].decode("ascii").rstrip(" ")
    offset += 60
    payload = raw[offset:offset + size]
    offset += size + (size % 2)
    members.append((name, payload))
if [name for name, _ in members] != ["debian-binary", "control.tar.xz", "data.tar.xz"]:
    raise SystemExit("fixture package ar members are unexpected")

def header(name, size):
    return (
        name.encode("ascii").ljust(16, b" ")
        + str(epoch).encode("ascii").ljust(12, b" ")
        + b"0     0     100644  "
        + str(size).encode("ascii").ljust(10, b" ")
        + b"`\n"
    )

def write(name, entries, bad_padding=False, trailing=b""):
    output = bytearray(b"!<arch>\n")
    for index, (member_name, payload) in enumerate(entries):
        output.extend(header(member_name, len(payload)))
        output.extend(payload)
        if len(payload) % 2:
            output.extend(b"X" if bad_padding and index == 1 else b"\n")
    output.extend(trailing)
    path = root / f"outer-{name}.deb"
    path.write_bytes(output)
    path.chmod(0o644)

debian, control, data = members
write("duplicate", [debian, control, control, data])
write("wrong-order", [debian, data, control])
write("unexpected", [debian, control, ("extra", b"x"), data])
write("long-name-extension", [debian, control, ("//", b"x"), data])
write("trailing", members, trailing=b"x")
write("wrong-version", [("debian-binary", b"2.1\n"), control, data])
odd_payload = control[1] if len(control[1]) % 2 else control[1] + b"x"
odd_control = (control[0], odd_payload)
write("bad-padding", [debian, odd_control, data], bad_padding=True)
control_bomb = lzma.compress(b"0" * (2 * 1024 * 1024 + 1), format=lzma.FORMAT_XZ)
write("control-decompression-bound", [debian, ("control.tar.xz", control_bomb), data])

with tarfile.open(fileobj=io.BytesIO(lzma.decompress(data[1])), mode="r:") as source_tar:
    source_members = source_tar.getmembers()
    duplicate = source_tar.getmember("./usr/lib/scribe/gpu-pack-release-authority.json")
    duplicate_payload = source_tar.extractfile(duplicate).read()
    rebuilt = io.BytesIO()
    with tarfile.open(fileobj=rebuilt, mode="w", format=tarfile.GNU_FORMAT) as output_tar:
        for member in source_members:
            payload = source_tar.extractfile(member).read() if member.isfile() else None
            output_tar.addfile(member, io.BytesIO(payload) if payload is not None else None)
        output_tar.addfile(copy.copy(duplicate), io.BytesIO(duplicate_payload))
duplicate_authority_data = ("data.tar.xz", lzma.compress(rebuilt.getvalue(), format=lzma.FORMAT_XZ))
write("duplicate-authority", [debian, control, duplicate_authority_data])
PY
for outer_attack in duplicate duplicate-authority wrong-order unexpected long-name-extension trailing wrong-version bad-padding control-decompression-bound; do
  if bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$test_root/outer-$outer_attack.deb" >/dev/null 2>&1; then
    echo "release verifier accepted $outer_attack outer archive attack." >&2; exit 1
  fi
done

if command -v cargo >/dev/null; then
  export SCRIBE_BUILD_REVISION="$revision"
  cargo build --locked --offline --release --manifest-path "$repo_root/tools/worker-pack-author/Cargo.toml" >/dev/null
  tool="$repo_root/tools/worker-pack-author/target/release/scribe-worker-pack-tool"
  fixture_pack="$test_root/fixture-pack"; mkdir -p "$fixture_pack/bin"
  install -m 0755 "$worker" "$fixture_pack/bin/scribe-inference-worker"
  "$tool" author --backend vulkan --target-os linux --target-arch x86_64 --pack-root "$fixture_pack" \
    --pack-id scribe-vulkan-linux-x64 --pack-version 0.1.0-fixture --security-epoch 1 \
    --provider transcribe-cpp-ggml-vulkan --worker-path bin/scribe-inference-worker --fixture-signing >/dev/null
  first_size="$(bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --fixture-pack "$fixture_pack" --tool "$tool")"
  second_size="$(bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --fixture-pack "$fixture_pack" --tool "$tool")"
  [[ "$first_size" == "$second_size" && "$first_size" == *'"verification":"fixture-only"'* ]] || { echo 'fixture pack size reporting is not deterministic or clearly test-only.' >&2; exit 1; }
  if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --production-pack "$fixture_pack" --tool "$tool" >/dev/null 2>&1; then
    echo 'production size reporting accepted fixture trust.' >&2; exit 1
  fi
  fixture_option_pack="$test_root/fixture-option-pack"; mkdir -p "$fixture_option_pack/bin"
  install -m 0755 "$worker" "$fixture_option_pack/bin/scribe-inference-worker"
  printf 'option injection fixture\n' >"$fixture_option_pack/--checkpoint=1"
  printf 'option injection fixture\n' >"$fixture_option_pack/--checkpoint-action=exec=touch marker"
  "$tool" author --backend vulkan --target-os linux --target-arch x86_64 --pack-root "$fixture_option_pack" \
    --pack-id scribe-vulkan-linux-options --pack-version 0.1.0-fixture --security-epoch 1 \
    --provider transcribe-cpp-ggml-vulkan --worker-path bin/scribe-inference-worker --fixture-signing >/dev/null
  if (cd "$fixture_option_pack" && bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --fixture-pack "$fixture_option_pack" --tool "$tool" >/dev/null 2>&1); then
    echo 'size reporter accepted option-like fixture pack paths.' >&2; exit 1
  fi
  [[ ! -e "$fixture_option_pack/marker" ]] || { echo 'size reporter executed a tar option injected through a pack filename.' >&2; exit 1; }
  ln -s "$fixture_pack" "$test_root/fixture-pack-link"
  ln -s "$tool" "$test_root/tool-link"
  if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --fixture-pack "$test_root/fixture-pack-link" --tool "$tool" >/dev/null 2>&1; then echo 'size reporter accepted a symlink pack-root argument.' >&2; exit 1; fi
  if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --fixture-pack "$fixture_pack" --tool "$test_root/tool-link" >/dev/null 2>&1; then echo 'size reporter accepted a symlink tool argument.' >&2; exit 1; fi
  if SOURCE_DATE_EPOCH="$epoch" bash "$repo_root/scripts/build-linux-release-package.sh" --desktop "$desktop" --cpu-worker "$worker" --output "$test_root/untrusted.deb" --version 0.1.0 --gpu-pack "$fixture_pack" >"$test_root/untrusted.out" 2>"$test_root/untrusted.err"; then
    echo 'production assembly accepted a fixture-signed Linux GPU pack.' >&2; exit 1
  fi
  grep -F 'signature key is not trusted' "$test_root/untrusted.err" >/dev/null || { echo 'fixture pack rejection was not attributed to empty production trust.' >&2; cat "$test_root/untrusted.err" >&2; exit 1; }
  [[ ! -e "$test_root/untrusted.deb" ]] || { echo 'failed GPU assembly published a package.' >&2; exit 1; }
fi

echo 'Linux release packaging tests passed.'
