---
title: Windows
description: Windows-specific input and output boundaries.
---

Scribe uses the normal desktop audio-capture path on Windows; no installer permission is expected for that path. Global hotkeys use the system-wide `RegisterHotKey` API.

Focused-app insertion captures the original foreground HWND and process, revalidates it before output, and uses the clipboard plus one `SendInput` batch. Target loss, activation denial, and elevated or higher-integrity applications fall back to copy-only without synthetic keystrokes. A clipboard race instead suppresses the paste and preserves the other app's newer clipboard content; the final transcript remains in Scribe for manual copying.

The normal GGUF path uses a statically linked CPU backend in Scribe's private
persistent inference child and needs no runtime package. Receipt-backed ONNX
uses native Sherpa ONNX in that same child. `Auto` resolves to CPU. An explicit
GPU preference reports that no verified accelerator is available.
