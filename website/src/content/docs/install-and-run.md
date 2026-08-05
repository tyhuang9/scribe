---
title: Install and run
description: Prepare a supported desktop environment and start Scribe from source.
---

## Before you start

Scribe currently runs from a source checkout. You need Rust 1.96 or newer, a desktop session supported by `eframe` and `global-hotkey`, and a microphone visible to the operating system. Windows x64 is the primary release target; Linux and macOS retain conservative fallbacks but are not release-qualified.

See [Project status](../project-status/) for the current scope and the repository references that maintain this guide.

## Start Scribe

From the repository root:

```bash
cargo run
```

For a quick compile check without launching the app:

```bash
cargo check
```

## Linux dependencies

On Ubuntu, install the microphone and tray build dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libasound2-dev libgtk-3-dev libayatana-appindicator3-1 libayatana-appindicator3-dev
```

Some distributions use the older `libappindicator3` package names. Scribe can run without tray libraries; close-to-tray behavior is then unavailable.

## Runtime requirement

A normal transcription needs only an installed compatible GGUF model. Scribe's `transcribe-cpp` 0.1.3 CPU adapter is statically linked and runs in-process, so it does not download a runtime package or start a sidecar process. The pinned Windows whisper.cpp package belongs to the retained legacy GGML path. Development fallbacks are compatibility tools, not evidence of Supported status. Read [Models and runtimes](../models-and-runtimes/) before installing a model.
