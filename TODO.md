# TODO

## MVP Hardening

- Add a first-run setup checklist for missing whisper.cpp executable and model paths.
- Add file picker buttons for executable and model paths.
- Surface microphone device selection instead of using only the OS default input device.
- Show a visible countdown for the max recording duration.
- Add structured transcription logs behind debug mode.
- Add tests for hotkey parsing, config normalization, and whisper.cpp stdout parsing.

## STT Backends

- Add Vosk runtime support.
- Add sherpa-onnx streaming runtime support.
- Add faster-whisper runtime support.
- Add a native whisper.cpp library backend to avoid child-process overhead.
- Add per-backend capability flags for streaming, timestamps, and language selection.

## Streaming And Voice UX

- Add streaming partial transcription events to `SttBackend`.
- Add VAD for auto-stop and silence trimming.
- Add voice commands such as "scratch that".
- Add an optional local cleanup/reasoning pass for punctuation and formatting.
- Add insertion into the active application after transcription completes.

## Model Management

- Add a model downloader with checksum verification.
- Add local model inventory scanning.
- Add disk usage reporting.
- Add per-model benchmark history for latency and accuracy notes.

## Desktop Integration

- Add tray-only mode.
- Add start minimized behavior.
- Add native notifications for completed transcriptions and errors.
- Improve Linux Wayland hotkey guidance if global registration fails.
