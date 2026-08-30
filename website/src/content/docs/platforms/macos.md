---
title: macOS
description: macOS privacy controls for microphone, hotkeys, and insertion.
---

macOS requires Microphone privacy access for recording. Depending on the global-hotkey backend and OS version, Input Monitoring can also be required.

Output is deliberately clipboard-only until a focus-safe native adapter is verified. Copy the transcript and paste it manually.

The normal GGUF path needs no managed runtime package, but macOS desktop/model combinations are not release-qualified. Development compatibility paths do not establish support.

Scribe requires macOS 13 or later for packaged releases. The app contains a universal CPU worker. Metal acceleration is only considered for an explicit GPU request when a verified, signed Metal worker pack is installed with the app; otherwise GPU is unavailable and Auto remains CPU-only. Scribe does not benchmark your Mac to change that decision. Metal Auto support needs separately reviewed release evidence for five cold runs, twenty warm runs, transcript parity and reliability, with GPU end-to-end p95 at most 110% of CPU.

The ONNX and Sherpa paths are CPU-only on macOS.

These permissions are feature-level choices. They do not enable cloud services or background synchronization: Scribe remains local-first.
