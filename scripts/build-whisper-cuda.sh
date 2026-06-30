#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WHISPER_DIR="${WHISPER_DIR:-$(cd "$SCRIBE_DIR/../whisper.cpp" && pwd)}"
BUILD_DIR="${BUILD_DIR:-$WHISPER_DIR/build-cuda}"
CUDA_ARGS=()
BUILD_TYPE="${CMAKE_BUILD_TYPE:-Release}"

if ! command -v nvcc >/dev/null 2>&1; then
  echo "nvcc not found. Install the CUDA Toolkit first, then rerun this script." >&2
  exit 1
fi

if command -v g++-12 >/dev/null 2>&1; then
  CUDA_ARGS+=("-DCMAKE_CUDA_HOST_COMPILER=$(command -v g++-12)")
fi

cmake \
  -S "$WHISPER_DIR" \
  -B "$BUILD_DIR" \
  -U "CUDAToolkit_*" \
  -U "CMAKE_CUDA_*" \
  -DGGML_CUDA=1 \
  -DCMAKE_BUILD_TYPE="$BUILD_TYPE" \
  "${CUDA_ARGS[@]}" \
  "$@"
cmake --build "$BUILD_DIR" -j --config Release

echo "CUDA whisper.cpp binary: $BUILD_DIR/bin/whisper-cli"
