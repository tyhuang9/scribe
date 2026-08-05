---
title: Troubleshooting
description: Resolve common environment and runtime issues without losing your transcript.
---

## No transcript appears

Confirm that a microphone is visible to the host OS and an Experimental GGUF model is installed and selected. Normal GGUF cards are runtime-ready without a separate package. Try the in-app recording control before diagnosing global hotkeys. Keep any visible transcript and copy it before changing settings.

## “No speech was detected”

An input meter above 0% only proves that samples are arriving. Speech must cross the VAD threshold for long enough to be accepted. Open **General**, speak at your normal distance, and move the **Input sensitivity** threshold below the live voice peaks but above the resting noise floor. Also check physical mute, Windows input-device selection, per-device gain, and microphone privacy access.

If normal speech cannot cross the threshold even at the most sensitive setting, test the microphone in the operating system. A very low signal is a capture/device problem, not a model failure.

## Runtime fails or a terminal appears

The default GGUF path uses statically linked `transcribe-cpp` 0.1.3 in-process and requires no Python, JSON PCM sidecar, localhost server, downloaded runtime package, or inference executable. Private legacy compatibility paths can still start short-lived CLI/Python processes and may create a console; they are not the normal UI path. If a normal GGUF fails, revalidate or reinstall the model rather than looking for a runtime package.

## Linux tray or startup issue

Start without tray initialization:

```bash
SCRIBE_DISABLE_TRAY=1 cargo run
```

Linux defaults to software rendering to avoid common EGL/Mesa crashes. If GPU rendering is known to work, opt in with `SCRIBE_USE_GPU=1`.

## WSLg startup issue

Scribe disables tray integration under WSL by default. If the desktop session remains unavailable, restart WSLg from Windows PowerShell:

```powershell
wsl.exe --shutdown
```

Then reopen WSL and try `cargo run`. See [Linux and WSL](../platforms/linux-wsl/) for X11 and Wayland overrides.

## Pasting fails

Copy the transcript and paste manually. Linux and macOS are intentionally clipboard-only. Windows falls back to copy-only when the original target changed, activation is denied, the clipboard changed externally, or the target has higher integrity.

If the issue persists, capture the exact error and the operating system/runtime combination when reporting it upstream.
