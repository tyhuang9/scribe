---
title: Local-first and permissions
description: Feature-level permissions and deliberate local behavior.
---

Scribe asks for OS-sensitive capabilities at the feature level rather than while installing a runtime or model. New configurations keep automatic focused-app insertion disabled until you enable it.

| Capability | What to expect |
| --- | --- |
| Microphone | The operating system or desktop audio stack controls microphone access. |
| Global hotkey | Can be restricted by a desktop session; Linux is opt-in by default. |
| Clipboard | Used for explicit copying or focused-app insertion. |
| Paste automation | Windows only, after target revalidation; unsafe cases and other platforms use the clipboard. |

Scribe does not provide cloud STT, accounts, sync, an always-on listener, a reasoning-cleanup pipeline, or plugins. The normal GGUF path is in-process with no Python or localhost server; private legacy process adapters remain only for configuration/artifact migration. Those boundaries are deliberate.

For platform-specific permission behavior, read the relevant page under **Platforms**.
