# Scribe revamp implementation record

**Status:** Phase 8 implemented on its stacked branch (2026-08-04). This document
preserves the Phase 0 audit and records each implemented phase against the
consolidated revamp plan. It does not claim that uncompleted later phases are
implemented.

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

## Verified Phase 0 current-state summary (historical)

At the Phase 0 base, Scribe was a Rust 2024 native desktop application using
`eframe`/egui. The application was organized as a monolithic `src/app.rs` UI/coordinator with small
modules for audio, settings/configuration, downloads, model metadata, output,
tray, hotkeys, and STT adapters. The Phase 0 end-to-end path was:

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
Every Phase 0 transcription call was batch-oriented: the app recorded to a WAV,
then started a short-lived child process. At that point there was no shared
`TranscriptionService`, `RuntimeRouter`, native Rust transcriber, ORT binding,
overlay target capture, session ID, VAD, or committed/tentative streaming path
in that baseline. The Phase 1 section below records the service and correlation
boundary now layered over this legacy execution path.

### Phase 0 call-site audit

| Concern | Verified location(s) | Phase 0 behavior | Target boundary |
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
| `final_text_to_paste_ms` | successful final-text-ready timestamp to paste automation completion | `final_text_ready_at` to the native Windows `SendInput` return timestamp; it does not claim that the target application consumed the text. Clipboard-only/failure and injected test results without native timing do not fabricate a successful paste timestamp. Output-start→output-complete remains a separate component metric. |
| `total_end_to_end_ms` | hotkey observation to output completion | `summary_lines` reports total observed; it excludes unobserved physical event/overlay/VAD work. |
| `realtime_factor` | transcription compute time / audio duration | Playground benchmark has an RTF helper; dictation latency does not persist this metric. |

Phase 0 instrumentation added on that branch was intentionally diagnostic only:
it records timestamps in `LatencyTrace` and displays a latest summary. The
trigger timestamp is `TriggerObservation::HotkeyPoll` when a registered
`GlobalHotKeyEvent` is drained by UI polling (or `AppAction` for an in-app
button), not the physical key-generation time. At that checkpoint it did not
create a session ID, reject stale events, correlate concurrent sessions, or
measure true first partials. Phase 1 has since added the session/request checks;
true first-partial measurement remains unimplemented.
`transcription_job_completed_at` is worker completion;
`final_text_ready_at` is set only on the successful final-text path. Failures
must not produce a final-text/paste latency claim. Measurements are reliable
only for non-overlapping sequential sessions and should not be compared as
cold/warm or cross-model benchmarks until the retained runtime work is
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
| Phase 0 sessions were not correlated; facade-level issue resolved in Phase 1 | **Resolved in Phase 4** | The authoritative coordinator now owns one active session, legal transitions, request/model/sequence correlation, cancellation, stop priority, and terminal outcomes. | Preserve the coordinator boundary while Phase 7 begins emitting incremental updates. |
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
- Extend the implemented session/request correlation into the authoritative
  Phase 4 coordinator, and add model-load/first-partial timestamps plus
  retained-engine cold/warm benchmark records.
- Replace callback mutex WAV writes with bounded native capture and one shared
  audio preparation path; add VAD/endpointing and target-window capture.
- Execute the manual matrix on the supported Windows environment and at least
  one Linux/macOS desktop session before marking platform support.

## Phase 1: common contract and current-model wrapper

**Implementation status:** complete on `revamp/phase-1-transcription-service`,
stacked on the Phase 0 branch. No merge is authorized or performed.

### Implemented vertical boundary

Normal dictation and Playground/model comparison now enter one application
facade:

```text
egui coordinator / normal dictation / Playground
                       |
                       v
             TranscriptionService
                       |
                       v
        private LegacyBatchAdapter : SpeechEngine
                       |
                       v
       existing stt::transcribe_with_config bridge
```

`src/app.rs` no longer calls `stt::transcribe_with_config`. The only call above
the old `src/stt/` adapters is inside private `src/transcription.rs`. The bridge
deliberately retains the existing backend implementations so the working
behavior is wrapped before it is replaced. Runtime installation/status UI still
uses legacy provider helpers; moving those branches behind `RuntimeRouter` and
neutral manifests is Phase 2/3 debt, not hidden completion.

The common contract introduces `ModelId`, `SessionId`, `RequestId`, normalized
transcript/segment/options/outcome types, conservative capabilities,
`SpeechEngine`, and the optional `SpeechStream` extension. Engine lifecycle
operations include health, load, unload, cancellation, and final
transcription. The legacy bridge is stateless: load/unload are explicit no-ops,
health is unimplemented, and cancellation returns an explicit unsupported error
instead of pretending to cancel a child process. Unsupported request options
are rejected rather than ignored.

Legacy adapter `duration_ms` values measure processing wall-clock time, not
utterance duration. Phase 1 therefore exposes them as
`TranscriptionOutcome.processing_duration_ms`; `Transcript.duration_ms` remains
unset until the native preparation path can provide true audio duration.
Timestamp capability is claimed only for the current faster-whisper and Vosk
adapters whose parsed results actually retain segment timing. Whisper.cpp and
the sherpa/Moonshine/Parakeet bridge remain conservative.

### Correlation and stale-result safety

- A monotonic session ID is allocated before native capture is started. Request
  IDs are allocated after WAV finalization and immediately before service
  dispatch; both IDs are carried unchanged through the service outcome.
- A newly accepted normal or Playground recording supersedes the other active
  source. An obsolete completion is rejected before transcript, latency,
  status, clipboard, or paste mutation.
- Playground requests are tracked per run as request-to-model mappings. A
  response with mismatched IDs or the wrong model is rejected, the expected
  card receives an actionable error, and no transcript is applied.
- Superseded Playground runs retain only bookkeeping needed to remove their own
  temporary WAV after every outstanding request completes. They cannot
  decrement a newer run, delete a newer recording, or overwrite newer UI state.
- The service independently verifies that legacy diagnostics returned the
  requested model ID, protecting normal dictation as well as Playground.

### Decisions and compatibility evidence

- **No `RuntimeRouter` yet.** Phase 1 is an extraction boundary. Introducing a
  router or another runtime before the working path was wrapped would violate
  the ordered plan.
- **No second logical runtime.** `OnnxSpeechRuntime` was not added and no ONNX
  benefit claim was made.
- **No model was promoted.** All Phase 0 compatibility statuses remain
  Experimental or Incompatible/NOT VERIFIED. Passing facade/unit tests does not
  prove model compatibility.
- `PreparedAudio` and acceleration resolution are deferred to Phase 2, where
  path-based inference is removed. Dictation phase/transcript-state types move
  with the authoritative coordinator and incremental transcript work. The
  structured cross-stage error taxonomy remains open; Phase 1 preserves current
  errors and adds explicit option/lifecycle/correlation failures.

### Automated verification

Final Phase 1 gates on 2026-08-03:

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | **PASS**. |
| `cargo check --all-targets --all-features` | **PASS**. |
| `cargo test --all-targets --all-features` | **PASS**: 200 discovered; 197 passed, 0 failed, 3 ignored environment-required smoke tests. |
| `cargo test transcription::tests::transcription_service_jfk_smoke_uses_the_whisper_cpp_facade --all-features -- --ignored --exact` | **PASS**: 1 passed using the Phase 0 Windows whisper.cpp 1.9.1 CLI, local `base.en` artifact, and JFK WAV fixture. |
| `cargo clippy --all-targets --all-features -- -D warnings` | **PASS**. |
| `cargo build --all-features` | **PASS**. |
| `git diff --check` | **PASS**. |
| Boundary scan for `transcribe_with_config` outside `src/stt/**` | **PASS**: only the documented private call in `src/transcription.rs`; no application dispatch call remains. |

Added deterministic coverage verifies neutral result mapping, processing-time
semantics, conservative capabilities for every legacy backend, explicit
lifecycle behavior, unsupported options, unknown models, model-ID validation,
monotonic IDs, current normal success/failure, current Playground
success/failure and exactly-once cleanup, stale same-source and cross-source
success/failure rejection, multi-request stale cleanup, mismatched service IDs,
and wrong-model Playground responses.

### Manual verification and measured results

The JFK fixture completed through `TranscriptionService` and the private
whisper.cpp bridge with non-empty text and matching correlation/model metadata.
Application-integrated microphone, GUI, hotkey, Playground, target-window, and
paste tests were **NOT VERIFIED** in this phase. Other ignored runtime tests
remain environment-gated. The targeted smoke was functional evidence, not a
controlled latency sample, so the Phase 0 CLI-only figures remain the only
performance baseline and no latency improvement is claimed. The manual matrix
records the precise remaining checks.

### Risks and next phase

| Risk | Level | Mitigation |
| --- | --- | --- |
| The facade still reaches four legacy process adapters through one private transitional bridge. | **Medium** | Phase 2 introduces the private router and primary runtime, keeps the bridge only as bounded rollback, and adds the boundary guard. |
| Current child processes cannot acknowledge cancellation through the common contract. | **Medium** | Dedicated runtime workers and cancel commands land with the primary runtime; unsupported is explicit meanwhile. |
| Audio remains a temporary WAV path across the service boundary. | **Medium** | Phase 2 adds canonical in-memory `PreparedAudio`; Phase 6 replaces callback WAV writes. |
| Runtime/UI provider branches remain in `app.rs`. | **Medium** | Move selection behind `RuntimeRouter` and model/runtime metadata behind neutral descriptors before retiring legacy branches. |
| Real runtime and desktop vertical slices were not manually exercised. | **High** for release confidence | Run the Windows fixture and manual matrix with exact runtime/model artifacts before any Supported status or release claim. |

Phase 2 must now introduce private `RuntimeKind`, the sole `RuntimeRouter`, and
the primary `TranscribeCppRuntime`; verify the pinned package and native API;
add canonical mono 16 kHz `f32` preparation and acceleration resolution; retain
the current CLI bridge until equivalent load/transcribe evidence passes; and
save new cold/warm latency evidence without changing any model to Supported.

## Phase 2: router, retained primary runtime, and prepared audio

### Implemented architecture

Phase 2 replaces the path-based service contract with a native-audio boundary:

```text
egui/coordinator/Playground
        |
        v
TranscriptionService (long-lived, bounded command/reply channels)
        |
        v
dedicated native worker -> RuntimeRouter (sole concrete-runtime match)
        |
        v
TranscribeCppRuntime -> thin C ABI shim -> pinned whisper.cpp DLL
```

- `PreparedAudio` decodes integer PCM 8/16/24/32 and float32 WAV, downmixes by
  arithmetic mean, converts samples to finite `f32` in `[-1, 1]`, and
  deterministically resamples to mono 16 kHz. The Phase 2 linear resampler has
  no anti-alias filter and performs sample-format/range normalization, not
  loudness normalization; the higher-quality shared DSP stage remains Phase 6.
- The app prepares one `Arc<PreparedAudio>` per capture on a native worker,
  deletes the capture WAV after preparation, and shares the buffer with every
  selected Playground request. PCM does not cross React, JavaScript, webview
  events, or general UI IPC.
- `TranscriptionService` owns a single named native worker with a capacity-one
  command queue and capacity-one replies. Router/engine lifecycle and inference
  execute only there; upstream's same-context non-concurrency rule is enforced.
  The worker unloads the retained model after five idle minutes, and explicit
  preload/health/unload operations use the same neutral service boundary.
- `TranscribeCppRuntime` implements `SpeechEngine`. Cancellation increments a
  lock-free request generation observed through whisper.cpp's native abort
  callback, so it can interrupt inference while the worker owns the engine.
  Failed/cancelled inference discards the context before accepting a retry.
- `RuntimeKind` and `TranscribeCppRuntime` are private to
  `src/runtime_router.rs`. An automated source-boundary test rejects those
  names outside that module and rejects concrete runtime imports in `app.rs`.
- Non-primary providers remain reachable only through the private transitional
  process bridge. That bridge creates a mode-`0600` canonical PCM16 WAV in an
  app-private directory, removes it with RAII, scavenges crash leftovers older
  than 24 hours, and is retained solely until Phase 11 replacement/retirement
  evidence exists.
- Application configuration now exposes neutral
  `AccelerationPreference::{Auto,Cpu,Gpu}`. The legacy
  `whisper_compute_mode` key and `cuda`/`prefer_gpu` values deserialize through
  aliases and serialize back as `acceleration_preference`.
- The verified package is CPU-only. Auto resolves to CPU with a diagnostic;
  explicit CPU is honored; explicit GPU fails actionably and is never silently
  downgraded.
- There is exactly **one logical runtime handler**. `OnnxSpeechRuntime` was not
  added because no named ONNX model has passed the Phase 3 evidence gate.

### Pinned runtime package and ABI decision

The Windows x64 package is the official `whisper.cpp` v1.9.1 release at commit
`f049fff95a089aa9969deb009cdd4892b3e74916`:

- Archive: `whisper-bin-x64.zip`, 7,982,101 bytes, SHA-256
  `7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539`.
- Build flags observed from the upstream release workflow:
  `BUILD_SHARED_LIBS=ON`, `WHISPER_SDL2=ON`, `GGML_NATIVE=OFF`,
  `GGML_BACKEND_DL=ON`, `GGML_CPU_ALL_VARIANTS=ON`, Release/x64.
- Entrypoints: `bin/whisper.dll` for the native path and the independently
  hash-checked `bin/whisper-cli.exe` compatibility entrypoint. SHA-256 and size
  for the DLL, CLI, common GGML libraries, and all nine CPU variants are in
  `runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json`.
- No CUDA, Vulkan, OpenVINO, HIP, Metal, or SYCL library is present. GPU support
  is therefore unverified and unavailable in this package.
- `native/whisper-f049fff` vendors the exact upstream header closure and
  license with provenance hashes. `native/whisper_shim.c` keeps all upstream
  structs passed by value on the C side; Rust sees only opaque handles,
  primitives, and copied callback data. Rust callbacks contain panics, reject
  null text and invalid timestamps, and the shim uses a restricted Windows DLL
  search path.
- The first native JFK run exposed that the release package's CPU backend is a
  dynamically loaded GGML plugin. The shim scores only the fixed set of nine
  hash-verified CPU variants, then loads the best one by absolute path via
  `ggml_backend_load`. It does not scan the directory or honor ambient
  `GGML_BACKEND_PATH`; the measured host selected the verified Cascadelake
  variant. Debug and release fixture tests pass.

All four checked local Whisper artifacts are pinned to Hugging Face repository
revision `5359861c739e955e79d9a303bcbc70fb988958b1` with exact byte sizes and
SHA-256 values. Direct downloads validate both before activation, and the
native handler revalidates them before every fresh in-process model load.

CLI fallback is allowed only for a native library/bootstrap availability
failure and only after the CLI plus common GGML dependencies independently
match the pinned hashes. Package integrity, model-load, audio, acceleration,
callback, and inference failures do not fall back. This package's CLI depends
on the same shared libraries, so fallback remains a narrow recovery mechanism,
not a way around integrity checks.

### Compatibility status

No model is promoted to Supported in Phase 2. The complete Phase 3
load/transcribe/cancel/unload/reload/acceleration/platform suite has not run.

| Model/artifact | Phase 2 evidence | Status | Streaming |
| --- | --- | --- | --- |
| `whisper_cpp_base_en` / local `ggml-base.en.bin` | Windows x64 CPU native load, JFK known-fixture transcription, retained second transcription, debug and release package smoke | **Experimental** | Final-only batch; no proven native stream |
| `whisper_cpp_tiny_en`, `small_en`, `medium_en` | Historical CLI timing only; native load/transcription not rerun in this phase | **Experimental** | Final-only batch |
| faster-whisper, Vosk, sherpa-onnx, Moonshine, Parakeet entries | Preserved through the private transitional bridge; no fresh primary-runtime compatibility proof | **Experimental or Incompatible**, unchanged pending Phase 3/11 evidence | Final-only batch |
| Named sherpa Zipformer candidate | Evidence gate not yet executed | **Experimental / NO HANDLER** | Not shipped |

### Measured results

Measurements use the same Windows machine, CPU backend, local base.en artifact,
and `C:\tmp\scribe-revamp-jfk.wav` fixture as the Phase 0 CLI baseline. The
Phase 0 20-run repeated-process result was median 1,279.5 ms and p95 1,452.8
ms; its five-run non-cache-purged process result was median 1,282.8 ms.

The Phase 2 ignored benchmark performed five fresh-service/model loads and 20
requests after one retained-model warmup:

| Metric | Median | p95 |
| --- | ---: | ---: |
| Native cold total | 1,084 ms | 1,105 ms |
| Native model verification + load component | 286 ms | 296 ms |
| Native warm total | 782 ms | 796 ms |
| Native warm decode component | 781 ms | 795 ms |

Compared with the Phase 0 repeated-process baseline, native cold total improved
about 15.3% at median and 23.9% at p95; retained-model warm total improved about
38.9% at median and 45.2% at p95. Release cancellation interrupted a synthetic
220-second active decode and returned after context cleanup in 781 ms; this is
not the optional streaming candidate's 250 ms evidence gate. These are fixture/runtime measurements, not
hotkey-to-paste or first-partial measurements. Memory, idle CPU, GPU, microphone,
overlay, target activation, clipboard, and full end-to-end latency remain
unverified.

### Commands and verification

Final Phase 2 gates on 2026-08-03:

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all -- --check` | PASS |
| Compile | `cargo check --all-targets --all-features` | PASS |
| Tests | `cargo test --all-targets --all-features` | PASS: 231 discovered, 226 passed, 0 failed, 5 ignored |
| Strict lint | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Debug build | `cargo build --all-features` | PASS |
| Debug native fixture | `cargo test transcription_service_jfk_smoke_uses_the_whisper_cpp_facade --all-features -- --ignored` with local paths | PASS; first load and retained warm request returned non-empty text |
| Benchmark | `cargo test --release native_runtime_jfk_cold_and_warm_benchmark --all-features -- --ignored --nocapture` | PASS; 5 cold + 20 warm results above |
| Native cancellation | release ignored `native_runtime_cancellation_interrupts_active_decode` | PASS; lock-free abort stopped a synthetic 220-second decode and completed cleanup in 781 ms |
| Release build | `cargo build --release --all-features` | PASS |
| Runtime package | `powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\bundle-whisper-runtime.ps1 -Profile release` | PASS; every file size/hash validated before staged copy |
| Release native fixture | `cargo test --release transcription_service_jfk_smoke_uses_the_whisper_cpp_facade --all-features -- --ignored` | PASS against the release runtime layout |
| Boundary | `runtime_router::tests::concrete_runtime_boundary_is_confined_to_the_router` plus `rg` review | PASS |

All Cargo invocations continued to emit the pre-existing non-fatal
`could not canonicalize path C:\Users\huang` warning.

### Risks, unverified behavior, and Phase 3 entry

| Item | Risk | Mitigation / next evidence |
| --- | --- | --- |
| The linear resampler has no anti-alias filter and no loudness normalization. | **Medium** audio-quality risk | Replace with the Phase 6 shared worker DSP and deterministic quality fixtures. |
| Native model paths use upstream's UTF-8 narrow `char *` API; non-Unicode paths are rejected and non-ASCII Windows paths were not exercised. | **Medium** compatibility risk | Add a non-ASCII Unicode-path fixture or a verified wide-path adapter before Supported status. |
| Upstream can assert/terminate on some invalid native inputs; crash isolation is incomplete. | **High** recovery risk | Add failure injection and crash recovery before Supported status; never treat model-load failure as CLI fallback. |
| The capacity-one dedicated worker intentionally serializes model comparison and inference. | **Medium** Playground/performance limitation | Phase 7 keeps one active decode and one newest pending snapshot; do not add an unsafe context pool. |
| Primary-runtime cancellation completes context cleanup in 781 ms on the release fixture; transitional process adapters remain non-cancellable. | **Medium** session-safety gap | Phase 4 must couple cancel to authoritative session sequencing; the optional streaming handler still must meet its separate 250 ms gate. |
| Runtime/model verification hashes a path immediately before reopening it, but Windows path-based verification cannot completely eliminate a same-user TOCTOU swap. | **Low** local hardening risk | Phase 9 activates immutable private staged artifacts and records file identity; never load an unverified external artifact. |
| Only base.en/JFK on Windows CPU has native behavior evidence. | **High** compatibility limitation | Phase 3 starts every model Experimental and runs the complete compatibility gate per artifact/platform. |
| No desktop microphone, hotkey, overlay, target focus, or paste row was executed. | **High** release-readiness gap | Execute the Windows manual matrix in later vertical-slice phases; current automation is not a desktop sign-off. |

Phase 3 begins from a one-handler GO for the native vertical runtime slice, but
model compatibility remains NO-GO for Supported status and native streaming
remains NO-GO. The named Zipformer evaluation may add the sole optional second
handler only if every quantitative gate passes; otherwise the final handler
count remains one.

## Phase 3 checkpoint - normalized catalog and compatibility gate

Phase 3 keeps exactly **one logical runtime handler**:
`TranscribeCppRuntime`. `OnnxSpeechRuntime` was not added. Runtime selection no
longer depends on the `whisper_cpp_` model-ID prefix: `RuntimeRouter` resolves a
closed manifest requirement and remains the only component that maps it to a
concrete handler.

### Normalized catalog decision

`src/model_catalog.rs` is now the authoritative source for the four primary
artifacts. Each immutable manifest records its minimum runtime version,
architecture, actual GGML format, immutable repository revision, filename,
exact byte size and SHA-256, languages, capabilities, roles, compatibility
status, and linked evidence. Runtime-package metadata remains separate from
model-artifact metadata.

The application-facing `ModelDescriptor` deliberately omits runtime kind,
backend/family, architecture, format, repository revision, filename, and hash.
The Models, Transcribe, and Playground views receive descriptors from
`TranscriptionService`, show explicit compatibility status, and no longer
expose backend filters, badges, family-coded quick actions, or legacy model
records. Opaque compatibility-provider lookup and primary CLI discovery are
confined to `compatibility_bridge.rs`; concrete legacy validation is confined
to `stt`. New managed downloads accept a neutral `ModelId` and resolve URL,
destination, exact size, and SHA-256 from the normalized manifest. A Rust test
and source gate reject model-family terms/IDs, backend field access, provider
lookup, runtime catalog calls, or concrete adapter imports in production
`app.rs`; they also reject concrete handler symbols outside the router and
concrete adapter paths outside the private compatibility bridge.

| Normalized model ID | Exact artifact | Evidence | Status | Roles | Streaming |
| --- | --- | --- | --- | --- | --- |
| `whisper_cpp_tiny_en` | `ggml-tiny.en.bin`, 77,704,715 bytes, SHA-256 `921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f` | Historical process fixture only | **Experimental** | None | Final-only batch |
| `whisper_cpp_base_en` | `ggml-base.en.bin`, 147,964,211 bytes, SHA-256 `a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002` | Windows x64 CPU native load/JFK/cancel/unload-reload/Auto-to-CPU partial evidence | **Experimental** | None | Final-only batch |
| `whisper_cpp_small_en` | `ggml-small.en.bin`, 487,614,201 bytes, SHA-256 `c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d` | Historical process fixture only | **Experimental** | None | Final-only batch |
| `whisper_cpp_medium_en` | `ggml-medium.en.bin`, 1,533,774,781 bytes, SHA-256 `cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356` | Historical process fixture only | **Experimental** | None | Final-only batch |

All use immutable Hugging Face revision
`5359861c739e955e79d9a303bcbc70fb988958b1` and require the pinned primary
runtime at or above 1.9.1; the router now enforces that minimum rather than
recording it passively. The validation rules reject duplicate IDs, unsafe or
malformed artifact metadata, status/evidence mismatches, `Supported` without
every required gate and a hashed machine-readable receipt, receipt claims not
bound to embedded runtime-package/corpus/results artifacts, duplicate roles,
and roles on a non-Supported model. Production descriptor and runtime-manifest
lookups enforce catalog validation. No model is Supported and no Fast English,
Balanced multilingual, High accuracy, or Low memory role is curated in this
phase.

The eleven older faster-whisper, Vosk, offline sherpa, Moonshine, and Parakeet
records remain only as private compatibility/migration records so existing
configuration, installed paths, and user artifacts are not silently deleted.
They are absent from the normalized Models catalog and contribute no evidence
to a shipped runtime handler. Their transitional execution source remains for
the Phase 11 evidence-based retirement decision.

### Named Zipformer evidence gate: NO-GO

The exact candidate is
`sherpa-onnx-streaming-zipformer-en-2023-06-26` with sherpa-onnx v1.13.4 at
commit `142807252687d81b40d6315f23470a1512a00de3`. Upstream documents it as an
English online Zipformer model and documents a native streaming C API. The
checked-out application, however, has only a distinct offline Zipformer entry
and a sherpa-onnx 1.13.3 Python batch runner. That runner is not candidate
evidence and was not silently substituted.

| Required gate | Result | Evidence / blocker |
| --- | --- | --- |
| Native v1.13.4 package and exact model pins | **UNVERIFIED / FAIL CLOSED** | No local v1.13.4 native package; no exact candidate revision, complete file list, byte sizes, or SHA-256 record. |
| Warm first-partial p95 <= 800 ms | **UNVERIFIED** | No native candidate handler or first-partial stream exists. |
| At least 30% below primary rolling-preview p95 | **UNVERIFIED** | Primary rolling preview is a Phase 7 deliverable, so the required same-machine comparator does not exist. Phase 2 warm final p95 796 ms is not reused as first-partial evidence. |
| RTF < 1 | **UNVERIFIED** | No pinned candidate measurement on the shared corpus. |
| Cancellation acknowledgement <= 250 ms | **UNVERIFIED** | No native candidate cancellation protocol or samples. The primary handler's 781 ms cleanup completion is unrelated evidence. |
| WER regression <= 3 absolute percentage points | **UNVERIFIED** | No versioned same-corpus references, normalization policy, or candidate results. A single editable Playground reference is not a compatibility corpus. |
| Common contract and unload/reload | **UNVERIFIED** | Candidate is not implemented behind `SpeechEngine`. |
| Crash recovery and memory | **UNVERIFIED** | No crash harness, working-set sampler, ceiling, or soak results. |
| Windows/platform matrix and native in-memory PCM topology | **UNVERIFIED** | No candidate platform package was installed or exercised; the old Python/WAV bridge is explicitly excluded from evidence. |

Because every quantitative/contract gate must pass and missing evidence is a
failure, the decision is **NO-GO**. The catalog contains a machine-checked gate
record, but there is no ONNX dependency, runtime variant, adapter, package,
selectable catalog entry, or simulated progress/control.

Primary sources consulted for this decision:

- sherpa-onnx v1.13.4 release:
  <https://github.com/k2-fsa/sherpa-onnx/releases/tag/v1.13.4>
- native streaming C API:
  <https://k2-fsa.github.io/sherpa/onnx/c-api/html/online_asr.html>
- exact online Zipformer model documentation:
  <https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html>

### Commands, measured results, and remaining risk

Phase 3 final verification on 2026-08-03:

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all -- --check` | PASS |
| Strict lint | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Tests | `cargo test --all-targets --all-features` | PASS: 252 discovered, 247 passed, 0 failed, 5 environment-required ignored |
| Debug build | `cargo build --all-features` | PASS |
| Release build | `cargo build --release --all-features` | PASS |
| Boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` plus Rust source-boundary test | PASS: one handler, manifest routing, neutral UI, family-coded IDs rejected, legacy provider/adapter selection confined to its private bridge |
| Runtime package | `bundle-whisper-runtime.ps1 -Profile release` | PASS after explicitly archiving the previous Phase 2 evidence package; fresh files revalidated and staged |
| Release fixture | ignored exact service JFK smoke with the pinned release package/base.en/JFK paths | PASS: cold load 290 ms, first decode 791 ms, retained decode 780 ms, explicit unload/reload passed |

Phase 3 intentionally makes no new latency-improvement claim: the inference
path is unchanged from Phase 2 and the smoke values are a single confirmation,
not the required 5-cold/20-warm measurement. The saved Phase 0-versus-Phase 2
results remain the current comparable latency evidence.

Known risks remain: the normalized UI and router boundary are enforced, but
flat configuration and private legacy provider records still contain
compatibility aliases until the Phase 4 schema migration and Phase 11
retirement; legacy managed-download helpers remain dormant but preserved for
the Phase 9 transaction rewrite; primary
native crash isolation is incomplete; only base.en on Windows CPU has current
native fixture evidence; no desktop/microphone/target/paste row was executed;
and the optional native-streaming requirement remains a concrete NO-GO until
the Phase 7 comparator and complete candidate evidence harness exist.

## Phase 4 checkpoint - authoritative sessions and typed settings

Phase 4 replaces the test-only session reducer with the application authority
and migrates the flat configuration into a durable sectioned schema. It does
not add a runtime handler or promote a model: the application still ships
exactly **one logical handler**, `TranscribeCppRuntime`, and all four normalized
models remain **Experimental**.

### Session coordinator and concurrent model loading

`SessionCoordinator` is the sole source of truth for one active Dictation or
Comparison session. It owns checked monotonic session/request allocation and
the legal phase path:

```text
Idle -> StartingCapture -> Capturing -> FinalizingCapture
     -> Transcribing -> Output -> Idle
```

Cancellation retires any active phase. Request events are accepted only when
the session, purpose, request, model, and sequence match the current state;
duplicates, stale completions, out-of-order updates, and wrong-model results
fail closed. Explicit stop outranks endpoint and maximum-duration stop reasons.
Normal output can begin only after its only final request succeeds, and the
coordinator is completed immediately after the one output attempt. Comparison
sessions wait for every registered request and retain request-scoped cleanup.

The normal dictation path starts neutral `TranscriptionService::preload_model`
work immediately after capture enters `Capturing`. Load completion remains
correlated to the initiating session and model. A stale completion is ignored;
a preload failure is non-fatal because the final service request may retry the
same validated runtime path. This overlaps model loading with capture without
adding an async runtime or exposing a concrete runtime above the service.

Superseding a run cancels native work and registered transitional process
trees before retiring the coordinator. An opaque service task captures both
native and compatibility cancellation generations and registers synchronously
before audio preparation is dispatched; it remains owned through WAV deletion
and transcription. A request cancelled before process registration cannot start later.
Transitional Unix children use a dedicated process group, while Windows wraps
each child and its descendants in a kill-on-close Job Object. Quit waits a
bounded interval for request/process registry drain and transient-audio cleanup.
Playground state is reset before the new microphone attempt, so a
capture-start failure cannot leave obsolete cards in Running state.

### Versioned settings and recovery behavior

`AppConfig` schema version 1 is split into General, Recording, Streaming,
Output, Overlay, History, Performance, and Developer sections. Streaming,
Overlay, and History intentionally contain only preserved extension data until
their real Phase 5/7/10 behaviors exist; no disconnected controls or fake
settings were added.

Legacy flat keys and compatibility aliases migrate field by field. Missing or
invalid values fall back only at the affected field, invalid managed-install
metadata does not discard a valid record, and unknown root, section, and nested
install fields survive round trips. A syntactically or structurally corrupt
document is copied to a timestamped `corrupt` backup before a salvaged document
is written. Every valid document that changes during migration receives a
timestamped `pre-v1-migration` backup, preserving rollback to Phase 3.

Normal UI edits are coalesced through an approximately 300 ms debounce. Quit
and `on_exit` flush pending settings. Runtime activation remains transactional;
after its immediate durable save it clears any older scheduled snapshot so the
snapshot cannot erase new runtime metadata. Writes use a same-directory
create-new temporary file, flush and file sync, atomic replacement, Unix parent
sync, and bounded Windows sharing-violation retries. Unix settings directories
and files are restricted to `0700` and `0600` respectively.

### PCM lifecycle hardening at the Phase 4 boundary

Phase 6 still owns the planned replacement of callback WAV writes with the
native fixed-capacity ring. Phase 4 nevertheless closes exit-path privacy gaps
in the inherited recorder: recordings are created with collision-resistant
process-qualified names, `create_new`, and private Unix permissions; failed
startup removes a partial file; Quit and `on_exit` wait up to two seconds for
recorder finalization and delete the WAV; and startup scavenges recording files
older than 24 hours. This is hardening of the preserved vertical slice, not a
claim that Phase 6 native capture is complete.

### Tests and measured results

Phase 4 adds coverage for legal/illegal transitions, busy/overflow behavior,
explicit-stop priority, cancellation in every phase, stale/cross-purpose/wrong
model events, sequence and duplicate rejection, multiple comparison requests,
preload correlation, preload dispatch during capture, settings migration and
salvage, corrupt and pre-migration backups, future-field preservation, debounce,
atomic replacement failure, transactional-save ordering, transitional process
cancellation generation races, process-tree termination, bounded cancellation
acknowledgement, Playground supersession, and recorder shutdown/deletion.

Final Phase 4 verification on 2026-08-03:

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all -- --check` | PASS |
| Compile | `cargo check --all-targets --all-features` | PASS |
| Strict lint | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Tests | `cargo test --all-targets --all-features` | PASS: 283 discovered, 278 passed, 0 failed, 5 environment-required ignored |
| Debug build | `cargo build --all-features` | PASS |
| Boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` plus Rust source-boundary test | PASS: one logical handler; normalized service/router boundary retained |

No comparable before/after latency claim is made in Phase 4. The decoding path
and fixture artifact are unchanged; the new preload overlap requires a live
microphone/hotkey run to measure and no such desktop run was available. The
saved Phase 0-versus-Phase 2 benchmark remains the current valid performance
evidence.

### Risks, unverified behavior, and Phase 5 entry

- **Medium:** live Windows hotkey, microphone, preload overlap, cancellation,
  target focus, and exactly-once paste remain manually unverified. Execute the
  matrix before release claims.
- **Medium:** Phase 6 must replace the inherited callback WAV writer; a hard
  process termination can still leave a file newer than the 24-hour scavenger
  threshold.
- **Low:** two simultaneously running Scribe processes can still race whole-file
  settings snapshots. Add single-instance enforcement or revision-aware locking
  during platform hardening.
- **Unverified:** no macOS/Linux compile or desktop exercise was performed in
  this Windows checkpoint; no model compatibility status changed.

Phase 5 may now consume coordinator state and typed Overlay settings while
keeping all concrete runtime selection private to `RuntimeRouter`.

## Phase 5 checkpoint - native shell, overlay, and target-safe output

Phase 5 replaces the stale six-backend-oriented shell with runtime-neutral
navigation and adds a pre-created native dictation overlay driven only by real
coordinator, level, transcript, and error state. It does not change runtime or
model compatibility: the application still contains exactly **one logical
runtime handler**, `TranscribeCppRuntime`; the four normalized primary models
remain **Experimental**; no model is **Supported**; and the exact Zipformer
candidate remains a documented **NO-GO** with no `OnnxSpeechRuntime` shipped.

### Shell, connected pages, and accessibility

The main shell now exposes Transcribe, General, Models, History, Advanced, and
About. Debug is visible only when the persisted Developer setting enables it.
Model comparison remains the existing functional comparison workflow and is
also reachable inside Models; no duplicate comparison system was created.
General and Advanced expose only settings already connected to application
behavior. History truthfully shows only the latest in-memory transcript until
Phase 10 adds persistent storage, search, and retention.

Reusable controls enforce at least a 44 px primary interaction height. The
shell and overlay use the checked-in Scribe light/dark tokens, visible focus,
keyboard navigation, non-color phase cues, and AccessKit labels. Overlay text
is a polite live region that distinguishes committed and tentative content;
the real microphone level has a named numeric progress semantic. Navigation
exposes a landmark, heading, and selected state, with a high-contrast visible
dot/bold cue rather than color alone. Light semantic text tokens are covered by
4.5:1 contrast tests. Reduced-motion state is obtained from the Windows system
setting; the overlay has no decorative animation that must be disabled.

### Pre-created overlay and platform boundary

An immediate eframe secondary viewport with a stable identity is submitted on
every frame from application startup, including while hidden. It is borderless,
transparent, always-on-top, excluded from the taskbar, inactive, and mouse
passthrough at the toolkit level. Live mode shows phase, elapsed time, measured
audio levels, committed text, and a visually separate tentative suffix;
Minimal shows a compact phase/level presentation; Off remains hidden. The
application never invents transcript fragments or download progress.

On Windows, the platform adapter additionally applies `WS_EX_NOACTIVATE`,
`WS_EX_TOOLWINDOW`, and `WS_EX_TRANSPARENT`, uses non-activating topmost window
positioning, and places the viewport within the captured target monitor's DPI-
aware work area. The original foreground handle and process are captured
before coordinator state changes can reveal the overlay. Every Scribe-owned
window is rejected by process identity. The viewport remains hidden unless the
actual HWND reports all required extended styles and non-activating placement
succeeds; lookup or Win32 call failure therefore fails closed. Other platforms
keep the effective overlay mode Off until equivalent no-focus enforcement
exists.

These implementation properties are covered by deterministic state, geometry,
and source tests. Physical Windows focus, taskbar, multi-monitor, mixed-DPI,
screen-reader, and click-through behavior remain **NOT VERIFIED** until the
manual matrix is executed.

### Concurrent capture startup, real levels, and safe output

Microphone startup now runs on a native worker so the UI can display Preparing
immediately. Model preload is dispatched for the same correlated session while
capture opens, and an explicit stop during pending startup is retained and
applied before a returned stream can become active. The current CPAL callback
publishes only a lock-free atomic aggregate level to the UI; no sample frames
cross into egui, JavaScript, webview events, or general UI IPC. This is an
interim Phase 5 meter: the inherited callback still writes WAV data behind a
mutex and Phase 6 must replace that path with the fixed-capacity SPSC ring and
native preparation worker.

Final output now carries an opaque captured target through the session. On
Windows, automatic paste is attempted exactly once only when that exact target
is still foreground immediately before output and the clipboard still contains
the finalized text Scribe placed there. A missing, closed, changed, or Scribe-
owned target results in copy-only fallback with no synthetic keystroke. An
external clipboard change before paste suppresses the keystroke without
overwriting the external value; the final text remains visible in Scribe. On
Windows, the clipboard sequence generation is also checked so same-text changes
to HTML, RTF, or other formats cannot masquerade as Scribe's clipboard write.
Clipboard restoration occurs only when the final transcript still owns the
clipboard, so an external clipboard change is not overwritten. Target
reactivation and image-format restoration remain Phase 8 work. macOS and Linux
conservatively use copy-only output because foreground-target safety is not yet
implemented there.

The Windows paste driver revalidates the target immediately before injection
and submits Control-down, V-down, V-up, and Control-up in one `SendInput` batch.
If Windows accepts only a prefix, Scribe makes a best-effort V/Control release
batch before reporting copy-only fallback. Any safe fallback is shown as an
attention state in the overlay instead of the generic success state.

Output is staged for the next UI update after the correlated coordinator enters
`Output`, so the overlay can present the real Pasting phase before clipboard or
synthetic-input work begins. Starting a newer session retires that pending
output before it can run. Terminal dictation failures use one cleanup path that
shows Error, schedules the overlay to hide, and releases its captured target;
starting another session also retires any prior success/error target promptly.

Tentative text is owned only by `OverlayController`; the application output
path receives the completed full-utterance result and never types tentative
text or backspaces corrections.

### Latency instrumentation and verification

The existing trace now separately records the first real native-callback meter update. Capture
startup and preload are concurrent, and overlay visibility is stamped when the
viewport visibility command is dispatched. That stamp is not evidence of a
physically presented frame. No live hotkey/microphone run was available, so
Phase 5 makes no before/after latency claim. The saved Phase 0-versus-Phase 2
same-fixture measurements remain the latest valid comparison.

Final Phase 5 verification on 2026-08-03:

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all -- --check` | PASS |
| Compile | `cargo check --all-targets --all-features` | PASS |
| Strict lint | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Tests | `cargo test --all-targets --all-features` | PASS: 323 discovered, 318 passed, 0 failed, 5 environment-required ignored |
| Debug build | `cargo build --all-features` | PASS |
| Boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` plus Rust source-boundary tests | PASS: one logical handler; concrete runtime selection remains private to the router |

The relevant deterministic coverage includes typed overlay settings and
unknown-field preservation; stale overlay session/revision rejection; Live,
Minimal, Off, hidden-viewport, accessibility, and geometry behavior; current-
process target rejection; target loss/change; exactly-once paste; clipboard
content/generation ownership races and failures; partial input-batch cleanup;
fail-closed HWND hardening; terminal overlay cleanup; a visible Pasting frame;
aggregate level availability/conversion; navigation landmark/selection,
semantic-token contrast, Debug gating, and explicit stop while capture startup
is pending. The final QA regressions additionally drive capture readiness after
that pending stop through WAV finalization and exactly one transcription
dispatch, exercise preload completion on both sides of capture readiness,
prove stale success/error events cannot arm the newer overlay's hide deadline,
expire a real hide deadline and retire its captured target, and verify
application-level output consumption is exactly once. Windows input injection
tests cover every partial batch length, the exact V/Control release sequence,
cleanup failure without retry, and the no-cleanup success path.

### Risks, unverified behavior, and Phase 6 entry

- **High release risk:** no real Windows hotkey, microphone, foreground target,
  paste, overlay focus/taskbar/click-through, screen-reader, multi-monitor, or
  mixed-DPI row has passed. Run the updated matrix and retain evidence.
- **Medium privacy/performance risk:** the inherited callback still locks and
  writes WAV samples. Phase 6 must move all PCM through the fixed-capacity ring,
  prepare canonical mono 16 kHz `f32` once, and delete the obsolete callback
  file path.
- **Medium output risk:** Phase 5 validates but does not reactivate a captured
  target. An HWND/PID can theoretically be reused, and no foreground check can
  be atomic with another process changing focus. Phase 8 owns process-handle/
  target-lifetime hardening, reactivation plus revalidation, richer clipboard
  restoration, and final output failure injection.
- **Medium portability risk:** non-Windows platforms intentionally suppress the
  overlay and auto-paste. Native safe adapters or a documented conservative
  support decision are still required.
- **Low maintainability risk:** `app.rs` remains large, although shell pages,
  controls, theme, overlay state/view, and platform integration now have
  dedicated modules. Continue bounded extraction as later phases introduce
  audio, streaming, output, and history ownership; do not duplicate systems.

Phase 6 can now replace the remaining callback/file capture path while reusing
the real overlay level/state interface and the authoritative session boundary.

## Phase 6 checkpoint - native audio pipeline, VAD, and endpointing

Phase 6 removes microphone WAV transport from the normal and comparison paths.
CPAL now feeds a fixed-capacity, preallocated SPSC ring; a dedicated native
worker produces the one canonical mono 16 kHz `f32` `PreparedAudio` shared by
normal and Playground final consumers and ready for Phase 7 preview snapshots.
The application still contains exactly **one logical
runtime handler**, `TranscribeCppRuntime`. All four normalized primary models
remain **Experimental**, zero models are **Supported**, and the exact Zipformer
candidate remains **NO-GO** without an `OnnxSpeechRuntime`.

### Callback boundary and worker DSP

The CPAL data callback now performs only sample-format conversion, bounded SPSC
enqueue, atomic dropped-sample/fault updates, and return. Its stream-error
callback performs one atomic fault update. Neither callback locks a mutex,
allocates, writes a file, calls the UI, or blocks. The ring is allocated before
the stream starts, has a two-second target capacity bounded between 65,536 and
2,000,000 interleaved samples, and fails the capture with a structured overflow
error rather than silently transcribing corrupt audio.

The consumer worker owns channel downmixing, streaming linear resampling to
16 kHz, finite/range normalization, deterministic bounded RMS normalization,
30 ms RMS/peak publication, adaptive noise-floor VAD, trimming, post-roll, and
construction of `PreparedAudio`. Loudness normalization occurs only after VAD
classification, targets 0.1 RMS, caps peaks at 0.95, and limits gain to 8x so
the detector and meter always observe the unamplified signal. The legacy WAV
decoder remains for fixtures and the private compatibility bridge; it is no
longer the microphone application contract.

The internal SPSC implementation uses one producer and one consumer with
release/acquire publication. Both handles are statically `!Sync`, mutation
requires exclusive access, and a restart producer can be minted only after the
old callback-owned token is destroyed. Its wrap, full-buffer, restart-handoff,
and 100,000-sample concurrency behavior is covered by deterministic tests. The
worker drains at most 4,096 interleaved samples before rechecking stop and fault
state. No new async runtime or audio dependency was added.

### Endpointing, recovery, and application integration

Typed Recording settings now persist and salvage VAD enablement plus the
required defaults: 150 ms speech confirmation, 450 ms internal pause, 900 ms
endpoint silence, 250 ms pre-roll, and 200 ms post-roll. Normalization clamps
the timings into an ordered safe range while preserving future unknown fields.
The Advanced controls edit these real values directly; no disconnected control
was introduced.

The worker records an explicit stop as soon as it observes the shortcut/UI
request and then retains up to the configured post-roll before finalizing.
Explicit stop outranks inferred endpoint and maximum duration in both the audio
worker and authoritative coordinator. VAD runs in both shortcut modes when
enabled so trimming and no-speech behavior stay consistent, but silence may end
capture only in Toggle mode; Hold-to-talk waits for shortcut release or the
maximum duration. Silence/no confirmed speech returns no audio, transitions the
session to a cancelled terminal state, pastes nothing, and leaves the prior
transcript untouched. With VAD disabled, capture never infers an endpoint and
returns the complete canonical recording.

The app receives structured capture state, levels, metrics, errors, and the
final `Arc<PreparedAudio>` only. It sends no PCM through a webview, JavaScript,
general UI event, or filesystem path. Normal dictation and Playground reuse the
same in-memory audio; comparison requests clone the `Arc` and release it after
the final correlated request. The latency trace now says `capture finalized`
instead of claiming a WAV was written.

Stream faults trigger at most two complete rebuild attempts, including fresh
device enumeration, config lookup, stream construction, and play, with a
bounded 50 ms backoff. A changed format fails visibly rather than mixing
incompatible samples, and exhaustion retains the last structured error.
Credible native input is bounded to 8-384 kHz and 1-32 channels. Loaded settings
and the worker independently cap capture at 600 seconds; prepared PCM has a
separate 602-second frame ceiling, and trimming compacts its owned buffer in
place instead of doubling peak memory. Dropped sessions request stop and move a
late worker to a named reaper rather than losing its join handle. Overflow,
stream fault ordering, fail-then-succeed/exhausted restart, format change,
resource bounds, and defensive drop are injected in tests. Physical device
disconnect/recovery remains manual evidence, not an automated compatibility
claim.

### Runtime-worker failure found by the gate

The first Phase 6 release benchmark attempt terminated with Windows
`STATUS_ACCESS_VIOLATION` after the first cold native load. The cold loop dropped
each `TranscriptionService`, but its worker thread could unload the dynamically
loaded runtime concurrently with creation of the next service. The fix makes
the last cloned worker handle send an explicit shutdown command, synchronously
unload on the owning native worker, and join that thread. Shutdown waits at most
five seconds for the native acknowledgement and detaches with a diagnostic if a
malfunctioning runtime does not respond. A lifecycle test proves clones retain
the worker and the normal final drop completes shutdown. The same
5-cold/20-warm release command then passed; the failed attempt is retained here
as diagnostic evidence rather than omitted.

### Commands and measured results

Final Phase 6 verification on 2026-08-04:

| Check | Command | Result |
| --- | --- | --- |
| Format | `cargo fmt --all -- --check` | PASS |
| Compile | `cargo check --all-targets --all-features` | PASS |
| Strict lint | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Tests | `cargo test --all-targets --all-features` | PASS: 358 discovered, 353 passed, 0 failed, 5 environment-required ignored |
| Debug build | `cargo build --all-features` | PASS |
| Boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` plus Rust source-boundary tests | PASS: one logical handler; concrete selection remains private to the router |
| Native service fixture | ignored exact JFK service smoke, pinned v1.9.1/base.en/CPU | PASS: non-empty final text; first load 4,135 ms, first decode 776 ms, retained decode 785 ms; explicit unload/reload passed |
| Release native benchmark | ignored exact 5-cold/20-warm JFK benchmark | PASS after synchronous worker-shutdown fix and review hardening: cold total median/p95 1,177/1,189 ms; cold load 335/356 ms; warm total 817/884 ms; warm decode 815/882 ms |

The release benchmark used the same machine, CPU resolution, pinned base.en
artifact, JFK fixture (352,078 bytes; SHA-256
`59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`), and
runtime package as the saved Phase 2 measurement. Phase 2 measured cold total
median/p95 1,084/1,105 ms and warm total 782/796 ms. The final Phase 6 result is
93/84 ms slower at cold median/p95 and 35/88 ms slower at warm median/p95.
Because this phase did not change inference and the runs were not interleaved,
this is recorded as observed variance, not an improvement or causal regression
claim. No live microphone/hotkey/overlay/target run was available, so
hotkey-to-overlay, hotkey-to-capture, first meter, first partial,
stop-to-final, final-to-paste, total duration, memory, and idle CPU remain
**NOT VERIFIED**.

Automated coverage added in this phase includes sample-format conversion,
downmix, 48 kHz and 44.1 kHz resampling, finite/range and bounded loudness
normalization, ring wrap/overflow/concurrency, exact 30 ms meter publication,
restart token handoff, bounded worker drain, adaptive noise floor, speech
confirmation, pause/endpoint timing, pre/post-roll, no-speech, VAD-disabled
capture, Toggle-only silence endpointing, Hold-to-talk release ownership,
explicit stop priority, structured stream faults, complete bounded
restart, input-format/resource bounds, in-memory audio ownership and drop,
background adaptation, sub-confirmation bursts, paused-speech resumption,
settings salvage/legacy migration/unknown-field round trip, stale session
safety, reachable explicit/max-duration no-speech no-output behavior, and
synchronous runtime shutdown. AccessKit coverage also proves that the maximum
duration and five VAD timing spin buttons are programmatically associated with
their visible labels.

### Risks, unverified behavior, and Phase 7 entry

- **High release risk:** no real Windows microphone, unplug/restart, endpoint,
  first-syllable, overflow-under-load, hotkey, overlay, target, or paste row has
  passed. The manual matrix remains release-blocking evidence.
- **Medium audio-quality risk:** the deterministic streaming linear resampler
  does not apply a dedicated anti-alias low-pass filter. Add a measured
  band-limited resampler only if fixture/listening evidence justifies its cost.
- **Medium concurrency risk:** the reproduced normal-shutdown overlap is fixed
  and the benchmark passed. If native unload exceeds the five-second shutdown
  deadline, the worker is deliberately detached and a newly created service
  could overlap that exceptional unload; a process-wide lifecycle gate and
  repeated process-level stress remain Phase 11 work.
- **Medium driver risk:** a permanently hung CPAL call leaves its capture worker
  and named reaper alive. Repeated starts must be blocked or supervised by one
  shared reaper before production; physical unplug/hang rows remain
  release-blocking evidence.
- **Medium portability risk:** CPAL compilation passed on the current Windows
  host only. Real macOS/Linux device recovery and conservative output behavior
  remain unverified.
- **Low compatibility risk:** stale recovery WAV cleanup remains intentionally
  available for older builds, and the private legacy adapter can still create
  a short-lived canonical WAV for transitional providers. Neither is used by
  normal microphone capture; removal waits for Phase 11 retirement evidence.

Phase 7 can consume the canonical native pipeline and authoritative
session/sequence boundary to add bounded rolling preview. Tentative text must
remain overlay-only, with one active decode and only the newest pending
snapshot retained.

## Phase 7 - Incremental transcription and stabilization

Phase 7 is implemented as rolling batch preview inside the existing native
boundary. It does not claim that the primary runtime has acquired a native
streaming API. The final handler count remains **one**:
`TranscribeCppRuntime`, selected only by `RuntimeRouter`. `OnnxSpeechRuntime`
was not added because the named Zipformer candidate still lacks the complete
pinned package, shared-corpus quality, lifecycle, cancellation, crash, memory,
and platform evidence required by the Phase 3 gate.

### Implemented data flow and decisions

```text
CPAL callback -> fixed SPSC ring -> native capture/DSP worker
  -> canonical 16 kHz buffer -> opaque replace-latest preview publisher
  -> one rolling scheduler -> TranscriptionService -> RuntimeRouter
  -> TranscribeCppRuntime -> committed/tentative text event
  -> SessionCoordinator correlation gate -> overlay only

capture stop -> close preview mailbox/drop pending -> bound active decode
  -> full PreparedAudio final pass -> final overlay replacement
  -> existing exactly-once output path
```

- The capture worker publishes at exact 4,000-frame/250 ms boundaries. Each
  snapshot contains at most the newest 48,000 frames/3 seconds; the stabilizer
  treats 10,400 frames/650 ms as rolling-boundary overlap. If the capture
  worker observes multiple elapsed intervals at once, it publishes only the
  newest complete boundary instead of cloning obsolete catch-up windows.
- Snapshot normalization operates on a cloned window. A deterministic test
  feeds identical audio through preview-on and preview-off pipelines and proves
  the final `PreparedAudio` values are identical.
- One named scheduler decode worker may be active and a capacity-one mailbox
  retains only the newest pending snapshot. Closing drops pending work. The
  normal stop path transfers the join handle into app-owned drain state and
  polls without blocking egui. It allows two seconds for a measured sub-second
  decode to finish, then requests cancellation once. If acknowledgement is
  still absent two seconds later, the handle remains owned and new dictation
  stays blocked until the worker exits; no timed-out worker is detached.
- `TranscriptionService::transcribe_preview` calls only the primary native
  worker. It rejects transitional legacy models instead of using the CLI/WAV
  fallback; the full-utterance final path retains its Phase 6 fallback policy.
- Stable-prefix reconciliation requires two compatible passes and a 700 ms
  horizon, caps comparison context at 60 words, rejects stale correlation
  sequences, deduplicates rolling overlap, and keeps display punctuation/case
  tentative until repeated.
- `SessionCoordinator` owns a separate preview request so preview sequencing
  cannot complete or block the final request. Every text event is accepted by
  the coordinator before it reaches `OverlayController`.
- Tentative and committed preview text never mutates the application transcript,
  `PendingOutput`, clipboard, paste, Playground result, or any third-party
  application. The final full-pass result allocates a newer overlay revision,
  clears tentative text, and remains the only output candidate.
- Preview uses a current settings snapshot, just like preload and final decode,
  while retaining the same warm native worker. Native segment timestamps are
  converted to absolute canonical-audio frames; untimed segments retain the
  conservative fallback alignment.
- The pinned fixture exposed Whisper's `[BLANK_AUDIO]` sentinel as the first
  nominally non-empty result. The private primary-runtime adapter now removes
  that model-specific sentinel, so it is neither rendered nor counted as first
  speech text.
- Advanced settings expose only real behavior: `Auto`, `Rolling preview`, and
  `Final text only`. `Auto` currently selects rolling batch preview because no
  model advertises proven native streaming. Playground remains final-only.
  Timing and stability constants are intentionally not configurable.
- The mode selector has an explicit AccessKit name. The visible transcript
  exposes both committed and tentative portions as non-live accessible text,
  while a separate polite live node announces committed deltas and final text
  only. Preview failure clears stale tentative text and announces that the
  final pass continues. Typed recovery guidance distinguishes a retryable
  terminal error from a still-draining preview worker, and inactive meter bars
  retain at least 3:1 contrast against the overlay background.

### Compatibility and streaming status

| Model/runtime state | Phase 7 result |
| --- | --- |
| Logical runtime handlers | **1** - `TranscribeCppRuntime` only |
| Native `SpeechStream` capability | **False** for every catalog model; no native-streaming claim |
| Rolling preview | Available only through the primary native router path; legacy CLI adapters fail closed to final-only |
| `whisper_cpp_tiny_en`, `base_en`, `small_en`, `medium_en` | Remain **Experimental**; no model promoted |
| Supported models | **0** |
| Zipformer / `OnnxSpeechRuntime` | **NO-GO**, not shipped; no new evidence satisfies the complete gate |

### Automated verification and measured evidence

| Check | Command | Result |
| --- | --- | --- |
| Format/lint/build | `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo build --all-features` | **PASS** |
| Unit/integration suite | `cargo test --all-targets --all-features` | **PASS** - 406 discovered, 400 passed, 0 failed, 6 environment-gated tests ignored |
| Preview scheduling/DSP | Targeted `streaming::tests` and native pipeline tests | **PASS** - exact cadence/window bounds, one-active/newest-pending, non-blocking retained-handle drain, final-audio identity, exact 650 ms boundary, 699/700 ms horizon, case/punctuation correction, non-empty deletion/reappearance, repeated words, overlap, bounded context, and sequence rejection |
| Output isolation and accessibility | App/coordinator/overlay tests | **PASS** - stale/wrong-model/late-after-close updates rejected; tentative text changes only overlay state; final revision supersedes partials and emits once; Playground/final-only never starts preview; stabilizer-shaped committed/tentative words render with exactly one boundary space and standalone closing punctuation binds without a space; tentative text is inspectable but excluded from polite live announcements |
| Architecture boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` plus Rust source scans | **PASS** - one handler; concrete selection remains private; app shell cannot construct or publish PCM preview snapshots |
| Pinned release fixture | Exact ignored `transcription_service_jfk_smoke_uses_the_whisper_cpp_facade` | **PASS** after stabilization - v1.9.1/base.en/CPU; load 294 ms, first decode 795 ms, warm decode 793 ms, unload/reload passed |
| Final-pass release benchmark | Exact ignored 5-cold/20-warm `native_runtime_jfk_cold_and_warm_benchmark` | **PASS** - cold total median/p95 1,087/1,099 ms; cold load 289/292 ms; warm total 781/800 ms; warm decode 780/798 ms |
| Rolling first-speech benchmark | Exact ignored 5-cold/20-warm `rolling_preview_jfk_first_partial_benchmark` | **PASS** - artifact size/hash and expected JFK speech validated; scheduler-start to first real speech text after filtering the blank sentinel: cold median/p95 2,039/2,049 ms; warm median/p95 1,730/1,754 ms |

The fixture measurements used the same Windows machine, pinned CPU package,
base.en artifact (147,964,211 bytes; SHA-256
`a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002`),
and JFK WAV (352,078 bytes; SHA-256
`59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`).
The preview benchmark is a scheduler-level fixture harness. It releases cloned
canonical fixture frames on the configured 250 ms cadence and exercises native
decode, but bypasses the production capture `Pipeline` and does not include a
real hotkey, microphone startup, driver buffering, overlay painting, or target
output. Its result is therefore a deterministic scheduler-start approximation,
not verified desktop hotkey-to-first-partial latency. Raw milliseconds were:
`cold=[2049,2047,2029,2039,2036]` and
`warm=[1766,1748,1711,1716,1727,1730,1714,1743,1741,1726,1713,1724,1740,1726,1739,1726,1754,1743,1734,1741]`.

For comparison, Phase 6 measured cold final total 1,177/1,189 ms and warm final
total 817/884 ms. Phase 7 measured 1,087/1,099 ms and 781/800 ms respectively.
The runs were not interleaved and Phase 7 did not change final inference, so the
difference is recorded as run-to-run variance, not an improvement claim. Phase
0's repeated-process base.en median/p95 was 1,279.5/1,452.8 ms and is not
directly comparable to retained-native timing. Phase 6 had no first-speech
path, so the truthful 1,730/1,754 ms warm result has no before value and does
not satisfy the Zipformer gate's 800 ms target.

One pre-review parallel suite run reproduced the legacy process-registration
race: another test advanced the global cancellation generation before the
fixture process registered. The test now retries only that pre-registration
race while retaining the registered-process termination assertion. The final
full parallel suite passed all 400 runnable tests, including the hardened case.

### Risks and Phase 8 entry

- **High - desktop evidence:** no real Windows microphone/hotkey/overlay run
  has verified first-partial latency, correction behavior, focus preservation,
  or exactly-once paste with preview enabled. Execute the updated manual rows.
- **Medium - preview load:** CPU rolling inference can occupy the one native
  worker and measured warm first real speech is about 1.7 seconds. Stop closes
  pending work, drains normally for two seconds, and cancels only a slower
  decode. Cancellation can discard warm state before the final pass. Measure
  stop-to-final under live speech and tune only with saved evidence.
- **Medium - alignment quality:** the retained runtime provides segment rather
  than word timing, so words are distributed within each timed segment and
  untimed segments use fallback window alignment. The reconciler is
  deterministic and conservative, but corpus-level correction/duplication
  quality is not yet measured.
- **Low - compatibility:** preview failure is nonfatal and visibly degrades to
  the existing final pass. No compatibility status or native-streaming flag was
  inflated.

Phase 8 can now harden final-pass shutdown and output safety on top of a tested
overlay-only preview path. The release remains **NO-GO** until the manual
Windows vertical slice, native-streaming Definition of Done, supported-model
evidence, and final latency matrix are complete.

## Phase 8 - Final pass and safe output

Phase 8 hardens the existing final-only output system; it does not add another
audio, output, runtime, or clipboard subsystem. The logical handler count
remains **one** (`TranscribeCppRuntime`), all four catalog models remain
**Experimental**, zero are Supported, and `OnnxSpeechRuntime` remains omitted.

### Finalization and output decisions

- Explicit stop retains the Phase 6 native post-roll path. Capture closes the
  preview mailbox, drops its pending snapshot, permits the active decode a
  two-second grace period, then requests cancellation once. If cancellation is
  not acknowledged within another two seconds, the user session fails
  terminally: its queued final pass and paste are discarded, the captured
  target is retired, and the owned worker handle is retained and reaped before
  a new session may use the one runtime. Scribe never detaches the worker or
  races a second handler.
- A normal preview drain still dispatches exactly one full-utterance
  `PreparedAudio` final request. Tentative overlay text is never an output
  candidate. VAD no-speech and a whitespace-only engine result both complete
  without arming output or replacing the previously finalized transcript.
- Raw model text and chosen final text are stored separately. No optional
  cleanup transform is configured in this build, so they are currently
  identical. If a future local transform makes them differ, the current
  History view exposes and copies the preserved raw text rather than silently
  destroying it.
- Windows target capture now records HWND, window thread, PID, process creation
  time, and a unique generation token installed as a property on the captured
  HWND after two stable foreground samples. Windows removes that property with
  the window, so a recycled HWND cannot satisfy the generation check. Property
  installation or lookup failure is copy-only. Before output, Scribe validates
  the live window/process/property identity, requests ordinary
  `SetForegroundWindow`, immediately revalidates the exact foreground target,
  and checks it once more adjacent to the single `SendInput` paste batch.
  Activation denial, dead/recycled identity, or focus mismatch is copy-only. No
  `AttachThreadInput`, forced focus bypass, or paste retry is used.
  Correlated session retirement removes only a still-matching property, and app
  shutdown retires any remaining captured targets.
- Clipboard restoration is transactional for empty state and a bounded
  set of native Windows text/locale/PNG/DIBV5 formats. Snapshot bytes are copied
  while one native clipboard open prevents replacement; payload size is bounded
  before allocation, and PNG/DIBV5 header dimensions before restoration.
  Conditional transcript replacement and conditional restoration each recheck
  the nonzero clipboard sequence and
  expected text while the same native open excludes another writer. Mixed
  source-order detection distinguishes a CF_DIBV5 source from bitmap formats
  converted by Windows. Supported source payloads are treated as bounded opaque
  clipboard bytes and restored together, while derived bitmap/DIB/palette and
  text conversions are regenerated by Windows. HTML, RTF, file-list, private,
  unsafe-header/size, unavailable, or zero-sequence state becomes explicit
  copy-only output with no synthetic keys; this layer does not claim complete
  PNG/DIB semantic decoding.
  A hidden message-only Scribe HWND owns native clipboard mutations so
  `EmptyClipboard`/`SetClipboardData` never operate with a null owner.
- Clipboard generation and exact transcript text are checked again after target
  activation, immediately before `SendInput`, because activation can wake a
  clipboard manager. A partial input batch sends individually checked
  Control-up and V-up cleanup events (one key-up retry each), never another
  paste chord. The transcript editor is disabled while correlated output is
  queued so a one-frame UI edit cannot diverge from the final text being sent.
- Output returns native-boundary timestamps for verified target activation and
  successful paste. A paste followed by clipboard-restore failure is recorded
  as a completed paste, shows non-retry guidance, and never sends a second
  paste command.
- Platforms without a verified focus/clipboard-generation adapter remain
  explicit copy-only fallbacks; Phase 8 does not claim macOS or Linux paste
  safety.

### Verification

| Check | Command | Result |
| --- | --- | --- |
| Format/lint/build | `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo build --all-features` | **PASS** |
| Unit/integration suite | `cargo test --all-targets --all-features` | **PASS** - 436 discovered, 430 passed, 0 failed, 6 environment-gated tests ignored |
| Finalization safety | App, coordinator, capture, and rolling-preview tests | **PASS** - post-roll preserved; no-speech/empty final produces no output; slow preview cancel is bounded and terminal; late/duplicate output remains exactly once |
| Target safety | 14 Windows platform probe tests | **PASS** - Scribe/dead/recycled/changing targets and property installation/loss rejected; stable external identity captured; activation denial/focus change fail closed; exact target generation revalidated |
| Clipboard/output safety | 26 injected-driver, format-validation, and Windows input-batch tests | **PASS** - zero sequences rejected; native payloads bounded; source/conversion order classified; supported-format selection validated; activation/snapshot/restore races rejected; missing target, failed paste, exactly-one paste, and individually checked key-release cleanup covered. Actual native mixed-format restoration remains NOT VERIFIED. |
| Output UI accessibility | AccessKit semantic tests | **PASS** - Transcript editor labelled and exposes queued-output disabled/description state; Advanced numeric and combo controls labelled; page and History card titles are semantic headings; copy actions have distinct names |
| Architecture boundary | `scripts/check-catalog-boundaries.py` and Rust source guards | **PASS** - one handler; runtime and native PCM boundaries unchanged |
| Pinned native fixture | Exact ignored base.en/JFK service smoke using the v1.9.1 CPU package | **PASS** - debug-harness first load 4,367 ms, first decode 801 ms, warm decode 792 ms; not comparable to the saved release benchmark |

Independent code, security, accessibility, QA, and final-integration reviews
all returned **GO** after their findings were fixed. These review results do
not replace physical desktop verification.

Phase 8 changes no inference path, model artifact, resolved backend, or preview
scheduler, so the saved Phase 7 native final/preview measurements remain the
current comparable latency evidence. Actual target-activation and paste timing
are now instrumented but cannot be measured truthfully without the real Windows
desktop matrix.

### Risks and Phase 9 entry

- **High - desktop evidence:** SetForegroundWindow policy, standard/elevated
  target behavior, image clipboard restoration, and the unavoidable final
  focus-check-to-SendInput race require the Windows OUT matrix. The code uses
  an HWND generation property, adjacent validation, and no retry; it does not claim
  atomic focus ownership.
- **Medium - clipboard formats:** HTML, file lists, and arbitrary private
  clipboard formats are intentionally not reconstructed. They fail closed to
  explicit copy-only output; text, image, and empty states have automated
  coverage.
- **Medium - hung runtime:** after four seconds the session is a terminal
  failure and no output can occur, but new runtime work remains blocked until
  the retained native worker actually exits.
- **Low - cleanup:** no local cleanup option exists, so raw and final text are
  identical. The split and recoverable raw view prevent a future cleanup path
  from silently discarding source text.

The release remains **NO-GO** until the Windows manual matrix, native-streaming
Definition of Done, Supported-model evidence, and complete comparable latency
report pass. Phase 9 may now replace installation with manifest-driven
transactions without changing the safe output boundary.

## Phase 9 - Verified model and runtime installation

Phase 9 replaces direct artifact mutation with one manifest-driven transaction
system shared by model and runtime installation. It does not add an audio,
model-management, or runtime-selection subsystem. The logical handler count
remains **one** (`TranscribeCppRuntime`); `OnnxSpeechRuntime` is still omitted,
all four normalized Whisper artifacts remain **Experimental**, and the number
of Supported models remains **0**.

### Installation and recovery decisions

- The primary Windows x64 runtime is pinned to whisper.cpp v1.9.1 commit
  `f049fff95a089aa9969deb009cdd4892b3e74916`. Its release archive is exactly
  7,982,101 bytes with SHA-256
  `7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539`.
  Activation accepts only the 13 manifest files: `whisper.dll`,
  `whisper-cli.exe`, `ggml.dll`, `ggml-base.dll`, and the nine pinned CPU
  backend DLLs. Missing, extra, wrong-sized, wrong-hash, linked, or reparse
  entries fail closed. The broad upstream Release directory is not itself a
  valid installed package because it contains unallowlisted executables and
  libraries.
- Downloads use bounded blocking workers, HTTP identity encoding, validated
  Range/Content-Range resume, durable partial files, cancellation that retains
  valid partials, and exact final size/SHA-256 checks. Invalid or oversized
  partials are quarantined before a clean retry. ZIP extraction uses the
  manifest as an exact allowlist, extracts only allowlisted entries, and
  rejects traversal, links/reparse points, duplicates, and missing entries.
  The pinned outer archive hash rejects modified archives, while exact-tree
  validation rejects extras in a staged or installed tree.
- Model and runtime activation use same-volume staging, durable renames, and a
  journal containing the prior and expected settings fingerprints. Startup
  promotes or rolls back only when the durable fingerprint proves which side
  committed; an ambiguous mismatch preserves both artifacts and gates further
  mutation for operator recovery. Removal uses the same fingerprinted journal
  protocol instead of deleting files before settings persistence.
- Exactly one previous known-good primary runtime is retained at `.previous`.
  Startup tries current, then previous, then an explicitly located immutable
  bundled package. A bundled candidate must pass the same exact-tree and smoke
  checks; development PATH/CLI discovery is never treated as a packaged
  fallback. Orphaned `.previous` state is reconciled after durable settings and
  removal journals are resolved.
- Runtime smoke tests execute in a child process controlled by a parent with a
  120-second deadline and 25 ms cancellation polling. On Windows the child
  suppresses operating-system fault dialogs so a malformed native package
  cannot strand the installer. Install, update, removal, and runtime switching
  are disabled while a session owns the artifact. Legacy unmanaged paths are
  preserved and removal changes settings only; user artifacts are not silently
  deleted.
- Normalized managed runtime installation currently fails closed outside
  Windows x64 because no pinned package manifest and native smoke evidence has
  been established for those platforms. This preserves buildability without
  claiming cross-platform installation support.

### Verification and measured evidence

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; `cargo clippy --all-targets --all-features -- -D warnings` | **PASS** |
| Unit/integration suite | `cargo test --all-targets --all-features` | **PASS** - 474 discovered, 468 passed, 0 failed, 6 environment-gated tests ignored |
| Debug/release builds | `cargo build --all-features`; `cargo build --release --all-features` | **PASS** |
| Architecture boundary | Exact `runtime_router::tests::concrete_runtime_boundary_is_confined_to_the_router` plus the full source-boundary suite | **PASS** - exactly one logical handler; concrete runtime selection remains router-private |
| Download/install failure injection | In-process HTTP, archive, activation, removal, and recovery tests | **PASS** - resume, ignored/invalid Range, cancellation, size/hash failure, traversal/extra/missing entries, smoke failure, activation/rollback, unchanged-model commit/rollback, config-fingerprint mismatch, and interrupted removal covered |
| Exact pinned runtime smoke | `local-transcriber.exe --scribe-install-smoke-parent ... cpu` against a temporary exact 13-file package | **PASS** - health 4,098 ms; load 4,341 ms; decode 732 ms; unload/reload 4,051 ms; exit 0 |
| Pinned model/service fixture | Exact ignored `transcription_service_jfk_smoke_uses_the_whisper_cpp_facade` with v1.9.1/base.en/JFK/CPU | **PASS** - first load 4,501 ms; first decode 840 ms; warm load 0 ms; warm decode 806 ms |

An earlier direct blocking helper invocation used the broad upstream Release
directory and ended with a Windows access violation dialog. That run bypassed
the bounded parent and is recorded as **failed evidence**. No process remained
after termination. The final smoke used the exact manifest tree through the
bounded parent, completed without a dialog, and passed. This verifies the
package/install boundary; it does not promote a model to Supported or replace
the complete compatibility suite.

Phase 9 changes installation and recovery rather than normal inference. The
saved Phase 7 comparable release measurements therefore remain the current
before/after latency evidence; the installer smoke timings above are neither
desktop end-to-end latency nor an improvement claim. PCM remains entirely in
native Rust workers, tentative text remains overlay-only, and history/audio
privacy behavior is unchanged pending Phase 10.

### Risks and Phase 10 entry

- **High - live installation evidence:** a real GitHub interruption/resume,
  power-loss recovery, live Models UI transaction, and physical Windows
  activation/rollback have not been executed. The deterministic protocol and
  failure-injection suite pass, but the manual DL rows remain NOT VERIFIED.
- **Medium - native package behavior:** the isolated exact package passed on
  this Windows machine, while the broad direct invocation crashed. Only the
  exact pinned tree is eligible; future package revisions require new hashes,
  smoke evidence, and review.
- **Medium - cross-platform availability:** normalized managed runtime install
  is intentionally unavailable without a pinned platform package. Existing
  compatibility paths remain conservative; no macOS/Linux installation claim
  is made.
- **Low - recovery operator path:** fingerprint ambiguity fails closed and
  preserves state, but the UI currently reports recovery-required rather than
  offering an automated destructive resolution.

The release remains **NO-GO** until the Windows manual matrix,
native-streaming Definition of Done, Supported-model evidence, and complete
comparable desktop latency report pass. Phase 10 may now add durable history
on top of the transactional artifact and settings boundary.

## Phase 10 - History, retention, and retry

Phase 10 adds one runtime-neutral history subsystem beneath the existing app
coordinator. It does not add a transcription, audio, output, or model-management
path. The logical runtime-handler count remains **one**
(`TranscribeCppRuntime`), `OnnxSpeechRuntime` remains omitted, all four normalized
Whisper artifacts remain **Experimental**, and **0** models are Supported.

### Persistence, privacy, and lifecycle decisions

- Bundled SQLite stores metadata in the platform Scribe data directory under
  `history/history.sqlite3`; retained audio is a separate relative file beneath
  `history/audio`. History-root resolution now fails closed instead of falling
  back to the process working directory. Unix permissions are restricted to
  `0700`/`0600`; Windows applies a protected owner/LocalSystem full-control DACL.
  The database, WAL, SHM, lock, root, audio directory, and every audio path are
  checked for links/reparse points before use.
- One bounded native worker owns SQLite, WAL mode, full synchronous durability,
  retention, audio staging, and reconciliation. A worker-owned cross-process
  lock prevents a second Scribe process from converting the first process's
  Pending rows to Failed or reconciling its audio. Startup converts only
  abandoned Pending rows, completes deletion journals, clears missing audio,
  and removes contained orphan/staging files.
- Normal dictation reserves a monotonic correlated history ID and enqueues the
  Pending row before decode. Queue admission is bounded to 100 ms and does not
  wait for SQLite or WAV fsync, so optional history is not on the transcription
  critical path. The single worker preserves create-before-complete/fail
  ordering. A terminal completion is enqueued, then safe output proceeds
  immediately from the immutable accepted final transcript; persistence is
  observed asynchronously and a slow/unavailable store produces a visible
  history warning without delaying paste.
- Rows use `Pending`, `Completed`, and `Failed` lifecycle states with raw/final
  text, runtime-neutral model ID, neutral metrics, pin state, optional coarse
  executable basename, retry count, and neutral output result. Raw runtime and
  filesystem errors are not persisted. Detailed failures remain transient;
  durable history receives bounded user-safe failure text.
- Retry requires a Failed row with retained canonical audio. Audio validation
  and decoding use the same already-open handle and leave the row Failed while
  the explicit retry runs. Only terminal `complete_retry` or `fail_retry`
  atomically updates the same row and increments its count, so a slow read or
  caller timeout cannot strand a row in Pending. Worker and UI retry leases are
  retained until terminal acknowledgement, consumed on every terminal error,
  and have an explicit idempotent release command with bounded retry for
  command-admission/cancellation ambiguity. Once a release is admitted, its
  background observer retains the receiver until acknowledgement or confirmed
  worker disconnection; lease-removal acknowledgement is independent of any
  later retention error. Active retry rows are protected from retention and
  mutation. Retry never arms automatic output.
- Settings default to `TranscriptOnly`, a maximum of 20 unpinned entries, no
  age limit, audio Off, and application identity Off. `Off`, `TranscriptOnly`,
  and `TranscriptAndAudio` are real behaviors. Optional transcript/audio age
  retention and the count cap never remove pinned rows. Settings migration,
  field salvage, atomic persistence, and unknown-field preservation remain the
  existing typed-settings system rather than a duplicate store.
- Retained audio is bounded to ten minutes of mono 16 kHz PCM16, staged and
  durably renamed, decoded without a validate/reopen race, and kept out of egui
  events. Native CPAL playback uses one bounded command worker; output callbacks
  allocate nothing, playback uses the native callback timestamp and retains the
  stream through the predicted queue delay, a full final callback buffer, and a
  50 ms safety margin before declaring completion. Shutdown is bounded and
  terminal state events cannot be dropped.
- The History page provides real keyset pagination, literal-wildcard search,
  final and distinct raw copy/view actions, pin/unpin, playback/stop, delete
  audio, confirmed full deletion, and retry eligibility. One correlated
  app-facing mutation is allowed at a time and retention edits coalesce to the
  newest policy, preventing unbounded helper threads and reordered destructive
  actions.
- Paste Again is an explicit 30-second one-shot arm. It captures and validates
  a fresh external target only when the hotkey is pressed, never reuses the
  historical target, and uses the Phase 8 safe output boundary exactly once.
  Starting/superseding work, retrying/deleting the row, disabling history,
  expiry, or any active session clears the arm. Tentative text and retry results
  are never pasted.
- The accepted final transcript and output configuration are snapshotted before
  history completion and immediately enter the existing safe-output state.
  Later edits cannot change that queued immutable snapshot; canceling queued
  output records `cancelled_by_user`. Correlation rejection consumes/fails the
  owning history context, while genuinely stale events remain non-mutating.

### Verification and measured evidence

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo build --all-features` | **PASS** |
| Unit/integration suite | `cargo test --all-targets --all-features` | **PASS** - 523 discovered, 517 passed, 0 failed, 6 environment-gated tests ignored |
| History contract | Focused `history::tests` plus app lifecycle tests | **PASS** - schema/lifecycle, single-owner lock, queued create ordering, search/keyset pagination, retention/pins, staged audio, retry terminality, bounded retry of ambiguous release, retention-independent lease acknowledgement, output metadata, deletion journal, missing/orphan reconciliation, unsafe paths, immutable final output, arm invalidation, and no retry paste covered |
| Native playback | `history_playback::tests` | **PASS** - bounded same-format/resampled multichannel fill, timestamp-derived multi-buffer drain deadline, correlated Stop, bounded shutdown, and invalid-ID rejection |
| Settings/privacy/accessibility | Settings migration/repository and History UI tests | **PASS** - legacy/future-field preservation, salvage/bounds, transcript-only defaults, structural contextual groups containing their headings/actions, live atomic busy/results/error state, destructive focus management, labelled search, expanded disclosure state, non-color state text, state-specific disabled explanations, confirmation, and 44 px actions |
| Architecture boundary | Rust boundary suite and `scripts/check-catalog-boundaries.py` | **PASS** - one logical runtime handler; no PCM or concrete runtime/model-family types cross into UI/history/output |

Phase 10 changes persistence after capture and around the final result, not the
native inference implementation, fixture, model artifact, or resolved backend.
No desktop latency improvement is claimed. The comparable Phase 7 release
measurements remain the current inference evidence: cold final total median/p95
1,087/1,099 ms, warm final total 781/800 ms, and warm rolling first real speech
1,730/1,754 ms. History queue admission is bounded but real hotkey-to-paste,
disk-stall, memory, and idle-CPU effects still require saved Phase 11 desktop
measurements on the same machine/corpus/backend.

### Risks and Phase 11 entry

- **High - desktop/privacy evidence:** real Windows history ACLs, search,
  retention, playback, retry, fresh-target repaste, restart reconciliation, and
  clipboard/focus races have not been exercised manually. The matrix remains
  NOT VERIFIED.
- **Medium - durability fault injection:** deterministic interruption,
  deletion-journal, missing/orphan, and lock tests pass, but physical power-loss
  and a genuinely stalled local disk have not been reproduced.
- **Medium - cross-platform behavior:** SQLite/history compiles on supported
  platforms and fails closed where permission hardening is unavailable; native
  playback device behavior and platform data-directory permissions still need
  the manual matrix.
- **Low - retained private data:** switching History Off prevents new rows but
  does not silently delete existing user data. Existing rows remain subject to
  the user's configured retention or explicit deletion.

The release remains **NO-GO** until Phase 11 diagnostics/hardening, the complete
Windows manual matrix, Supported-model compatibility evidence, native-streaming
Definition of Done, and comparable before/after desktop latency report pass.

## Phase 11 - Diagnostics, compatibility, hardening, and retirement

### 1. Summary

Phase 11 implements runtime-neutral session diagnostics and redacted export, a
local benchmark command/view, expanded architecture boundaries, normalized
one-handler release packaging, legacy dead-path retirement, accessibility
hardening, and bounded process-safe native shutdown. The implementation phases
are complete. The release remains **NO-GO** because the dated Windows desktop
matrix, physical crash soak, complete Supported-model evidence, native-streaming
Definition of Done, and desktop latency/memory/idle-CPU evidence are not complete.

### 2. Files changed

- **Created `src/diagnostics.rs`:** allowlisted, runtime-neutral session
  diagnostics; 50-record in-memory bound; replace-by-session behavior; redacted
  create-new export with synchronized writes and Unix owner-only permissions.
- **Created `src/architecture_guard.rs`:** cargo-test source guards for exact
  handler count, router-private runtime selection, neutral application/UI
  boundaries, family-logic confinement, native-only PCM, and final-only output.
- **Modified `src/app.rs`:** session metric capture and failure attribution;
  diagnostics export; semantic/accessibility improvements to diagnostics and
  benchmark views; bounded process-exit coordination across preview,
  compatibility cancellation, and native runtime shutdown. No-speech feedback
  now distinguishes a capture whose maximum input RMS never reached the
  existing speech-activation floor and gives hardware mute/gain guidance.
- **Modified `src/audio.rs` and `src/audio/pipeline.rs`:** capture-wide maximum
  10 ms VAD-frame RMS and 30 ms meter-window peak scalars are retained with
  completion metrics, including a final partial meter window. No PCM is
  persisted for this diagnostic and the VAD threshold is unchanged.
- **Modified `src/benchmark.rs` and `src/main.rs`:** privacy-bounded
  `--benchmark` CLI using `TranscriptionService`, allowlisted JSON reporting,
  create-new output, sanitized errors, and pre-UI command dispatch.
- **Modified `src/transcription.rs`:** runtime-neutral resolved-backend accessor;
  explicit bounded native-worker shutdown; cooperative cancellation;
  deadline-bounded command/ack/join; panic/disconnect and concurrent-drop tests.
- **Modified `src/streaming.rs`:** bounded rolling-preview shutdown with no
  same-process detach; process-safe hard-abort fallback after the deadline; new
  active-decoder ownership regression.
- **Modified `src/managed_downloads.rs` and `src/models.rs`:** removed
  unreachable runner-specific model download implementations, output parsers,
  family URL branches, tests, and dead allowances. Phase 9 pinned transactional
  preparation remains authoritative.
- **Modified `src/runtime_router.rs`, `src/core.rs`, and
  `src/text_output.rs`:** removed stale dead code and a duplicate output entry
  point while preserving the one router and exactly-once final-output boundary.
- **Modified `scripts/check-catalog-boundaries.py`:** expanded the independent
  source scanner to enforce one handler, private manifest routing, neutral UI,
  native PCM, final-only output, and confined legacy selection.
- **Modified `scripts/build-release-bundle.sh`:** release output now invokes only
  the primary whisper.cpp bundler. Legacy development scripts are retained but
  are not release inputs.
- **Modified `README.md` and `TODO.md`:** corrected stale runtime/package/audio
  claims and removed completed Phase 10/11 work.
- **Created `docs/SCRIBE_REVAMP_IMPLEMENTATION_REPORT.md`; modified this record
  and `docs/MANUAL_TEST_MATRIX.md`:** final architecture, compatibility,
  privacy, latency, crash, verification, and remaining-gate evidence.

The untracked consolidated specification files and `.stitch` tree remain
unchanged.

### 3. Architecture and design decisions

- The final logical handler count is **one**: `TranscribeCppRuntime`.
  `OnnxSpeechRuntime` remains absent because the exact sherpa-onnx v1.13.4
  Zipformer evidence gate has no qualifying native package/corpus results.
- `RuntimeRouter` remains the only application-level runtime selector.
  Benchmark and diagnostics obtain resolved backend information from a neutral
  outcome accessor and never import concrete handlers.
- Diagnostics are ephemeral by default and structurally privacy-bounded instead
  of redacting arbitrary debug objects after collection. Missing measurements
  serialize as null rather than being invented.
- Low-input diagnosis uses the same downmixed, resampled native signal as the
  meter and VAD. A no-speech capture is classified as silent/too-low only when
  its maximum window RMS never reaches the unchanged 0.012 minimum activation
  floor; louder short bursts and non-voice input retain the generic no-speech
  result. Only maximum RMS/peak scalars enter the allowlisted diagnostics.
- The benchmark reuses the application service boundary. It reports only
  allowlisted metadata/timings and does not become a second transcription path.
- Legacy adapters, aliases, scripts, and user artifacts are retained privately
  because removing user-owned data or migration compatibility would be unsafe.
  Unreachable family-specific downloader and release-selection paths were
  removed now because normalized manifests replaced their roles.
- Native shutdown has one two-second process-exit budget. Cooperative cancel is
  attempted first; command admission, acknowledgement, and thread completion
  are deadline bounded. A completed or panicked worker is joined. If native code
  remains live at the deadline, Scribe hard-aborts without running Rust/DLL
  teardown. It never detaches a native worker in the same process and never
  waits indefinitely. A helper-process runtime was rejected for this phase
  because it would replace the verified retained native C-API vertical slice;
  the explicit hard-abort policy is smaller and safe for exit-only failure.
- The benchmark heatmap uses theme-specific dark fills and explicit high-
  contrast text. Missing diagnostics capability has a visible explanation,
  not a hover-only affordance.

### 4. Risks and assumptions

- **High - Windows desktop evidence:** shortcut, microphone, overlay focus,
  clipboard, target identity, history, install recovery, device restart,
  multi-monitor/DPI, and standard/elevated targets remain manually unverified.
  Mitigation: execute every dated Windows matrix row and attach evidence.
- **High - native shutdown soak:** two release-test access violations were
  observed before shutdown hardening. Deterministic ownership/deadline tests and
  the exact 25-run rolling fixture pass afterward, but a prolonged physical app
  close/restart soak is absent. Mitigation: run a Windows WER-monitored soak;
  retain dumps/events for any recurrence.
- **High - compatibility claims:** no model has the complete required matrix.
  Mitigation: keep all four artifacts Experimental and promote only after the
  full gate passes.
- **Medium - hard-abort fallback:** a genuinely wedged native runtime terminates
  Scribe without normal settings/history flush after the shared two-second exit
  budget. This is intentionally preferable to an unload access violation or
  infinite hang; in-flight text is never pasted. Mitigation: keep writes
  transactional, diagnose the native hang from the prior allowlisted metrics,
  and consider a supervised runtime process only if real hangs recur.
- **Medium - preview latency:** warm rolling p95 is 1,849 ms, above the 1,200 ms
  product target and native-streaming definition. Mitigation: retain the batch
  preview exception as Experimental and continue representative-corpus tuning;
  do not describe it as native streaming.
- **Medium - cross-platform evidence:** conservative code paths compile where
  available but macOS/Linux desktop behavior is not exercised here. Mitigation:
  run their build/manual fallback matrices before a platform support claim.
- **Low - export ACL variance:** Unix owner-only mode is explicit; Windows uses
  the selected directory's ACL. The UI default is Scribe's private config
  directory. Mitigation: document exports as user-controlled files and verify
  Windows ACL inheritance in the manual matrix.

### 5. Testing and measured evidence

| Check | Command/evidence | Result |
| --- | --- | --- |
| Format/check/lint | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; `cargo clippy --all-targets --all-features -- -D warnings` | **PASS** |
| Full automated suite | `cargo test --all-targets --all-features` | **PASS** - 623 discovered, 614 passed, 0 failed, 9 environment-gated tests ignored |
| Builds | `cargo build --all-features`; `cargo build --release --all-features` | **PASS** |
| Architecture boundaries | Rust `architecture_guard` suite and WSL `scripts/check-catalog-boundaries.py` | **PASS** - exactly one handler, router-private selection, neutral UI, native PCM, final-only output |
| Diagnostics | Focused `diagnostics::tests` | **PASS** - allowlist, null metrics, bounded replace, private-marker absence, export I/O preservation |
| Low-input diagnosis | Focused audio-pipeline and app no-speech tests | **PASS** - full/partial-window maximum levels; low-input/FIFINE guidance; threshold-level non-speech remains generic |
| Live FIFINE probe | Two no-save CPAL probes on Windows against the configured 48 kHz F32 stereo input | **DIAGNOSED** - callbacks and both channels were healthy with no phase cancellation, but the strongest observed 10 ms mono RMS was 0.001559 (0/1,498 windows at 0.012); Windows privacy was Allow and endpoint gain was 100%/+7 dB, isolating hardware mute/physical gain or acoustic input rather than runtime/model failure |
| Benchmark | Focused `benchmark::tests` and release CLI report | **PASS** - 15 tests; one fixture report recorded 1/403/838/1,243 ms prepare/load/backend/total and no private payload |
| Native shutdown | Focused streaming/transcription tests | **PASS** - active preview is never detached; stuck command returns at deadline and recovers; panic/disconnect joins; 20x concurrent last-clone stress completes |
| Accessibility | Exhaustive score/theme contrast plus focused diagnostics/target tests | **PASS / reviewer GO** - minimum 13.18:1 light and 8.77:1 dark; semantic headings/descriptions and 44 px primary action |
| Security review | Specialist privacy/security audit and post-fix review | **PASS** - no Critical, High, or Medium finding; sanitized benchmark errors and private export mode addressed Low findings |
| Runtime package | Pinned PowerShell bundler to disposable output | **PASS** - Windows x64 CPU v1.9.1; 13 manifest files plus manifest |
| Service fixture | Release JFK service smoke | **PASS** - first load/decode 299/857 ms; warm load/decode 0/826 ms |
| Cancellation | Release primary cancellation fixture | **PASS** - 840 ms acknowledgement; the absent ONNX-handler 250 ms gate does not apply |
| Rolling release fixture | Exact 5 cold + 20 warm run after shutdown fix | **PASS** - cold 2,042/2,077 ms; warm 1,783/1,849 ms median/p95 |
| Diff hygiene | `git diff --check` and explicit status review | **PASS** |

One rolling release attempt before the shutdown fix timed out, and Windows
showed an access-violation dialog for the release test executable. It is retained
as failure evidence rather than hidden by the successful post-fix rerun.

Comparable 11-second base.en CPU latency:

| Measurement | Phase 0 before median/p95 | Phase 11 after median/p95 | Change |
| --- | ---: | ---: | ---: |
| Cold process/load/transcribe | 1,282.8/1,333.1 ms | 1,182/1,197 ms | -7.9%/-10.2% |
| Repeated process vs retained warm | 1,279.5/1,452.8 ms | 846/926 ms | -33.9%/-36.3% |
| Cold load | not separated | 308/330 ms | no comparison claim |
| Warm load | process included load | 0/0 ms | retained-model observation |
| Cold RTF | not separated | 0.107/0.109 | no comparison claim |
| Warm RTF | not separated | 0.077/0.084 | no comparison claim |

The warm delta includes process elimination and retained model state. It is not
an end-to-end desktop, accuracy, memory, or Supported-model claim.

### 6. What could not be verified

- A real Windows FIFINE input was probed without saving audio and confirmed the
  silent/too-low failure signature. A successful spoken GUI/hotkey/overlay/
  target/paste/history run after correcting hardware mute/gain remains absent.
- Hotkey-to-overlay, hotkey-to-capture, first meter, first partial,
  stop-to-final, final-to-paste, total duration, memory, and idle CPU lack saved
  desktop median/p95 results. Instrumentation is present; values are not.
- No physical shutdown/restart soak or Windows crash dump analysis followed the
  access-violation fix.
- The four Experimental models lack the complete platform, acceleration,
  cancel, unload/reload, crash recovery, accuracy, and memory suite.
- The named Zipformer native streaming candidate was not available as a fully
  pinned, qualified package and therefore did not enter the application.
- macOS/Linux UI, microphone, hotkey, and clipboard-only behavior was not run.
- CodeRabbit reviewed earlier stacked snapshots and its findings were
  addressed. Its CLI was unavailable for a dedicated final-snapshot rerun;
  compiler/tests plus local security, accessibility, performance, and
  integration reviews cover the final tree.

### 7. Next steps

1. Execute the dated Windows 11 manual matrix and shutdown/restart soak.
2. Save same-machine desktop latency, RTF, memory, and idle-CPU observations.
3. Complete the full compatibility suite before promoting any model.
4. Tune rolling preview against a representative corpus while retaining its
   bounded work and tentative-overlay-only guarantees.
5. Exercise conservative macOS/Linux builds and copy-only output.
6. Publish the pinned Windows archive and exercise real network interruption,
   resume, recovery, and rollback.

These are evidence/release tasks and should be independently reviewable rather
than folded into an unrelated implementation branch.

### 8. Self review

- The added diagnostics and benchmark types are intentionally allowlisted; no
  generic error/debug serialization path remains.
- Shutdown handling is more complex than a simple join because it must avoid
  both the observed DLL-unload race and an infinite UI hang. Deadline ownership
  and hard-abort behavior are documented and fault-injected.
- `app.rs` remains large. Phase 5 split composition/pages/platform adapters, but
  further page extraction is a maintainability improvement, not a reason to
  duplicate state systems.
- Private legacy adapters remain technical debt, but are excluded from runtime
  routing, the normalized UI, and release packaging. Removing them before
  verified replacement roles would violate artifact/config preservation.
- No placeholder control, fake progress, second runtime, tentative output path,
  or duplicate settings/history/download/transcription system was introduced.
- The final report deliberately does not turn compilation or smoke timing into
  compatibility, native-streaming, desktop-latency, or release claims.

### 9. Confidence assessment

- **Overall: Medium.** The implementation and automated boundaries are strong;
  release evidence is intentionally incomplete.
- **Correctness: Medium-High.** The automated suite and targeted fault injection
  cover the implemented contracts; physical desktop/native shutdown behavior
  still needs soak evidence.
- **Maintainability: Medium-High.** Runtime selection and contracts are sharply
  bounded, though the remaining `app.rs` size and private legacy adapters carry
  debt.
- **Test coverage: High for deterministic/native-neutral contracts; Low for
  physical desktop coverage.** Confidence rises after the dated manual matrix.
- **Security/privacy: High for the inspected implementation.** PCM, tentative
  text, exports, output target validation, and persistence defaults are bounded;
  Windows export ACL/manual clipboard races still need observation.
- **Production readiness: Low.** Release remains NO-GO until the explicit manual,
  compatibility, streaming, soak, and measurement gates pass.

## Post-Phase 11 tray wakeup regression checkpoint

Recorded 2026-08-05 on Windows x64 after a report that tray commands stopped
responding once the primary viewport was hidden.

- Corrected root cause: native tray events were delivered and queued, but
  eframe 0.27.2 converts `Context::request_repaint` into winit
  `window.request_redraw()`. Pinned winit 0.29.15 implements that with
  `RedrawWindow(..., RDW_INTERNALPAINT)` and explicitly documents that an
  invisible Windows window does not receive the `WM_PAINT` used for
  `RedrawRequested`. The first callback-only fix therefore woke the event loop
  but still could not run `App::update` for the hidden root viewport.
- Live failure evidence: against the exact isolated release process, selecting
  Show queued the command but left the root HWND hidden. Posting one asynchronous
  `WM_PAINT` to that HWND immediately drained the queued command and restored
  the window. This ruled out stale binaries, menu IDs, handler registration, and
  command mapping.
- Corrected fix: the root HWND is captured from `CreationContext`. Every tray
  callback still publishes to the bounded runtime-neutral channel and now posts
  an asynchronous native paint wake. While closed to tray, a one-shot native
  timer rearms from the existing 40/100/500 ms repaint policy so capture,
  transcription, history, and menu-state completion continue after the initial
  action. Show, Quit, and service teardown cancel the timer. The handler is
  registered before icon construction to remove its one-shot initialization
  race. No audio, transcription, runtime, output, history, or settings system
  was duplicated.
- Automated evidence: four focused tray tests pass, including wake delivery,
  newest-intent bounded-queue behavior, Win32 timer bounds, and menu-command
  mapping. Formatting, all-target/all-feature checking, strict Clippy, the full
  suite (533 passed, 6 environment-gated ignored), and debug plus isolated
  release builds pass. The corrected release is at
  `C:\tmp\scribe-tray-wake-v2-target\release\local-transcriber.exe`.
- Corrected live evidence: actual Windows tray-popup clicks against the v2
  release changed the root HWND from hidden to visible for Show and visible to
  hidden for Hide while the process remained responsive. A Start click did not
  retain a Stop label after 2.2 seconds and may have failed immediately because
  of application/model state; Start/Stop, Copy, and Quit remain unverified.
- Remaining evidence: manual matrix row UI-04 remains **NOT VERIFIED** until a
  human exercises Show, Hide, Start/Stop Recording, Copy Last Transcript, and
  Quit from the Windows notification area with the main window hidden.

Risk is **Low** for the bounded command bridge and **Medium** for the pinned
eframe Win32 scheduling workaround until UI-04 and hidden idle-CPU sampling are
completed. The `tray-icon` handlers remain process-lifetime callbacks, while the
active publisher and native timer are explicitly replaced/cancelled with the
single tray-service lifecycle.

## Post-Phase 11 low-input diagnostic checkpoint

Recorded 2026-08-05 on Windows x64 after the configured FIFINE A8 repeatedly
ended as no-speech. Two no-save CPAL probes verified F32, 48 kHz, stereo input,
matching channels, and complete callback delivery. The independent probe's
maximum 10 ms mono RMS was 0.001559 and none of 1,498 windows reached the
unchanged 0.012 VAD activation floor. Windows microphone privacy was Allow and
the endpoint was unmuted at 100% with +7 dB gain. The evidence isolates a
silent/too-low acoustic or hardware mute/gain path rather than runtime, device
selection, sample conversion, or channel cancellation.

The native pipeline now retains only capture-wide maximum 10 ms VAD-frame RMS
and 30 ms meter-window peak scalars. Those values flow through capture metrics into the
allowlisted redacted diagnostics; no PCM is persisted or sent through UI/IPC.
A shared application classifier gives actionable mute/gain guidance below the
minimum activation floor and preserves generic no-speech feedback for louder
short or non-voice input. The VAD, runtime selection, and transcription path are
unchanged.

Formatting, strict all-target/all-feature Clippy, the full suite (557
discovered; 548 passed, 9 environment-gated ignored), debug/release builds, and
diff hygiene passed. The rebuilt packaged v1.9.1/base.en/CPU fixture reported
1/313/839/1,154 ms prepare/load/backend/total. The updated desktop process was
launched successfully. A spoken GUI/hotkey/paste run after physically
correcting the A8 top mute or bottom gain remains **NOT VERIFIED** and no manual
matrix row is promoted by this checkpoint.

## Input sensitivity slider

General > Audio exposes one model-independent `Input sensitivity` slider. Its
track combines the latest microphone RMS with the persisted activation
threshold; while dragging or keyboard-adjusting, a compact bubble shows the
current whole-dB threshold. There is no test button, Automatic/Manual selector,
idle numeric meter, second meter, waveform, calibration action, or
speech/clipping label.
The threshold thumb remains usable when input is unavailable.

When General is visible and dictation is idle, the existing native
CPAL/ring/pipeline service owns a `MeterOnly` session. That intent retains no
prepared audio and creates no preview, transcript, output, history, overlay, or
audio file. During active dictation, the slider reads the existing recording
session's atomic level telemetry instead of opening another stream. Monitor
teardown is acknowledged before deferred dictation or retained-audio playback
starts, so the owners never overlap. Deferred recording and playback are
mutually exclusive; recording takes priority and playback is rejected while a
capture is queued or active. Leaving General stops only the idle monitor,
clears its envelope/repaint state, and never stops active dictation.

The capture worker continues to publish normalized mono RMS every 30 ms through
latest-value atomics. Each publication increments a revision counter. The UI
uses a 30 ms attack, 240 ms release, and a 160 ms stale-sample deadline; stale
or absent input decays to the track minimum instead of freezing. Repaint remains
at the approximately 33 Hz meter cadence only while capture, monitoring, or
release animation is active.

The internal slider range is -72 to 0 dBFS, with a default threshold near
-42 dBFS. Values remain internal: the accessible slider exposes only a
normalized range and adjustment actions. The base track is split at the thumb
into distinct threshold and remainder fills. The live fill keeps one thickness;
crossing remains spatially visible because the fill extends beyond the thumb,
and its high-contrast color changes from the below-threshold tone to success. Keyboard focus
uses a neutral heavier thumb outline and center dot rather than an accent halo.
The custom control has a 44 px interaction target, click/drag, focus, Left/Right
arrows, and AccessKit increment/decrement/set-value actions.

The persisted `manual_activation_rms` field is the only runtime sensitivity
setting. An obsolete `sensitivity_mode` property is preserved as unknown
compatibility data when encountered, but cannot select a second behavior.
Pointer and keyboard changes update the in-memory value immediately,
use the existing 300 ms debounced settings store, and write a shared atomic read
by the VAD on the next 10 ms frame. Existing confirmation, 3 dB-class release
hysteresis, pause/hangover, endpointing, pre-roll, and post-roll behavior is
unchanged. The advanced endpointing option controls only automatic stop in
Toggle mode; it cannot bypass sensitivity gating. Per-device thresholds remain
deferred because the current device selection exposes only a display name
rather than a stable identifier.

The earlier `qa/microphone-test-final-*.png` captures document the superseded
button-and-diagnostics UI and are not evidence of the current slider design.
Physical microphone, device-disconnect, and permission-failure verification of
the redesigned lifecycle remains **NOT VERIFIED**.
