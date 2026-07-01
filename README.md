# Scribe

Scribe is a lightweight local-first desktop speech-to-text app built with Rust and egui/eframe. It does not use Tauri, Electron, React, a Python server, cloud STT, account sync, or any always-running model process.

The app shell stays small and only invokes an STT runtime when the user records audio and starts transcription.

## Current Features

- Native egui desktop UI with Transcribe, Models, Playground, and Settings pages aligned to `DESIGN.md`.
- Local JSON config for hotkey, active model, Playground ordering, managed model/runtime metadata, performance mode, theme mode, audio input device, and max recording duration.
- One-time migration from the old Local Transcriber config path when a Scribe config does not exist.
- Global hotkey support with `Ctrl+Shift+Space` as the default and configurable toggle or hold-to-talk behavior.
- Local microphone recording through `cpal`, optional microphone device selection, and temporary WAV output through `hound`.
- `whisper.cpp`, `faster-whisper`, and Vosk backend integration through bundled/managed runtime discovery, managed downloaded models, and simple `Auto` / `Prefer GPU` / `CPU only` performance modes where the runtime supports them.
- Models page install/select/uninstall flow for whisper.cpp `tiny.en`, `base.en`, `small.en`, and `medium.en` files plus faster-whisper and Vosk model directories.
- Non-blocking UI for recording and transcription using background threads and channels, with a diagnostic latest-transcription latency breakdown.
- Tray/menu integration with close-to-tray behavior and Show, Hide, Start/Stop Recording, Copy Last Transcript, and Quit actions.
- Optional insertion of the completed transcript into the focused app through clipboard plus paste automation.
- Model metadata for:
  - whisper.cpp tiny.en
  - whisper.cpp base.en
  - whisper.cpp small.en
  - whisper.cpp medium.en
  - Vosk small English
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
- Playground that shows the full catalog, keeps enable/disable controls for testing, supports persisted drag reordering, disabled-model grouping, and sends the same WAV file through enabled models.
- Transcript copy and clear actions.

## Requirements

- Rust 1.96 or newer.
- Linux, macOS, or Windows desktop session supported by `eframe` and `global-hotkey`.
- A microphone visible to the host OS.
- Real transcription requires a whisper.cpp, faster-whisper, or Vosk runtime discoverable as a bundled sidecar, a managed runtime under the app data directory, or a development fallback environment variable.
- NVIDIA GPU transcription requires an NVIDIA driver plus a runtime that can use CUDA or another supported GPU backend.

On Ubuntu, install the microphone and tray build dependencies:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config libasound2-dev libgtk-3-dev libayatana-appindicator3-1 libayatana-appindicator3-dev
```

If your distribution uses the older AppIndicator package names, install
`libappindicator3-1` and `libappindicator3-dev` instead. Scribe can still run
without these tray libraries; close-to-tray behavior is simply disabled.

To bypass tray startup entirely while debugging desktop-session issues:

```bash
SCRIBE_DISABLE_TRAY=1 cargo run
```

When running under WSL, Scribe disables tray integration by default because
AppIndicator/GTK tray initialization is unreliable in WSLg. The main window
still works. To opt into tray behavior under WSL:

```bash
SCRIBE_ENABLE_TRAY=1 cargo run
```

On Linux, global hotkey registration is disabled by default because some
desktop/X sessions terminate the app when global key hooks are initialized.
Use the in-app Start/Stop button, or opt in explicitly:

```bash
SCRIBE_ENABLE_GLOBAL_HOTKEY=1 cargo run
```

## Run

```bash
cargo run
```

On Linux, the app defaults to software rendering to avoid common EGL/Mesa driver
crashes in lightweight desktop or WSL-style environments. To opt back into GPU
rendering:

```bash
SCRIBE_USE_GPU=1 cargo run
```

The old `LOCAL_TRANSCRIBER_USE_GPU=1` opt-in is still accepted for compatibility.

On Linux, Winit's automatic backend selection can be brittle under WSLg when
both `WAYLAND_DISPLAY` and `DISPLAY` are advertised. Scribe avoids that auto
path under WSL by choosing Wayland explicitly when it is available, then X11 as
a fallback. If startup still fails, restart WSLg from Windows PowerShell:

```powershell
wsl.exe --shutdown
```

Then reopen WSL and run `cargo run` again. To override Scribe's WSL default and
force X11 for one run:

```bash
SCRIBE_FORCE_X11=1 cargo run
```

To force Wayland for one run:

```bash
SCRIBE_FORCE_WAYLAND=1 cargo run
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

## Models and Runtime

Open `Models` to install a local whisper.cpp, faster-whisper, or Vosk model, select the active model, or uninstall models to free storage. Scribe stores managed models under the app data directory and does not expose model path settings in the normal UI. The Models view shows the runtime each model uses plus rough model/runtime storage estimates before install.

Managed model files live under the app data `models` directory. Managed runtime copies live under the app data `runtimes` directory. Legacy external model paths can still be read when valid, but they are not treated as app-managed installs and are not deleted by uninstall.

Runtime discovery is internal. Scribe checks for bundled runtime sidecars next to the executable, then managed runtime copies under the app data directory. Development builds can still use `SCRIBE_WHISPER_CPP_CLI`, `SCRIBE_WHISPER_CUDA_CLI`, `SCRIBE_FASTER_WHISPER_CLI`, or `SCRIBE_VOSK_CLI` as fallback runtime paths.

When running from a source checkout on Unix, the Models page can also use the
checked-in `scripts/bundle-*-runtime.sh` helpers as a development fallback. If a
packaged sidecar is not already staged, clicking `Install runtime` builds the
runtime directly into Scribe's managed app-data runtime directory.

Builds can stage the supported whisper.cpp runtime next to the executable:

```bash
scripts/bundle-whisper-runtime.sh
```

By default this copies the CPU-capable whisper.cpp sidecar into
`target/debug/runtimes/whisper_cpp`. For a release build, run:

```bash
scripts/build-release-bundle.sh
```

The release bundle places whisper.cpp files under
`target/release/runtimes/whisper_cpp` and stages the faster-whisper Python
sidecar under `target/release/runtimes/faster_whisper`. It also stages the
Vosk Python sidecar under `target/release/runtimes/vosk`. These are the same
locations the app checks before falling back to user-managed or development
runtime paths.

To stage only the faster-whisper runtime during development:

```bash
scripts/bundle-faster-whisper-runtime.sh
```

The faster-whisper runtime is a generated Python virtual environment with a
small Scribe runner. The runner downloads CTranslate2 faster-whisper model
directories through faster-whisper's Hugging Face integration when a model is
installed from the app.

To stage only the Vosk runtime during development:

```bash
scripts/bundle-vosk-runtime.sh
```

The Vosk runtime is a generated Python virtual environment pinned to
`vosk==0.3.45` with a small Scribe runner. The runner downloads and extracts
`vosk-model-small-en-us-0.15.zip` from the official Vosk model catalog when
the Vosk small English model is installed from the app. The Vosk catalog lists
that model as 40M and Apache 2.0. The upstream model catalog does not publish a
checksum alongside the ZIP; release packaging should record a SHA256 in the
runtime manifest before distributing a fixed bundle.

GPU-capable bundles are intentionally opt-in because the CUDA runtime payload is
large. On a machine with Ollama's CUDA libraries available, run:

```bash
SCRIBE_BUNDLE_CUDA=1 scripts/build-release-bundle.sh
```

This copies `libggml-cuda.so` plus its required CUDA shared libraries into the
bundled runtime. The app prefers those bundled CUDA libraries over host-specific
CUDA config when they are present. When both Ollama CUDA v12 and v13 runtimes
exist, the bundler prefers v12 for wider driver compatibility. Set
`CUDA_RUNTIME_DIR=/path/to/cuda_v13` or another CUDA runtime directory to
override that choice.

`Settings` exposes one performance control shared by supported local runtimes:

- `Auto`: let the runtime choose the device.
- `Prefer GPU`: pass the selected GPU device to the runtime.
- `CPU only`: force CPU mode.

For CUDA development without installing the full CUDA Toolkit, Scribe can use a dynamic-backend whisper.cpp build with Ollama's local CUDA runtime:

```bash
scripts/build-whisper-ollama-cuda-backend.sh
```

Then launch with the runtime path in the environment, for example:

```bash
SCRIBE_WHISPER_CPP_CLI=/home/tyhuang/Projects/whisper.cpp/build-dl-ollama/bin/whisper-cli cargo run
```

For a native CUDA Toolkit build, install a toolkit that provides `nvcc`, then
build the adjacent whisper.cpp checkout with GGML_CUDA enabled:

```bash
sudo apt-get install -y nvidia-cuda-toolkit
scripts/build-whisper-cuda.sh
```

The backend calls whisper.cpp like this:

```bash
whisper-cli -m /path/to/model.bin -f /path/to/audio.wav -nt -dev 0
```

`Auto` omits explicit whisper.cpp device flags and lets faster-whisper fall back
to CPU when CUDA is unavailable. `Prefer GPU` appends `-dev <device>` for
whisper.cpp and asks faster-whisper for CUDA. `CPU only` appends `-ng` for
whisper.cpp and asks faster-whisper for CPU/int8 mode. Vosk is CPU-oriented in
this build and ignores the GPU preference.

## Config

Settings are stored in a platform-specific config directory using the `directories` crate.

Scribe stores new config under the Scribe application directory. On first launch, if no Scribe config exists and an old Local Transcriber config is present, Scribe reads and saves a migrated copy into the new config path.

The config stores:

- selected default model
- Playground-enabled models
- persisted model playground order
- managed model install metadata
- managed runtime install metadata
- global hotkey and hotkey mode
- performance mode and internal whisper.cpp runtime options
- theme mode
- optional audio input device name
- last used backend
- deprecated path/debug fields for migration compatibility
- max recording duration
- close-to-tray behavior
- automatic focused-app transcript insertion
- clipboard restore after insertion
- paste automation delay

Temporary WAV files are deleted after transcription in normal operation. The latest transcription latency breakdown is diagnostic-only and is not persisted.

## Notes

- sherpa-onnx, Moonshine, and Parakeet have provider adapters and catalog entries, but their managed runtime packages are not bundled yet. Normal model install actions remain disabled until a backend has a runtime package.
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
- `src/core.rs`: testable recording/transcription workflow reducer.
- `src/hotkey.rs`: global hotkey parsing and registration.
- `src/models.rs`: shared STT model/result/status structs.
- `src/stt/mod.rs`: backend trait and dispatch.
- `src/stt/whisper_cpp.rs`: whisper.cpp child-process integration.
- `src/stt/faster_whisper.rs`: faster-whisper child-process integration.
- `src/text_output.rs`: focused-app transcript insertion through clipboard plus paste automation.
- `src/tray.rs`: tray icon, tray menu, and tray command mapping.
