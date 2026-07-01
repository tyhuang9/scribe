#!/usr/bin/env python3
import argparse
import array
import hashlib
import json
import shutil
import sys
import time
import urllib.request
import wave
import zipfile
from pathlib import Path


KNOWN_MODELS = {
    "vosk-model-small-en-us-0.15": {
        "url": "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip",
        "sha256": None,
    }
}


def json_error(message: str) -> int:
    print(json.dumps({"error": message}), file=sys.stderr)
    return 1


def import_vosk():
    try:
        from vosk import KaldiRecognizer, Model, SetLogLevel
    except Exception as exc:  # pragma: no cover - exercised by bundled runtime smoke
        raise RuntimeError(
            "vosk is not installed in this runtime; rebuild the Vosk bundle"
        ) from exc
    SetLogLevel(-1)
    return KaldiRecognizer, Model


def is_vosk_model_dir(path: Path) -> bool:
    graph = path / "graph"
    has_graph = (graph / "HCLG.fst").is_file() or (
        (graph / "HCLr.fst").is_file() and (graph / "Gr.fst").is_file()
    )
    return (
        path.is_dir()
        and (path / "am" / "final.mdl").is_file()
        and (path / "conf" / "model.conf").is_file()
        and has_graph
    )


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


def safe_extract_zip(archive: zipfile.ZipFile, destination: Path) -> None:
    destination = destination.resolve()
    for member in archive.infolist():
        target = (destination / member.filename).resolve()
        if target != destination and destination not in target.parents:
            raise RuntimeError(f"unsafe ZIP member path: {member.filename}")
    archive.extractall(destination)


def extracted_model_root(extract_dir: Path) -> Path:
    if is_vosk_model_dir(extract_dir):
        return extract_dir
    for child in sorted(extract_dir.iterdir()):
        if child.is_dir() and is_vosk_model_dir(child):
            return child
    raise RuntimeError(f"download did not contain a valid Vosk model: {extract_dir}")


def cmd_download_model(args: argparse.Namespace) -> int:
    try:
        spec = KNOWN_MODELS.get(args.model)
        if spec is None:
            return json_error(f"unsupported Vosk model: {args.model}")

        output_dir = Path(args.output).resolve()
        if is_vosk_model_dir(output_dir):
            print(json.dumps({"model": args.model, "path": str(output_dir)}))
            return 0

        output_dir.parent.mkdir(parents=True, exist_ok=True)
        if output_dir.exists():
            remove_path(output_dir)

        archive_path = output_dir.with_name(f"{output_dir.name}.zip.partial")
        extract_dir = output_dir.with_name(f".{output_dir.name}.partial")
        remove_path(archive_path)
        remove_path(extract_dir)
        extract_dir.mkdir(parents=True)

        try:
            download_file(spec["url"], archive_path, spec["sha256"])
            with zipfile.ZipFile(archive_path) as archive:
                safe_extract_zip(archive, extract_dir)
            model_root = extracted_model_root(extract_dir)
            shutil.move(str(model_root), str(output_dir))
        finally:
            remove_path(archive_path)
            remove_path(extract_dir)

        if not is_vosk_model_dir(output_dir):
            raise RuntimeError(f"download did not create a complete Vosk model at {output_dir}")

        print(json.dumps({"model": args.model, "path": str(output_dir)}))
        return 0
    except Exception as exc:
        return json_error(str(exc))


def segment_from_result(payload: dict) -> dict | None:
    text = str(payload.get("text", "")).strip()
    if not text:
        return None
    words = payload.get("result") or []
    start_ms = None
    end_ms = None
    if words:
        first = words[0]
        last = words[-1]
        if "start" in first:
            start_ms = int(float(first["start"]) * 1000)
        if "end" in last:
            end_ms = int(float(last["end"]) * 1000)
    return {"start_ms": start_ms, "end_ms": end_ms, "text": text}


def mono_pcm16(data: bytes, channels: int) -> bytes:
    if channels == 1:
        return data
    samples = array.array("h")
    samples.frombytes(data)
    if sys.byteorder != "little":
        samples.byteswap()

    mono = array.array("h")
    for index in range(0, len(samples), channels):
        frame = samples[index : index + channels]
        if len(frame) != channels:
            break
        mono.append(round(sum(frame) / channels))

    if sys.byteorder != "little":
        mono.byteswap()
    return mono.tobytes()


def cmd_transcribe(args: argparse.Namespace) -> int:
    try:
        KaldiRecognizer, Model = import_vosk()
        model_path = Path(args.model)
        audio_path = Path(args.audio)
        if not is_vosk_model_dir(model_path):
            return json_error(f"model path is not a complete Vosk model: {model_path}")
        if not audio_path.exists():
            return json_error(f"audio path does not exist: {audio_path}")

        started = time.monotonic()
        model = Model(str(model_path))
        segments = []
        with wave.open(str(audio_path), "rb") as wav:
            channels = wav.getnchannels()
            if channels < 1:
                return json_error("WAV audio has no channels")
            if wav.getsampwidth() != 2:
                return json_error("Vosk runner expects 16-bit PCM WAV audio")
            recognizer = KaldiRecognizer(model, wav.getframerate())
            recognizer.SetWords(True)
            while True:
                data = wav.readframes(4000)
                if len(data) == 0:
                    break
                data = mono_pcm16(data, channels)
                if recognizer.AcceptWaveform(data):
                    segment = segment_from_result(json.loads(recognizer.Result()))
                    if segment:
                        segments.append(segment)
            segment = segment_from_result(json.loads(recognizer.FinalResult()))
            if segment:
                segments.append(segment)

        text = "\n".join(segment["text"] for segment in segments if segment["text"])
        payload = {
            "text": text,
            "segments": segments,
            "duration_ms": int((time.monotonic() - started) * 1000),
        }
        print(json.dumps(payload, ensure_ascii=False))
        return 0
    except Exception as exc:
        return json_error(str(exc))


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description="Scribe Vosk runtime runner")
    subcommands = root.add_subparsers(dest="command", required=True)

    download = subcommands.add_parser("download-model")
    download.add_argument("--model", required=True)
    download.add_argument("--output", required=True)
    download.set_defaults(func=cmd_download_model)

    transcribe = subcommands.add_parser("transcribe")
    transcribe.add_argument("--model", required=True)
    transcribe.add_argument("--audio", required=True)
    transcribe.set_defaults(func=cmd_transcribe)

    return root


def main() -> int:
    args = parser().parse_args()
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
