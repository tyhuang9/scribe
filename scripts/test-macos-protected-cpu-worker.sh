#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

[[ "$(uname -s)" == Darwin ]] || { echo 'protected CPU contract requires native macOS.' >&2; exit 1; }
[[ "$#" == 2 && "$1" == --app ]] || { echo 'Usage: test-macos-protected-cpu-worker.sh --app <verified Scribe.app>' >&2; exit 2; }
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# Python owns the complete fixture lifetime, including uncertain worker exit.
exec python3 -B "$repo_root/build_support/native_macos_worker_contract.py" cpu --app "$2"
