---
title: Settings and environment
description: Configure local behavior without turning Scribe into a background service.
---

Settings are stored as local JSON in a platform-specific Scribe configuration directory.

The shell provides **Transcribe**, **General**, **Models**, **History**, **Advanced**, and **About** pages, plus opt-in **Debug** tools. Settings cover the active model, hotkey and mode, microphone selection, AI voice detection or a manual dBFS input threshold, recording/endpointing, overlay mode, history/privacy, performance, tray behavior, focused-app insertion, clipboard restoration, and paste delay.

Settings use a versioned schema with field-level salvage, unknown-field preservation, corrupt-file backup, debounced saves, and atomic same-directory replacement. A first launch can migrate an older Local Transcriber configuration only when no Scribe configuration already exists.

## Useful environment switches

| Variable | Purpose |
| --- | --- |
| `SCRIBE_DISABLE_TRAY=1` | Start without tray initialization while diagnosing desktop-session issues. |
| `SCRIBE_ENABLE_TRAY=1` | Opt into tray behavior under WSL. |
| `SCRIBE_ENABLE_GLOBAL_HOTKEY=1` | Opt into Linux global-hotkey registration. |
| `SCRIBE_USE_GPU=1` | Opt into GPU rendering on Linux. |
| `SCRIBE_FORCE_X11=1` | Force X11 for a WSL/Linux run. |
| `SCRIBE_FORCE_WAYLAND=1` | Force Wayland for a WSL/Linux run. |

These switches are diagnostic or environment-specific controls, not release-readiness guarantees.
