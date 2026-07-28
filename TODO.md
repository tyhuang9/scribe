# TODO

## Runtime Portability

- Make cross-platform runtime tests and CI green for all six local backends, including packaged sidecar discovery, managed runtime install/update/uninstall, and the Unix-only source-checkout fallback.
- Run the portable packager in Windows/Linux/macOS CI and publish its immutable ZIPs/catalog at a real HTTPS release origin; no hosted artifact records are checked in yet.

## MVP Hardening

- Expand first-run setup with richer validation and recovery guidance.
- Add structured transcription logs behind debug mode.
- Add visual regression notes/screenshots for the Stitch-aligned egui pages.
- Split the remaining egui rendering out of `src/app.rs` into smaller page/component modules.

## STT Backends

- Verify pinned whisper.cpp sidecars and portable manifest validation in cross-platform CI; artifact hosting remains incomplete.
- Harden Vosk release packaging with recorded model/runtime SHA256 checksums.
- Add checksum verification for sherpa-onnx, Moonshine, and Parakeet model archives and runtime packages.
- Add sherpa-onnx streaming runtime support once `SttBackend` exposes partial transcription events.
- Build relocatable faster-whisper and sherpa-family standalone packages in platform CI; raw generated virtual environments remain development-only.
- Add a native whisper.cpp library backend to avoid child-process overhead.
- Expand backend capability flags for timestamps, language selection, and streaming options.

## Streaming And Voice UX

- Add streaming partial transcription events to `SttBackend`.
- Add VAD for auto-stop and silence trimming.
- Add voice commands such as "scratch that".
- Add an optional local-only cleanup/reasoning pass for punctuation and formatting; keep it off by default and never send audio or text to a cloud service.

## Model Management

- Publish production runtime archives and the release-generated trusted catalog from CI.
- Add checksum verification for downloaded whisper.cpp models.
- Add local model inventory scanning.
- Add disk usage reporting.
- Add per-model benchmark history for latency and accuracy notes.

## Desktop Integration

- Add start minimized behavior.
- Add native notifications for completed transcriptions and errors.
- Improve Linux Wayland hotkey guidance if global registration fails.
