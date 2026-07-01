#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROFILE="${SCRIBE_PROFILE:-debug}"

export SCRIBE_SHERPA_FAMILY_RUNTIME_ID="parakeet"
export SCRIBE_SHERPA_FAMILY_BACKEND="Parakeet"
export SCRIBE_SHERPA_FAMILY_WRAPPER="scribe-parakeet"
export SCRIBE_SHERPA_FAMILY_RUNTIME_DEST="${SCRIBE_PARAKEET_RUNTIME_DEST:-$SCRIBE_DIR/target/$PROFILE/runtimes/parakeet}"

exec "$SCRIPT_DIR/bundle-sherpa-onnx-runtime.sh" "$@"
