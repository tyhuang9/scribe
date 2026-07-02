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
DEST="${SCRIBE_FAST_WHISPER_RUNTIME_DEST:-$SCRIBE_DIR/target/$PROFILE/runtimes/faster_whisper}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
VENV_DIR="$DEST/venv"
VENV_PYTHON="$VENV_DIR/bin/python"
RUNNER_SRC="$SCRIPT_DIR/faster_whisper_runner.py"
RUNNER_DST="$DEST/bin/faster_whisper_runner.py"
WRAPPER="$DEST/bin/scribe-faster-whisper"
PIP_VERSION="${SCRIBE_PIP_VERSION:-${SCRIBE_PIP_VERSION_DEFAULT:-26.1.2}}"
SETUPTOOLS_VERSION="${SCRIBE_SETUPTOOLS_VERSION:-${SCRIBE_SETUPTOOLS_VERSION_DEFAULT:-82.0.1}}"
WHEEL_VERSION="${SCRIBE_WHEEL_VERSION:-${SCRIBE_WHEEL_VERSION_DEFAULT:-0.47.0}}"
FASTER_WHISPER_VERSION="${SCRIBE_FASTER_WHISPER_VERSION:-${SCRIBE_FASTER_WHISPER_VERSION_DEFAULT:-1.2.1}}"
NVIDIA_CUBLAS_CU12_VERSION="${SCRIBE_NVIDIA_CUBLAS_CU12_VERSION:-${SCRIBE_NVIDIA_CUBLAS_CU12_VERSION_DEFAULT:-12.9.2.10}}"
NVIDIA_CUDNN_CU12_VERSION="${SCRIBE_NVIDIA_CUDNN_CU12_VERSION:-${SCRIBE_NVIDIA_CUDNN_CU12_VERSION_DEFAULT:-9.24.0.43}}"
PLATFORM="$(uname -s)-$(uname -m)"

usage() {
  cat <<EOF
Bundle the faster-whisper Python runtime next to the Scribe executable.

Environment:
  SCRIBE_PROFILE=debug|release                 target profile; default: debug
  SCRIBE_FAST_WHISPER_RUNTIME_DEST=/path       destination runtime directory
  PYTHON_BIN=/path/to/python3                  Python used to create the venv
  SCRIBE_REBUILD_FAST_WHISPER_RUNTIME=1        recreate the venv before install
  SCRIBE_FASTER_WHISPER_VERSION=$FASTER_WHISPER_VERSION
  SCRIBE_BUNDLE_FAST_WHISPER_CUDA=1            include pip CUDA/cuDNN runtime libs
  SCRIBE_NVIDIA_CUBLAS_CU12_VERSION=$NVIDIA_CUBLAS_CU12_VERSION
  SCRIBE_NVIDIA_CUDNN_CU12_VERSION=$NVIDIA_CUDNN_CU12_VERSION

Output:
  $DEST/bin/scribe-faster-whisper
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

if [[ "${SCRIBE_REBUILD_FAST_WHISPER_RUNTIME:-0}" =~ ^(1|true|TRUE|yes|YES|on|ON)$ ]]; then
  rm -rf "$VENV_DIR"
elif [[ -d "$VENV_DIR" && ! -x "$VENV_PYTHON" ]]; then
  echo "faster-whisper venv is incomplete; recreating $VENV_DIR" >&2
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

"$VENV_PYTHON" -m pip install --upgrade "faster-whisper==$FASTER_WHISPER_VERSION"

cuda_bundled=false
case "${SCRIBE_BUNDLE_FAST_WHISPER_CUDA:-0}" in
  1|true|TRUE|yes|YES|on|ON)
    "$VENV_PYTHON" -m pip install --upgrade \
      "nvidia-cublas-cu12==$NVIDIA_CUBLAS_CU12_VERSION" \
      "nvidia-cudnn-cu12==$NVIDIA_CUDNN_CU12_VERSION"
    cuda_bundled=true
    ;;
esac

cp "$RUNNER_SRC" "$RUNNER_DST"
chmod 755 "$RUNNER_DST"

cat > "$WRAPPER" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PYTHON="$RUNTIME_DIR/venv/bin/python"
RUNNER="$SCRIPT_DIR/faster_whisper_runner.py"

if [[ ! -x "$PYTHON" ]]; then
  echo "faster-whisper runtime Python is missing: $PYTHON" >&2
  exit 127
fi

USE_NVIDIA_LIBRARY_PATH=1
if [[ "${1:-}" == "transcribe" ]]; then
  previous=""
  for arg in "$@"; do
    if [[ "$previous" == "--device-mode" && "$arg" == "cpu" ]]; then
      USE_NVIDIA_LIBRARY_PATH=0
      break
    fi
    previous="$arg"
  done
fi

if [[ "$USE_NVIDIA_LIBRARY_PATH" == "1" ]]; then
  NVIDIA_LIBRARY_PATH="$("$PYTHON" "$RUNNER" nvidia-library-path 2>/dev/null || true)"
  if [[ -n "$NVIDIA_LIBRARY_PATH" ]]; then
    export LD_LIBRARY_PATH="$NVIDIA_LIBRARY_PATH${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  fi
fi

exec "$PYTHON" "$RUNNER" "$@"
EOF
chmod 755 "$WRAPPER"

cat > "$DEST/runtime-manifest.json" <<EOF
{
  "manifest_version": 1,
  "runtime_id": "faster_whisper",
  "backend": "faster-whisper",
  "version": "$FASTER_WHISPER_VERSION",
  "runner": "bin/scribe-faster-whisper",
  "python": "venv/bin/python",
  "platform": "$PLATFORM",
  "cuda_bundled": $cuda_bundled,
  "dependencies": {
    "pip": "$PIP_VERSION",
    "setuptools": "$SETUPTOOLS_VERSION",
    "wheel": "$WHEEL_VERSION",
    "faster-whisper": "$FASTER_WHISPER_VERSION",
    "nvidia-cublas-cu12": "$NVIDIA_CUBLAS_CU12_VERSION",
    "nvidia-cudnn-cu12": "$NVIDIA_CUDNN_CU12_VERSION"
  }
}
EOF

echo "Bundled faster-whisper runtime: $DEST"
if [[ "$cuda_bundled" == "true" ]]; then
  echo "CUDA/cuDNN Python runtime libraries were included."
else
  echo "CUDA/cuDNN Python runtime libraries not bundled. Set SCRIBE_BUNDLE_FAST_WHISPER_CUDA=1 to include them."
fi
