---
title: Hotkeys and recording
description: Use the default hotkey or the in-app recording control.
---

The default global hotkey is `Ctrl+Shift+Space`. In **General**, choose whether it toggles recording or works as hold-to-talk, then select a different hotkey if necessary.

The in-app Start/Stop control remains the dependable fallback when a desktop session blocks global hotkeys. Recording and transcription run away from the UI thread. Normal dictation keeps canonical PCM in native memory and creates no routine WAV file.

## Microphone and speech detection

Open **General** to watch the selected microphone in **Recording input**. The native meter-only monitor starts while that page is open and does not create a transcript, history entry, or retained audio file.

**AI voice detection** is the default. Its read-only microphone meter shows incoming level while Silero decides what is speech. Choose **Manual volume threshold** when you need a literal cutoff: move the `−72..0 dBFS` input-threshold marker below your normal voice but above resting room noise. Each quieter 30 ms window is replaced with silence before preview, transcription, and retained-history audio. Loud background sounds can still pass manual detection. An explicit shortcut release or Stop action always takes priority over automatic endpointing.

## Linux note

Linux global-hotkey registration is disabled by default because some desktop and X sessions can terminate the app during hook initialization. Use the in-app control, or explicitly opt in:

```bash
SCRIBE_ENABLE_GLOBAL_HOTKEY=1 cargo run
```

Linux output is deliberately clipboard-only; Scribe leaves the completed transcript on the clipboard.
