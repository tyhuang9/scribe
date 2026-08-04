# TODO

## Release Evidence

- Complete the dated Windows 11 manual matrix for hotkey, microphone, overlay,
  target focus, clipboard restoration, exactly-once output, install recovery,
  history, multi-monitor/DPI, and standard/elevated targets.
- Capture comparable desktop median/p95 latency, RTF, memory, and idle-CPU
  measurements on the same machine, fixture, model, and resolved backend.
- Run conservative build/fallback checks on macOS and Linux; normalized runtime
  install remains unavailable until each platform has a pinned measured package.

## MVP Hardening

- Expand first-run setup with richer validation and recovery guidance.
- Complete the redacted diagnostics export and structured per-session metrics.
- Add visual regression notes/screenshots for the Stitch-aligned egui pages.
- Split the remaining egui rendering out of `src/app.rs` into smaller page/component modules.

## Compatibility

- Run the complete load/fixture/cancel/unload/reload/acceleration/platform suite
  before promoting any of the four Whisper artifacts from Experimental.
- Evaluate the exact sherpa-onnx v1.13.4 Zipformer candidate only with the named
  native package, shared corpus, WER, memory, cancellation, lifecycle, and
  Windows first-partial thresholds. Keep `OnnxSpeechRuntime` absent on NO-GO.
- Retire transitional faster-whisper, Vosk, sherpa/Moonshine/Parakeet adapters
  only after intended roles have verified replacements; preserve config aliases
  and user artifacts. Remove the unreachable legacy downloader helpers and
  their dead-code allowances with that Phase 11 retirement.

## Streaming And Voice UX

- Measure and tune rolling preview against a representative corpus without
  weakening bounded work, stable-prefix, or overlay-only tentative text rules.
- Add voice commands such as "scratch that".
- Add an optional local-only cleanup/reasoning pass for punctuation and formatting; keep it off by default and never send audio or text to a cloud service.

## Model Management

- Publish the pinned Windows runtime archive through the release pipeline and
  exercise real network interruption/resume and power-loss recovery.
- Add local model inventory scanning for safely adopting exact verified files.
- Add disk usage reporting.
- Add per-model benchmark history for latency and accuracy notes.

## History And Retention

- Add bundled SQLite Pending/Completed/Failed entries, transcript-only default
  retention, optional separate audio, search/pagination, pin/delete, retry, and
  startup reconciliation.

## Desktop Integration

- Add start minimized behavior.
- Add native notifications for completed transcriptions and errors.
- Improve Linux Wayland hotkey guidance if global registration fails.
