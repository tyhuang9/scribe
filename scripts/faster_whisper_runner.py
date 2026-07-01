#!/usr/bin/env python3
import argparse
import importlib.util
import json
import os
from pathlib import Path
import sys
import time


def json_error(message: str) -> int:
    print(json.dumps({"error": message}), file=sys.stderr)
    return 1


def _append_existing_path(paths: list[str], path: Path) -> None:
    if path.is_dir():
        value = str(path)
        if value not in paths:
            paths.append(value)


def nvidia_library_paths() -> list[str]:
    paths: list[str] = []
    for value in os.environ.get("SCRIBE_FASTER_WHISPER_LIBRARY_PATHS", "").split(
        os.pathsep
    ):
        if value:
            _append_existing_path(paths, Path(value))

    for module_name in ("nvidia.cublas.lib", "nvidia.cudnn.lib"):
        try:
            spec = importlib.util.find_spec(module_name)
        except ModuleNotFoundError:
            continue
        if spec and spec.origin:
            _append_existing_path(paths, Path(spec.origin).parent)

    runtime_root = Path(__file__).resolve().parent.parent
    for candidate in (
        runtime_root / "lib",
        runtime_root / "cuda",
        runtime_root / "cuda_v12",
        runtime_root / "cuda_v13",
        Path("/usr/local/lib/ollama"),
        Path("/usr/local/lib/ollama/cuda_v12"),
        Path("/usr/local/lib/ollama/cuda_v13"),
    ):
        _append_existing_path(paths, candidate)
    return paths


def cuda_error_message(message: str) -> str:
    if "libcublas.so.12" not in message and "cublas" not in message.lower():
        return message
    return (
        f"{message}. CUDA 12 runtime libraries were not visible to faster-whisper. "
        "Rebuild the runtime with SCRIBE_BUNDLE_FAST_WHISPER_CUDA=1, or make "
        "libcublas.so.12 available through SCRIBE_FASTER_WHISPER_LIBRARY_PATHS."
    )


def import_faster_whisper():
    try:
        from faster_whisper import WhisperModel
        from faster_whisper.utils import download_model
    except Exception as exc:  # pragma: no cover - exercised by bundled runtime smoke
        message = cuda_error_message(str(exc))
        if message != str(exc):
            raise RuntimeError(message) from exc
        raise RuntimeError(
            "faster-whisper is not installed in this runtime; rebuild the faster-whisper bundle"
        ) from exc
    return WhisperModel, download_model


def choose_device(mode: str, gpu_device: int) -> tuple[str, str, int | None]:
    if mode == "cpu":
        return "cpu", "int8", None
    if mode == "gpu":
        return "cuda", "float16", gpu_device

    try:
        import ctranslate2

        if ctranslate2.get_cuda_device_count() > gpu_device:
            return "cuda", "float16", gpu_device
    except Exception:
        pass
    return "cpu", "int8", None


def cmd_nvidia_library_path(_args: argparse.Namespace) -> int:
    print(":".join(nvidia_library_paths()))
    return 0


def cmd_download_model(args: argparse.Namespace) -> int:
    try:
        _whisper_model, download_model = import_faster_whisper()
        output_dir = Path(args.output).resolve()
        output_dir.parent.mkdir(parents=True, exist_ok=True)
        path = download_model(args.model, output_dir=str(output_dir))
        print(json.dumps({"model": args.model, "path": str(path)}))
        return 0
    except Exception as exc:
        return json_error(cuda_error_message(str(exc)))


def cmd_transcribe(args: argparse.Namespace) -> int:
    try:
        WhisperModel, _download_model = import_faster_whisper()
        model_path = Path(args.model)
        audio_path = Path(args.audio)
        if not model_path.exists():
            return json_error(f"model path does not exist: {model_path}")
        if not audio_path.exists():
            return json_error(f"audio path does not exist: {audio_path}")

        device, compute_type, device_index = choose_device(args.device_mode, args.gpu_device)
        model_kwargs = {
            "device": device,
            "compute_type": compute_type,
        }
        if device_index is not None:
            model_kwargs["device_index"] = device_index

        started = time.monotonic()
        model = WhisperModel(str(model_path), **model_kwargs)
        segments, info = model.transcribe(
            str(audio_path),
            beam_size=args.beam_size,
            vad_filter=args.vad_filter,
        )
        segment_payload = [
            {
                "start_ms": int(segment.start * 1000),
                "end_ms": int(segment.end * 1000),
                "text": segment.text.strip(),
            }
            for segment in segments
        ]
        text = "\n".join(
            segment["text"] for segment in segment_payload if segment["text"]
        )
        payload = {
            "text": text,
            "segments": segment_payload,
            "duration_ms": int((time.monotonic() - started) * 1000),
            "device": device,
            "compute_type": compute_type,
            "language": getattr(info, "language", None),
            "language_probability": getattr(info, "language_probability", None),
        }
        print(json.dumps(payload, ensure_ascii=False))
        return 0
    except Exception as exc:
        return json_error(cuda_error_message(str(exc)))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description="Scribe faster-whisper runtime runner")
    subcommands = root.add_subparsers(dest="command", required=True)

    nvidia_paths = subcommands.add_parser("nvidia-library-path")
    nvidia_paths.set_defaults(func=cmd_nvidia_library_path)

    download = subcommands.add_parser("download-model")
    download.add_argument("--model", required=True)
    download.add_argument("--output", required=True)
    download.set_defaults(func=cmd_download_model)

    transcribe = subcommands.add_parser("transcribe")
    transcribe.add_argument("--model", required=True)
    transcribe.add_argument("--audio", required=True)
    transcribe.add_argument(
        "--device-mode", choices=("auto", "gpu", "cpu"), default="auto"
    )
    transcribe.add_argument("--gpu-device", type=int, default=0)
    transcribe.add_argument("--beam-size", type=int, default=5)
    transcribe.add_argument("--vad-filter", action="store_true")
    transcribe.set_defaults(func=cmd_transcribe)

    return root


def main() -> int:
    os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
    args = parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
