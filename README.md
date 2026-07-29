# Scribe

Scribe is a lightweight local-first desktop speech-to-text app built with Rust and egui/eframe. It does not use Tauri, Electron, React, cloud STT, an account or sync service, a Python server, any always-running model process, or a plugin system.

The app shell stays small and only invokes an STT runtime when the user records audio and starts transcription.

## Current Features

- Native egui desktop UI with Transcribe, Models, Playground, and Settings pages aligned to `DESIGN.md`.
- Local JSON config for hotkey, active model, Playground ordering, managed model/runtime metadata, performance mode, theme mode, audio input device, recording duration, live preview, and opt-in voice editing.
- One-time migration from the old Local Transcriber config path when a Scribe config does not exist.
- Global hotkey support with `Ctrl+Shift+Space` as the default; users can type or capture a supported standard key combination and choose toggle or hold-to-talk behavior.
- Local microphone recording through `cpal`, optional microphone device selection, and temporary WAV output through `hound`.
- Six runnable local STT backends: `whisper.cpp`, `faster-whisper`, Vosk, sherpa-onnx, Moonshine, and Parakeet. They use bundled/managed runtime discovery, managed downloaded models, and simple `Auto` / `Prefer GPU` / `CPU only` performance modes where the runtime supports them.
- Experimental sherpa-onnx-family support (sherpa-onnx, Moonshine, and Parakeet) runs through managed, short-lived Python sidecars and currently provides batch transcription only; streaming needs a future backend API.
- Models page install/select/uninstall flow for whisper.cpp `tiny.en`, `base.en`, `small.en`, and `medium.en` files plus faster-whisper, Vosk, sherpa-onnx, Moonshine, and Parakeet model directories.
- Non-blocking UI for recording and transcription using background threads and channels, with a diagnostic latest-transcription latency breakdown.
- Tray/menu integration with close-to-tray behavior and Show, Hide, Start/Stop Recording, Copy Last Transcript, and Quit actions.
- Optional insertion of the completed transcript into the focused app through clipboard plus paste automation.
- Optional current-recording voice commands with deterministic destructive edits and on-demand local Qwen rewriting. Ordinary dictation never starts the editor model.
- Raw, read-only whisper.cpp live preview while recording; commands are evaluated only after authoritative final transcription.
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
  - Moonshine tiny English
  - Parakeet Unified 0.6B int8
- Playground model selection is explicit: choose installed models to test, keep their drag order, and send the same WAV file through every selected ready model. Models with a missing runtime stay visible with repair guidance and block partial runs.
- Transcript copy and clear actions.

## Requirements

- Rust 1.96 or newer.
- Linux, macOS, or Windows desktop session supported by `eframe` and `global-hotkey`.
- A microphone visible to the host OS.
- Real transcription requires a supported runtime discoverable as a bundled sidecar, a managed runtime under the app data directory, or a development fallback environment variable.
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

Wayland tray support and click behavior depend on the compositor's
StatusNotifier/AppIndicator implementation. If tray initialization is
unavailable, close-to-tray is disabled and a normal window close exits Scribe.

On Linux, global hotkey registration is disabled by default because some
desktop/X sessions terminate the app when global key hooks are initialized.
Use the in-app Start/Stop button, or opt in explicitly:

```bash
SCRIBE_ENABLE_GLOBAL_HOTKEY=1 cargo run
```

## Permissions And Input Automation

Scribe should ask for OS-sensitive behavior at the feature level, not during
runtime/model installation. New configs keep focused-app insertion disabled
until the user enables it in Settings. Linux global hotkeys remain opt-in with
`SCRIBE_ENABLE_GLOBAL_HOTKEY=1`.

| Capability | Linux | macOS | Windows |
| --- | --- | --- | --- |
| Microphone capture | Desktop/session prompt depends on the audio stack. | Requires macOS Microphone privacy access. | Uses the normal desktop audio capture path; no installer permission is expected. |
| Global hotkeys | Disabled by default. A future Wayland-native path should use the [XDG Global Shortcuts portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html). | May require Input Monitoring depending on the global-hotkey backend and OS version. | Uses the system-wide [`RegisterHotKey`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey) API; no installer permission prompt is expected. |
| Clipboard access | Used only for copy or focused-app insertion. | Used only for copy or focused-app insertion. | Used only for copy or focused-app insertion. |
| Focused-app insertion | Uses clipboard plus paste automation; Wayland falls back to clipboard-only. A portal-based input path would belong behind the [XDG Remote Desktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html). | Requires user-granted Accessibility access when macOS blocks synthetic input. | Uses paste-key automation through [`SendInput`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput); it cannot reliably inject into higher-integrity/elevated apps. |

Relevant Apple controls live in Privacy & Security, including
[Microphone](https://support.apple.com/guide/mac-help/change-privacy-security-settings-on-mac-mchl211c911f/mac),
Input Monitoring, and
[Accessibility](https://support.apple.com/guide/mac-help/allow-accessibility-apps-to-access-your-mac-mh43185/mac).

## Run

```bash
cargo run
```

The native recording control uses short, bounded state, hover, and press
transitions. To make those transitions immediate for the current process, set
the reduced-motion override before starting Scribe:

```bash
SCRIBE_REDUCED_MOTION=1 cargo run
```

The override is read once at startup and is not written to the Scribe config.

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

Open `Models` to install a local model, select it, or uninstall it. The standard release always includes the small CPU whisper.cpp runtime. Other backends and GPU packs are optional downloads only when the build embeds trusted release metadata for the current OS, architecture, and device pack. If a release has no matching metadata, the action is disabled; Scribe does not guess a URL or trust mutable remote metadata.

Managed model files live under the app data `models` directory. Managed runtime copies live under the app data `runtimes` directory. Legacy external model paths can still be read when valid, but they are not treated as app-managed installs and are not deleted by uninstall.

Runtime discovery is internal. CPU mode uses bundled whisper.cpp CPU. Auto uses an installed GPU pack only when its version, SHA-256, platform, and device metadata exactly match the catalog embedded in this build, then falls back to bundled CPU. Prefer GPU requires a verified GPU pack (or explicit GPU product) and never silently selects the CPU bundle. Unix development builds can still use `SCRIBE_*_CLI` paths as development fallbacks.

When running a debug build from a source checkout on Unix, the Models page can
also use the checked-in `scripts/bundle-*-runtime.sh` helpers as a development
fallback. If a packaged sidecar is not already staged, clicking `Install
runtime` builds the runtime directly into Scribe's managed app-data runtime
directory. This source-checkout bundle-script fallback is Unix-only: Windows
development builds need packaged sidecars staged next to the executable, or explicit
development runtime paths through the corresponding `SCRIBE_*_CLI` environment
variables. Release builds do not use source-checkout scripts unless
`SCRIBE_ALLOW_DEV_RUNTIME_INSTALL=1` is set for explicit Unix development or
smoke testing.

Builds can stage the supported whisper.cpp runtime next to the executable:

```bash
scripts/bundle-whisper-runtime.sh
```

By default this copies the CPU-capable whisper.cpp sidecar into
`target/debug/runtimes/whisper_cpp`. Release CI first packages each optional portable runtime and builds a catalog containing its real immutable URL, SHA-256, and exact sizes:

```bash
python3 scripts/package-runtime-artifact.py \
  --runtime-dir /ci/portable/vosk --runtime-id vosk --version 0.3.45 \
  --os linux --arch x86_64 --device cpu --entrypoint bin/scribe-vosk \
  --release-base-url "$RELEASE_BASE_URL" \
  --catalog-version 1.0.0 --output-dir dist/artifacts \
  --catalog dist/runtime-artifacts.json

# Run once after all platform packagers finish writing parallel-safe fragments.
python3 scripts/package-runtime-artifact.py --merge-catalog-fragments \
  --catalog-version 1.0.0 --catalog dist/runtime-artifacts.json

WHISPER_BUILD_DIR=/ci/whisper-build \
WHISPER_SOURCE_VERSION=1.7.6 \
WHISPER_SOURCE_COMMIT="${PINNED_WHISPER_COMMIT:?set PINNED_WHISPER_COMMIT to the audited lowercase full commit}" \
SCRIBE_RUNTIME_ARTIFACT_CATALOG=dist/runtime-artifacts.json \
scripts/build-release-bundle.sh --mode standard
```

`RELEASE_BASE_URL` must be the real immutable release directory; the packager rejects reserved placeholder hosts. `build.rs` embeds `SCRIBE_RUNTIME_ARTIFACT_CATALOG` before Cargo compiles the app. The standard product contains only bundled CPU whisper.cpp. `--mode offline-cpu` is a separate all-CPU product requiring relocatable platform-CI runtime inputs. `--mode gpu` is a separate whisper.cpp GPU product. `scripts/build-release-bundle.ps1` provides the equivalent Windows flow with `-WhisperBuildDir`, pinned provenance, and `-CatalogPath`. An explicit `SCRIBE_ALLOW_EMPTY_RUNTIME_CATALOG=1` or `-AllowEmptyCatalog` produces a CPU-only release; it is not the hybrid release default.

`package-runtime-artifact.py` rejects links, raw virtual environments, missing/mismatched manifests, duplicate target tuples, cross-target packaging, and oversized packages. It runs the target-native entrypoint with `--help` before publishing. Parallel packaging jobs write independent catalog fragments; the explicit merge step rejects duplicate tuples and publishes one deterministic catalog. Release CI must upload the generated ZIPs at the catalog URLs; this repository does not claim that artifact hosting already exists.

Runtime activation and configuration replacement are journaled for process-crash recovery. Before activation, Scribe flushes every staged runtime file and its directory tree from the leaves through the staging root. For power-loss durability, Scribe flushes file contents and containing-directory metadata on Unix and uses write-through moves on Windows; Windows removals first move entries to ignored same-directory tombstones with write-through before reclaiming them. A successful durability barrier permits transaction cleanup. If a post-commit barrier fails, Scribe reports a warning and retains the journal and backup so startup can finish or roll back from the configuration that actually survived. These guarantees depend on the filesystem and storage hardware honoring `fsync`/write-through requests.

### Current-recording voice editor

Voice editing is off by default and currently available on Windows x64. Settings offers Compact Qwen3 0.6B Q8 and Balanced Qwen3 1.7B Q8. The standard release bundle still contains only the CPU whisper.cpp STT runtime; the pinned llama.cpp runtime and selected Qwen model are downloaded only after an explicit Install action.

The reserved English commands are `scratch that`, `undo that`, `start over`, `new line`, `new paragraph`, `replace X with Y`, `make that ...`, `rewrite that ...`, and `turn that into ...`. Prefix a reserved phrase with `literal` to dictate it. Deterministic commands run locally without AI. Only an explicit rewrite command starts an ephemeral `llama-server`, which exits after its bounded request.

Final results carry a recording session ID. Stale transcription or edit events are ignored, preview text cannot execute commands, and external output happens at most once. AI ambiguity, invalid output, timeout, or failure preserves the original transcript and suppresses automatic insertion until the user chooses Retry, Use original, or Copy. Scribe retains the original only in memory until the next recording or exit.

Voice-AI release metadata is intentionally separate from the bundled whisper runtime. Release CI must obtain the exact audited upstream llama.cpp b9637 ZIP/license and Qwen GGUF files, publish byte-identical artifacts at an immutable Scribe-controlled HTTPS base URL, then build a schema-2 catalog:

```powershell
python scripts/prepare-llama-runtime.py `
  --archive dist/upstream/llama-b9637-bin-win-cpu-x64.zip `
  --license-file dist/upstream/llama.cpp-LICENSE `
  --output-dir dist/portable/voice-intent-llama

python scripts/package-runtime-artifact.py `
  --runtime-dir dist/portable/voice-intent-llama `
  --runtime-id voice_intent_llama_cpp --version b9637 `
  --os windows --arch x86_64 --device cpu `
  --entrypoint bin/llama-server.exe `
  --release-base-url $env:RELEASE_BASE_URL `
  --catalog-version 1.0.0 --output-dir dist/artifacts `
  --catalog dist/runtime-artifacts.json

python scripts/package-runtime-artifact.py --merge-catalog-fragments `
  --catalog-version 1.0.0 --catalog dist/runtime-artifacts.json

python scripts/package-intent-model-artifact.py --tier compact `
  --model-file dist/upstream/Qwen3-0.6B-Q8_0.gguf `
  --release-base-url $env:RELEASE_BASE_URL --output-dir dist/artifacts `
  --catalog-version 1.0.0 --catalog dist/runtime-artifacts.json

python scripts/package-intent-model-artifact.py --tier balanced `
  --model-file dist/upstream/Qwen3-1.7B-Q8_0.gguf `
  --release-base-url $env:RELEASE_BASE_URL --output-dir dist/artifacts `
  --catalog-version 1.0.0 --catalog dist/runtime-artifacts.json

python scripts/package-intent-model-artifact.py --verify-ready `
  --os windows --arch x86_64 `
  --catalog-version 1.0.0 --catalog dist/runtime-artifacts.json

.\scripts\build-release-bundle.ps1 -Mode Standard -VoiceAi `
  -WhisperBuildDir C:\ci\whisper-build -WhisperVersion 1.7.6 `
  -WhisperSourceCommit $env:PINNED_WHISPER_COMMIT `
  -CatalogPath dist/runtime-artifacts.json
```

The checked-in development catalog deliberately has no runtime or model download URL. `-VoiceAi` and `SCRIBE_BUILD_VOICE_AI=1` fail the build unless the current platform runtime and both exact model tiers are present. Hugging Face redirects are not embedded or followed by the app.

To stage only the faster-whisper runtime during development:

```bash
scripts/bundle-faster-whisper-runtime.sh
```

The development faster-whisper runtime is a generated Python virtual environment with a
small Scribe runner and is not a portable production artifact. Python package versions are pinned by
`scripts/runtime-dependencies.env`; release builds can override those pins with
the matching `SCRIBE_*_VERSION` environment variables. The runner downloads
CTranslate2 faster-whisper model directories through faster-whisper's Hugging
Face integration when a model is installed from the app.

To check whether pinned runtime dependencies have newer PyPI releases:

```bash
scripts/check-runtime-dependency-updates.py
```

Production Python runtimes must instead be built as relocatable standalone packages by target-platform CI. The app itself does not install arbitrary latest PyPI packages on user machines.
It asks users to update managed runtimes when installed runtime metadata is older
than the version recorded in Scribe's runtime catalog.

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

To stage only one of the sherpa-onnx-family runtimes during development:

```bash
scripts/bundle-sherpa-onnx-runtime.sh
scripts/bundle-moonshine-runtime.sh
scripts/bundle-parakeet-runtime.sh
```

These runtimes are generated Python virtual environments with pinned
`sherpa-onnx`, `sherpa-onnx-bin`, and `numpy` dependencies plus a small Scribe
runner. Each backend gets a separate managed runtime directory and wrapper, but
they share the same runner. The runner downloads official sherpa-onnx model
archives for:

- `sherpa-onnx-zipformer-small-en-2023-06-26.tar.bz2`
- `sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2`
- `sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2`

The model archives are validated by required ONNX/ORT and `tokens.txt` files
before Scribe marks them installed. Release packaging should record SHA256
checksums for these archives before distributing fixed bundles.

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
whisper.cpp and asks faster-whisper for CPU/int8 mode. Vosk, sherpa-onnx,
Moonshine, and Parakeet are CPU-oriented in this build and ignore the GPU
preference.

## Config

Settings are stored in a platform-specific config directory using the `directories` crate.

Scribe stores new config under the Scribe application directory. On first launch, if no Scribe config exists and an old Local Transcriber config is present, Scribe reads and saves a migrated copy into the new config path.

The config stores:

- selected default model
- Playground-selected models (the persisted `playground_selected_models` key; older `playground_enabled_models` and `enabled_models` keys migrate on load)
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
- live transcription preview toggle
- voice editing toggle and Compact/Balanced tier
- close-to-tray behavior
- automatic focused-app transcript insertion
- clipboard restore after insertion
- paste automation delay

Temporary WAV files are deleted after transcription in normal operation. The latest transcription latency breakdown is diagnostic-only and is not persisted.

## Notes

- sherpa-onnx, Moonshine, and Parakeet use experimental, managed sherpa-onnx Python sidecars in this build. The sidecars are short-lived local processes and currently run batch transcription only; true streaming partial transcription still needs a `SttBackend` streaming API.
- Voice editing is local, optional, and off by default. Normal dictation bypasses it; only exact command candidates enter the deterministic editor or short-lived local rewrite process.
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
- `src/intent_server.rs`: authenticated, bounded, one-shot llama.cpp rewrite transaction.
- `src/live_preview.rs`: raw provisional whisper.cpp preview state and overlap merging.
- `src/models.rs`: shared STT model/result/status structs.
- `src/stt/mod.rs`: backend trait and dispatch.
- `src/stt/whisper_cpp.rs`: whisper.cpp child-process integration.
- `src/stt/faster_whisper.rs`: faster-whisper child-process integration.
- `src/stt/sherpa_onnx.rs`: sherpa-onnx-family child-process integration for sherpa-onnx, Moonshine, and Parakeet.
- `src/text_output.rs`: focused-app transcript insertion through clipboard plus paste automation.
- `src/tray.rs`: tray icon, tray menu, and tray command mapping.
- `src/voice_editor.rs`: deterministic current-recording command parser and edit evaluator.
