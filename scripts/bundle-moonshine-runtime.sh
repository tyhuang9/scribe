#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROFILE="${SCRIBE_PROFILE:-debug}"

export SCRIBE_SHERPA_FAMILY_RUNTIME_ID="moonshine"
export SCRIBE_SHERPA_FAMILY_BACKEND="Moonshine"
export SCRIBE_SHERPA_FAMILY_WRAPPER="scribe-moonshine"
export SCRIBE_SHERPA_FAMILY_RUNTIME_DEST="${SCRIBE_MOONSHINE_RUNTIME_DEST:-$SCRIBE_DIR/target/$PROFILE/runtimes/moonshine}"

exec "$SCRIPT_DIR/bundle-sherpa-onnx-runtime.sh" "$@"
