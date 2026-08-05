---
title: Windows
description: Windows-specific input and output boundaries.
---

Scribe uses the normal desktop audio-capture path on Windows; no installer permission is expected for that path. Global hotkeys use the system-wide `RegisterHotKey` API.

Focused-app insertion captures the original foreground HWND and process, revalidates it before output, and uses the clipboard plus one `SendInput` batch. Target loss, activation denial, clipboard races, and elevated or higher-integrity applications fall back to copy-only without synthetic keystrokes.

The normal GGUF path uses a statically linked CPU backend and needs no runtime package. `Auto` resolves to CPU. An explicit GPU preference reports that no verified accelerator is available. A pinned Windows x64 package exists only for retained GGML compatibility.
