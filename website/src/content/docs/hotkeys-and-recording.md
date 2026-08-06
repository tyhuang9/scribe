---
title: Hotkeys and recording
description: Use the default hotkey or the in-app recording control.
---

The default global hotkey is `Ctrl+Shift+Space`. In **General**, choose whether it toggles recording or works as hold-to-talk, then select a different hotkey if necessary.

The in-app Start/Stop control remains the dependable fallback when a desktop session blocks global hotkeys. Recording and transcription run away from the UI thread. Normal dictation keeps canonical PCM in native memory and creates no routine WAV file.

## Microphone and speech detection

Open **General** to watch the selected microphone with the **Input sensitivity** slider. The native meter-only monitor starts while that page is open: its live fill shows incoming level and its thumb sets the speech-activation threshold. It does not create a transcript, history entry, or retained audio file. Input above 0% can still be background noise below the threshold; move the threshold below your speaking level while leaving it above the room's noise floor.

Scribe's voice activity detector (VAD) decides which frames contain speech. It uses a noise floor, hysteresis, pre-roll, post-roll, and silence timing so brief fluctuations do not clip or repeatedly toggle speech. An explicit shortcut release or Stop action always takes priority over automatic endpointing.

## Linux note

Linux global-hotkey registration is disabled by default because some desktop and X sessions can terminate the app during hook initialization. Use the in-app control, or explicitly opt in:

```bash
SCRIBE_ENABLE_GLOBAL_HOTKEY=1 cargo run
```

Linux output is deliberately clipboard-only; Scribe leaves the completed transcript on the clipboard.
