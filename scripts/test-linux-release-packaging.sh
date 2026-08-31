#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'
trap 'status=$?; echo "Linux release packaging contract check failed at line $LINENO (exit $status)." >&2; exit "$status"' ERR

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
for command in cc dpkg-deb python3; do command -v "$command" >/dev/null || { echo "$command is required." >&2; exit 1; }; done
for script in build-linux-release-package.sh verify-linux-release-package.sh report-linux-worker-pack-sizes.sh linux-release-package-common.sh; do bash -n "$repo_root/scripts/$script"; done
python3 - "$repo_root/runtime-manifests/linux-release-package-x86_64.json" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); raw = path.read_bytes(); document = json.loads(raw)
expected = {"schema_version":1,"target":"x86_64-unknown-linux-gnu","package_format":"deb","package_name":"scribe","desktop_path":"usr/bin/local-transcriber","authority_root":"usr/lib/scribe","cpu_worker_path":"usr/lib/scribe/scribe-inference-worker","pack_root":"usr/lib/scribe/workers/packs","catalog_path":"usr/lib/scribe/worker-pack-catalog.json","inventory_path":"usr/lib/scribe/linux-release-inventory.json","production_trust":"empty","gpu_packs":[]}
if document != expected or raw != (json.dumps(expected, separators=(",", ":")) + "\n").encode():
    raise SystemExit("Linux release package contract is not canonical default-deny")
PY
[[ "$(cat "$repo_root/runtime-manifests/gpu-auto-qualification-linux-x86_64.json")" == '{"schema_version":1,"mode":"default_deny","target_os":"linux","target_arch":"x86_64","entries":[]}' ]] || { echo 'Linux Auto qualification must remain canonical and empty.' >&2; exit 1; }

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
  [[ ! -e "$test_root/rejected-$label.deb" ]] || { echo "rejected $label input published a package." >&2; exit 1; }
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

for build in first:077 second:022 third:002; do
  name="${build%%:*}"; caller_umask="${build##*:}"
  (umask "$caller_umask"; SOURCE_DATE_EPOCH="$epoch" SCRIBE_BUILD_REVISION="$revision" bash "$repo_root/scripts/build-linux-release-package.sh" \
    --desktop "$desktop" --cpu-worker "$worker" --output "$test_root/$name.deb" --version 0.1.0 >/dev/null)
  bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$test_root/$name.deb" >/dev/null
  bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$test_root/$name.deb" >/dev/null
done
cmp -s "$test_root/first.deb" "$test_root/second.deb" || { echo 'identical release inputs did not produce an identical .deb.' >&2; exit 1; }
cmp -s "$test_root/first.deb" "$test_root/third.deb" || { echo 'caller umask changed the assembled .deb.' >&2; exit 1; }
cmp -s "$test_root/first.deb.sizes.json" "$test_root/second.deb.sizes.json" || { echo 'identical release inputs did not produce an identical size report.' >&2; exit 1; }
cmp -s "$test_root/first.deb.sizes.json" "$test_root/third.deb.sizes.json" || { echo 'caller umask changed the size report.' >&2; exit 1; }
ln -s "$test_root/first.deb" "$test_root/package-link.deb"
if bash "$repo_root/scripts/verify-linux-release-package.sh" --package "$test_root/package-link.deb" >/dev/null 2>&1; then echo 'release verifier accepted a symlink package argument.' >&2; exit 1; fi
if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$test_root/package-link.deb" >/dev/null 2>&1; then echo 'size reporter accepted a symlink package argument.' >&2; exit 1; fi
cp "$test_root/first.deb.sizes.json" "$test_root/first.deb.sizes.backup"
python3 - "$test_root/first.deb.sizes.json" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1]); value = json.loads(path.read_bytes()); value["installed_size_bytes"] += 1; path.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")), encoding="utf-8")
PY
if bash "$repo_root/scripts/report-linux-worker-pack-sizes.sh" --package "$test_root/first.deb" >/dev/null 2>&1; then echo 'size reporter accepted a stale installed-size sidecar.' >&2; exit 1; fi
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
tamper_worker() { printf 'tampered\n' >>"$1/usr/lib/scribe/scribe-inference-worker"; }
add_unexpected() { printf 'unexpected\n' >"$1/usr/lib/scribe/unexpected"; }
add_unexpected_directory() { mkdir "$1/usr/lib/scribe/unexpected-directory"; }
replace_catalog() { printf '%s' '{"schema_version":1,"packs":[{}]}' >"$1/usr/lib/scribe/worker-pack-catalog.json"; }
replace_contract() { printf '%s\n' '{}' >"$1/usr/lib/scribe/linux-release-package.json"; }
replace_worker_with_link() { rm "$1/usr/lib/scribe/scribe-inference-worker"; ln -s /bin/true "$1/usr/lib/scribe/scribe-inference-worker"; }
add_maintainer_script() { printf '#!/bin/sh\nexit 0\n' >"$1/DEBIAN/postinst"; chmod 0755 "$1/DEBIAN/postinst"; }
make_authority_world_writable() { chmod 0777 "$1/usr/lib/scribe"; }
make_pack_root_group_writable() { chmod 0775 "$1/usr/lib/scribe/workers/packs"; }
replace_desktop_with_text() { printf 'not an ELF\n' >"$1/usr/bin/local-transcriber"; chmod 0755 "$1/usr/bin/local-transcriber"; }
replace_worker_with_wrong_arch() { cp "$test_root/wrong-arch" "$1/usr/lib/scribe/scribe-inference-worker"; chmod 0755 "$1/usr/lib/scribe/scribe-inference-worker"; }
attack_package tampered-worker tamper_worker
attack_package unexpected-file add_unexpected
attack_package unexpected-directory add_unexpected_directory
attack_package nonempty-catalog replace_catalog
attack_package wrong-contract replace_contract
attack_package worker-symlink replace_worker_with_link
attack_package maintainer-script add_maintainer_script
attack_package world-writable-authority make_authority_world_writable
attack_package group-writable-pack-root make_pack_root_group_writable
attack_package non-elf-desktop replace_desktop_with_text
attack_package wrong-arch-worker replace_worker_with_wrong_arch

if command -v cargo >/dev/null; then
  export SCRIBE_BUILD_REVISION="$revision"
  cargo build --locked --release --manifest-path "$repo_root/tools/worker-pack-author/Cargo.toml" >/dev/null
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
