# TODO

> **Historical test snapshot:** The Phase 11 count below—623 discovered, 614
> passed, 0 failed, 9 ignored—was recorded on 2026-08-05. It is not a current
> rebaseline. Current implementation truth is maintained in
> `docs/TECHNICAL_OVERVIEW.md`; live qualification remains in
> `docs/MANUAL_TEST_MATRIX.md`.

The remaining items below are release evidence or follow-up improvements.

## Release Evidence

- Complete the dated Windows 11 manual matrix for hotkey, microphone, overlay,
  target focus, clipboard restoration, exactly-once output, install recovery,
  history, multi-monitor/DPI, and standard/elevated targets.
- Capture comparable desktop median/p95 latency, RTF, memory, and idle-CPU
  measurements on the same machine, fixture, model, and resolved backend.
- Run conservative build checks on macOS and Linux. Their desktop/model
  combinations remain unqualified.
- Produce reviewed Windows CUDA/Vulkan Auto evidence for each supported
  pack/model/device/driver lane: at least five cold and twenty warm runs,
  transcript parity, reliability, and p95 no more than 110% of CPU.
- Exercise mixed-GPU AC/battery, driver update, suspend/resume, device loss,
  insufficient VRAM, and clean-installer lanes before adding any production
  Auto qualification entry.

## MVP Hardening

- Expand first-run setup with richer validation and recovery guidance.
- Add visual regression notes/screenshots for the Stitch-aligned egui pages.
- Split the remaining egui rendering out of `src/app.rs` into smaller page/component modules.
- Run an extended Windows shutdown/restart soak after the native worker join fix;
  retain Windows Error Reporting evidence if any access violation recurs.

## Compatibility

- Run the complete load/fixture/cancel/unload/reload/acceleration/platform suite
  before promoting any of the five normal catalog models from Experimental.
- Preserve legacy user configuration and artifact files without recognizing or
  executing them in production. Do not represent their presence as a supported
  runtime path or assume an installer removes them.

## Streaming And Voice UX

- Measure and tune rolling preview against a representative corpus without
  weakening bounded work, stable-prefix, or overlay-only tentative text rules.
- Add voice commands such as "scratch that".
- Add an optional local-only cleanup/reasoning pass for punctuation and formatting; keep it off by default and never send audio or text to a cloud service.

## Model Management

- Exercise real network interruption/resume and power-loss recovery for both
  pinned GGUF artifacts and the receipt-backed ONNX bundle.
- Add local model inventory scanning for safely adopting exact verified files.
- Add disk usage reporting.
- Add per-model benchmark history for latency and accuracy notes.

## Desktop Integration

- Add start minimized behavior.
- Add native notifications for completed transcriptions and errors.
- Improve Linux Wayland hotkey guidance if global registration fails.
