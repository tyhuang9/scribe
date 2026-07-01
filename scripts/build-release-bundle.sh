#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

cargo build --release --manifest-path "$SCRIBE_DIR/Cargo.toml"

SCRIBE_PROFILE=release "$SCRIPT_DIR/bundle-whisper-runtime.sh"
case "${SCRIBE_SKIP_FASTER_WHISPER:-0}" in
  1|true|TRUE|yes|YES|on|ON)
    faster_whisper_runtime="skipped"
    ;;
  *)
    SCRIBE_PROFILE=release "$SCRIPT_DIR/bundle-faster-whisper-runtime.sh"
    faster_whisper_runtime="$SCRIBE_DIR/target/release/runtimes/faster_whisper"
    ;;
esac
case "${SCRIBE_SKIP_VOSK:-0}" in
  1|true|TRUE|yes|YES|on|ON)
    vosk_runtime="skipped"
    ;;
  *)
    SCRIBE_PROFILE=release "$SCRIPT_DIR/bundle-vosk-runtime.sh"
    vosk_runtime="$SCRIBE_DIR/target/release/runtimes/vosk"
    ;;
esac

cat <<EOF
Release bundle ready:
  executable:             $SCRIBE_DIR/target/release/local-transcriber
  whisper.cpp runtime:    $SCRIBE_DIR/target/release/runtimes/whisper_cpp
  faster-whisper runtime: $faster_whisper_runtime
  Vosk runtime:           $vosk_runtime
EOF

case "${SCRIBE_BUNDLE_CUDA:-0}" in
  1|true|TRUE|yes|YES|on|ON)
    echo "CUDA runtime libraries were included in this bundle."
    ;;
  *)
    cat <<'EOF'

For a GPU-capable bundle, rerun with:
  SCRIBE_BUNDLE_CUDA=1 scripts/build-release-bundle.sh
EOF
    ;;
esac

case "${SCRIBE_BUNDLE_FAST_WHISPER_CUDA:-0}" in
  1|true|TRUE|yes|YES|on|ON)
    echo "faster-whisper CUDA/cuDNN Python runtime libraries were included."
    ;;
  *)
    cat <<'EOF'

For a GPU-capable faster-whisper Python bundle, rerun with:
  SCRIBE_BUNDLE_FAST_WHISPER_CUDA=1 scripts/build-release-bundle.sh
EOF
    ;;
esac
