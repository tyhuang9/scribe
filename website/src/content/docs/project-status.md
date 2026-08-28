---
title: Project status and reference
description: The current documented scope and the source references behind this guide.
---

## Current documented scope

Scribe is a local-first Rust desktop application with Transcribe, General, Models, History, Advanced, and About pages plus opt-in Debug tools. Native workers capture and prepare microphone audio, produce bounded rolling preview, finalize the utterance, and can copy or safely insert the completed transcript.

## Backends and models

The final application has exactly one logical runtime boundary, selected only by
`RuntimeRouter` in the private persistent inference child. The normal UI always
includes six static Experimental entries: four pinned GGUF artifacts and the
receipt-backed `moonshine-tiny-en-int8-onnx` and `moonshine-base-en-int8-onnx`
bundles; the Supported count is zero. Moonshine Base is a 286,930,831-byte
converted five-file INT8 artifact, including its MIT / Useful Sensors 2024
license file; source and converter revisions are unrecorded. Its scoped
Windows/Sherpa 1.13.5 fixture gate passed load/health/silence, normalized
known-WAV equality, and unload/reload in 140.40 seconds total—not a latency
measurement. Cancellation, restart recovery, latency, resource use,
accelerators, and non-Windows behavior remain unverified. When a trusted catalog
response is available, Models can additionally
display non-duplicate remote GGUF variants. These variants are neither bundled
nor static entries, and they cannot execute until their source facts are
verified and their artifacts are installed. GGUF uses statically linked native
`transcribe-cpp`; ONNX uses native Sherpa ONNX in the same child. VAD has a
separate worker/process path. No model advertises native streaming; preview uses
shared rolling batch decoding.

The current release decision is **NO-GO** pending the dated Windows manual matrix, physical shutdown/restart soak, complete compatibility suites, desktop latency/resource measurements, and conservative Linux/macOS exercises. Automated gates passing does not promote a model or replace those manual results.

The physical microphone/VAD transcription path and the complete tray action set remain manually unverified. Live Windows evidence covers tray Show/Hide, while Start/Stop Recording, Copy Last Transcript, and Quit still require the UI-04 matrix. These are implemented behaviors, not release-qualified reliability claims.

## Known platform limits

- **Windows:** focused-app insertion cannot reliably target elevated applications.
- **Linux and WSL:** global hotkeys are opt-in; tray behavior is disabled by default under WSL; output is clipboard-only.
- **macOS:** microphone and Input Monitoring protections can require user approval; output is clipboard-only.

## Source references

This page is a concise scope summary, not a replacement for engineering records. Maintain it against the end-user [README](https://github.com/tyhuang9/scribe/blob/main/README.md), [technical overview](https://github.com/tyhuang9/scribe/blob/main/docs/TECHNICAL_OVERVIEW.md), [embedded runtime record](https://github.com/tyhuang9/scribe/blob/main/docs/EMBEDDED_STT_AND_MODELS.md), [implementation report](https://github.com/tyhuang9/scribe/blob/main/docs/SCRIBE_REVAMP_IMPLEMENTATION_REPORT.md), [manual matrix](https://github.com/tyhuang9/scribe/blob/main/docs/MANUAL_TEST_MATRIX.md), and application code. Newer source/records take precedence over historical phase reports. Do not promote unverified work, local reports, or future plans as shipped behavior.
