---
title: Linux and WSL
description: Linux desktop, Wayland, X11, and WSL behavior.
---

Linux global hotkeys are disabled by default because initialization can destabilize some desktop or X sessions. Use the in-app recording control or opt in with `SCRIBE_ENABLE_GLOBAL_HOTKEY=1`.

Scribe defaults to software rendering on Linux to reduce EGL/Mesa driver failures. Use `SCRIBE_USE_GPU=1` only when the local environment is known to work.

Under WSL, tray integration is disabled by default because AppIndicator/GTK startup can be unreliable in WSLg. The main window remains available. To opt into tray behavior:

```bash
SCRIBE_ENABLE_TRAY=1 cargo run
```

Linux and WSL output is deliberately clipboard-only until a focus-safe native adapter is verified. For WSL display issues, use the documented X11/Wayland overrides in [Settings and environment](../../settings-and-environment/).

The normal GGUF path needs no managed runtime package, but Linux desktop/model combinations are not release-qualified. Development compatibility paths do not change that status.
