# Scribe

Scribe is a lightweight local-first desktop speech-to-text app built with Rust
and egui/eframe. It does not use Tauri, Electron, React, cloud STT, an account
or sync service, or a plugin system. The normal GGUF path is in-process and has
no Python, localhost server, runtime package, or inference executable.

The runtime-consolidated implementation has private GGUF and ONNX runtime
variants and zero Supported models. The normal UI exposes package-free embedded
GGUF models: one pinned fallback plus trusted discovered or locally imported
variants, all Experimental. Three older GGML records remain resolution-only
migration compatibility. The release is still NO-GO pending the documented
manual and compatibility evidence; see
`docs/SCRIBE_REVAMP_IMPLEMENTATION_REPORT.md` and the newer
`docs/EMBEDDED_STT_AND_MODELS.md` implementation record.

The application/runtime boundary remains narrow. Scribe may invoke the selected
runtime during startup integrity/health validation, and it starts session model
loading concurrently when the user records audio.

## Documentation

The curated documentation site lives in `website/` and is published with GitHub
Pages at [tyhuang9.github.io/scribe](https://tyhuang9.github.io/scribe/). The
checked-in application code, this README, and the revamp implementation report
remain the source of truth for implementation and release-readiness claims.

To maintain the site locally:

```bash
npm ci --prefix website
npm run docs:dev
npm run docs:check
npm run docs:build
```

The documentation workflow checks pull requests that change the site and
deploys eligible changes after they reach `main`. GitHub Pages is configured to
use **GitHub Actions** as its source. The default project site uses
`SITE_URL=https://tyhuang9.github.io` and `BASE_PATH=/scribe`.

## Current Features

- Native egui desktop UI with Transcribe, General, Models, History, Advanced, About, and opt-in Debug navigation aligned to the checked-in Scribe design tokens.
- Versioned local JSON settings with field-level salvage, unknown-field preservation, debounced atomic replacement, and legacy migration.
- One-time migration from the old Local Transcriber config path when a Scribe config does not exist.
- Global hotkey support with `Ctrl+Shift+Space` as the default and configurable toggle or hold-to-talk behavior.
- Native microphone capture through `cpal`; callback samples enter a fixed-capacity SPSC ring and workers perform downmixing, 16 kHz resampling, metering, exact-window Silero VAD, endpointing, post-roll, and post-capture normalization without sending PCM through the UI.
- Two private runtime variants selected only by the runtime-neutral `TranscriptionService` and `RuntimeRouter`: the statically linked `transcribe-cpp` 0.1.3 GGUF adapter and the isolated CPU-only sherpa-onnx worker. Zero models are Supported.
- Trusted GGUF discovery/import plus resumable, exact-hash model installation with staged native smoke tests, atomic activation, and crash recovery. Runtime-package transactions remain only for retained GGML compatibility.
- Non-blocking native workers for capture, model preload, Silero-confirmed bounded batch preview with one terminal tail, final transcription, and diagnostic latency breakdowns.
- Tray/menu integration with close-to-tray behavior and Show, Hide, Start/Stop Recording, Copy Last Transcript, and Quit actions. Show/Hide has live Windows evidence; the remaining tray actions still require the documented manual matrix.
- A Windows background-recording overlay that stays hidden while Scribe is foreground, offers privacy-safe Compact status and optional Live preview modes, and exposes a non-activating discard control without changing the captured paste target.
- Optional Windows insertion of the completed transcript into the captured app; other platforms use an explicit clipboard-only fallback.
- Runtime-neutral metadata for trusted GGUF variants, with one pinned fallback and private source-owned discovery. Older tiny/base/small/medium family distinctions remain private compatibility data.
- Debug comparison selection is explicit: choose installed models, retain drag order, and decode the same native prepared audio through the shared `TranscriptionService`.
- Transcript copy and clear actions.

## Requirements

- A current stable Rust toolchain with Rust 2024 edition support. The recorded automated verification used Rust 1.96.0; the project does not currently declare a tested minimum Rust version.
- A Windows, Linux, or macOS desktop session compatible with `eframe` and `global-hotkey`. Windows x64 is the primary release target; Linux and macOS retain conservative build/output fallbacks but are not release-qualified.
- Windows source builds require the Visual Studio 2022 C++ build tools and a Windows-native CMake on `PATH`. An MSYS CMake cannot select the required Visual Studio generator.
- A microphone visible to the host OS.
- Normal transcription requires an installed compatible GGUF model. Its CPU runtime is statically linked in-process; no separate runtime package or sidecar process is required.
- GPU transcription is not currently verified. An explicit GPU preference fails clearly instead of silently changing the backend.

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
trusted Experimental models. The page searches its immutable local inventory
by friendly name and language, filters by language, and refreshes remote data
only when you choose Refresh. Installed and Available sections use the same
compact card design; unavailable speed or accuracy metadata is shown as
`Not rated` instead of being inferred from file size. Install is disabled with a clear
message when the destination volume cannot satisfy the download plus Scribe's
1 GiB safety reserve. A model is not labelled Supported until its full
compatibility matrix passes; currently none has.

Managed model files live under the app-data `models` directory. A local GGUF
can also be fingerprinted and smoke-tested in place; Scribe does not copy,
upload, or delete that source file. Legacy external paths remain readable where
the private compatibility bridge still needs them, but they are not treated as
managed installs and uninstall never deletes them.

The default trusted GGUF route uses `transcribe-cpp` 0.1.3 as a statically
linked, in-process CPU adapter. It has no downloaded runtime package, CLI,
localhost service, or Python dependency. Model installation pins source facts,
validates size and SHA-256, runs an isolated native smoke, and atomically
activates the artifact. Valid partials support HTTP Range resume.

The safe embedded adapter is CPU-only. `Auto` resolves to CPU, `CPU` requests it
explicitly, and `GPU` fails clearly because no verified accelerator backend
ships.

Runtime selection is private to `RuntimeRouter`. `TranscribeCppRuntime` owns
embedded GGUF execution and `OnnxSpeechRuntime` owns the isolated CPU-only
sherpa-onnx worker. Neither runtime makes a model Supported: the named
Moonshine and Zipformer fixtures remain Experimental until their exact artifact
and platform evidence gates pass.

### ONNX runtime validation fixtures

The isolated sherpa-onnx worker is CPU-only and has no live model discovery or
download path. The native runtime remains Experimental until each exact
artifact has completed the platform, license, and benchmark evidence gate.
Two ignored tests exercise real, locally supplied bundles without contacting a
network service:

```powershell
$env:SCRIBE_ONNX_AUDIO = 'C:\fixtures\speech.wav'
$env:SCRIBE_ONNX_MOONSHINE_ROOT = 'C:\models\moonshine-tiny-en'
cargo test onnx_worker::tests::native_moonshine_offline_fixture_uses_the_typed_bundle_contract -- --ignored --exact

$env:SCRIBE_ONNX_ZIPFORMER_ROOT = 'C:\models\sherpa-onnx-streaming-zipformer-en-20M-2023-02-17'
cargo test onnx_worker::tests::native_zipformer_fixture_uses_true_online_streaming -- --ignored --exact
```

Moonshine requires exactly `encoder_model.ort`, `decoder_model_merged.ort`,
and `tokens.txt`; the `.ort` files are sherpa's ONNX Runtime-optimized bundle
artifacts and retain explicit Encoder/MergedDecoder roles. The experimental
Zipformer fixture requires exactly `encoder-epoch-99-avg-1.int8.onnx`,
`decoder-epoch-99-avg-1.int8.onnx`, `joiner-epoch-99-avg-1.int8.onnx`, and
`tokens.txt`. These are typed bundle roles; Scribe does not infer them from
arbitrary filenames.

To manually verify the actual hidden-worker executable boundary (Hello,
Health, and Shutdown), build Scribe first and point the ignored test at that
exact binary:

```powershell
$env:SCRIBE_ONNX_WORKER_EXE = '.\target\debug\local-transcriber.exe'
cargo test onnx_worker::tests::hidden_worker_manual_protocol_smoke -- --ignored --exact
```

Release builds use only a cached archive or `SHERPA_ONNX_ARCHIVE_DIR` containing
the exact reviewed sherpa-onnx 1.13.5 static archive. They never fetch a native
archive. An explicit debug-only download escape hatch exists solely for local
developer recovery: `SHERPA_ONNX_ALLOW_DEBUG_DOWNLOAD=1`.

### Runtime packaging and legacy development tools

The repository still packages a pinned whisper.cpp v1.9.1 compatibility
runtime for retained GGML models and a narrowly scoped bootstrap fallback when
the primary native GGUF adapter cannot initialize. It is an in-process DLL path
with a hash-verified CLI fallback, not the normal GGUF route. Build the
qualified Windows x64 release from local, previously acquired pinned sources
with:

```powershell
.\scripts\build-windows-release.ps1 `
  -RuntimeSource C:\path\to\verified-whisper-runtime `
  -ModelSource C:\path\to\whisper-base.en-Q8_0.gguf
```

The bundler performs a locked, offline build for `x86_64-pc-windows-msvc`,
then stages an explicit artifact allowlist in a unique transaction directory.
Only after the compatibility runtime, executable architecture, exact pinned
base.en GGUF, redistribution notices, offline load/decode/cancel/unload smoke,
and generated hash inventory all validate is the directory atomically renamed
to `artifacts/Scribe-windows-x64`. Cargo's `target` tree remains build input and
is never used or mutated as the distributable bundle. The scripts do not
download the model. The app validates the exact artifact again before use;
packaging variants do not create additional logical runtime kinds.

The repository retains separate faster-whisper, Vosk, sherpa-onnx, Moonshine,
Parakeet, and CUDA scripts solely for bounded development investigation and
migration compatibility. The release bundler does not invoke them. Their
presence is neither application-level support nor compatibility evidence, and
the normalized UI cannot select those legacy adapters.

Acceleration is runtime-neutral in settings. The normal embedded GGUF path and
retained Windows package are CPU-only; explicit `GPU` reports that no verified
accelerator is available.

For a local benchmark that uses the same `TranscriptionService` boundary as the
desktop application:

```bash
cargo run --release -- --benchmark path/to/fixture.wav --model whisper_cpp_tiny_en --output benchmark.json
```

The report contains allowlisted machine, model, backend, capability, and timing
metadata only. It omits transcript text, audio, source paths, runtime output,
and raw error chains, and refuses to overwrite an existing report.

## Windows release downloads

Windows x64 releases are published from GitHub Actions when a `v*` tag is
created. Download `Scribe-Setup-<version>.exe` from the GitHub release for a
normal per-user installation, or `Scribe-<version>-windows-x64.zip` for a
portable copy. Scribe installs speech models separately from its Models page;
models are not bundled in the installer.

## Config

Settings are stored in a platform-specific config directory using the `directories` crate.

Scribe stores new config under the Scribe application directory. On first launch, if no Scribe config exists and an old Local Transcriber config is present, Scribe reads and saves a migrated copy into the new config path.

The config stores:

- selected default model (fresh profiles use the bundled `whisper_cpp_base_en`)
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
- An installed selected model may be loaded during startup integrity/health validation; session loading still begins concurrently with capture.
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
- `src/core.rs`: authoritative one-active-session state machine.
- `src/diagnostics.rs`: bounded allowlisted session metrics and redacted export.
- `src/history/mod.rs`: SQLite history lifecycle, retention, retry, and reconciliation.
- `src/hotkey.rs`: global hotkey parsing and registration.
- `src/models.rs`: normalized runtime-neutral model descriptors and catalog.
- `src/runtime_router.rs`: the only application-level concrete runtime selector.
- `src/transcription.rs`: neutral `TranscriptionService` and worker lifecycle.
- `src/streaming.rs`: bounded rolling preview and transcript stabilization.
- `src/text_output.rs`: transactional Windows target insertion and conservative cross-platform clipboard output.
- `src/tray.rs`: tray icon, tray menu, and tray command mapping.
