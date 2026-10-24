#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ "$(uname -s)" == Darwin ]] || { echo 'native Metal metadata helper tests require macOS.' >&2; exit 1; }
[[ "$#" == 0 ]] || { echo 'Usage: test-macos-metal-metadata-helper.sh' >&2; exit 2; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# No shell EXIT trap may delete a fixture whose worker retirement is unknown.
exec python3 -B "$repo_root/build_support/native_macos_worker_contract.py" metal
