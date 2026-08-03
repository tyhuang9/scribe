# Scribe revamp: Phase 0 baseline and architecture record

**Status:** Phase 0 documentation baseline (2026-08-03). This document records the
checked-out repository and the target architecture from the consolidated revamp
plan. It does not claim that later phases are implemented.

## How to read this document

- **Verified** means observed in the current checkout, source, lockfile, or a
  command result recorded below.
- **Proposed** means the target state required by the revamp plan; it is not yet
  an implementation result.
- **NOT VERIFIED** means that the repository or this environment did not provide
  evidence. It must not be presented as a compatibility or performance claim.

The Phase 0 branch intentionally leaves application behavior unchanged. It adds
this audit, the manual test scaffold in
[`MANUAL_TEST_MATRIX.md`](MANUAL_TEST_MATRIX.md), baseline timestamp semantics
in `src/app.rs` and `src/hotkey.rs`, and non-Linux warning gating in
`src/main.rs`.

## Verified current-state summary

Scribe is a Rust 2024 native desktop application using `eframe`/egui. The
application is organized as a monolithic `src/app.rs` UI/coordinator with small
modules for audio, settings/configuration, downloads, model metadata, output,
tray, hotkeys, and STT adapters. The current end-to-end path is:

```text
egui app / hotkey or Start button
        |
        v
src/audio.rs -- cpal input -> mutexed hound WAV writer -> temporary WAV
        |
        v
src/app.rs background thread
        |
        v
stt::transcribe_with_config (backend string match)
        |
        +--> WhisperCppBackend      -> whisper-cli child process
        +--> FasterWhisperBackend   -> faster-whisper Python runner process
        +--> VoskBackend            -> Vosk Python runner process
        `--> SherpaOnnxBackend      -> sherpa-onnx/Moonshine/Parakeet runner process
        |
        v
TranscriptResult -> UI transcript -> optional clipboard + paste automation
```

The six user-visible backend labels are `whisper.cpp`, `faster-whisper`,
`Vosk`, `sherpa-onnx`, `Moonshine`, and `Parakeet`. They are implemented by four
Rust adapters: the three dedicated adapters plus one sherpa-family adapter.
Every current transcription call is batch-oriented: the app records to a WAV,
then starts a short-lived child process. There is no shared
`TranscriptionService`, `RuntimeRouter`, native Rust transcriber, ORT binding,
overlay target capture, session ID, VAD, or committed/tentative streaming path
in this baseline.

### Current call-site audit

| Concern | Verified location(s) | Current behavior | Target boundary |
| --- | --- | --- | --- |
| Application dispatch | `src/app.rs:1702-1785` | Default and Playground jobs call `stt::transcribe_with_config` on worker threads after WAV finalization. | `TranscriptionService` only; no concrete backend imports above it. |
| Runtime selection | `src/stt/mod.rs:64-232` | `runtime_status` and `transcribe_with_config` match backend strings. | One `RuntimeRouter` match on `RuntimeKind`. |
| Runtime adapters | `src/stt/whisper_cpp.rs`, `faster_whisper.rs`, `vosk.rs`, `sherpa_onnx.rs` | Validate paths, construct CLI arguments, spawn child process, parse JSON/text. | Private `TranscribeCppRuntime`; conditional private ONNX adapters behind one `OnnxSpeechRuntime`. |
| Model catalog | `src/models.rs:238-389` | Flat `SttModelInfo` values with backend labels, prose tiers, and optional download names. | Versioned `ModelManifest` with runtime, architecture, format, pinned artifact, hash/size, capabilities, and compatibility state. |
| Runtime catalog | `src/runtime_catalog.rs:54-178` | Six backend specs; Python versions are pinned for five runtimes, whisper.cpp version is unset. Model hash/version fields are `None`. | Runtime-pack descriptors remain separate from model manifests; activation requires health/smoke check. |
| Model path/validation | `src/config.rs:381-454`, `481-494` | Backend string selects file/directory validation and storage path. | Manifest validation owns artifact layout; installers do not select decoders by name. |
| Downloads | `src/managed_downloads.rs:25-124`, `src/app.rs:2054-2150` | Backend-specific URL/runner branches; downloads are extracted by runners or direct helpers. | Resumable `*.partial`, exact size, SHA-256, staged extraction, smoke test, atomic activation. |
| Output | `src/text_output.rs` and `src/app.rs:1480-1510` | Clipboard set, optional paste automation, optional clipboard restoration. No target window identity is captured. | Capture original target; paste final text once, or copy with an explicit target-unavailable message. |
| Audio | `src/audio.rs:record_to_wav` | cpal callback locks a shared WAV writer and writes samples; no bounded ring buffer or common preparation stage. | Native bounded capture/ring buffer and one `PreparedAudio` conversion path. |
| Settings | `src/config.rs` | A flat `AppConfig` JSON with compatibility cleanup/defaults; no schema versioned module split. | Typed/versioned schema, migrations, salvage, validation, atomic writes. |
| History | Current UI keeps only the latest transcript; temporary WAV is removed after a job. | No durable history/audio retention subsystem is present. | Transcript-only history by default; optional separate audio files and retry. |

## Current runtime/model inventory and retirement ledger

The table is deliberately explicit about evidence. “Migrate” means retain the
user-facing model choice while replacing its backend behind a common handler;
it does not mean support has already been demonstrated.

| Current model/backend | Current call sites | Proposed handler | Keep, migrate, or remove | Evidence/blocker |
| --- | --- | --- | --- | --- |
| whisper.cpp `tiny.en`, `base.en`, `small.en`, `medium.en` (`ggml-*.bin`) | `models.rs:241-278`; `stt/mod.rs:119-151`; `stt/whisper_cpp.rs`; path/download branches in `config.rs`, `managed_downloads.rs`, and `app.rs` | `TranscribeCppRuntime` (primary) | **Migrate; keep catalog only after manifest smoke test** | Current whisper-cli path is runnable in source; current Windows report is whisper.cpp **1.9.1**. The installed dependency/API and GGUF family support for a future `transcribe-cpp` package are **NOT VERIFIED**. Existing `.bin` artifacts need a pinned replacement/compatibility decision. |
| faster-whisper `tiny.en`, `base.en`, `small.en`, `medium.en`, `large-v3`, `turbo`, `distil-large-v3` (CTranslate2 directories) | `models.rs:291-357`; `stt/mod.rs:152-182`; `stt/faster_whisper.rs`; dedicated runtime/download/UI branches | `TranscribeCppRuntime` only if an equivalent curated GGUF artifact passes contract tests; otherwise retire | **Migrate conditionally; remove old Python path after parity** | Current runtime is a short-lived Python runner pinned to faster-whisper **1.2.1** (CTranslate2). No transcribe-cpp support or artifact conversion evidence exists yet. Do not mark these models Supported in the target catalog until load/transcribe/capability tests pass. |
| Vosk small English (`vosk-model-small-en-us-0.15`) | `models.rs:280-289`; `stt/mod.rs:183-206`; `stt/vosk.rs`; Vosk runtime/download branches | No target handler unless a named, measured compatibility case is approved; otherwise no production catalog entry | **Remove from target catalog (proposed)** | Current Vosk **0.3.45** Python sidecar is a separate execution family and does not satisfy the two-handler target. No documented material benefit over the primary runtime is recorded. Keep only as a migration fixture until replacement/removal is decided. |
| sherpa-onnx Zipformer Small | `models.rs:361-370`; `stt/mod.rs:207-233`; `stt/sherpa_onnx.rs` | Conditional `OnnxSpeechRuntime` private adapter | **Experimental migration candidate** | Current managed sherpa-onnx runner is pinned **1.13.3**, batch-only. Adding `OnnxSpeechRuntime` requires a named benefit, reproducible benchmark/compatibility evidence, and contract tests. None is recorded yet, so this is **NOT VERIFIED** and must not be promoted. |
| Moonshine tiny English | `models.rs:371-379`; `stt/sherpa_onnx.rs` family table and dispatch; download/UI branches | Conditional `OnnxSpeechRuntime` private adapter | **Experimental migration candidate** | Current sherpa-family runner is pinned **1.13.3**, batch-only. Low-latency benefit is described in catalog prose but has not been measured against the primary runtime. Treat as Experimental/NOT VERIFIED. |
| Parakeet Unified 0.6B int8 | `models.rs:381-389`; `stt/sherpa_onnx.rs` family table and dispatch; download/UI branches | Conditional `OnnxSpeechRuntime` private adapter | **Experimental migration candidate; otherwise remove** | Current separate `parakeet-cli`/sherpa-family path is batch-only. Current docs call it experimental; no ORT dependency, load smoke test, or comparative evidence is present. A compatibility handler is not justified until the plan's material-benefit gate passes. |

### Model-specific branching removal checklist

The following current branches are known migration work, not claims that they
are already removed:

- [ ] `stt/mod.rs` backend string dispatch and runtime status checks move behind
  `RuntimeRouter`/`TranscriptionService`.
- [ ] `app.rs` runtime install, packaged-runtime resolution, download, model
  cards, and device/settings branches consume manifest/catalog DTOs rather than
  backend names.
- [ ] `config.rs` model validation and storage paths are manifest-driven.
- [ ] `models.rs` catalog/backend capability functions become declarative
  manifests; no model-name match is used for runtime selection.
- [ ] `managed_downloads.rs` uses one verified installer pipeline rather than
  backend-specific activation branches.
- [ ] `src/stt/*` concrete process/decoder types remain private to runtime
  adapters; application modules import only common service types.
- [ ] A repository check prevents architecture/runtime imports or matches above
  `transcription/runtimes/`.

### Compatibility status and streaming declaration

The current catalog's declarations are not equivalent to the target plan's
evidence gate. This is the explicit Phase 0 ledger:

| Catalog entries | Current declaration | Phase 0 target status | Current streaming mode |
| --- | --- | --- | --- |
| whisper.cpp `tiny.en`, `base.en`, `small.en`, `medium.en` | Runnable/non-experimental | **Experimental / NOT VERIFIED** until the selected primary runtime and artifact smoke tests pass | Final-only batch (native streaming **NOT VERIFIED**) |
| faster-whisper seven entries | Runnable/non-experimental | **Experimental / NOT VERIFIED** pending an equivalent primary-runtime artifact | Final-only batch (native streaming **NOT VERIFIED**) |
| Vosk small English | Runnable/non-experimental | **Incompatible with the two-handler target unless a new decision is recorded; removal proposed** | Final-only batch |
| sherpa-onnx Zipformer Small, Moonshine, Parakeet | Experimental | **Experimental / NOT VERIFIED** pending optional-handler benefit and contract evidence | Final-only batch; current source explicitly reports streaming false |
| Any other imported/local artifact | Not in the curated catalog | **Incompatible until manifest validation and smoke tests pass** | Not applicable |

No entry can be called **Supported** under the revamp definition at this
checkpoint because required runtime/version/platform/load/transcription evidence
has not been completed. No current entry is silently promoted to Supported by
the proposed documentation.

## Dependencies and executable runtime baselines

Direct dependency requirements from `Cargo.toml` and resolved versions from
`Cargo.lock` are:

| Crate | Manifest requirement | Locked direct version |
| --- | --- | --- |
| anyhow | `1` | 1.0.102 |
| arboard | `3.6` | 3.6.1 |
| cpal | `0.15` | 0.15.3 |
| crossbeam-channel | `0.5` | 0.5.15 |
| directories | `5` | 5.0.1 |
| eframe | `0.27` | 0.27.2 |
| enigo | `0.6` | 0.6.1 |
| global-hotkey | `0.6` | 0.6.4 |
| hound | `3.5` | 3.5.1 |
| libloading | `0.8` | 0.8.9 (a transitive 0.7.4 is also present) |
| serde | `1` | 1.0.228 |
| serde_json | `1` | 1.0.150 |
| thiserror | `2` | 2.0.18 (a transitive 1.0.69 is also present) |
| tray-icon | `0.23.1` | 0.23.1 |
| ureq | `2` | 2.12.1 |
| winit | `0.29` | 0.29.15 |

There is no `transcribe-cpp`, `transcribe-rs`, ONNX Runtime/ORT, or native
speech binding in `Cargo.toml` or `Cargo.lock`. The current runtime layer is
child-process based. The checked-in Python runtime pins are:

```text
faster-whisper       1.2.1
vosk                 0.3.45
sherpa-onnx          1.13.3
sherpa-onnx-bin      1.13.3
numpy                2.5.0
pip                  26.1.2
setuptools           82.0.1
wheel                0.47.0
```

Feature disposition verified from `Cargo.toml`: `serde` enables `derive`,
`tray-icon` disables default features, and the remaining direct dependencies
use their manifest defaults. `cpal` 0.15.3 and `hound` 3.5.1 therefore resolve
with default features. External whisper.cpp build flags (including CUDA/other
GPU support) are **NOT VERIFIED** from this repository; the baseline executable
used for the measurements below is the CPU-only local bundle.

The runtime catalog leaves whisper.cpp's version unset. A current Windows
`whisper-cli --version` report of **1.9.1** is recorded as an observed external
runtime fact, not as a lockfile guarantee. `parakeet-cli` is a separate current
sidecar entry point; its exact executable version is **NOT VERIFIED**.

Model artifact records currently contain estimated sizes only; `version` and
`sha256` are `None` for every entry in `runtime_catalog.rs`. Artifact source,
repository revision, exact size/hash, staged extraction, and health/load smoke
checks are therefore **NOT VERIFIED**.

## Target architecture (proposed)

The implementation target from the consolidated plan is intentionally small:

```text
Scribe coordinator / egui / settings / history / output
                         |
                         v
                 TranscriptionService
                         |
                         v
                    RuntimeRouter
                    /           \
                   v             v
       TranscribeCppRuntime   OnnxSpeechRuntime
          (preferred)          (optional, justified)
                   \             /
                    `-----+-----'
                          v
                  Unified Transcript
```

`RuntimeRouter` is the only application-level match on `RuntimeKind`. The UI,
recorder, installer, history, output, and settings depend on common transcript
and manifest types, never concrete runtime or decoder types. `OnnxSpeechRuntime`
is absent unless one named model demonstrates a material benefit that the
primary handler cannot reasonably provide and passes the shared contract suite.
ONNX CTC/transducer/autoregressive selection, if ever needed, stays private to
that handler. Runtime packs (if packaging needs them) are distribution details,
not additional logical runtime kinds.

### Retirement ledger for current components

| Existing component | Proposed disposition | Boundary/rollback note |
| --- | --- | --- |
| `src/stt/mod.rs::SttBackend` and backend-string match | Replace with service/router contracts | Keep the old path until the selected model passes equivalent tests; revert this slice independently if routing regresses. |
| `WhisperCppBackend` | Wrap/migrate into `TranscribeCppRuntime` | Keep the working CLI adapter as a temporary fallback during migration; remove only after parity. |
| `FasterWhisperBackend` | Retire after an equivalent curated model/contract path exists | CTranslate2 compatibility is a blocker; do not silently convert or delete installed models. |
| `VoskBackend` | Remove from target production catalog unless a new evidence-backed decision is made | Existing users need an explicit migration/removal message; keep fixture tests while the decision is open. |
| `SherpaOnnxBackend` | Collapse into conditional `OnnxSpeechRuntime` private adapter or retire | No second logical handler is activated without benefit evidence. |
| Flat `SttModelInfo`/`BackendCapabilities` | Migrate to versioned `ModelManifest` and runtime capability intersection | Preserve old config IDs through a migration map where possible. |
| `managed_downloads` backend branches | Replace with verified, resumable installer | Failed activation leaves the previous install untouched; staged artifacts are disposable. |
| Mutexed callback WAV writer | Replace with bounded native capture/preparation path | Keep WAV export only as an explicit history/recovery artifact, not the inference boundary. |

## Latency metric contract and baseline limitations

### Contract

Each future dictation session must report (or explicitly mark unavailable) the
following monotonic intervals with model ID, model architecture, logical
runtime, runtime package version, resolved compute backend, streaming mode, and
cold/warm state:

| Metric | Definition | Current source/status |
| --- | --- | --- |
| `hotkey_to_overlay_visible_ms` | hotkey event to overlay visible | `LatencyTrace.overlay_visible_at` exists but is never populated; no overlay. **NOT VERIFIED**. |
| `hotkey_to_capture_started_ms` | hotkey event to capture ready | `activation_at` to `recorder_started_at`; recorder ready is recorded after `cpal` stream `.play()` acknowledgement. `activation_at` is the app's hotkey poll observation, not the physical OS event. |
| `speech_start_detected_ms` | capture start to VAD speech start | No VAD. **NOT VERIFIED**. |
| `model_load_ms` | load/prewarm start to ready | Fields exist in `LatencyTrace` but are never populated; current model starts after recording. **NOT VERIFIED**. |
| `first_partial_ms` | hotkey/capture reference to first changed partial | Field exists but all current adapters are batch-only. **NOT VERIFIED**. |
| `recording_duration_ms` | capture start to stop | Derivable from recorder/stop timestamps; not displayed as a dedicated value. |
| `recording_end_to_final_text_ms` | stop/WAV finalization to successful final transcript | `stop_requested_at` to `final_text_ready_at` is shown as “Stop to final text”; `transcription_job_completed_at` is worker completion and is not final-text readiness on failures. |
| `post_processing_ms` | final transcript to chosen output text | No post-processing stage. **NOT VERIFIED**. |
| `final_text_to_paste_ms` | successful final-text-ready timestamp to paste automation completion | `final_text_ready_at` to `paste_completed_at`; this means Enigo/clipboard automation returned after its configured delay and restoration, not that the target application consumed the text. Clipboard-only/failure is not a successful paste. Output-start→output-complete remains a separate component metric. |
| `total_end_to_end_ms` | hotkey observation to output completion | `summary_lines` reports total observed; it excludes unobserved physical event/overlay/VAD work. |
| `realtime_factor` | transcription compute time / audio duration | Playground benchmark has an RTF helper; dictation latency does not persist this metric. |

Current instrumentation added on this branch is intentionally diagnostic only:
it records timestamps in `LatencyTrace` and displays a latest summary. The
trigger timestamp is `TriggerObservation::HotkeyPoll` when a registered
`GlobalHotKeyEvent` is drained by UI polling (or `AppAction` for an in-app
button), not the physical key-generation time. It does not create a session ID,
reject stale events, correlate concurrent sessions, or measure true first
partials. `transcription_job_completed_at` is worker completion;
`final_text_ready_at` is set only on the successful final-text path. Failures
must not produce a final-text/paste latency claim. Measurements are reliable
only for non-overlapping sequential sessions and should not be compared as
cold/warm or cross-model benchmarks until the service/session work is
implemented.

### Baseline command evidence

Commands run in the checkout on 2026-08-03 final source gate (HEAD
`536a85f813943dbc8beaa684fc5901ff281f6577`, source diff hash
`6c39139e80fac94c8ce735e7962ed3a4ac75e0a7`, 14:20:24.998–14:20:30.146
`-05:00`):

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | **PASS**. |
| `cargo check --all-targets --all-features` | **PASS**. |
| `cargo test --all-targets --all-features` | **PASS**: 174 discovered; 172 passed, 0 failed, 2 ignored. The ignored tests require local runtime/model/GPU fixtures. |
| `cargo clippy --all-targets --all-features -- -D warnings` | **PASS**. |
| `cargo build --all-features` | **PASS**. |

All commands emitted the same non-fatal environment warning: `could not
canonicalize path C:\Users\huang`.

No application-integrated microphone, GUI, tray, global-hotkey, retained model
load, or focused-app paste measurement was performed in this environment. The
CLI-only child-runtime measurements below bypass the application coordinator.
See the manual matrix for explicit `NOT VERIFIED` rows.

### CLI-only whisper.cpp baseline

The root-agent baseline used the official JFK WAV fixture and timed each full
fresh process invocation (stdout/stderr suppressed; exit code checked):

```powershell
target\debug\runtimes\whisper_cpp\bin\whisper-cli.exe `
  -m "$env:APPDATA\Scribe\Scribe\data\models\whisper.cpp\ggml-base.en.bin" `
  -f C:\tmp\scribe-revamp-jfk.wav -nt
```

Host package output reported whisper.cpp **1.9.1**. These are process-level
measurements, not retained-engine measurements; every invocation reloads the
model and Windows standby/filesystem cache was not purged.

| Fixture/model | Runs | Result |
| --- | ---: | --- |
| JFK / whisper.cpp `base.en`, repeated process | 20 | 0 failures; min **1236.0 ms**, median **1279.5 ms**, p95 **1452.8 ms**, max **1469.7 ms**, mean **1303.0 ms**. |
| JFK / whisper.cpp `base.en`, cold-process sample (cache not purged) | 5 | min **1273.6 ms**, median **1282.8 ms**, p95/max **1333.1 ms**, mean **1291.0 ms**. |

The baseline cannot attribute hotkey, capture, first-partial, final-text-ready,
or paste phases because it bypasses the app coordinator. A future benchmark
must record the command, model revision, runtime package, resolved backend,
streaming mode, cold/warm state, and fixture checksum alongside each sample.

## Risks and assumptions

| Risk | Level | Evidence/assumption | Mitigation before the affected phase ships |
| --- | --- | --- | --- |
| Runtime/model compatibility is aspirational | **High** | No transcribe-cpp/ORT dependency or curated load/transcribe smoke evidence exists; current model labels are not proof of support. | Pin runtime/artifact revisions; run contract and platform matrix; leave entries Experimental/Incompatible until evidence passes. |
| Audio callback can block on a mutexed WAV writer | **High** | `src/audio.rs` writes through `Arc<Mutex<Option<WavWriter>>>` in the cpal callback. | Move capture to a bounded native queue/ring buffer and keep filesystem/model work off the callback. |
| Sessions are not correlated | **Medium** | `AppEvent` and current traces have no session ID or stale-event rejection; metrics are valid only for sequential non-overlapping sessions. | Add session IDs and coordinator-owned state transitions before concurrency/streaming work. |
| Output/permissions differ by desktop and target integrity | **Medium** | Enigo/clipboard behavior, Wayland restrictions, macOS Accessibility, and elevated Windows targets were not manually run. | Execute the platform matrix; capture target identity and fall back to clipboard without guessing. |
| Download integrity and rollback are incomplete | **Medium** | Model metadata has estimated sizes but no hash/revision; current installer paths are backend-specific. | Add partial/resume, exact size/hash, staged smoke test, atomic activation, and previous-known-good rollback. |
| Privacy/history behavior is not durable yet | **Low** | Current temporary WAV is removed after jobs and latest transcript is in memory; there is no history retention subsystem. | Default to transcript-only history, audio off, explicit retention, and startup reconciliation when history lands. |

Phase 0 assumes the existing six catalog labels and current working dictation
path remain available while replacement handlers are validated. It does not
assume that any model family named in the plan is supported by a future runtime.

## Rollout and rollback boundaries

Phase boundaries are safety boundaries, not a license to remove a working path
early:

1. **Phase 0 (this checkpoint):** documentation and baseline evidence plus the
   concurrent diagnostic timestamp/non-Linux warning-gating source edits; no
   intended dictation behavior change. Roll back the three source files and two
   docs together as one branch/commit set.
2. **Contract extraction:** wrap the currently selected working backend first;
   retain the old adapter until equivalent transcription and output tests pass.
3. **Router/runtime migration:** activate one manifest-routed primary path at a
   time. Keep the previous CLI path available for a bounded rollback while
   health/load/transcribe checks run.
4. **Catalog/download changes:** never activate a partial, wrong-size, or
   checksum-failing artifact. Stage and smoke-test before atomic activation, and
   retain the previous known-good runtime/model install.
5. **Optional ONNX handler:** this is a gated decision. If benefit evidence or
   contract tests fail, do not ship it; keep one logical handler.
6. **Output/session/history changes:** never paste tentative text. On target
   loss or output failure, copy the finalized text and preserve recoverable
   history/audio according to privacy settings.

Rollback for a later phase is a branch/revert to the last passing phase. Runtime
and model activation must be independently reversible without deleting user
models or settings. Database/history migrations must be additive or backed up
before rewrite.

## Phase 0 implementation checklist and exit criteria

### Checklist

- [x] Repository structure, UI framework, audio path, hotkey path, output path,
      settings, model catalog, runtime catalog, and download call sites audited.
- [x] Six current backend labels and four Rust adapter destinations recorded.
- [x] Exact direct dependency/runtime pin baselines recorded.
- [x] Current architecture and target two-handler architecture documented.
- [x] Model-specific branching removal checklist created.
- [x] Baseline latency metric contract and current instrumentation limitations
      recorded.
- [x] Baseline build/check/lint/test commands run with failures preserved.
- [x] Manual platform matrix created; unrun platform rows are marked explicitly.
- [x] No functional behavior intentionally changed: the concurrent source
      delta is diagnostic timing plus non-Linux warning gating only; no model,
      recording, transcription, or output path was intentionally changed.
- [x] Existing source snapshot builds/tests after the Phase 0 latency and cfg
      warning changes.

### Acceptance criteria

Phase 0 exits only when all of the following are true:

1. This audit and the manual matrix are present and kept current.
2. No functional behavior intentionally changed by Phase 0.
3. The current application builds and the baseline checks pass (the two ignored
   runtime/GPU tests require external fixtures and remain explicitly listed).
4. Every current backend/model has a proposed destination: primary handler,
   optional compatibility handler, or removal.
5. The next phase can be implemented without guessing which current path is
   being replaced or how to roll it back.

### Stacked-branch integration note

This checkout is based on `agent/playground-model-selector` at HEAD
`536a85f813943dbc8beaa684fc5901ff281f6577`, 11 commits ahead of `main` (the
aggregate main diff is 2,738 additions and 517 deletions across 7 files). The
Phase 0 delta itself is narrow—three concurrent source files plus these two
docs—but any PR/integration must target or wait for that stacked base. Targeting
`main` directly is a **NO-GO** because it would mix unrelated branch work.

## Open evidence tasks (concrete, not claims)

- Verify the selected `transcribe-cpp` crate/package version, enabled features,
  platform builds, model formats, and each candidate family with a real
  load/transcribe smoke test.
- Decide whether any current faster-whisper, Vosk, sherpa-onnx, Moonshine, or
  Parakeet model meets the measured-benefit gate for the optional ONNX handler.
- Add pinned artifact repository revisions, exact sizes, SHA-256 values, and a
  signed catalog/update policy.
- Add session IDs, stale-event rejection, model load/first-partial timestamps,
  and cold/warm benchmark records.
- Replace callback mutex WAV writes with bounded native capture and one shared
  audio preparation path; add VAD/endpointing and target-window capture.
- Execute the manual matrix on the supported Windows environment and at least
  one Linux/macOS desktop session before marking platform support.
