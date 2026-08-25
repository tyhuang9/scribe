---
title: Install and run
description: Install Scribe on Windows or prepare a desktop development environment from source.
---

## Windows installer

[Download Scribe for Windows](https://github.com/tyhuang9/scribe/releases/latest/download/Scribe-Setup.exe){.primary-link}

The installer is for Windows x64 and installs Scribe for the current user, with
a Start menu entry and uninstaller. It includes the pinned English Base model,
its attribution notices, the compatible runtime files, and a hash inventory.
Older installers remain on the [Scribe Releases page](https://github.com/tyhuang9/scribe/releases).

The installer is currently unsigned. Windows may show a SmartScreen or unknown
publisher warning. Download it only from the link above or the official Releases
page, confirm the repository is `tyhuang9/scribe`, and review the release notes
before deciding whether to continue.

## Before you start

For source builds, you need a current stable Rust toolchain with Rust 2024 edition support, a desktop session compatible with `eframe` and `global-hotkey`, and a microphone visible to the operating system. Recorded automated verification used Rust 1.96.0; the project does not currently declare a tested minimum Rust version. Windows x64 is the primary release target; Linux and macOS retain conservative fallbacks but are not release-qualified.

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

A normal transcription needs only an installed compatible GGUF model. Scribe's `transcribe-cpp` 0.1.3 CPU adapter is statically linked and runs in-process, so it does not download a runtime package or start a sidecar process. The pinned Windows whisper.cpp package serves retained legacy GGML and a narrowly scoped bootstrap fallback when the primary native GGUF adapter cannot initialize. Development fallbacks are compatibility tools, not evidence of Supported status. Read [Models and runtimes](../models-and-runtimes/) before installing a model.
