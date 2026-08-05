#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

cargo build --release --all-features --manifest-path "$SCRIBE_DIR/Cargo.toml"

# The normalized release ships one logical handler. Legacy compatibility
# packaging scripts remain available for existing unmanaged artifacts, but
# they are deliberately not invoked by the release bundle.
SCRIBE_PROFILE=release "$SCRIPT_DIR/bundle-whisper-runtime.sh"

cat <<EOF
Release bundle ready:
  executable:          $SCRIBE_DIR/target/release/local-transcriber
  primary runtime:     $SCRIBE_DIR/target/release/runtimes/whisper_cpp
  logical handlers:    1
  compatibility packs: not bundled
EOF

case "${SCRIBE_BUNDLE_CUDA:-0}" in
  1|true|TRUE|yes|YES|on|ON)
    echo "CUDA runtime libraries were included in the primary package."
    ;;
  *)
    cat <<'EOF'

For a GPU-capable primary package, rerun with:
  SCRIBE_BUNDLE_CUDA=1 scripts/build-release-bundle.sh
EOF
    ;;
esac
