---
title: macOS
description: macOS privacy controls for microphone, hotkeys, and insertion.
---

macOS requires Microphone privacy access for recording. Depending on the global-hotkey backend and OS version, Input Monitoring can also be required.

Output is deliberately clipboard-only until a focus-safe native adapter is verified. Copy the transcript and paste it manually.

The normal GGUF path needs no managed runtime package, but macOS desktop/model combinations are not release-qualified. Development compatibility paths do not establish support.

These permissions are feature-level choices. They do not enable cloud services or background synchronization: Scribe remains local-first.
