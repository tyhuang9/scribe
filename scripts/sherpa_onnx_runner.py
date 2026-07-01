#!/usr/bin/env python3
import argparse
import array
import hashlib
import json
import shutil
import sys
import tarfile
import time
import urllib.request
import wave
from pathlib import Path


KNOWN_MODELS = {
    "sherpa-onnx-zipformer-small-en-2023-06-26": {
        "backend": "sherpa-onnx",
        "kind": "transducer",
        "url": "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-small-en-2023-06-26.tar.bz2",
        "sha256": None,
        "encoder": ["encoder-epoch-99-avg-1.int8.onnx", "encoder-epoch-99-avg-1.onnx", "encoder*.onnx"],
        "decoder": ["decoder-epoch-99-avg-1.onnx", "decoder-epoch-99-avg-1.int8.onnx", "decoder*.onnx"],
        "joiner": ["joiner-epoch-99-avg-1.int8.onnx", "joiner-epoch-99-avg-1.onnx", "joiner*.onnx"],
    },
    "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27": {
        "backend": "Moonshine",
        "kind": "moonshine_v2",
        "url": "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2",
        "sha256": None,
        "encoder": ["encoder_model.ort"],
        "decoder": ["decoder_model_merged.ort"],
    },
    "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming": {
        "backend": "Parakeet",
        "kind": "nemo_transducer",
        "url": "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2",
        "sha256": None,
        "encoder": ["encoder.int8.onnx"],
        "decoder": ["decoder.int8.onnx"],
        "joiner": ["joiner.int8.onnx"],
    },
}


def json_error(message: str) -> int:
    print(json.dumps({"error": message}), file=sys.stderr)
    return 1


def import_sherpa_onnx():
    try:
        import sherpa_onnx
    except Exception as exc:  # pragma: no cover - exercised by bundled runtime smoke
        raise RuntimeError(
            "sherpa-onnx is not installed in this runtime; rebuild the sherpa-onnx bundle"
        ) from exc
    return sherpa_onnx


def find_model_file(model_dir: Path, patterns: list[str]) -> Path | None:
    for pattern in patterns:
        candidate = model_dir / pattern
        if candidate.is_file():
            return candidate
        matches = sorted(path for path in model_dir.glob(pattern) if path.is_file())
        if matches:
            return matches[0]
    return None


def is_model_dir_for_spec(path: Path, spec: dict) -> bool:
    if not path.is_dir() or not (path / "tokens.txt").is_file():
        return False
    if find_model_file(path, spec["encoder"]) is None:
        return False
    if find_model_file(path, spec["decoder"]) is None:
        return False
    if spec["kind"] in {"transducer", "nemo_transducer"}:
        return find_model_file(path, spec["joiner"]) is not None
    return True


def model_spec_for_dir(backend: str, path: Path) -> dict | None:
    for spec in KNOWN_MODELS.values():
        if spec["backend"] == backend and is_model_dir_for_spec(path, spec):
            return spec
    return None


def remove_path(path: Path) -> None:
    if path.is_dir():
        shutil.rmtree(path)
    elif path.exists():
        path.unlink()


def download_file(url: str, destination: Path, expected_sha256: str | None) -> None:
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=60) as response:
        with destination.open("wb") as output:
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
                output.write(chunk)

    if expected_sha256 and digest.hexdigest().lower() != expected_sha256.lower():
        raise RuntimeError(
            f"download checksum mismatch for {url}: expected {expected_sha256}, got {digest.hexdigest()}"
        )


def safe_extract_tar(archive_path: Path, destination: Path) -> None:
    destination = destination.resolve()
    with tarfile.open(archive_path, mode="r:*") as archive:
        for member in archive.getmembers():
            target = (destination / member.name).resolve()
            if target != destination and destination not in target.parents:
                raise RuntimeError(f"unsafe tar member path: {member.name}")
            if member.issym() or member.islnk():
                raise RuntimeError(f"unsupported tar link member: {member.name}")
        archive.extractall(destination)


def extracted_model_root(extract_dir: Path, spec: dict) -> Path:
    if is_model_dir_for_spec(extract_dir, spec):
        return extract_dir
    for child in sorted(path for path in extract_dir.rglob("*") if path.is_dir()):
        if is_model_dir_for_spec(child, spec):
            return child
    raise RuntimeError(f"download did not contain a valid {spec['backend']} model: {extract_dir}")


def write_model_manifest(output_dir: Path, model_name: str, spec: dict) -> None:
    manifest = {
        "model": model_name,
        "backend": spec["backend"],
        "kind": spec["kind"],
        "source": spec["url"],
    }
    (output_dir / "scribe-model.json").write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )


def cmd_download_model(args: argparse.Namespace) -> int:
    try:
        spec = KNOWN_MODELS.get(args.model)
        if spec is None:
            return json_error(f"unsupported sherpa-onnx family model: {args.model}")

        output_dir = Path(args.output).resolve()
        if is_model_dir_for_spec(output_dir, spec):
            print(json.dumps({"model": args.model, "path": str(output_dir)}))
            return 0

        output_dir.parent.mkdir(parents=True, exist_ok=True)
        if output_dir.exists():
            remove_path(output_dir)

        archive_path = output_dir.with_name(f"{output_dir.name}.tar.bz2.partial")
        extract_dir = output_dir.with_name(f".{output_dir.name}.partial")
        remove_path(archive_path)
        remove_path(extract_dir)
        extract_dir.mkdir(parents=True)

        try:
            download_file(spec["url"], archive_path, spec["sha256"])
            safe_extract_tar(archive_path, extract_dir)
            model_root = extracted_model_root(extract_dir, spec)
            shutil.move(str(model_root), str(output_dir))
        finally:
            remove_path(archive_path)
            remove_path(extract_dir)

        if not is_model_dir_for_spec(output_dir, spec):
            raise RuntimeError(
                f"download did not create a complete {spec['backend']} model at {output_dir}"
            )

        write_model_manifest(output_dir, args.model, spec)
        print(json.dumps({"model": args.model, "path": str(output_dir)}))
        return 0
    except Exception as exc:
        return json_error(str(exc))


def mono_float32_from_wav(audio_path: Path):
    try:
        import numpy as np
    except Exception as exc:  # pragma: no cover - exercised by bundled runtime smoke
        raise RuntimeError("numpy is not installed in this runtime") from exc

    with wave.open(str(audio_path), "rb") as wav:
        channels = wav.getnchannels()
        if channels < 1:
            raise RuntimeError("WAV audio has no channels")
        if wav.getsampwidth() != 2:
            raise RuntimeError("sherpa-onnx runner expects 16-bit PCM WAV audio")
        sample_rate = wav.getframerate()
        samples = array.array("h")
        samples.frombytes(wav.readframes(wav.getnframes()))
        if sys.byteorder != "little":
            samples.byteswap()

    audio = np.asarray(samples, dtype=np.float32)
    if channels > 1:
        frame_count = len(audio) // channels
        audio = audio[: frame_count * channels].reshape(frame_count, channels).mean(axis=1)
    audio = audio / 32768.0
    return sample_rate, audio


def create_recognizer(sherpa_onnx, backend: str, model_path: Path):
    spec = model_spec_for_dir(backend, model_path)
    if spec is None:
        raise RuntimeError(f"model path is not a complete {backend} model: {model_path}")

    tokens = model_path / "tokens.txt"
    encoder = find_model_file(model_path, spec["encoder"])
    decoder = find_model_file(model_path, spec["decoder"])
    if encoder is None or decoder is None:
        raise RuntimeError(f"model path is missing encoder or decoder files: {model_path}")

    if spec["kind"] == "moonshine_v2":
        return sherpa_onnx.OfflineRecognizer.from_moonshine_v2(
            encoder=str(encoder),
            decoder=str(decoder),
            tokens=str(tokens),
            debug=False,
        )

    joiner = find_model_file(model_path, spec["joiner"])
    if joiner is None:
        raise RuntimeError(f"model path is missing joiner file: {model_path}")

    kwargs = {
        "num_threads": 1,
        "provider": "cpu",
        "debug": False,
        "decoding_method": "greedy_search",
    }
    if spec["kind"] == "nemo_transducer":
        kwargs["model_type"] = "nemo_transducer"

    return sherpa_onnx.OfflineRecognizer.from_transducer(
        str(encoder),
        str(decoder),
        str(joiner),
        str(tokens),
        **kwargs,
    )


def cmd_transcribe(args: argparse.Namespace) -> int:
    try:
        sherpa_onnx = import_sherpa_onnx()
        model_path = Path(args.model)
        audio_path = Path(args.audio)
        if not audio_path.exists():
            return json_error(f"audio path does not exist: {audio_path}")

        sample_rate, audio = mono_float32_from_wav(audio_path)
        recognizer = create_recognizer(sherpa_onnx, args.backend, model_path)

        started = time.monotonic()
        stream = recognizer.create_stream()
        stream.accept_waveform(sample_rate, audio)
        recognizer.decode_stream(stream)
        text = str(getattr(stream.result, "text", "")).strip()
        segment = {"start_ms": None, "end_ms": None, "text": text} if text else None
        payload = {
            "text": text,
            "segments": [segment] if segment else [],
            "duration_ms": int((time.monotonic() - started) * 1000),
        }
        print(json.dumps(payload, ensure_ascii=False))
        return 0
    except Exception as exc:
        return json_error(str(exc))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description="Scribe sherpa-onnx family runtime runner")
    subcommands = root.add_subparsers(dest="command", required=True)

    download = subcommands.add_parser("download-model")
    download.add_argument("--model", required=True)
    download.add_argument("--output", required=True)
    download.set_defaults(func=cmd_download_model)

    transcribe = subcommands.add_parser("transcribe")
    transcribe.add_argument("--backend", choices=["sherpa-onnx", "Moonshine", "Parakeet"], required=True)
    transcribe.add_argument("--model", required=True)
    transcribe.add_argument("--audio", required=True)
    transcribe.set_defaults(func=cmd_transcribe)

    return root


def main() -> int:
    args = parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
