#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WHISPER_DIR="${WHISPER_DIR:-$SCRIBE_DIR/../whisper.cpp}"
WHISPER_BUILD_DIR_INPUT="${WHISPER_BUILD_DIR:-${BUILD_DIR:-}}"
WHISPER_BUILD_DIR="${WHISPER_BUILD_DIR_INPUT:-$WHISPER_DIR/build-dl-ollama}"
PROFILE="${SCRIBE_PROFILE:-debug}"
DEST="${SCRIBE_RUNTIME_DEST:-$SCRIBE_DIR/target/$PROFILE/runtimes/whisper_cpp}"
INCLUDE_CUDA="${SCRIBE_BUNDLE_CUDA:-0}"
BUILD_RUNTIME="${SCRIBE_BUILD_WHISPER_RUNTIME:-auto}"
OLLAMA_LIB_DIR="${OLLAMA_LIB_DIR:-/usr/local/lib/ollama}"
case "$(uname -s)" in
  Linux) PLATFORM_OS=linux ;;
  Darwin) PLATFORM_OS=macos ;;
  *) echo "Unsupported release OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) PLATFORM_ARCH=x86_64 ;;
  arm64|aarch64) PLATFORM_ARCH=aarch64 ;;
  *) echo "Unsupported release architecture: $(uname -m)" >&2; exit 1 ;;
esac
PLATFORM="$PLATFORM_OS-$PLATFORM_ARCH"
SOURCE_VERSION="${WHISPER_SOURCE_VERSION:-}"
SOURCE_COMMIT="${WHISPER_SOURCE_COMMIT:-}"

if [[ "$PROFILE" == "release" ]]; then
  if [[ -z "$WHISPER_BUILD_DIR_INPUT" || -z "$SOURCE_VERSION" || -z "$SOURCE_COMMIT" ]]; then
    echo "Release bundling requires WHISPER_BUILD_DIR, WHISPER_SOURCE_VERSION, and WHISPER_SOURCE_COMMIT from a pinned CI build." >&2
    exit 1
  fi
  if [[ ! "$SOURCE_COMMIT" =~ ^[0-9a-f]{40,64}$ ]]; then
    echo "WHISPER_SOURCE_COMMIT must be a lowercase 40-64 character commit digest." >&2
    exit 1
  fi
  if [[ ! "$SOURCE_VERSION" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$ ]]; then
    echo "WHISPER_SOURCE_VERSION is not a safe immutable version identifier." >&2
    exit 1
  fi
  BUILD_RUNTIME=0
fi

usage() {
  cat <<'USAGE'
Bundle the whisper.cpp runtime next to the Scribe executable.

Environment:
  SCRIBE_PROFILE=debug|release          target profile directory, default debug
  SCRIBE_RUNTIME_DEST=/path             override destination runtime directory
  WHISPER_DIR=/path/to/whisper.cpp      adjacent whisper.cpp checkout
  WHISPER_BUILD_DIR=/path/to/build      whisper.cpp build directory
  SCRIBE_BUILD_WHISPER_RUNTIME=auto|0   build missing runtime with existing script
  SCRIBE_BUNDLE_CUDA=1                  also copy libggml-cuda and CUDA deps
  CUDA_RUNTIME_DIR=/path                directory containing CUDA runtime libs
  OLLAMA_LIB_DIR=/path                  default /usr/local/lib/ollama

Default output:
  target/$SCRIBE_PROFILE/runtimes/whisper_cpp/
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

truthy() {
  case "${1,,}" in
    1|true|yes|y|on) return 0 ;;
    *) return 1 ;;
  esac
}

ensure_whisper_runtime() {
  local cli="$WHISPER_BUILD_DIR/bin/whisper-cli"
  if [[ -x "$cli" ]]; then
    return
  fi
  case "${BUILD_RUNTIME,,}" in
    0|false|no|off)
      echo "whisper-cli not found at $cli" >&2
      echo "Run scripts/build-whisper-ollama-cuda-backend.sh or set WHISPER_BUILD_DIR." >&2
      exit 1
      ;;
    auto|1|true|yes|on)
      echo "whisper-cli not found; building whisper.cpp runtime into $WHISPER_BUILD_DIR"
      BUILD_DIR="$WHISPER_BUILD_DIR" "$SCRIPT_DIR/build-whisper-ollama-cuda-backend.sh"
      ;;
    *)
      echo "Unsupported SCRIBE_BUILD_WHISPER_RUNTIME=$BUILD_RUNTIME" >&2
      exit 1
      ;;
  esac
}

copy_glob() {
  local required="$1"
  local pattern="$2"
  local destination="$3"
  local matches=()
  shopt -s nullglob
  matches=($pattern)
  shopt -u nullglob
  if [[ "${#matches[@]}" -eq 0 ]]; then
    if [[ "$required" == "required" ]]; then
      echo "No files matched required pattern: $pattern" >&2
      exit 1
    fi
    return
  fi
  cp -a "${matches[@]}" "$destination/"
}

clean_glob() {
  local pattern="$1"
  local matches=()
  shopt -s nullglob
  matches=($pattern)
  shopt -u nullglob
  if [[ "${#matches[@]}" -gt 0 ]]; then
    rm -f -- "${matches[@]}"
  fi
}

select_cuda_runtime_dir() {
  if [[ -n "${CUDA_RUNTIME_DIR:-}" ]]; then
    if [[ -f "$CUDA_RUNTIME_DIR/libggml-cuda.so" ]]; then
      echo "$CUDA_RUNTIME_DIR"
      return
    fi
    echo "CUDA_RUNTIME_DIR does not contain libggml-cuda.so: $CUDA_RUNTIME_DIR" >&2
    exit 1
  fi

  for candidate in "$OLLAMA_LIB_DIR/cuda_v12" "$OLLAMA_LIB_DIR/cuda_v13"; do
    if [[ -f "$candidate/libggml-cuda.so" ]]; then
      echo "$candidate"
      return
    fi
  done

  echo "No CUDA runtime directory found. Set CUDA_RUNTIME_DIR or OLLAMA_LIB_DIR." >&2
  exit 1
}

ensure_whisper_runtime

SOURCE_BIN="$WHISPER_BUILD_DIR/bin"
WHISPER_CLI="$SOURCE_BIN/whisper-cli"
BIN_DEST="$DEST/bin"
CUDA_DEST="$DEST/cuda"

mkdir -p "$BIN_DEST"
mkdir -p "$CUDA_DEST"

clean_glob "$BIN_DEST/whisper-cli"
clean_glob "$BIN_DEST/libwhisper.so*"
clean_glob "$BIN_DEST/libggml.so*"
clean_glob "$BIN_DEST/libggml-base.so*"
clean_glob "$BIN_DEST/libggml-cpu-*.so"
clean_glob "$CUDA_DEST/libggml-cuda.so"
clean_glob "$CUDA_DEST/libcudart.so*"
clean_glob "$CUDA_DEST/libcublas.so*"
clean_glob "$CUDA_DEST/libcublasLt.so*"
rm -f -- "$DEST/runtime-manifest.json"

copy_glob required "$WHISPER_CLI" "$BIN_DEST"
copy_glob required "$SOURCE_BIN/libwhisper.so*" "$BIN_DEST"
copy_glob required "$SOURCE_BIN/libggml.so*" "$BIN_DEST"
copy_glob required "$SOURCE_BIN/libggml-base.so*" "$BIN_DEST"
copy_glob optional "$SOURCE_BIN/libggml-cpu-*.so" "$BIN_DEST"

cuda_bundled=false
cuda_source=""
if truthy "$INCLUDE_CUDA"; then
  cuda_source="$(select_cuda_runtime_dir)"
  mkdir -p "$CUDA_DEST"
  copy_glob required "$cuda_source/libggml-cuda.so" "$CUDA_DEST"
  copy_glob required "$cuda_source/libcudart.so*" "$CUDA_DEST"
  copy_glob required "$cuda_source/libcublas.so*" "$CUDA_DEST"
  copy_glob required "$cuda_source/libcublasLt.so*" "$CUDA_DEST"
  cuda_bundled=true
fi

device=cpu
if [[ "$cuda_bundled" == "true" ]]; then
  device=gpu
fi
python3 - "$DEST/runtime-manifest.json" "$SOURCE_VERSION" "$SOURCE_COMMIT" "$PLATFORM" "$device" <<'PY'
import json, pathlib, sys
path, version, commit, platform, device = sys.argv[1:]
manifest = {
    "manifest_version": 1,
    "runtime_id": "whisper_cpp",
    "backend": "whisper.cpp",
    "version": version,
    "source_commit": commit,
    "whisper_cli": "bin/whisper-cli",
    "entrypoint": "bin/whisper-cli",
    "platform": platform,
    "device": device,
    "cuda_bundled": device == "gpu",
    "portable": True,
}
pathlib.Path(path).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
PY

echo "Bundled whisper.cpp runtime: $DEST"
if [[ "$cuda_bundled" == "true" ]]; then
  echo "Bundled CUDA runtime libraries from: $cuda_source"
else
  echo "CUDA libraries not bundled. Set SCRIBE_BUNDLE_CUDA=1 to include them."
fi
