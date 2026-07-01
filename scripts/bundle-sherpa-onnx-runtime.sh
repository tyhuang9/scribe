#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROFILE="${SCRIBE_PROFILE:-debug}"
RUNTIME_ID="${SCRIBE_SHERPA_FAMILY_RUNTIME_ID:-sherpa_onnx}"
BACKEND="${SCRIBE_SHERPA_FAMILY_BACKEND:-sherpa-onnx}"
WRAPPER_NAME="${SCRIBE_SHERPA_FAMILY_WRAPPER:-scribe-sherpa-onnx}"
DEST="${SCRIBE_SHERPA_FAMILY_RUNTIME_DEST:-${SCRIBE_SHERPA_ONNX_RUNTIME_DEST:-$SCRIBE_DIR/target/$PROFILE/runtimes/$RUNTIME_ID}}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
VENV_DIR="$DEST/venv"
VENV_PYTHON="$VENV_DIR/bin/python"
RUNNER_SRC="$SCRIPT_DIR/sherpa_onnx_runner.py"
RUNNER_DST="$DEST/bin/sherpa_onnx_runner.py"
WRAPPER="$DEST/bin/$WRAPPER_NAME"

usage() {
  cat <<EOF
Bundle a sherpa-onnx family Python runtime next to the Scribe executable.

Environment:
  SCRIBE_PROFILE=debug|release                  target profile; default: debug
  SCRIBE_SHERPA_FAMILY_RUNTIME_ID=name          runtime id; default: sherpa_onnx
  SCRIBE_SHERPA_FAMILY_BACKEND=name             backend label; default: sherpa-onnx
  SCRIBE_SHERPA_FAMILY_WRAPPER=name             wrapper name; default: scribe-sherpa-onnx
  SCRIBE_SHERPA_FAMILY_RUNTIME_DEST=/path       destination runtime directory
  SCRIBE_SHERPA_ONNX_RUNTIME_DEST=/path         sherpa-onnx destination alias
  PYTHON_BIN=/path/to/python3                   Python used to create the venv
  SCRIBE_REBUILD_SHERPA_ONNX_RUNTIME=1          recreate the venv before install
  SCRIBE_SHERPA_ONNX_VERSION=1.13.3             optional pinned PyPI version
  SCRIBE_NUMPY_VERSION=2.3.2                    optional pinned NumPy version

Output:
  $DEST/bin/$WRAPPER_NAME
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

if [[ ! -f "$RUNNER_SRC" ]]; then
  echo "Missing runner script: $RUNNER_SRC" >&2
  exit 1
fi

if [[ "${SCRIBE_REBUILD_SHERPA_ONNX_RUNTIME:-0}" =~ ^(1|true|TRUE|yes|YES|on|ON)$ ]]; then
  rm -rf "$VENV_DIR"
elif [[ -d "$VENV_DIR" && ! -x "$VENV_PYTHON" ]]; then
  echo "$BACKEND venv is incomplete; recreating $VENV_DIR" >&2
  rm -rf "$VENV_DIR"
fi

mkdir -p "$DEST/bin"

if [[ ! -x "$VENV_PYTHON" ]]; then
  "$PYTHON_BIN" -m venv "$VENV_DIR"
fi

"$VENV_PYTHON" -m pip install --upgrade pip setuptools wheel

if [[ -n "${SCRIBE_SHERPA_ONNX_VERSION:-}" ]]; then
  "$VENV_PYTHON" -m pip install --upgrade \
    "sherpa-onnx==$SCRIBE_SHERPA_ONNX_VERSION" \
    "sherpa-onnx-bin==$SCRIBE_SHERPA_ONNX_VERSION"
elif ! "$VENV_PYTHON" -c "import sherpa_onnx" >/dev/null 2>&1; then
  "$VENV_PYTHON" -m pip install --upgrade sherpa-onnx sherpa-onnx-bin
fi

if [[ -n "${SCRIBE_NUMPY_VERSION:-}" ]]; then
  "$VENV_PYTHON" -m pip install --upgrade "numpy==$SCRIBE_NUMPY_VERSION"
elif ! "$VENV_PYTHON" -c "import numpy" >/dev/null 2>&1; then
  "$VENV_PYTHON" -m pip install --upgrade numpy
fi

cp "$RUNNER_SRC" "$RUNNER_DST"
chmod 755 "$RUNNER_DST"

cat > "$WRAPPER" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PYTHON="$RUNTIME_DIR/venv/bin/python"
RUNNER="$SCRIPT_DIR/sherpa_onnx_runner.py"

if [[ ! -x "$PYTHON" ]]; then
  echo "sherpa-onnx runtime Python is missing: $PYTHON" >&2
  exit 127
fi

exec "$PYTHON" "$RUNNER" "$@"
EOF
chmod 755 "$WRAPPER"

versions="$("$VENV_PYTHON" - <<'PY'
import json
try:
    from importlib.metadata import PackageNotFoundError, version
except ImportError:
    from importlib_metadata import PackageNotFoundError, version

payload = {}
for package in ("sherpa-onnx", "sherpa-onnx-bin", "sherpa-onnx-core", "numpy"):
    try:
        payload[package.replace("-", "_")] = version(package)
    except PackageNotFoundError:
        payload[package.replace("-", "_")] = None
print(json.dumps(payload))
PY
)"

cat > "$DEST/runtime-manifest.json" <<EOF
{
  "runtime_id": "$RUNTIME_ID",
  "backend": "$BACKEND",
  "runner": "bin/$WRAPPER_NAME",
  "runner_revision": 2,
  "python": "venv/bin/python",
  "versions": $versions,
  "model_sources": {
    "sherpa-onnx": "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-small-en-2023-06-26.tar.bz2",
    "Moonshine": "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2",
    "Parakeet": "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2"
  }
}
EOF

echo "Bundled $BACKEND runtime: $DEST"
