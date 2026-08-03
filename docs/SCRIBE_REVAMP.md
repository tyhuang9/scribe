# Scribe revamp implementation record

**Status:** Phase 2 implemented on its stacked branch (2026-08-03). This document
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
| `final_text_to_paste_ms` | successful final-text-ready timestamp to paste automation completion | `final_text_ready_at` to `paste_completed_at`; this means Enigo/clipboard automation returned after its configured delay and restoration, not that the target application consumed the text. Clipboard-only/failure is not a successful paste. Output-start→output-complete remains a separate component metric. |
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
| Phase 0 sessions were not correlated; facade-level issue resolved in Phase 1 | **Low** remaining | Phase 1 adds session/request IDs, cross-source supersession, model correlation, and stale-event rejection before output. The authoritative coordinator/cancellation state machine is still pending. | Preserve these guards and consolidate them into the Phase 4 coordinator before streaming work. |
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
