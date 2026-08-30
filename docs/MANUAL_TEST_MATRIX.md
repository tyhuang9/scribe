# Scribe manual test matrix

**Status:** living qualification checklist. The dated Phase 0-11 automated
snapshots in this document are historical evidence, not a current rebaseline.
No manual desktop, microphone,
model-runtime, tray, hotkey, overlay, accessibility, or paste test was executed
during the Phase 0-11 automated work. Every manual row below therefore remains **NOT VERIFIED** until
an operator records evidence. Automated Rust checks are listed separately and
are not a substitute for the platform rows.

## Test conditions and evidence rules

Run the matrix against a packaged/debug build that includes the intended runtime
and at least one installed model. Use a fresh temporary app-data directory for
destructive install/uninstall tests; do not use a user's production history.
Capture for each run:

- Test ID, date, OS/version, desktop/session (X11/Wayland, Win32, macOS), Scribe
  build/commit, runtime executable/version, model ID/version, and CPU/GPU mode.
- A short screen recording or screenshot for UI/overlay/tray rows.
- Scribe status text/logs and the latency summary for recording/transcription
  rows (do not attach private transcripts or audio unless explicitly approved).
- For failures, the exact user-facing message, stage, and whether retry/rollback
  restored Idle state.

Use a neutral fixture phrase such as **“Schedule a meeting with Alex tomorrow.”**
For no-speech tests, use a silent WAV or remain silent for the configured
endpoint. For model/download tests, use the catalog's smallest recommended model
and a deliberately truncated/corrupt copy in the temporary data directory.

Status values:

- **PASS** — expected result observed and evidence attached to the test record.
- **FAIL** — behavior differs; file a defect with the captured evidence.
- **BLOCKED** — prerequisite unavailable; record why.
- **NOT VERIFIED** — not yet run (the Phase 0 status for all manual rows).

## Historical automated baseline (verified in Phase 0)

| Check | Command | Result |
| --- | --- | --- |
| Format gate | `cargo fmt --all -- --check` | PASS |
| Compile/check | `cargo check --all-targets --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS — 174 discovered, 172 passed, 0 failed, 2 ignored environment-required runtime/GPU smoke tests. |
| Clippy | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Build | `cargo build --all-features` | PASS |

The final source gate was run at HEAD `536a85f813943dbc8beaa684fc5901ff281f6577`
(source diff hash `6c39139e80fac94c8ce735e7962ed3a4ac75e0a7`,
2026-08-03 14:20:24.998–14:20:30.146 `-05:00`). All commands emitted the same
non-fatal warning: `could not canonicalize path C:\Users\huang`.

## Historical automated Phase 1 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format gate | `cargo fmt --all -- --check` | PASS |
| Compile/check | `cargo check --all-targets --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS — 200 discovered, 197 passed, 0 failed, 3 ignored environment-required smoke tests. |
| Facade fixture smoke | `cargo test transcription::tests::transcription_service_jfk_smoke_uses_the_whisper_cpp_facade --all-features -- --ignored --exact` | PASS — local whisper.cpp 1.9.1 CLI + `base.en` + JFK WAV returned non-empty text and matching IDs/model metadata through `TranscriptionService`. |
| Clippy | `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| Build | `cargo build --all-features` | PASS |
| Source boundary | `rg` scan for `transcribe_with_config` outside `src/stt/**` | PASS — the sole call outside `src/stt/**` is the private legacy bridge in `src/transcription.rs`; `src/app.rs` has none. |

Phase 1 automated tests cover accepted and stale normal/Playground events,
cross-source supersession in both directions, mismatched IDs, wrong-model
responses, per-run multi-request cleanup, and service-side model validation.
These tests prove coordinator event filtering and temporary-file ownership; they
do not prove that a real microphone/runtime/target application works on a
desktop. Execute `REC-04`, `STT-01`, `STT-02`, `STT-05`, and `OUT-01` manually
on Windows before treating the wrapped path as release-verified.

## Historical automated Phase 2 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS — 231 discovered, 226 passed, 0 failed, 5 ignored environment-required tests |
| Debug native fixture | ignored `transcription_service_jfk_smoke_uses_the_whisper_cpp_facade` with pinned v1.9.1 package, base.en, and JFK WAV | PASS — non-empty final text, CPU resolution, cold model load, then retained warm reuse |
| Native latency benchmark | release ignored `native_runtime_jfk_cold_and_warm_benchmark` | PASS — five cold and 20 warm runs; cold total median/p95 1,084/1,105 ms; warm 782/796 ms |
| Native cancellation | release ignored `native_runtime_cancellation_interrupts_active_decode` | PASS — native abort stopped a synthetic 220-second active decode; error/context cleanup returned in 781 ms |
| Release build/package | `cargo build --release --all-features`; verified PowerShell bundle script | PASS — release package staged only after exact size/SHA-256 validation |
| Release native fixture | release ignored service smoke against `target/release/runtimes/whisper_cpp` | PASS |
| Runtime boundary/integrity | boundary scan, tampered-file rejection, manifest/hash tests, GPU resolution tests | PASS |

These results verify one Windows CPU runtime/model fixture in-process. They do
not verify a live desktop, microphone, hotkey, overlay, target window, paste,
memory/idle CPU, non-ASCII Unicode model path, live-session cancellation, or any other model. All
manual rows remain NOT VERIFIED.

## Historical automated Phase 3 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 252 discovered, 247 passed, 0 failed, 5 ignored environment-required tests |
| Normalized catalog | Catalog validation, evidence-link/typed-receipt binding, role-gating, malformed-artifact, duplicate-ID, capability-intersection, minimum-runtime enforcement, and ID-prefix independence tests | PASS - five primary descriptors, all Experimental, zero curated roles; Moonshine is receipt-backed, CPU-only, and final-text-only |
| Architecture boundary | Rust source-boundary test plus `wsl.exe python3 scripts/check-catalog-boundaries.py` | PASS - one logical handler; neutral production UI including Playground; family-coded quick actions/IDs rejected; legacy provider and concrete adapter selection confined to its private bridge |
| Release build/package | `cargo build --release --all-features`; verified PowerShell package script | PASS |
| Release primary fixture | ignored exact service JFK smoke with pinned v1.9.1/base.en/JFK paths | PASS - cold load 290 ms, first decode 791 ms, retained decode 780 ms; explicit unload/reload passed |
| Exact Zipformer candidate | Fail-closed machine-readable evidence gate | **NO-GO** - no v1.13.4 native package/model pins, first-partial/comparator, corpus WER, <=250 ms cancel, lifecycle/crash/memory, or platform evidence; no second handler shipped |

These automated results verify catalog truthfulness and retain the Phase 2
primary vertical runtime slice. They do not promote a model, prove live desktop
behavior, or satisfy native streaming. All manual rows remain NOT VERIFIED.

## Historical automated Phase 4 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 283 discovered, 278 passed, 0 failed, 5 ignored environment-required tests |
| Session authority | Coordinator transition, stop-priority, cancellation, correlation, stale/duplicate/out-of-order, preload, comparison, and exactly-once output gates | PASS |
| Settings durability | Legacy/missing/invalid/truncated/future inputs; field salvage; unknown-field round trip; corrupt and rollback backup; debounce; injected atomic failure; transactional save ordering | PASS |
| Cancellation/privacy | Pre-dispatch cancellation ticket; stale-registration rejection; Windows Job Object/Unix process-group tree termination; bounded registry drain; recorder shutdown and PCM deletion; failed-start cleanup; Unix private-file setup | PASS automated paths; live microphone/process-exit behavior remains NOT VERIFIED |
| Architecture boundary | Rust boundary tests and `wsl.exe python3 scripts/check-catalog-boundaries.py` | PASS - exactly one logical handler; concrete selection remains private to the router |

Phase 4 automated tests prove state and persistence behavior under deterministic
inputs. They do not prove real desktop focus, microphone driver shutdown,
process termination during OS shutdown, or live paste behavior. Those rows
remain NOT VERIFIED.

## Historical automated Phase 5 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 323 discovered, 318 passed, 0 failed, 5 ignored environment-required tests |
| Overlay/settings | Typed Live/Minimal/Off and position migration; unknown-field preservation; stale session/revision rejection; stale completion/hide-deadline isolation; expired-deadline target cleanup; viewport/accessibility/geometry tests | PASS automated paths; physical presentation remains NOT VERIFIED |
| Target/output safety | Current-process target rejection; missing/changed target copy-only; exact target and app-level output consumption exactly once; content and generation clipboard races; correlated one-frame-deferred output; every partial Windows input-batch length with exact key release | PASS automated paths; real foreground applications, rich clipboard formats, HWND lifetime, and integrity levels remain NOT VERIFIED |
| Audio/UI boundary | Lock-free aggregate meter availability, conversion, and clamping tests; pending explicit stop is exercised through capture readiness, finalized WAV consumption, and one dispatch; preload completion is exercised before and after capture readiness; UI receives no PCM | PASS interim meter; callback WAV/mutex replacement remains Phase 6 work |
| Architecture boundary | Rust boundary tests and `wsl.exe python3 scripts/check-catalog-boundaries.py` | PASS - exactly one logical handler; runtime/model-family selection remains private |

Phase 5 automated tests prove deterministic controller and platform-adapter
logic, not physical operating-system behavior. The Windows viewport must still
be checked for no activation, taskbar exclusion, click-through, monitor/DPI
placement, AccessKit announcements, and safe interaction with real target
applications. Non-Windows overlay and automatic paste intentionally fail
closed in the current implementation.

## Historical automated Phase 6 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 358 discovered, 353 passed, 0 failed, 5 environment-required tests ignored |
| Native capture/DSP | SPSC FIFO/wrap/concurrency/overflow; conversion/downmix/resample/normalization; 30 ms RMS/peak publication; exact 512-sample Silero cadence, timing, pre/post-roll, no-speech, failure-closed, and meter-only tests | PASS automated paths; physical microphones and driver timing remain NOT VERIFIED |
| Stop/error recovery | Explicit-over-endpoint/max priority; structured overflow/stream/format faults; two-attempt restart bound; no-speech no-output; in-memory audio ownership | PASS deterministic injection; live unplug/restart remains NOT VERIFIED |
| Settings | Defaults, ordered range normalization, field salvage, and future Recording-field round trip | PASS |
| Accessibility semantics | AccessKit relationships for maximum-duration and all five VAD timing spin buttons | PASS automated semantics; physical screen-reader behavior remains NOT VERIFIED |
| Architecture boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` plus Rust source-boundary test | PASS - exactly one logical handler; runtime/model-family selection remains private |
| Pinned primary fixture | Exact ignored JFK service smoke | PASS - non-empty final text through the retained v1.9.1/base.en CPU runtime |
| Release latency fixture | Exact ignored 5-cold/20-warm JFK benchmark | Initial run exposed `STATUS_ACCESS_VIOLATION`; synchronous final-worker shutdown fixed it. Final rerun PASS - cold total median/p95 1,177/1,189 ms; warm 817/884 ms. |

Phase 6 proves that normal capture no longer writes callback WAVs and that the
native worker produces one canonical in-memory `PreparedAudio`. It does not
prove real device recovery, acoustic VAD quality, first-syllable retention,
meter cadence on a physical driver, or any desktop output behavior. Those rows
remain NOT VERIFIED.

## Historical automated Phase 7 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/lint/build | `cargo fmt --all -- --check`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 406 discovered, 400 passed, 0 failed, 6 environment-gated tests ignored |
| Rolling scheduler/stabilizer | Exact cadence/window, one-active/newest-pending, retained-handle non-blocking drain, exact 650 ms boundary, 699/700 ms horizon, two-pass stability, case/punctuation correction, non-empty deletion/reappearance, repeated-word, overlap, bounded-context, and correlation tests | PASS |
| Native audio boundary | Preview window normalization and final-audio identity; source scan prevents app-shell snapshot/PCM publication | PASS - preview PCM stays in native capture/service workers |
| Output isolation | Coordinator-first acceptance, overlay-only partials, monotonic final replacement, Playground/final-only exclusion | PASS - tentative text cannot create `PendingOutput` or replace the application transcript |
| Overlay accessibility | AccessKit selector naming, stabilizer-shaped word and closing-punctuation boundary composition, inspectable non-live tentative text, committed/final-only live announcements, preview-degradation notice, typed recovery guidance, and inactive meter contrast | PASS automated rendering semantics and contrast; physical keyboard/screen-reader/reduced-motion behavior remains NOT VERIFIED |
| Architecture boundary | `wsl.exe python3 scripts/check-catalog-boundaries.py` and Rust boundary tests | PASS - one `TranscribeCppRuntime`; no ONNX handler or native-streaming claim |
| Pinned release smoke/final benchmark | Exact ignored base.en/JFK service smoke and 5-cold/20-warm final benchmark | PASS - cold total median/p95 1,087/1,099 ms; warm total 781/800 ms |
| Rolling first speech text | Exact ignored 5-cold/20-warm scheduler-level fixture harness with artifact hashes and expected speech checks | PASS as non-desktop evidence - cold median/p95 2,039/2,049 ms; warm 1,730/1,754 ms; `[BLANK_AUDIO]` is filtered privately and not counted |

The rolling latency harness includes 250 ms cloned canonical frame publication
and native decode, but bypasses the production capture pipeline and does not
include a real hotkey, microphone driver, overlay paint, or target application.
All desktop rows remain NOT VERIFIED. Five catalog models
remain Experimental, zero are Supported, native streaming remains false, and
the Zipformer/second-handler decision remains NO-GO.

For the real Moonshine subprocess smoke, build Scribe and set `SCRIBE_ONNX_WORKER_EXE` to the built executable. Run the ignored `transcription::tests::diagnostic_real_hugging_face_bundle_install_load_and_decode` test with `SCRIBE_ONNX_BUNDLE_TEST=1`, a dedicated `SCRIBE_ONNX_BUNDLE_STORAGE_DIR`, `SCRIBE_ONNX_BUNDLE_WAV` and its exact lowercase `SCRIBE_ONNX_BUNDLE_WAV_SHA256`, plus `SCRIBE_ONNX_BUNDLE_EXPECTED_TRANSCRIPT`. Verify stage, child Hello/load/health/silence smoke, known spoken-WAV decode, unload/reload, and activation. This remains manual evidence until the exact fixture and result are versioned.

## Historical automated Phase 8 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/lint/build | `cargo fmt --all -- --check`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 436 discovered, 430 passed, 0 failed, 6 environment-gated tests ignored |
| Final pass | App/coordinator/capture/preview tests | PASS - post-roll and one full final path retained; no-speech/empty final and timed-out preview cancellation produce zero output |
| Windows target identity | 14 injected native target probes | PASS - dead/recycled/changing targets, generation-property loss, and activation denial fail closed; stable HWND/thread/PID/process-creation/property identity reactivates and revalidates |
| Clipboard/output transaction | 26 injected-driver, native-format-validation, and Windows input-batch cases | PASS - zero sequence rejection, one-open bounded snapshot, source-order classification, supported-format selection, activation/snapshot/restore races, target loss, failed paste, exactly-one output, and individually checked key-release cleanup. Actual native mixed-format restoration remains NOT VERIFIED. |
| Output UI accessibility | AccessKit control relationships and semantic-role tests | PASS - Transcript editor is labelled and exposes its temporary disabled/output state; Advanced numeric/combo controls are labelled; page and History card titles are headings; copy actions are unambiguous. Physical screen-reader behavior remains NOT VERIFIED. |
| Runtime/PCM boundary | Catalog boundary script and existing source guards | PASS - one logical handler and native-only PCM remain unchanged |
| Pinned native smoke | Exact ignored base.en/JFK service fixture | PASS - debug-harness first load 4,367 ms, first decode 801 ms, warm decode 792 ms; retained release numbers remain the comparable evidence |

## Historical automated Phase 9 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Phase gates | Format/check/strict Clippy/debug+release build/full suite | PASS - 474 discovered, 468 passed, 0 failed, 6 ignored |
| Transactional installation | Download/resume/hash/extraction/smoke/activation/removal/rollback failure injection | PASS |
| Exact pinned package | Bounded parent smoke against exact 13-file whisper.cpp v1.9.1 Windows x64 package | PASS - no fault dialog; health/load/decode/unload-reload completed |
| Architecture | Boundary guard and release package checks | PASS - exactly one logical runtime handler |

## Historical automated Phase 10 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Phase gates | Format/check/strict Clippy/build/full suite | PASS - 523 discovered, 517 passed, 0 failed, 6 ignored |
| History lifecycle | Focused database, audio, retention, retry, bounded release retry, retention-independent lease acknowledgement, deletion, reconciliation, process-lock, and app-correlation tests | PASS |
| Playback and UI | Native playback tests plus AccessKit/history interaction tests | PASS - bounded native audio, callback-timestamp drain deadline, bounded shutdown, reliable terminal state, live/busy result announcements, structural contextual groups/actions, destructive focus restoration, expanded disclosures, state-specific disabled reasons, confirmations, and 44 px actions |
| Privacy/output | Immutable final snapshot, neutral durable failures, one-shot fresh-target repaste, active-work/deletion/Off invalidation, and retry-no-output tests | PASS |
| Architecture | Rust and script boundary guards | PASS - one logical runtime handler; PCM remains native |

These are deterministic code-level checks, not a physical Windows paste run.
OUT-01, OUT-05, OUT-06, target elevation, image clipboard round trips, and the
new activation/paste latency timestamps remain NOT VERIFIED on a desktop.

## Prerequisites and test data

| Code | Prerequisite |
| --- | --- |
| P1 | Scribe debug/release build launches in a real desktop session with the app data directory isolated. |
| P2 | A microphone is connected and selectable; test one USB and one Bluetooth device when available. |
| P3 | A known-good installed model/runtime pair. Record exact IDs and versions; compatibility is not implied by a catalog label. |
| P4 | A text target for paste: browser text input, VS Code/editor, terminal, and a native desktop field where available. |
| P5 | Optional second monitor or mixed-DPI scaling for overlay placement. |
| P6 | Permission access as applicable: Windows microphone/input automation, macOS Microphone + Accessibility/Input Monitoring, Linux audio/clipboard and optional global-hotkey permission. |
| P7 | Test account/data is disposable; backup clipboard content and do not use sensitive text. |

## Core UI, startup, and tray

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| UI-01 | Win/Linux/macOS | P1 | Launch Scribe; visit Transcribe, General, Models, History, Advanced, and About; enable Developer > Debug and visit Debug; close and relaunch. | Window opens without panic; page navigation and close/reopen behavior are stable. Debug is absent until enabled, and the functional comparison workflow is reachable from Models. Capture startup log/screenshot. | **NOT VERIFIED** |
| UI-02 | Win/Linux/macOS | P1, P7 | Change a setting, restart, and inspect the value. Then load a config with one unknown/invalid field in the isolated data dir. | Valid settings persist; invalid data does not erase all valid settings; the original is backed up before a lossy salvage. Automated migration tests pass; verify the desktop-visible recovery behavior here. | **NOT VERIFIED** |
| UI-03 | Win/Linux/macOS | P1 | Toggle theme/performance/audio/input settings available in the current build; verify labels and disabled states. | Controls are keyboard reachable, labels are understandable, and unavailable runtime actions explain why. | **NOT VERIFIED** |
| UI-04 | Win/Linux/macOS | P1 | If tray is supported, hide window, open tray menu, Show, Hide, Start/Stop Recording, Copy Last Transcript, Quit. | Tray commands affect the app exactly once; Quit exits; no duplicate recording. Capture tray menu and status. | **NOT VERIFIED** |
| UI-05 | Linux (X11/Wayland) | P1, P6 | Run once with tray/hotkey defaults and once with explicit `SCRIBE_ENABLE_GLOBAL_HOTKEY=1`; try `SCRIBE_DISABLE_TRAY=1`. | Unsupported session paths fail visibly and main window remains usable; no silent process exit. | **NOT VERIFIED** |
| UI-06 | Windows | P1, P5 | Start dictation while Scribe is foreground, then Alt+Tab away and back; repeat after minimizing and hiding Scribe to tray, on each monitor, at 100/125/150/200% scaling, and near each work-area edge. | The overlay stays hidden over foreground Scribe, restores its current phase/timer/meter/text when Scribe is known to be background, hides again on refocus, and remains in the selected top/bottom position within the captured target monitor's work area without stealing focus. | **NOT VERIFIED** |
| UI-07 | Win/Linux/macOS | P1, P3 | Open Models online and offline. Search installed, imported, and curated models by friendly name/language; switch All languages/English/Multilingual; collapse and expand Installed/Available; activate Refresh explicitly; inspect available, downloading, failed, legacy, installed, and active cards. | Opening, searching, filtering, expanding, and scrolling use the current in-memory snapshot without HTTP or filesystem probes. Only Refresh accesses the remote catalog. Cards keep friendly names, show Experimental and `Not rated` where metadata is unavailable, announce result/import status, expose primary actions by mouse/Enter/Space, and retain separate Cancel/Details/Remove actions. Insufficient disk space disables installation before network I/O. Existing legacy paths/files remain untouched. | **NOT VERIFIED** |
| UI-08 | Windows | P1, P2, P4 | With Live preview, begin dictation from another app; verify taskbar, Alt+Tab, original target focus, body clicks, and the 44 x 44 X. Dictate a short line, then continue past the available width. Cancel once during Preparing and once during Recording. Repeat presentation in Compact status and Off, including one visible failure/notice state. | The overlay has no taskbar/Alt+Tab entry and remains always on top without activation. Body clicks reach the underlying app; only the X accepts input and clicking it does not move keyboard focus. Live is a 600 x 62 glass pill: short/exact-fit text starts at the preview origin, the first overflow switches to a grapheme-safe no-ellipsis tail, and later words remain visible at the fixed right edge while prior ink moves left. Compact is a 200 x 62 matching shell with static brand, proportional timer, and X only; failures replace the timer with visible `Error`/`Notice` feedback while full recovery detail remains accessible. Off stays hidden. If native control hardening fails, the X is absent while the passive display remains safe. | **NOT VERIFIED** |
| UI-09 | Linux/macOS | P1, P2 | Select Live preview or Compact status and begin dictation. | Until a native no-focus adapter exists, the effective overlay remains Off and the UI explains the conservative limitation; no focus-stealing window appears. | **NOT VERIFIED** |
| UI-10 | Windows | P1, P2, P6 | Navigate all main pages and controls by keyboard, inspect visible focus, invoke the foreground discard control, run Narrator/Accessibility Insights while foreground/background overlay ownership changes, and enable the OS reduced-motion setting. | All controls are reachable and labeled, including `Cancel recording and discard it`; focus is visible, state is not conveyed by color alone, primary controls and the overlay X are at least 44 px, committed updates have exactly one polite live-region owner, tentative text is non-live, and no disallowed motion occurs. | **NOT VERIFIED** |
| UI-11 | Windows | P1, P2 | Start capture and speak softly/loudly, then stay silent; compare overlay meter with input. | The meter reflects distinct native RMS and peak values at the approximately 33 Hz worker cadence, clamps safely, and displays no fabricated activity. Record first-meter latency. | **NOT VERIFIED** |

## Hotkeys and recording lifecycle

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| HK-01 | Windows | P1, P2, P4, P6 | Focus a text field; press and hold configured `Ctrl+Shift+Space`; speak fixture; release. | One recording starts on press, stops on release, and returns to Idle after finalization. Record latency summary and screen evidence. | **NOT VERIFIED** |
| HK-02 | Linux | P1, P2, P4, P6 | Repeat HK-01 with global hotkey opt-in and with the Start/Stop button when global hotkey is disabled. | Opt-in hotkey works where supported; button path remains available when hotkeys are disabled. | **NOT VERIFIED** |
| HK-03 | macOS | P1, P2, P4, P6 | Grant required permissions, then repeat HK-01. Revoke Accessibility/Input Monitoring and repeat. | Granted path records; denied path gives actionable permission guidance and does not paste unpredictably. | **NOT VERIFIED** |
| HK-04 | Win/Linux/macOS | P1, P2 | Configure Toggle mode; press once, speak, press again. | First press starts and second press finalizes exactly one session. | **NOT VERIFIED** |
| HK-05 | Win/Linux/macOS | P1, P2 | Configure Hold mode; press/release without speech; then press/release twice rapidly. | No-speech path does not paste empty/partial text; rapid events do not create overlapping sessions. | **NOT VERIFIED** |
| HK-06 | Win/Linux/macOS | P1 | Register a shortcut known to be used by another app; restart/reconfigure. | Conflict is reported; app remains usable through visible Start/Stop control. | **NOT VERIFIED** |
| REC-01 | Win/Linux/macOS | P1, P2 | Record fixture for 3–5 seconds; stop; inspect status and the recovery-recording directory. | Capture begins after stream start, preserves configured post-roll, prepares mono 16 kHz audio in memory, starts transcription, and creates no normal capture WAV. Capture exact latency phases. | **NOT VERIFIED** |
| REC-02 | Win/Linux/macOS | P1, P2 | Select a missing microphone/device, then unplug the active device during capture; reconnect before the second bounded retry. Repeat with a changed device format. | Missing device is actionable; same-format recovery resumes within at most two attempts; exhaustion or format change fails visibly; no hang or paste occurs and a fresh session can start. | **NOT VERIFIED** |
| REC-03 | Win/Linux/macOS | P1, P2 | Let recording run to configured maximum duration. | Recording stops deterministically and finalization follows the same safe path as explicit stop. | **NOT VERIFIED** |
| REC-04 | Win/Linux/macOS | P1, P2 | On Windows, use the overlay X during microphone startup and active recording; also use Scribe's foreground discard control. On every platform, cancel/stop before speech and during finalization, attempt an immediate second normal or Playground session, and repeat once through tray Quit. | Discard returns to Idle with `Recording discarded.`, never starts final transcription, pastes, creates history, or retains audio/partial text. A second capture remains blocked only while the abandoned native worker drains. Stale completion cannot affect the next session; ordinary Stop still finalizes and explicit stop wins over inferred endpoint/max at the same boundary. | **NOT VERIFIED** |
| REC-05 | Win/Linux/macOS | P1, P2 | In Toggle mode with defaults, begin in silence, speak after at least 250 ms, pause for 450 ms and resume, then finish with at least 900 ms silence. Repeat in Hold-to-talk, with automatic endpointing disabled, and with an immediate shortcut stop. | Silero confirmation retains the first syllable through bounded pre-roll; 450 ms pause does not finalize; 900 ms silence endpoints only in Toggle mode when enabled; Hold-to-talk waits for release; explicit stop retains about 200 ms post-roll and outranks inferred silence. Record actual timings and audio evidence only with consent. | **NOT VERIFIED** |
| REC-06 | Win/Linux/macOS | P1, P2, P7 | Hardware-mute the selected microphone or turn its physical gain to minimum, record once through normal dictation and once through Playground, then export redacted diagnostics. Restore gain and make a short audible non-voice burst that reaches the meter but does not satisfy speech confirmation. | Both low-input captures paste nothing and use the shared silent/too-low guidance; a selected FIFINE names its top mute control and physical gain knob. Diagnostics contain only maximum input RMS/peak scalars, never PCM. The audible short burst retains generic no-speech feedback. | **NOT VERIFIED** |

## Runtime, model, and transcription flows

### Stage 4 Windows GPU worker-pack checkpoint

Recorded 2026-08-29 on Windows x64. This is automated fixture/hardware evidence
for the private worker path, not a packaged-desktop manual PASS and not Auto
qualification.

| Area | Evidence | Status |
| --- | --- | --- |
| Vulkan fixture build | Clean fixture-only pack from `10d4ec2`, Vulkan SDK 1.4.357.0, `scribe-vulkan-windows-x64` `0.1.0-fixture7`, digest `563e1cf17db85bf02c40dda7d074e981c589931aa890f986446df70428aad62b`; three files, 98,017,192 installed bytes, 31,801,892 compressed bytes, 98,016,256-byte worker payload | **PASS fixture tooling** |
| MSVC shell compatibility | Clean fixture-only rebuild from `94ba0ff` under Visual Studio 17.14 shell using exact v143 `14.44.35207` tool payloads and Windows SDK `10.0.26100.0`; pack `0.1.0-fixture-toolchain2`, digest `edd7cc74481720c19c21decfa4676af8c7b2dfb32abb50e2c5ba9a56c88fd306`; exact compiler path reported by CMake; wrong tool hash/component and process-environment leakage rejected by contract tests. GitHub hosted Windows/Visual Studio 18 run `33290948682` at `c973841` passed the exact toolchain contracts, independent CPU desktop/worker builds, Vulkan-enabled lint and 1,361 tests, validated portable/installer builds, and payload parity. Hosted PR lint/test uses `ui-harness,inference-worker,vulkan-acceleration`; CUDA remains confined to the exact production pack/toolkit gate and is not claimed by this run. | **PASS locally and hosted CI** |
| Explicit-GPU SCIF/model smoke | RTX 4080 SUPER; stable ID `native:0000:01:00.0`; driver `windows-display:32.0.16.1088`; 16,824,401,920 memory bytes; pinned model SHA-256 `3b46ca40bccbf7609c68d88a36d96077a04ca7c87f2060ede06f129fac3e7652`; pinned WAV SHA-256 `59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`; expected `ask not` phrase present; `warm_reused=true`; CPU launches zero | **PASS isolated hardware smoke** |
| CUDA provider | CUDA Toolkit/nvcc 12.8.93 was absent. Fixture builds require that exact developer toolkit; production mode additionally requires a complete canonical file inventory with exact SHA-256 values, which is intentionally empty until reviewed artifacts are provisioned. | **BLOCKED locally / fail closed** |
| Production trust and release | No reviewed production public key or protected signing service exists. The candidate-ref release workflow contains no signing secret reference, rejects every GPU-pack request, and packages CPU only. Official publication also requires `SCRIBE_GPU_PACK_RELEASE_POLICY`; `gpu_packs_required` remains fail-closed until a separately protected trusted workflow signs fixed verified artifacts. | **NOT VERIFIED / fail closed** |
| Auto/performance qualification | Auto remains CPU/default-denied and does not launch probes. Five-cold/twenty-warm performance, device-loss, suspend/resume, driver-update, and packaged installer lanes were not run. | **NOT VERIFIED** |

Reproduce the hardware smoke only with the exact ignored-test environment
documented in `GPU_WORKER_PACKS.md`; fixture trust must never be used for a
release artifact. The production manual rows below remain unchanged.

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| STT-01 | Win/Linux/macOS | P1, P2, P3, P4 | Install/select the known-good model; transcribe fixture through the normal flow. | Non-empty final transcript appears, backend/model identity is visible in diagnostics, and exactly one finalized output is produced. | **NOT VERIFIED** |
| STT-02 | Win/Linux/macOS | P1, P2, P3 | Run the same WAV in Playground for every installed/ready model. | Each selected ready model completes independently; missing runtime/model blocks only that card with repair guidance. | **NOT VERIFIED** |
| STT-03 | Win/Linux/macOS | P1, P2, P3 | Run a second transcription immediately, then after the model has been idle for more than five minutes. | The persistent STT worker reports immediate warm reuse; it unloads the model after the five-minute idle timeout and the next request reloads it. Record worker generation/PID, load/decode metrics, and whether the worker was reused or replaced. | **NOT VERIFIED** |
| STT-04 | Win/Linux/macOS | P1, P2, P3 | Attempt transcription with a missing runtime, missing model file, and incomplete model directory. | Native inference does not run; actionable status identifies what to install/repair; no paste occurs. Record whether lazy worker startup was needed for the rejected request. | **NOT VERIFIED** |
| STT-05 | Win/Linux/macOS | P1, P2, P3 | Terminate the active STT worker generation or force a non-zero child exit during transcription. | Failure is surfaced, app returns to Idle/Error, the supervisor can create a fresh generation, and retry is safe; no stale result is applied. | **NOT VERIFIED** |
| STT-06 | Win/Linux/macOS | P1, P2 | Remain silent, then repeat with audible non-speech/noise until endpoint or stop. | Empty/no-speech results never paste. Exact sequential 512-sample Silero decisions alone classify speech in a separate VAD-only worker; that worker never receives STT controls. RMS remains a meter/diagnostic: after Silero reports no speech, only a capture whose maximum diagnostic RMS stayed below the low-input guidance floor receives silent/too-low hardware guidance. | **NOT VERIFIED** |
| STT-07 | Win/Linux/macOS | P1, P2, P3 | In Advanced, run Auto, Rolling preview, and Final text only against the same utterance; repeat once in Playground. Capture the committed/tentative overlay states and first-partial latency. | Auto and Rolling use bounded batch preview only for the primary native model; Final text only and Playground emit no partials. Tentative text stays in the overlay, corrections do not backspace another app, and the final result replaces the preview once. No model advertises native streaming. | **NOT VERIFIED** |
| STT-08 | Win/Linux/macOS | P1, P2, P3 | Change Auto/GPU/CPU-only acceleration preference where supported; run fixture on each available mode. | In Stage 4, Auto remains CPU and diagnoses why verified GPU candidates are default-denied; explicit CPU is honored; explicit GPU uses only a verified compatible pack/device and fails clearly without silent CPU fallback. Record resolved backend/device, pack/driver identity, power policy, quarantine, fallback history, and errors. | **NOT VERIFIED** |
| STT-09 | Windows | P1, P3 | Install and select the receipt-backed `moonshine-tiny-en-int8-onnx` bundle; transcribe the approved fixture and record its receipt, model identity, worker generation, cancellation behavior, and final-text output. | The installed receipt validates before activation; native Sherpa ONNX inference runs in the persistent `--scribe-inference-worker` child, produces a final transcript, and does not claim native streaming. Record all failures and the exact build/model/fixture evidence. | **NOT VERIFIED** |
| STT-10 | Windows | P1, P3, P7 | With Scribe running, inspect the process tree, canonical executable paths, and worker launch arguments during a GGUF transcription, a receipt-backed ONNX transcription, and an AI-VAD capture. Inspect stdout/stderr capture for each child and confirm no local listener is opened. Repeat after removing or replacing the adjacent inference worker and with explicit GPU selected. | The desktop process owns no GGUF or ASR ONNX model/session/recognizer handles. One persistent adjacent `scribe-inference-worker.exe --scribe-inference-worker` child owns GGUF and native Sherpa ONNX inference; a separate `local-transcriber.exe --scribe-vad-worker` instance owns VAD only. Both use private SCIF v5 stdin/stdout pipes, complete the expected capability handshake, keep stdout protocol-only, send diagnostics to stderr, and open no localhost/TCP/HTTP transport or nested ONNX worker. A missing/wrong worker fails clearly, and explicit GPU never launches the CPU worker. Use a disposable profile and terminate the workers after the run. | **NOT VERIFIED** |

## Output, clipboard, and target safety

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| OUT-01 | Windows | P1, P2, P3, P4, P7 | Put sentinel text on clipboard; focus browser/editor; run HK-01 with auto-insert enabled. | Final text is pasted once into the original field and clipboard is restored when configured. Verify target text and clipboard sentinel manually. | **NOT VERIFIED** |
| OUT-02 | Linux X11 | P1, P2, P3, P4, P6, P7 | Repeat OUT-01 on X11. | Until native target identity and clipboard generation are verified, final text is copied only with an explicit status and no synthetic key input. | **NOT VERIFIED** |
| OUT-03 | Linux Wayland | P1, P2, P3, P4, P6, P7 | Repeat OUT-01 on Wayland. | If synthetic paste is blocked, final text is copied only with an explicit notice; no paste into an unrelated field. | **NOT VERIFIED** |
| OUT-04 | macOS | P1, P2, P3, P4, P6, P7 | Grant Accessibility, repeat OUT-01; revoke permission and repeat. | Until native target identity and clipboard generation are verified, both permission states remain explicit copy-only with no synthetic key input. | **NOT VERIFIED** |
| OUT-05 | Win/Linux/macOS | P1, P2, P3, P4 | Begin dictation in target A, switch focus to unrelated target B before completion. | On Windows, no synthetic key is sent and final text remains copied because the captured target no longer matches. On Linux/macOS, current behavior is conservatively copy-only. Never accept text pasted into B as PASS. | **NOT VERIFIED** |
| OUT-06 | Win/Linux/macOS | P1, P2, P3, P4 | Close target app during finalization; then change the clipboard from another app before paste and, in a separate run, after paste but before restoration. Repeat once with the same visible text and once with different text. | No unrelated paste occurs when ownership changes before input. After a completed paste, restoration never overwrites the independent change; Scribe reports that restoration was skipped and records `inserted_clipboard_restore_skipped` without transcript content in history/diagnostics. | **NOT VERIFIED** |
| OUT-07 | Win/Linux/macOS | P1, P2, P3 | Disable auto-insert; transcribe fixture and use Copy Transcript. | No synthetic key input occurs; explicit copy places the final transcript on clipboard. | **NOT VERIFIED** |
| OUT-08 | Windows | P1, P2, P3, P4, P7 | Copy an image to the clipboard, transcribe and auto-insert, then inspect the clipboard; repeat with mixed text+image, empty, HTML/RTF/file-list, >64 MiB, invalid PNG/DIB header or dimensions, and unsupported private payloads. | Supported text/locale/PNG/DIBV5 source payloads are copied as bounded opaque bytes under native single-open transactions and restored while Scribe retains the same nonzero sequence; Windows regenerates documented conversions. Unsafe-size/header, unavailable, or unsupported formats become copy-only with no synthetic keys and explicit status. | **NOT VERIFIED** |
| OUT-09 | Windows | P1, P2, P3, P4 | Begin in a normal target, then close/reopen it or restart its process before final output; repeat against an elevated target and while Windows denies foreground activation. | HWND/PID/process-creation mismatch or activation denial produces copy-only output. Scribe never forces focus, retries paste, or targets the replacement window. | **NOT VERIFIED** |
| OUT-10 | Windows | P1, P2, P3, P4, P7 | Enable Windows clipboard history and a representative third-party clipboard manager. Repeat OUT-01 with the manager idle, then while it observes/captures the Scribe write. Inspect target text, clipboard contents, status, and output outcome. | The transcript is pasted at most once. Benign sequence churn is accepted only while Scribe's owner, exact Unicode text, private marker, and Unicode/marker/synthesized-text format set remain intact. Any lost marker/owner, read error, or added rich/custom format prevents input or skips restoration without overwriting newer data. | **NOT VERIFIED** |
| OUT-11 | Windows | P1, P2, P3, P4, P7 | Copy representative formatted selections from Microsoft Office and a browser (plain text plus HTML/RTF and an embedded image where available), then run auto-insert into both a browser field and a native editor. | Unsupported rich/custom source formats remain explicit copy-only with no synthetic input. Supported bounded native formats restore exactly; no Office/browser payload is truncated, fabricated, or overwritten, and no private transcript content appears in diagnostics. | **NOT VERIFIED** |

## Downloads, install, settings, privacy, and recovery

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| DL-01 | Windows | P1, disposable data dir, network | Start a recommended model download; cancel after measurable progress; restart Scribe and resume. Repeat with a server/proxy that ignores Range. | Progress is byte-backed; cancellation preserves a verified partial; valid `206` appends only the requested suffix; ignored Range restarts cleanly rather than duplicating bytes; exact final size/SHA-256 is required before activation. | **NOT VERIFIED** |
| DL-02 | Windows | P1, disposable data dir, network | Replace a model partial/final file with truncated, oversized, and same-sized wrong-hash data; attempt install each time. | Invalid data is never runnable; unsafe partials are quarantined before clean retry; prior installed metadata/artifact remains intact. | **NOT VERIFIED** |
| DL-03 | Windows | P1, disposable data dir, network | Interrupt runtime download, extraction, smoke, and activation in separate runs; restart after each. Also modify the pinned archive, omit a manifest file, and place an extra file directly in a staged tree. | The modified archive fails its outer hash; missing allowlist entries and staged-tree extras fail closed; startup resolves only journal states proven by durable settings fingerprint; ambiguous state is preserved and gates mutation; current or exactly one previous known-good runtime remains usable. | **NOT VERIFIED** |
| DL-04 | Windows | P1, disposable data dir | Install the pinned runtime, run the smoke, remove it while idle, and restart after interrupting removal before and after settings persistence. Repeat removal during an active dictation. | Exact 13-file tree is required; bounded smoke produces no blocking crash dialog; removal preserves unrelated models and legacy unmanaged files; restart restores or finishes only from the durable fingerprint; active-session mutation is disabled. | **NOT VERIFIED** |
| DL-05 | Linux/macOS | P1, disposable data dir | Open runtime maintenance and attempt normalized managed runtime installation. | The UI reports that no verified pinned package exists for this platform; no staging/activation occurs and no support claim is shown. | **NOT VERIFIED** |
| DL-06 | Windows x64 packaged build | Fresh disposable profile; disconnect network before launch | Build with `build-windows-release.ps1`, confirm the final bundle is outside Cargo's `target` tree, compare every staged file with `bundle-inventory.json`, and confirm the desktop, adjacent CPU inference worker, model, and attribution files are present. Disconnect the network, then launch, transcribe the local fixture, cancel one transcription, unload/reload, and restart. Delete Base while it is active and the last installed model, restart, simulate an update restoring its executable-sibling file, restart again, then choose Install. Repeat with the inference worker missing/replaced and with the bundled model missing, truncated, and replaced by same-sized wrong bytes. | The exact base.en model is immediately Installed/ready offline and requires exact size/SHA-256. Inference launches only the canonical adjacent worker; missing or wrong worker bytes fail clearly without loading inference into the desktop. Delete removes only the manifest-defined executable sibling, persists its exclusion, and leaves no active model when no replacement is ready. An update-restored copy stays excluded and is removed before loading; Install verifies a restored copy or uses the verified managed flow and clears the exclusion. Missing/corrupt bytes fail closed with repair guidance and no automatic download. | **NOT VERIFIED** |
| SET-01 | Win/Linux/macOS | P1, P7 | Edit hotkey, recording mode, active model, microphone, performance mode, theme, and auto-insert; restart. | Values persist with no silent reset; invalid values are rejected or salvaged. | **NOT VERIFIED** |
| SET-02 | Win/Linux/macOS | P1, P7 | Copy the config aside, corrupt one field/file, launch, then inspect backup/recovery. Repeat with a valid legacy flat config. | Valid fields survive; corrupt input receives a timestamped backup; legacy input receives a pre-migration backup; future root/section/install fields survive the rewrite. | **NOT VERIFIED** |
| PRIV-01 | Win/Linux/macOS | P1, P7 | Inspect the platform Scribe history directory after successful, no-speech, overflow, failed, and Playground transcription in default Transcript only mode. | Normal dictation stores transcript metadata only; no retained WAV exists. No-speech and Playground create no history row. PCM remains in native workers and is released after consumers finish. Record filenames/permissions without exposing transcript content. | **NOT VERIFIED** |
| PRIV-02 | Win/Linux/macOS | P1, P7, disposable data dir | Exercise Off, Transcript only, and Transcript + audio; pin one entry; exceed the count cap; configure transcript/audio age limits; restart. | Off creates no new row. Transcript only is default and creates no audio. Transcript + audio creates one bounded mono 16 kHz WAV. Pinned entries survive automatic retention; delete-audio preserves transcript metadata; switching Off does not silently delete existing data. | **NOT VERIFIED** |
| RECOV-01 | Win/Linux/macOS | P1, P2, P3 | Force microphone, runtime, output, and config failures one at a time; start a fresh dictation after each. | Each failure has a user-facing stage/message, no stale result leaks into the next run, and Idle is recoverable. | **NOT VERIFIED** |
| RECOV-02 | Win/Linux/macOS | P1, P3, Transcript + audio | Fail a transcription with retained audio, press Retry, and observe the destination app. Repeat with corrupt/missing audio and interrupt/restart during retry. | Retry decodes the same row/audio, increments retry count only at a terminal outcome, never creates a duplicate row, and never pastes. Corrupt/missing audio remains Failed with actionable status; restart leaves no stranded Pending row. | **NOT VERIFIED** |

## History, playback, retention, and repaste

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| HIST-01 | Win/Linux/macOS | P1, P7, history enabled | Create more than 20 unpinned entries; search literal `%`, `_`, transcript, model, and app text; use Load more; pin one entry. | Results are newest-first with no duplicate/gap across pages; wildcard characters are literal; the pinned row survives count retention. | **NOT VERIFIED** |
| HIST-02 | Win/Linux/macOS | P1, P7 | Complete one row whose raw/final text differs; copy each, pin/unpin, delete audio, then confirm full deletion. | Raw and final text are separately visible/copyable. Pin state persists. Delete audio leaves metadata; full deletion removes the row and contained audio only after confirmation. | **NOT VERIFIED** |
| HIST-03 | Win/Linux/macOS | P1, output device, retained audio | Play retained audio, Stop while loading and while playing, switch output device, and attempt a corrupt/oversized/replaced WAV. | UI shows one correlated pending/active state; Stop reliably clears it; canonical bounded audio plays natively; invalid audio fails without allocation spike, retry, or crash. | **NOT VERIFIED** |
| HIST-04 | Windows | P1, P4, P7, completed history row | Arm Paste again, focus a fresh normal target, press the shortcut once. Repeat after expiry, deletion, History Off, UI/tray recording start, active retry, target close/change, and elevated target. | Only the first valid idle arm invokes safe output exactly once. Every invalidation clears private text; active recording hotkey performs normal Stop/Toggle and never pastes old history; unsafe targets are copy-only. | **NOT VERIFIED** |
| HIST-05 | Windows | P1, disposable data dir | Start a dictation that creates Pending, force terminate, relaunch; repeat by launching a second Scribe while the first owns an active row. Interrupt full deletion before/after file removal. | Relaunch marks only abandoned Pending failed and reconciles the deletion journal. A second process cannot open/reconcile the same store. No unrelated file is touched. | **NOT VERIFIED** |
| HIST-06 | Windows | P1, disposable data dir | Inspect owner/DACLs of history root, DB/WAL/SHM/lock, and audio; attempt junction/symlink/reparse substitution before launch. | Root/files are private to owner and LocalSystem; Scribe fails closed on reparse paths or insecure/unhardenable storage and never falls back to the working directory. | **NOT VERIFIED** |

## Platform coverage and sign-off

| Platform/session | Required rows | Operator/evidence | Status |
| --- | --- | --- | --- |
| Windows 11 desktop, standard-integrity target | UI, HK, REC, STT, OUT, DL, SET, RECOV | **NOT VERIFIED** — no Windows GUI/microphone run through Phase 6; only the pinned non-GUI native fixture ran. | **NOT VERIFIED** |
| Windows elevated target (if supported) | OUT-01, OUT-05, OUT-06 | **NOT VERIFIED** — SendInput integrity boundary not exercised. | **NOT VERIFIED** |
| Ubuntu/Debian X11 | UI, HK-02, REC, STT, OUT-02, DL, SET | **NOT VERIFIED** — no Linux desktop/audio run in Phase 0. | **NOT VERIFIED** |
| Linux Wayland | UI, HK-02, REC, STT, OUT-03, RECOV | **NOT VERIFIED** — clipboard/paste and global-hotkey portal behavior not exercised. | **NOT VERIFIED** |
| macOS desktop | UI, HK-03, REC, STT, OUT-04, SET, RECOV | **NOT VERIFIED** — permissions and accessibility behavior not exercised. | **NOT VERIFIED** |
| Multi-monitor/mixed-DPI | UI-06, OUT-05 | **NOT VERIFIED** — no multi-monitor run. | **NOT VERIFIED** |
| USB + Bluetooth microphones | REC-01, REC-02 | **NOT VERIFIED** — no physical devices available to this documentation run. | **NOT VERIFIED** |

### Mixed-DPI main-window acceptance

Run this check on Windows with an unmaximized Scribe window, one 1920 x 1080
display at 100% scaling, and a second display at 125%, 150%, or 200% scaling:

1. Open Scribe at its default 1180 x 815 size and drag it by the title bar so
   the window crosses onto the other display. The complete window should remain
   at the drop location instead of snapping back to the source monitor.
2. Repeat using Win + Shift + Left/Right Arrow, then maximize and restore on
   each display. The window should remain usable and retain normal title-bar,
   Snap, and restore behavior.
3. Resize to the smallest allowed window. It should stop at 840 x 500 logical
   pixels, with route content scrolling or reflowing and no horizontal clipping.

Record the source/target monitor scaling and a screenshot of the final window
position. Keep this row **NOT VERIFIED** until a physical run is completed.

### Completion rule

This living matrix remains valid only while each implemented phase updates its
automated checkpoint and any affected manual steps. Platform/release sign-off
requires a dated PASS/FAIL/BLOCKED result with evidence in the owning test
report. Do not change a manual row to PASS based solely on compilation or a unit
test. Rows that cannot be run on a platform remain **NOT VERIFIED** and are
listed as release risks until an explicit support decision is made.

## Historical automated Phase 11 checkpoint

Recorded 2026-08-04 on Windows x64. Automated evidence does not change any
manual row above to PASS.

| Area | Evidence | Status |
| --- | --- | --- |
| Repository gates | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict all-target/all-feature Clippy; full tests; debug/release builds | **PASS** - 535 tests discovered, 529 passed, 0 failed, 6 ignored because they require explicit local runtime/fixture environments |
| Handler boundary | Rust architecture guard plus WSL Python source scanner | **PASS** - exactly one `TranscribeCppRuntime`; no `OnnxSpeechRuntime`; router-private selection; neutral app/UI; native PCM; final-only output |
| Diagnostics privacy | Five focused diagnostics tests plus source review | **PASS** - bounded replace, allowlisted context/metrics, null absent values, no private marker, export failure preserves in-memory data |
| Clipboard lease follow-up (2026-08-20) | 32 focused output tests, app-level restore-skipped persistence test, strict Clippy/check, and full all-target/all-feature rerun | **PASS** - 1,123 tests discovered; 1,108 passed, 0 failed, 15 ignored. Owner/marker/text mismatch, unavailable sequence/read error, post-close sequence capture, safe sequence churn, guarded restore, exactly-once terminal completion, history, and privacy-safe diagnostics are deterministic. One preceding aggregate run hit an unrelated 70 ms ONNX timing failure; its exact rerun and the clean aggregate rerun passed. |
| Benchmark privacy | Fifteen focused benchmark tests plus one release CLI report | **PASS** - service-boundary execution, sanitized errors, create-new output, no transcript/audio/path/stdout/stderr/raw errors in report |
| Accessibility | Exhaustive heatmap contrast test and focused diagnostics/target semantics; specialist re-review | **PASS / GO** - minimum contrast 13.18:1 light and 8.77:1 dark; visible disabled reason; semantic headings/descriptions; 44 px action |
| Native shutdown | Active-decoder ownership, bounded stuck-command recovery, panic/disconnect, and 20x concurrent last-clone tests | **PASS** - cooperative cancellation and one shared two-second exit budget; no same-process detach; hard abort prevents DLL teardown after timeout |
| Runtime/package | Pinned Windows runtime bundler and base.en service fixtures | **PASS** - v1.9.1 CPU exact package; service first load/decode 299/857 ms; warm 0/826 ms; cancellation acknowledgement 840 ms |
| Comparable final latency | 5 cold + 20 warm release runs, same 11 s JFK/base.en/CPU fixture | **PASS as measurement only** - cold 1,182/1,197 ms; warm 846/926 ms median/p95; cold RTF 0.107/0.109; warm RTF 0.077/0.084 |
| Rolling batch preview | Exact post-fix 5 cold + 20 warm run | **PASS as Experimental evidence** - cold 2,042/2,077 ms; warm 1,783/1,849 ms median/p95; warm p95 remains above the 1,200 ms target |
| Security review | Specialist review plus low-finding remediation | **PASS** - no Critical, High, or Medium finding; report path errors sanitized and private/create-new output applied |

Two release-test access-violation dialogs were observed before the bounded
shutdown fix. One pre-fix rolling run timed out. The same 25-run rolling fixture
passed afterward, but this does not substitute for a WER-monitored physical
desktop close/restart soak. That soak remains **NOT VERIFIED**.

Final handler and compatibility state:

- Logical runtime handlers: **1** (`TranscribeCppRuntime`).
- Supported models: **0**.
- Experimental models: `whisper_cpp_tiny_en`, `whisper_cpp_base_en`,
  `whisper_cpp_small_en`, and `whisper_cpp_medium_en`.
- Native streaming models: **0**. The shared preview is bounded rolling batch
  decoding, not native streaming.
- `OnnxSpeechRuntime`: omitted; the exact v1.13.4 Zipformer evidence gate is
  **NO-GO**.

The release remains **NO-GO**. Required missing evidence includes the dated
Windows rows above, desktop median/p95 phase metrics, memory/idle CPU, physical
shutdown soak, complete Supported-model compatibility suites, native-streaming
Definition of Done, and macOS/Linux fallback exercises.

## Historical tray wakeup regression checkpoint

Recorded 2026-08-05 on Windows x64. The initial repaint-callback fix failed its
physical retest. A live native probe proved the tray Show command was queued and
that one asynchronous `WM_PAINT` to the hidden root HWND immediately executed
it. Pinned eframe/winit request a redraw from the invisible window, which cannot
receive the `WM_PAINT` required to run `App::update`.

The corrected implementation captures the root HWND, posts an asynchronous
paint wake for each tray action, and uses a one-shot 40/100/500 ms Win32 timer
while hidden so capture and transcription continue to be polled. Saturation
evicts the oldest queued action and preserves the newest intent. Four focused
tray tests, formatting, all-target/all-feature checking, strict Clippy, the full
suite (533 passed, 6 environment-gated ignored), and debug plus isolated release
builds pass. Actual tray-popup clicks against the corrected release verified
Show changed the root HWND hidden -> visible and Hide changed it visible ->
hidden. Start did not retain a Stop label after 2.2 seconds and may have failed
immediately; it is not counted as verified.

UI-04 intentionally remains **NOT VERIFIED**: the operator must still hide the
window and exercise Show, Hide, Start/Stop Recording, Copy Last Transcript, and
Quit from the real Windows notification area. Record whether each command acts
exactly once, whether Show restores and focuses the primary window, whether a
hidden recording reaches a terminal state, and the hidden idle CPU percentage.

## Historical low-input diagnostic regression checkpoint

The 2026-08-05 Windows x64 no-save CPAL probe remains valid hardware evidence:
the selected FIFINE A8 delivered healthy 48 kHz stereo callbacks but near-silent
samples with maximum 10 ms mono RMS 0.001559. Its comparison against the former
0.012 RMS activation floor is historical and no longer describes speech
classification.

AI voice detection is the default and uses exact sequential Silero decisions at
its fixed default probability threshold. Its microphone meter is telemetry only.
Manual volume threshold exposes a literal `−72..0 dBFS` cutoff: each 30 ms
window below it is replaced with silence before preview, transcription, or
retained-history audio. Capture-wide maximum RMS/peak values remain diagnostic
low-input guidance. In Manual mode, no speech with healthy input recommends
lowering the input threshold; genuinely weak hardware input retains mute/gain
guidance.

REC-06 and STT-06 remain **NOT VERIFIED** until an operator corrects the
physical mute/gain state and executes both the low-input and restored-speech
rows through the real GUI/hotkey/target/output path.

## Voice-detection mode and input-meter rows

| ID | Platform | Steps | Expected result | Status |
| --- | --- | --- | --- | --- |
| MIC-01 | Win/Linux/macOS | Open Settings > Recording with a working selected microphone, select `AI voice detection`, speak softly/loudly, then stay silent. | The read-only `Microphone level` meter reflects RMS telemetry with a 60 ms attack, 120 ms peak hold, 320 ms release, and 250 ms stale-input reset. Silero decides speech; no threshold marker is shown and monitoring retains no audio or transcript artifact. | **NOT VERIFIED** - automated envelope, Meter semantics, UI, and no-retention paths pass; physical input still requires an operator. |
| MIC-02 | Win/Linux/macOS | Select `Manual volume threshold`, set `−42 dBFS`, adjust it with pointer, Left/Right, Home, and End, dictate below/equal/above the marker, then restart Scribe. | The combined bar exposes `Input threshold` with a whole-number `−72..0 dBFS` Slider. Audio below it is silenced in 30 ms windows; equality passes and the live fill switches completely to the above-threshold color. The mode and cutoff persist. The mode and threshold are disabled from microphone request through finalization, but remain editable during passive monitoring and microphone errors. | **NOT VERIFIED** - persistence, DSP, accessibility, and lockout paths are automated; acoustic boundary behavior still requires an operator. |
| MIC-03 | Win/Linux/macOS | With Settings > Recording open, begin dictation; stop, switch the selected input, start retained-audio playback, leave Recording, and return. | Idle monitoring yields before dictation/playback with no duplicate input stream. Active dictation supplies the live input meter and is not stopped by navigation. Idle monitoring follows the new device and stops outside Recording. | **NOT VERIFIED** - ownership/deferred-start tests pass; device/driver timing still requires an operator. |
| MIC-04 | Win/Linux/macOS | Navigate the mode selector and both meter states using keyboard and a screen reader in light and dark themes. | The mode selector exposes two labelled RadioButtons. AI exposes one non-live `Microphone level` Meter; Manual exposes one `Input threshold` Slider with `−72..0` values and 1 dB value actions. Continuous microphone level changes are not announced. | **PARTIAL** - automated AccessKit, pointer/keyboard interaction, and lockout assertions pass; screen-reader and visual theme checks still require an operator. |

### Voice-detection and meter implementation evidence

The current automated suite covers AI and manual mode selection,
click/drag/arrow/Home/End interaction, the combined input-threshold Slider and
read-only AI Meter AccessKit contracts, RMS meter attack/hold/release and stale
reset, meter revisions, no-retention meter capture, exact 512-sample Silero
cadence, manual 30 ms gating,
timing/pre-roll regressions, settings round-tripping, and monitor ownership
handoff. The earlier
`qa/microphone-test-final-*.png` screenshots show the superseded UI and must not
be used as current visual evidence. A fresh physical-microphone screenshot and
the manual rows above remain required.

Final repository gate on 2026-08-05: `cargo test --all-targets --all-features`
discovered 623 tests and passed 614 with 0 failures and 9 environment-gated
tests ignored. Formatting, strict Clippy, debug build, release build, source
boundary scanning, and diff hygiene also passed on the same source state.

## Foreground-aware recording overlay checkpoint

Recorded 2026-08-17 on Windows x64 using the Visual Studio 2022 developer
environment and Windows-native CMake. Automated evidence does not change any
manual row above to PASS.

| Area | Evidence | Status |
| --- | --- | --- |
| Repository gates | Final clean-head gate at `6eae6b1`: formatting, all-target/all-feature check, strict Clippy, base-to-head diff check, and Astro docs check (8 files; 0 diagnostics). The serialized suite discovered 963 tests, passed 952, failed 0, and ignored 11 explicit local runtime/fixture tests. | **PASS** |
| Presentation policy | Focused/unfocused/minimized/tray-hidden/unknown-focus, Off/Hidden/non-Windows, state preservation, and first-successful-presentation timing tests; WGC manifests record unchanged foreground HWNDs across actual presentation | **PASS automated/native fixture policy** - physical focus transitions between Scribe and arbitrary third-party apps remain NOT VERIFIED |
| Display and control separation | Distinct layered HWNDs; final WGC-visible/uncloaked 120-DPI geometry plus automated 96/120/144/192-DPI layout contracts; display `HTTRANSPARENT`, control `HTCLIENT`, both `MA_NOACTIVATE`; 320 x 52 Compact and 600 x 62 Live logical geometry; UIA reports the 44 x 44 logical control as an exact 55 x 55 desktop target at 120 DPI | **PASS native fixture contracts** - taskbar/Alt+Tab enumeration, physical underlying-app clicks, and the full mixed-DPI matrix remain NOT VERIFIED |
| Discard lifecycle | Session-correlated pending/active discard, stale action rejection, startup cancellation, app-owned worker draining, capture-admission blocking, preview cancellation, audio release, and zero transcript/history/output tests | **PASS automated lifecycle** - physical microphone startup/driver shutdown remains NOT VERIFIED |
| Preview and accessibility | Grapheme-safe one-rendered-row tail, committed/tentative styling, bounded notices, exclusive foreground/background live-region ownership, exact cancel name/tooltip, static elapsed semantics in both modes, and DPI-derived physical bounds for root/status/meter/elapsed/preview/announcement/button. The native probe confirms ElementFromPoint resolves the X and a forced post-AccessKit Verify failure leaves both HWNDs hidden with empty UIA subtrees. | **PASS automated/native provider semantics** - Narrator/Accessibility Insights speech and tooltip dwell remain NOT VERIFIED |
| Native visual fidelity | Hardware `Windows.Graphics.Capture` output from exact head `6d5492c` for Live/Compact in light/dark at 120 DPI, automated scaling contracts at 96/120/144/192 DPI, plus the rebuilt same-state source comparison in `design-qa-evidence/overlay-native/` | **PASS deterministic native visual gate** - no missing display, black X tile, seam, clipping, or focus change; painted translucency intentionally omits native blur |
| Security review | Specialist review of external-target isolation, native hardening profiles, stale action handling, cancellation ordering, privacy, and configuration compatibility | **PASS / GO** - no reportable security findings |

Reproduce the deterministic overlay gate from the repository root with:

```powershell
& 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\Tools\Launch-VsDevShell.ps1' -Arch amd64 -SkipAutomaticLocation
$env:CMAKE = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
$env:CARGO_TARGET_DIR = "$PWD\target\native-layered"
cargo fmt --all --check
cargo test --all-features overlay::native_windows --no-fail-fast
cargo test --all-features overlay::view --no-fail-fast
cargo test --all-features ui::theme::tests --no-fail-fast
cargo clippy --all-targets --all-features -- -D warnings
```

The exact fixture-launch and WGC command shape is documented in
`docs/UI_HARNESS.md`. The remaining release gate is the physical Windows
execution of UI-06, UI-08, UI-10, UI-11, REC-04, and the applicable
output-target rows. Capture a screen recording plus foreground-window evidence;
message-level tests and deterministic WGC fixtures do not prove physical
click-through into every underlying app, taskbar/Alt+Tab exclusion,
screen-reader speech, real microphone behavior, or the full monitor/DPI matrix.
