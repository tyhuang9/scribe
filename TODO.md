# TODO

## MVP Hardening

- Expand first-run setup with richer validation and recovery guidance.
- Add structured transcription logs behind debug mode.
- Add visual regression notes/screenshots for the Stitch-aligned egui pages.
- Split the remaining egui rendering out of `src/app.rs` into smaller page/component modules.

## STT Backends

- Add Vosk runtime support.
- Add sherpa-onnx streaming runtime support.
- Add faster-whisper runtime support.
- Add Moonshine runtime support.
- Add Parakeet runtime support.
- Add a native whisper.cpp library backend to avoid child-process overhead.
- Expand backend capability flags for timestamps, language selection, and streaming options.

## Streaming And Voice UX

- Add streaming partial transcription events to `SttBackend`.
- Add VAD for auto-stop and silence trimming.
- Add voice commands such as "scratch that".
- Add an optional local cleanup/reasoning pass for punctuation and formatting.

## Model Management

- Add checksum verification for downloaded whisper.cpp models.
- Add local model inventory scanning.
- Add disk usage reporting.
- Add per-model benchmark history for latency and accuracy notes.

## Desktop Integration

- Add start minimized behavior.
- Add native notifications for completed transcriptions and errors.
- Improve Linux Wayland hotkey guidance if global registration fails.
