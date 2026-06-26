# Local Transcriber

Local Transcriber is a lightweight local-first desktop speech-to-text MVP built with Rust and egui/eframe. It does not use Tauri, Electron, React, a Python server, or any always-running model process.

The app shell stays small and only invokes an STT runtime when the user records audio and starts transcription.

## Current Features

- Native egui desktop UI with Transcribe, Models, Model Playground, and Settings pages.
- Local JSON config for hotkey, model selections, executable paths, model paths, debug mode, and max recording duration.
- Global hotkey support with `Ctrl+Shift+Space` as the default.
- Local microphone recording through `cpal`, saved as a temporary WAV file through `hound`.
- `whisper.cpp` backend integration through a configured executable path and model file path.
- Non-blocking UI for recording and transcription using background threads and channels.
- Tray/menu integration with close-to-tray behavior and Show, Hide, Start/Stop Recording, Copy Last Transcript, and Quit actions.
- Optional insertion of the completed transcript into the focused app through clipboard plus paste automation.
- Model metadata for:
  - whisper.cpp tiny.en
  - whisper.cpp base.en
  - whisper.cpp small.en
  - whisper.cpp medium.en
  - Vosk small English placeholder
  - faster-whisper tiny.en
  - faster-whisper base.en
  - faster-whisper small.en
  - faster-whisper medium.en
  - faster-whisper large-v3
  - faster-whisper turbo
  - faster-whisper distil-large-v3
  - sherpa-onnx Zipformer Small
  - Moonshine
  - Parakeet 0.6B
- Model Playground that shows the full catalog, supports enable/disable, persisted drag reordering, disabled-model grouping, and sends the same WAV file through enabled models.
- Transcript copy and clear actions.

## Requirements

- Rust 1.96 or newer.
- Linux, macOS, or Windows desktop session supported by `eframe` and `global-hotkey`.
- A microphone visible to the host OS.
- `whisper.cpp` built separately if you want real transcription.

On Ubuntu, install the microphone and tray build dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libasound2-dev libgtk-3-dev libappindicator3-dev
```

## Run

```bash
cargo run
```

On Linux, the app defaults to software rendering to avoid common EGL/Mesa driver
crashes in lightweight desktop or WSL-style environments. To opt back into GPU
rendering:

```bash
LOCAL_TRANSCRIBER_USE_GPU=1 cargo run
```

If your system still reports Mesa/Zink/EGL errors, run it explicitly with Mesa's
software driver:

```bash
LIBGL_ALWAYS_SOFTWARE=1 cargo run
```

For a quick compile check:

```bash
cargo check
```

## Configure whisper.cpp

This MVP does not bundle whisper.cpp or model files.

1. Build whisper.cpp separately.
2. Download a compatible `.bin` model, such as `ggml-tiny.en.bin`.
3. Launch Local Transcriber.
4. Open `Models`.
5. Set the `whisper.cpp executable` path.
   - Newer whisper.cpp builds usually produce `whisper-cli`.
   - Older builds may produce `main`.
6. Set the model path for the whisper.cpp model you want to use.
7. Select that model as the default.
8. Return to `Transcribe`, record, stop, and wait for the transcript.

The backend calls whisper.cpp like this:

```bash
whisper-cli -m /path/to/model.bin -f /path/to/audio.wav -nt
```

## Config

Settings are stored in a platform-specific config directory using the `directories` crate. The exact config file path is shown on the Settings page after launch.

The config stores:

- selected default model
- enabled models
- persisted model playground order
- global hotkey
- whisper.cpp executable path
- model file paths
- last used backend
- debug mode
- max recording duration
- close-to-tray behavior
- automatic focused-app transcript insertion
- clipboard restore after insertion
- paste automation delay

Temporary WAV files are deleted after transcription unless debug mode is enabled.

## Notes

- Non-whisper.cpp backends are visible and configurable for playground planning, but intentionally return clear "not wired yet" errors.
- The app does not load models at launch.
- Recording and transcription run off the UI thread.
- Global hotkeys and paste automation can fail on some Linux Wayland/session configurations; the app remains usable through the Start/Stop button and falls back to copying transcripts to the clipboard.
- The window close button hides the app to the tray when tray integration is available. Use the tray Quit action to exit fully.

## Development

```bash
cargo fmt
cargo check
```

The main modules are:

- `src/app.rs`: egui UI, app state, event polling, and background job dispatch.
- `src/audio.rs`: microphone capture and temporary WAV writing.
- `src/config.rs`: local JSON config loading/saving.
- `src/hotkey.rs`: global hotkey parsing and registration.
- `src/models.rs`: shared STT model/result/status structs.
- `src/stt/mod.rs`: backend trait and dispatch.
- `src/stt/whisper_cpp.rs`: whisper.cpp child-process integration.
- `src/text_output.rs`: focused-app transcript insertion through clipboard plus paste automation.
- `src/tray.rs`: tray icon, tray menu, and tray command mapping.
