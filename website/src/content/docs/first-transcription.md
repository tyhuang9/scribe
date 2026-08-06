---
title: First transcription
description: Record a short clip and keep the result local.
---

1. Start Scribe and open **Models**.
2. Install and select a trusted Experimental GGUF variant, or validate a local GGUF in place. The normal path needs no separate runtime package.
3. Open **General**, choose the microphone, and speak while watching **Input sensitivity**. Set its threshold below your normal voice level.
4. Focus the intended destination, then press `Ctrl+Shift+Space`; or use the in-app Start/Stop control when global hotkeys are unavailable.
5. Speak, then stop. The overlay may show committed and tentative rolling-preview text.
6. Review the final transcript. If focused-app insertion is enabled and the Windows target is still safe, Scribe pastes the final text once. Unsafe targets use the clipboard fallback. If another app changes the clipboard during output, Scribe preserves that content and leaves the final transcript in Scribe for manual copying.

Scribe can load an installed selected model during startup integrity/health validation. Session loading also starts concurrently with capture, and a loaded model can remain available briefly for warm reuse.

> All current catalog models are Experimental. A model file being installable does not make it Supported.

> The physical-microphone, VAD, hotkey, overlay, and exactly-once paste vertical slice still requires the documented Windows manual matrix before release qualification.

If the result does not appear, keep the transcript panel open and work through [Troubleshooting](../troubleshooting/).
