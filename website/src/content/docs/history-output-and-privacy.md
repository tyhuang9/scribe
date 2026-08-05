---
title: History, output, and privacy
description: Know where a transcript goes and what Scribe does not store or send.
---

## Transcript output

The app keeps the latest transcript visible, with copy and clear actions. Tentative preview text stays in Scribe. It can optionally insert finalized text into the captured Windows application by using the clipboard and one paste action.

Enable focused-app insertion deliberately in **Advanced**. Scribe revalidates the original Windows target immediately before output and pastes final text exactly once. If the target or clipboard state is unsafe, it copies the result and reports the fallback without synthetic keystrokes. Linux and macOS are deliberately clipboard-only.

## Local history

History is a private local SQLite store with optional separate audio files. The default mode is **Transcript only**, audio retention is Off, and retention keeps at most 20 unpinned entries unless you change it. **Off** creates no new entries; **Transcript + audio** enables local retry and playback at the cost of retaining speech audio.

The History page supports search, pagination, copy, Windows-safe Paste again, pinning, deletion, playback, retention, and retry when retained audio exists. Startup reconciles interrupted pending rows and audio-file changes.

## Local-first boundary

Scribe has no cloud speech-to-text service, user accounts, synchronization, or plugin system. The normal GGUF path is in-process and has no Python or localhost server. Private legacy process adapters remain for configuration/artifact migration and are not selectable through the normal UI. Settings, history, and managed model metadata stay in platform-specific local app-data locations.

Normal capture creates no temporary WAV. Retained audio exists only when you select **Transcript + audio**. Review text before copying or pasting it into another app: that destination may have its own storage and privacy policy.
