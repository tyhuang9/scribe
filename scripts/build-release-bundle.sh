#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIBE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
MODE="standard"

if [[ "${1:-}" == "--mode" ]]; then
  MODE="${2:-}"
elif [[ -n "${1:-}" ]]; then
  echo "Usage: $0 [--mode standard|offline-cpu|gpu]" >&2
  exit 2
fi

case "$MODE" in
  standard|offline-cpu|gpu) ;;
  *) echo "Unsupported release mode: $MODE" >&2; exit 2 ;;
esac

if [[ -z "${SCRIBE_RUNTIME_ARTIFACT_CATALOG:-}" ]]; then
  if [[ "${SCRIBE_ALLOW_EMPTY_RUNTIME_CATALOG:-0}" != "1" ]]; then
    echo "Set SCRIBE_RUNTIME_ARTIFACT_CATALOG to a release-generated catalog before building, or explicitly set SCRIBE_ALLOW_EMPTY_RUNTIME_CATALOG=1 for a CPU-only release." >&2
    exit 1
  fi
else
  catalog_dir="$(cd "$(dirname "$SCRIBE_RUNTIME_ARTIFACT_CATALOG")" && pwd)"
  SCRIBE_RUNTIME_ARTIFACT_CATALOG="$catalog_dir/$(basename "$SCRIBE_RUNTIME_ARTIFACT_CATALOG")"
  export SCRIBE_RUNTIME_ARTIFACT_CATALOG
  python3 - "$SCRIBE_RUNTIME_ARTIFACT_CATALOG" <<'PY'
import json, pathlib, sys
path = pathlib.Path(sys.argv[1])
catalog = json.loads(path.read_text(encoding="utf-8"))
if catalog.get("schema_version") not in (1, 2) or not catalog.get("catalog_version") or not catalog.get("artifacts"):
    raise SystemExit("Release runtime catalog must use schema 1 or 2 and contain at least one real artifact")
PY
fi

if [[ "${SCRIBE_BUILD_VOICE_AI:-0}" == "1" ]]; then
  if [[ -z "${SCRIBE_RUNTIME_ARTIFACT_CATALOG:-}" ]]; then
    echo "Voice-AI releases require a schema-2 catalog with a pinned llama runtime and both mirrored Qwen tiers." >&2
    exit 1
  fi
  export SCRIBE_REQUIRE_VOICE_INTENT_ARTIFACTS=1
else
  unset SCRIBE_REQUIRE_VOICE_INTENT_ARTIFACTS || true
fi

if [[ -z "${WHISPER_BUILD_DIR:-}" || -z "${WHISPER_SOURCE_VERSION:-}" || ! "${WHISPER_SOURCE_COMMIT:-}" =~ ^[0-9a-f]{40,64}$ ]]; then
  echo "Release builds require WHISPER_BUILD_DIR, WHISPER_SOURCE_VERSION, and a lowercase full WHISPER_SOURCE_COMMIT." >&2
  exit 1
fi
if [[ ! "$WHISPER_SOURCE_VERSION" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]{0,127}$ ]]; then
  echo "WHISPER_SOURCE_VERSION is not a safe immutable version identifier." >&2
  exit 1
fi
if [[ ! -x "$WHISPER_BUILD_DIR/bin/whisper-cli" ]]; then
  echo "Pinned whisper build does not contain executable bin/whisper-cli: $WHISPER_BUILD_DIR" >&2
  exit 1
fi
if [[ "$MODE" == "offline-cpu" ]]; then
  for variable in \
    SCRIBE_PORTABLE_FASTER_WHISPER_CPU_RUNTIME \
    SCRIBE_PORTABLE_VOSK_CPU_RUNTIME \
    SCRIBE_PORTABLE_SHERPA_ONNX_CPU_RUNTIME \
    SCRIBE_PORTABLE_MOONSHINE_CPU_RUNTIME \
    SCRIBE_PORTABLE_PARAKEET_CPU_RUNTIME
  do
    if [[ -z "${!variable:-}" ]]; then
      echo "Offline CPU releases require $variable from platform CI." >&2
      exit 1
    fi
  done
fi

cargo build --release --manifest-path "$SCRIBE_DIR/Cargo.toml"

if [[ "$MODE" != "offline-cpu" ]]; then
  for runtime_id in faster_whisper vosk sherpa_onnx moonshine parakeet; do
    rm -rf -- "$SCRIBE_DIR/target/release/runtimes/$runtime_id"
  done
fi

case "$MODE" in
  gpu) SCRIBE_PROFILE=release SCRIBE_BUNDLE_CUDA=1 "$SCRIPT_DIR/bundle-whisper-runtime.sh" ;;
  *) SCRIBE_PROFILE=release SCRIBE_BUNDLE_CUDA=0 "$SCRIPT_DIR/bundle-whisper-runtime.sh" ;;
esac

stage_portable_runtime() {
  local runtime_id="$1"
  local source="$2"
  local entrypoint="$3"
  local destination="$SCRIBE_DIR/target/release/runtimes/$runtime_id"
  if [[ ! -d "$source" ]]; then
    echo "Portable runtime input does not exist: $source" >&2
    exit 1
  fi
  if find "$source" -type l -print -quit | grep -q .; then
    echo "Portable runtime input contains a symbolic link: $source" >&2
    exit 1
  fi
  if find "$source" -iname pyvenv.cfg -print -quit | grep -q .; then
    echo "Raw Python virtual environments are development-only: $source" >&2
    exit 1
  fi
  python3 - "$source" "$runtime_id" "$(uname -s)" "$(uname -m)" "$entrypoint" <<'PY'
import json, pathlib, sys
root, runtime_id, os_name, arch_name, entrypoint = sys.argv[1:]
os_key = {"Linux": "linux", "Darwin": "macos"}.get(os_name)
arch_key = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "aarch64", "aarch64": "aarch64"}.get(arch_name)
manifest_path = pathlib.Path(root) / "runtime-manifest.json"
try:
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
except Exception as error:
    raise SystemExit(f"Invalid portable runtime manifest {manifest_path}: {error}")
expected = {
    "manifest_version": 1,
    "runtime_id": runtime_id,
    "platform": f"{os_key}-{arch_key}",
    "device": "cpu",
    "entrypoint": entrypoint,
    "portable": True,
}
if not os_key or not arch_key or any(manifest.get(key) != value for key, value in expected.items()):
    raise SystemExit(f"Portable runtime manifest does not match expected identity: {expected}")
if not isinstance(manifest.get("version"), str) or not manifest["version"].strip():
    raise SystemExit("Portable runtime manifest version is missing")
if not (pathlib.Path(root) / pathlib.PurePosixPath(entrypoint)).is_file():
    raise SystemExit(f"Portable runtime entrypoint is missing: {entrypoint}")
PY
  rm -rf -- "$destination"
  mkdir -p "$destination"
  cp -a "$source"/. "$destination"/
}

if [[ "$MODE" == "offline-cpu" ]]; then
  for spec in \
    "faster_whisper:SCRIBE_PORTABLE_FASTER_WHISPER_CPU_RUNTIME" \
    "vosk:SCRIBE_PORTABLE_VOSK_CPU_RUNTIME" \
    "sherpa_onnx:SCRIBE_PORTABLE_SHERPA_ONNX_CPU_RUNTIME" \
    "moonshine:SCRIBE_PORTABLE_MOONSHINE_CPU_RUNTIME" \
    "parakeet:SCRIBE_PORTABLE_PARAKEET_CPU_RUNTIME"
  do
    runtime_id="${spec%%:*}"
    variable="${spec##*:}"
    source="${!variable:-}"
    if [[ -z "$source" ]]; then
      echo "Offline CPU releases require $variable from platform CI." >&2
      exit 1
    fi
    case "$runtime_id" in
      faster_whisper) entrypoint="bin/scribe-faster-whisper" ;;
      vosk) entrypoint="bin/scribe-vosk" ;;
      sherpa_onnx) entrypoint="bin/scribe-sherpa-onnx" ;;
      moonshine) entrypoint="bin/scribe-moonshine" ;;
      parakeet) entrypoint="bin/scribe-parakeet" ;;
    esac
    stage_portable_runtime "$runtime_id" "$source" "$entrypoint"
  done
fi

if [[ "$MODE" == "gpu" && -n "${SCRIBE_PORTABLE_FASTER_WHISPER_GPU_RUNTIME:-}" ]]; then
  echo "A faster-whisper GPU runtime must be packaged as a separate verified artifact; it is not copied into the whisper GPU product." >&2
  exit 1
fi

cat <<EOF
Release bundle ready ($MODE):
  executable:          $SCRIBE_DIR/target/release/local-transcriber
  whisper.cpp runtime: $SCRIBE_DIR/target/release/runtimes/whisper_cpp
EOF

if [[ "$MODE" == "standard" ]]; then
  echo "  contents:            bundled CPU whisper.cpp only; optional runtimes require trusted catalog metadata"
elif [[ "$MODE" == "offline-cpu" ]]; then
  echo "  contents:            bundled all-CPU runtimes supplied by platform CI"
else
  echo "  contents:            explicit GPU product; faster-whisper is included only when a portable CI input was supplied"
fi
if [[ "${SCRIBE_BUILD_VOICE_AI:-0}" == "1" ]]; then
  echo "  voice AI:            pinned CPU llama runtime and both direct mirrored Qwen tiers"
fi
