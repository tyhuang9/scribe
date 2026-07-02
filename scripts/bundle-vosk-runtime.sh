#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEPS_ENV="$SCRIPT_DIR/runtime-dependencies.env"
if [[ -f "$DEPS_ENV" ]]; then
  # shellcheck source=runtime-dependencies.env
  source "$DEPS_ENV"
fi
PROFILE="${SCRIBE_PROFILE:-debug}"
DEST="${SCRIBE_VOSK_RUNTIME_DEST:-$SCRIBE_DIR/target/$PROFILE/runtimes/vosk}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
VENV_DIR="$DEST/venv"
VENV_PYTHON="$VENV_DIR/bin/python"
RUNNER_SRC="$SCRIPT_DIR/vosk_runner.py"
RUNNER_DST="$DEST/bin/vosk_runner.py"
WRAPPER="$DEST/bin/scribe-vosk"
PIP_VERSION="${SCRIBE_PIP_VERSION:-${SCRIBE_PIP_VERSION_DEFAULT:-26.1.2}}"
SETUPTOOLS_VERSION="${SCRIBE_SETUPTOOLS_VERSION:-${SCRIBE_SETUPTOOLS_VERSION_DEFAULT:-82.0.1}}"
WHEEL_VERSION="${SCRIBE_WHEEL_VERSION:-${SCRIBE_WHEEL_VERSION_DEFAULT:-0.47.0}}"
VOSK_VERSION="${SCRIBE_VOSK_VERSION:-${SCRIBE_VOSK_VERSION_DEFAULT:-0.3.45}}"
PLATFORM="$(uname -s)-$(uname -m)"

usage() {
  cat <<EOF
Bundle the Vosk Python runtime next to the Scribe executable.

Environment:
  SCRIBE_PROFILE=debug|release             target profile; default: debug
  SCRIBE_VOSK_RUNTIME_DEST=/path           destination runtime directory
  PYTHON_BIN=/path/to/python3              Python used to create the venv
  SCRIBE_REBUILD_VOSK_RUNTIME=1            recreate the venv before install
  SCRIBE_VOSK_VERSION=$VOSK_VERSION               pinned PyPI vosk version

Output:
  $DEST/bin/scribe-vosk
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

if [[ "${SCRIBE_REBUILD_VOSK_RUNTIME:-0}" =~ ^(1|true|TRUE|yes|YES|on|ON)$ ]]; then
  rm -rf "$VENV_DIR"
elif [[ -d "$VENV_DIR" && ! -x "$VENV_PYTHON" ]]; then
  echo "Vosk venv is incomplete; recreating $VENV_DIR" >&2
  rm -rf "$VENV_DIR"
fi

mkdir -p "$DEST/bin"

if [[ ! -x "$VENV_PYTHON" ]]; then
  "$PYTHON_BIN" -m venv "$VENV_DIR"
fi

"$VENV_PYTHON" -m pip install --upgrade \
  "pip==$PIP_VERSION" \
  "setuptools==$SETUPTOOLS_VERSION" \
  "wheel==$WHEEL_VERSION"

installed_vosk_version="$("$VENV_PYTHON" - <<'PY'
try:
    from importlib.metadata import PackageNotFoundError, version
except ImportError:
    from importlib_metadata import PackageNotFoundError, version

try:
    print(version("vosk"))
except PackageNotFoundError:
    pass
PY
)"

if [[ "$installed_vosk_version" != "$VOSK_VERSION" ]]; then
  "$VENV_PYTHON" -m pip install --upgrade "vosk==$VOSK_VERSION"
fi

cp "$RUNNER_SRC" "$RUNNER_DST"
chmod 755 "$RUNNER_DST"

cat > "$WRAPPER" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PYTHON="$RUNTIME_DIR/venv/bin/python"
RUNNER="$SCRIPT_DIR/vosk_runner.py"

if [[ ! -x "$PYTHON" ]]; then
  echo "Vosk runtime Python is missing: $PYTHON" >&2
  exit 127
fi

exec "$PYTHON" "$RUNNER" "$@"
EOF
chmod 755 "$WRAPPER"

cat > "$DEST/runtime-manifest.json" <<EOF
{
  "manifest_version": 1,
  "runtime_id": "vosk",
  "backend": "Vosk",
  "version": "$VOSK_VERSION",
  "runner": "bin/scribe-vosk",
  "runner_revision": 3,
  "python": "venv/bin/python",
  "platform": "$PLATFORM",
  "dependencies": {
    "pip": "$PIP_VERSION",
    "setuptools": "$SETUPTOOLS_VERSION",
    "wheel": "$WHEEL_VERSION",
    "vosk": "$VOSK_VERSION"
  },
  "model_source": "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip"
}
EOF

echo "Bundled Vosk runtime: $DEST"
