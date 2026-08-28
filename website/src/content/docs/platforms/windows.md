---
title: Windows
description: Windows-specific input and output boundaries.
---

Scribe uses the normal desktop audio-capture path on Windows; no installer permission is expected for that path. Global hotkeys use the system-wide `RegisterHotKey` API.

The Windows installer detects an existing per-user Scribe installation and offers **Update**, **Repair**, or **Remove** as appropriate. Update and Repair keep the selected install location and shortcut choices. Remove runs the existing Scribe uninstaller, then closes the outer setup without installing anything. It deletes the installed application and its registration, but keeps Scribe settings, history, downloaded models, and runtimes stored outside the application folder.

For automation, a successful Remove closes the outer Inno Setup through its pre-install cancellation path, so Setup reports exit code `2` even when the uninstaller succeeded. Verify that Scribe's uninstall registration is gone rather than treating exit code `2` alone as an uninstall failure.

Focused-app insertion captures the original foreground HWND and process, revalidates it before output, and uses the clipboard plus one `SendInput` batch. Target loss, activation denial, and elevated or higher-integrity applications fall back to copy-only without synthetic keystrokes. A clipboard race instead suppresses the paste and preserves the other app's newer clipboard content; the final transcript remains in Scribe for manual copying.

The normal GGUF path uses a statically linked CPU backend in Scribe's private
persistent inference child and needs no runtime package. Receipt-backed ONNX
uses native Sherpa ONNX in that same child. `Auto` resolves to CPU. An explicit
GPU preference reports that no verified accelerator is available.
