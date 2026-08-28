---
title: Troubleshooting
description: Resolve common environment and runtime issues without losing your transcript.
---

## No transcript appears

Confirm that a microphone is visible to the host OS and an Experimental catalog
model is installed and selected. Normal GGUF and receipt-backed ONNX catalog
entries are runtime-ready without a separate package. Try the in-app recording
control before diagnosing global hotkeys. Keep any visible transcript and copy
it before changing settings.

## “No speech was detected”

An input meter above 0% only proves that samples are arriving. In **AI voice detection**, Silero still decides what counts as speech. In **Manual volume threshold**, audio below the `−72..0 dBFS` marker is silenced, so lower the marker below normal voice peaks while keeping it above the resting noise floor. Also check physical mute, Windows input-device selection, per-device gain, and microphone privacy access.

If normal speech does not cross a manual threshold even at `−72 dBFS`, test the microphone in the operating system. A very low signal is a capture/device problem, not a model failure.

## Runtime fails or a terminal appears

The normal GGUF path uses statically linked `transcribe-cpp` 0.1.3 in Scribe's
private persistent inference child; receipt-backed ONNX uses native Sherpa ONNX
there too. Neither path requires Python, a JSON-PCM sidecar, localhost server,
downloaded runtime package, GGML/DLL route, or inference CLI. VAD has its own
separate worker/process path. If a model fails, revalidate or reinstall that
model rather than looking for a runtime package.

## Vulkan source build fails or does not use the GPU

Vulkan acceleration is an opt-in source-build feature for GGUF models, not a
published-release capability. Confirm that the Khronos Vulkan SDK is installed,
then open a new PowerShell session and check both the SDK variable and shader
compiler before rebuilding:

```powershell
$env:VULKAN_SDK
Get-Command glslc
cargo check --features vulkan-acceleration
```

Also update the display driver's Vulkan support. In a feature build, **Auto**
may truthfully report CPU when no compatible Vulkan device initializes; that is
the intended fallback. **GPU** is strict and returns a backend-unavailable error
instead of silently using CPU. These controls do not accelerate the Moonshine
ONNX bundle. The Linux `SCRIBE_USE_GPU` setting below controls desktop rendering
and is unrelated to transcription acceleration.

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

Copy the transcript and paste manually. Linux and macOS are intentionally clipboard-only. On Windows, a changed target, activation denial, or a higher-integrity target produces an explicit copy-only fallback. If another app changes the clipboard during Scribe's output transaction, Scribe preserves that newer clipboard content, sends no paste command, and leaves the final transcript in Scribe for you to copy manually.

If the issue persists, capture the exact error and the operating system/runtime combination when reporting it upstream.
