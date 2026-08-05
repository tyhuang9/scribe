# TODO

Phase 11 implementation gates discovered 623 tests: 614 passed, 0 failed, and 9
explicit local-runtime/fixture tests were ignored. The remaining items below are
release evidence or follow-up improvements; see
`docs/SCRIBE_REVAMP_IMPLEMENTATION_REPORT.md` for the final NO-GO rationale.

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
- Add visual regression notes/screenshots for the Stitch-aligned egui pages.
- Split the remaining egui rendering out of `src/app.rs` into smaller page/component modules.
- Run an extended Windows shutdown/restart soak after the native worker join fix;
  retain Windows Error Reporting evidence if any access violation recurs.

## Compatibility

- Run the complete load/fixture/cancel/unload/reload/acceleration/platform suite
  before promoting any of the four Whisper artifacts from Experimental.
- Evaluate the exact sherpa-onnx v1.13.4 Zipformer candidate only with the named
  native package, shared corpus, WER, memory, cancellation, lifecycle, and
  Windows first-partial thresholds. Keep `OnnxSpeechRuntime` absent on NO-GO.
- Remove the preserved private faster-whisper, Vosk, and
  sherpa/Moonshine/Parakeet compatibility adapters only after intended roles
  have verified replacements. Keep release packaging and normalized UI isolated
  from them; preserve config aliases and user artifacts.

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

## Desktop Integration

- Add start minimized behavior.
- Add native notifications for completed transcriptions and errors.
- Improve Linux Wayland hotkey guidance if global registration fails.
