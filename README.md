# Scribe

Scribe is a lightweight local-first desktop speech-to-text app built with Rust
and egui/eframe. It does not use Tauri, Electron, React, cloud STT, an account
or sync service, a Python server, or a plugin system.

The runtime-consolidated implementation has one logical handler, zero Supported
models, and four Experimental whisper.cpp artifacts. Its final automated Phase
11 gate discovered 623 tests: 614 passed, 0 failed, and 9 explicit
runtime/fixture tests remained ignored. The release is still NO-GO pending the
documented manual and compatibility evidence; see
`docs/SCRIBE_REVAMP_IMPLEMENTATION_REPORT.md`.

The app shell stays small and only invokes an STT runtime when the user records audio and starts transcription.

## Current Features

- Native egui desktop UI with General, Models, History, Advanced, About, and opt-in Debug navigation aligned to the checked-in Scribe design tokens.
- Versioned local JSON settings with field-level salvage, unknown-field preservation, debounced atomic replacement, and legacy migration.
- One-time migration from the old Local Transcriber config path when a Scribe config does not exist.
- Global hotkey support with `Ctrl+Shift+Space` as the default and configurable toggle or hold-to-talk behavior.
- Native microphone capture through `cpal`; callback samples enter a fixed-capacity SPSC ring and native workers perform downmixing, 16 kHz resampling, normalization, metering, VAD, endpointing, and post-roll without sending PCM through the UI.
- One application-level logical runtime handler, `TranscribeCppRuntime`, selected only by the private `RuntimeRouter`. The normalized catalog currently exposes four Experimental whisper.cpp artifacts and zero Supported models.
- Manifest-driven, resumable, exact-hash model/runtime installation with staged native smoke tests, atomic activation, one previous-known-good runtime, and crash recovery.
- Non-blocking native workers for capture, model preload, rolling batch preview, final transcription, and diagnostic latency breakdowns.
- Tray/menu integration with close-to-tray behavior and Show, Hide, Start/Stop Recording, Copy Last Transcript, and Quit actions.
- Optional Windows insertion of the completed transcript into the captured app; other platforms use an explicit clipboard-only fallback.
- Runtime-neutral model metadata for whisper.cpp tiny.en, base.en, small.en, and medium.en. Family/backend distinctions remain private manifest/adapter data.
- Debug comparison selection is explicit: choose installed models, retain drag order, and decode the same native prepared audio through the shared `TranscriptionService`.
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
| Focused-app insertion | Clipboard-only until a focus-safe native adapter is verified. A future input path would belong behind the [XDG Remote Desktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html). | Clipboard-only until a focus-safe native adapter is verified. | Captures and revalidates the original HWND/process/window-generation property, then uses one [`SendInput`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput) batch; activation denial and higher-integrity/elevated targets fall back to copy-only. |

Relevant Apple controls live in Privacy & Security, including
[Microphone](https://support.apple.com/guide/mac-help/change-privacy-security-settings-on-mac-mchl211c911f/mac),
Input Monitoring, and
[Accessibility](https://support.apple.com/guide/mac-help/allow-accessibility-apps-to-access-your-mac-mh43185/mac).

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

Open `Models` to import a local GGUF or install, select, update, and remove
trusted Experimental models. The catalog can search model/language/filename,
filter its trusted results by installed/recommended/multilingual/size metadata,
and sort them; unavailable metadata such as download counts and native
streaming is not presented as a control. Install is disabled with a clear
message when the destination volume cannot satisfy the download plus Scribe's
1 GiB safety reserve. A model is not labelled Supported until its full
compatibility matrix passes; currently none has.

Managed model files live under the app-data `models` directory and managed
runtime packages under `runtimes`. Legacy external paths remain readable where
the private compatibility bridge still needs them, but they are not treated as
managed installs and uninstall never deletes them.

On Windows x64, the primary package is pinned to whisper.cpp v1.9.1 commit
`f049fff95a089aa9969deb009cdd4892b3e74916`. Installation validates the release
archive size and SHA-256, extracts only the 13 allowlisted files, runs native
health/load/transcription/unload/reload smoke behavior in an isolated child,
and atomically activates the result. Valid partials support HTTP Range resume.
Settings fingerprints and recovery journals make activation/removal
restart-safe; exactly one previous known-good runtime is retained for rollback.

The pinned package is CPU-only. `Auto` resolves to the health-tested CPU
backend, `CPU` requests it explicitly, and `GPU` fails clearly because no
verified accelerator package ships. Normalized managed runtime installation
fails closed on platforms without a pinned, measured package.

Runtime selection is private to `RuntimeRouter`. There is one logical handler,
`TranscribeCppRuntime`; `OnnxSpeechRuntime` is absent because the named
Zipformer candidate has not passed the complete evidence gate.

### Runtime packaging and legacy development tools

Release bundles contain only the pinned primary whisper.cpp package and the
application. Build one with:

```bash
scripts/build-release-bundle.sh
```

The bundler stages the primary package under
`target/release/runtimes/whisper_cpp`. The app validates the exact package
manifest before use; packaging variants do not create additional logical
runtime kinds.

The repository retains separate faster-whisper, Vosk, sherpa-onnx, Moonshine,
Parakeet, and CUDA scripts solely for bounded development investigation and
migration compatibility. The release bundler does not invoke them. Their
presence is neither application-level support nor compatibility evidence, and
the normalized UI cannot select those legacy adapters.

Acceleration is runtime-neutral in settings. The shipped Windows package
currently resolves `Auto` and `CPU` to its health-tested CPU backend; explicit
`GPU` reports that no verified accelerator package is installed.

For a local benchmark that uses the same `TranscriptionService` boundary as the
desktop application:

```bash
cargo run --release -- --benchmark path/to/fixture.wav --model whisper_cpp_base_en --output benchmark.json
```

The report contains allowlisted machine, model, backend, capability, and timing
metadata only. It omits transcript text, audio, source paths, runtime output,
and raw error chains, and refuses to overwrite an existing report.

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
- close-to-tray behavior
- automatic focused-app transcript insertion
- clipboard restore after insertion
- paste automation delay

Normal microphone and Debug comparison capture remains canonical PCM in native
memory and creates no routine WAV file. Diagnostics retain at most 50
allowlisted, transcript-free session records in memory. An explicit export
writes only those redacted records and timing/backend metadata.

## Notes

- No model advertises native streaming. Experimental primary models use the
  shared bounded rolling batch preview and keep tentative text in Scribe only.
- Scribe has no cleanup/reasoning pipeline today. If one is added later, it must be local, optional, and off by default, and it must never send audio or text to a cloud service.
- The app does not load models at launch.
- Recording and transcription run off the UI thread.
- Global hotkeys can fail on some Linux Wayland/session configurations; the app remains usable through the Start/Stop button, and non-Windows output is deliberately clipboard-only.
- The window close button hides the app to the tray when tray integration is available. Use the tray Quit action to exit fully.

## Development

```bash
cargo fmt
cargo check
```

The main modules are:

- `src/app.rs`: egui UI, app state, event polling, and background job dispatch.
- `src/audio.rs`: microphone capture into the fixed-capacity native ring.
- `src/audio/pipeline.rs`: native preparation, metering, VAD, and endpointing.
- `src/benchmark.rs`: privacy-bounded command-line benchmark/reporting path.
- `src/config.rs`: local JSON config loading/saving.
- `src/coordinator.rs`: authoritative one-active-session state machine.
- `src/diagnostics.rs`: bounded allowlisted session metrics and redacted export.
- `src/history.rs`: SQLite history lifecycle, retention, retry, and reconciliation.
- `src/hotkey.rs`: global hotkey parsing and registration.
- `src/models.rs`: normalized runtime-neutral model descriptors and catalog.
- `src/runtime_router.rs`: the only application-level concrete runtime selector.
- `src/transcription.rs`: neutral `TranscriptionService` and worker lifecycle.
- `src/streaming.rs`: bounded rolling preview and transcript stabilization.
- `src/text_output.rs`: transactional Windows target insertion and conservative cross-platform clipboard output.
- `src/tray.rs`: tray icon, tray menu, and tray command mapping.
