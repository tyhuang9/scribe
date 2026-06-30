#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WHISPER_DIR="${WHISPER_DIR:-$(cd "$SCRIBE_DIR/../whisper.cpp" && pwd)}"
BUILD_DIR="${BUILD_DIR:-$WHISPER_DIR/build-dl-ollama}"
OLLAMA_LIB_DIR="${OLLAMA_LIB_DIR:-/usr/local/lib/ollama}"
BUILD_TYPE="${CMAKE_BUILD_TYPE:-Release}"

if [[ ! -f "$OLLAMA_LIB_DIR/cuda_v13/libggml-cuda.so" && ! -f "$OLLAMA_LIB_DIR/cuda_v12/libggml-cuda.so" ]]; then
  echo "No Ollama CUDA backend found under $OLLAMA_LIB_DIR." >&2
  exit 1
fi

cmake \
  -S "$WHISPER_DIR" \
  -B "$BUILD_DIR" \
  -DCMAKE_BUILD_TYPE="$BUILD_TYPE" \
  -DBUILD_SHARED_LIBS=ON \
  -DGGML_BACKEND_DL=ON \
  -DGGML_NATIVE=OFF \
  -DGGML_CPU_ALL_VARIANTS=ON \
  -DGGML_BACKEND_DIR="$OLLAMA_LIB_DIR" \
  "$@"
cmake --build "$BUILD_DIR" --target whisper-cli -j2 --config Release

echo "Dynamic-backend whisper.cpp binary: $BUILD_DIR/bin/whisper-cli"
echo "Use CUDA backend: $OLLAMA_LIB_DIR/cuda_v13/libggml-cuda.so or $OLLAMA_LIB_DIR/cuda_v12/libggml-cuda.so"
echo "Use CUDA library dirs: $OLLAMA_LIB_DIR:$OLLAMA_LIB_DIR/cuda_v13 or $OLLAMA_LIB_DIR:$OLLAMA_LIB_DIR/cuda_v12"
