# Scribe manual test matrix

**Status:** living Phase 7 matrix (2026-08-04). No manual desktop, microphone,
model-runtime, tray, hotkey, overlay, accessibility, or paste test was executed
during the Phase 0-7 automated work. Every manual row below therefore remains **NOT VERIFIED** until
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

## Automated baseline (verified in Phase 0)

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

## Automated Phase 1 checkpoint

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

## Automated Phase 2 checkpoint

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

## Automated Phase 3 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 252 discovered, 247 passed, 0 failed, 5 ignored environment-required tests |
| Normalized catalog | Catalog validation, evidence-link/hashed-receipt binding, role-gating, malformed-artifact, duplicate-ID, capability-intersection, minimum-runtime enforcement, and ID-prefix independence tests | PASS - four primary descriptors, all Experimental, zero curated roles |
| Architecture boundary | Rust source-boundary test plus `wsl.exe python3 scripts/check-catalog-boundaries.py` | PASS - one logical handler; neutral production UI including Playground; family-coded quick actions/IDs rejected; legacy provider and concrete adapter selection confined to its private bridge |
| Release build/package | `cargo build --release --all-features`; verified PowerShell package script | PASS |
| Release primary fixture | ignored exact service JFK smoke with pinned v1.9.1/base.en/JFK paths | PASS - cold load 290 ms, first decode 791 ms, retained decode 780 ms; explicit unload/reload passed |
| Exact Zipformer candidate | Fail-closed machine-readable evidence gate | **NO-GO** - no v1.13.4 native package/model pins, first-partial/comparator, corpus WER, <=250 ms cancel, lifecycle/crash/memory, or platform evidence; no second handler shipped |

These automated results verify catalog truthfulness and retain the Phase 2
primary vertical runtime slice. They do not promote a model, prove live desktop
behavior, or satisfy native streaming. All manual rows remain NOT VERIFIED.

## Automated Phase 4 checkpoint

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

## Automated Phase 5 checkpoint

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

## Automated Phase 6 checkpoint

| Check | Command | Result |
| --- | --- | --- |
| Format/check/lint/build | `cargo fmt --all -- --check`; `cargo check --all-targets --all-features`; strict Clippy; `cargo build --all-features` | PASS |
| Unit/integration tests | `cargo test --all-targets --all-features` | PASS - 358 discovered, 353 passed, 0 failed, 5 environment-required tests ignored |
| Native capture/DSP | SPSC FIFO/wrap/concurrency/overflow; conversion/downmix/resample/normalization; 25 Hz RMS/peak; adaptive VAD, timing, pre/post-roll, no-speech, and disabled-VAD tests | PASS automated paths; physical microphones and driver timing remain NOT VERIFIED |
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

## Automated Phase 7 checkpoint

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
All desktop rows remain NOT VERIFIED. Four catalog models
remain Experimental, zero are Supported, native streaming remains false, and
the Zipformer/second-handler decision remains NO-GO.

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
| UI-06 | Windows | P1, P5 | Start dictation with the target on each monitor and with mixed display scaling; repeat near each work-area edge. | The pre-created overlay appears in the selected top/bottom position within the captured target monitor's work area, uses appropriate physical sizing, and never steals target focus. | **NOT VERIFIED** |
| UI-07 | Win/Linux/macOS | P1, P3 | Open Models and inspect every visible card, search, device choice, and runtime-maintenance row. | Exactly the four normalized primary entries are visible; each has an Experimental text cue/reason, CPU-only capability, no backend/family filter or badge, and no curated role. Existing legacy paths/files remain untouched. | **NOT VERIFIED** |
| UI-08 | Windows | P1, P2, P4 | With overlay Live, begin dictation from another app; verify taskbar, Alt+Tab, mouse interaction through the overlay, and original target focus. Repeat in Minimal and Off. | The overlay has no taskbar/Alt+Tab entry, is always on top without activation, is mouse-pass-through, and does not redirect keyboard input. Live shows real phase/level/text, Minimal is compact, and Off stays hidden. | **NOT VERIFIED** |
| UI-09 | Linux/macOS | P1, P2 | Select Live or Minimal and begin dictation. | Until a native no-focus adapter exists, the effective overlay remains Off and the UI explains the conservative limitation; no focus-stealing window appears. | **NOT VERIFIED** |
| UI-10 | Windows | P1, P2, P6 | Navigate all main pages and controls by keyboard, inspect visible focus, run a screen reader while overlay state changes, and enable the OS reduced-motion setting. | All controls are reachable and labeled, focus is visible, state is not conveyed by color alone, primary app controls are at least 44 px, overlay announcements are polite, and no disallowed motion occurs. | **NOT VERIFIED** |
| UI-11 | Windows | P1, P2 | Start capture and speak softly/loudly, then stay silent; compare overlay meter with input. | The meter reflects distinct native RMS and peak values at the 25 Hz worker cadence, clamps safely, and displays no fabricated activity. Record first-meter latency. | **NOT VERIFIED** |

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
| REC-04 | Win/Linux/macOS | P1, P2 | Cancel/stop during capture before speech and during finalization; immediately start another normal or Playground session; repeat once through tray Quit. | Cancel never pastes; in-memory PCM is released; stale completion cannot overwrite the next session; explicit stop wins if inferred endpoint/max occurs at the same boundary. Coordinator/ownership paths are automated, but the live race still requires this check. | **NOT VERIFIED** |
| REC-05 | Win/Linux/macOS | P1, P2 | In Toggle mode with defaults, begin in silence, speak after at least 250 ms, pause for 450 ms and resume, then finish with at least 900 ms silence. Repeat in Hold-to-talk, with VAD disabled, and with an immediate shortcut stop. | First syllable is retained by pre-roll; 450 ms pause does not finalize; 900 ms silence endpoints only in Toggle mode; Hold-to-talk waits for release; VAD disabled never endpoints; explicit stop retains about 200 ms post-roll and outranks inferred silence. Record actual timings and audio evidence only with consent. | **NOT VERIFIED** |

## Runtime, model, and transcription flows

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| STT-01 | Win/Linux/macOS | P1, P2, P3, P4 | Install/select the known-good model; transcribe fixture through the normal flow. | Non-empty final transcript appears, backend/model identity is visible in diagnostics, and exactly one finalized output is produced. | **NOT VERIFIED** |
| STT-02 | Win/Linux/macOS | P1, P2, P3 | Run the same WAV in Playground for every installed/ready model. | Each selected ready model completes independently; missing runtime/model blocks only that card with repair guidance. | **NOT VERIFIED** |
| STT-03 | Win/Linux/macOS | P1, P2, P3 | Run a second transcription immediately, then after the model has been idle for more than five minutes. | Windows primary runtime reports immediate warm reuse; the dedicated worker unloads after the five-minute idle timeout and the next request reloads. Other platforms preserve the compatibility path. Record load/decode metrics. | **NOT VERIFIED** |
| STT-04 | Win/Linux/macOS | P1, P2, P3 | Attempt transcription with a missing runtime, missing model file, and incomplete model directory. | No child process is started; actionable status identifies what to install/repair; no paste occurs. | **NOT VERIFIED** |
| STT-05 | Win/Linux/macOS | P1, P2, P3 | Kill the short-lived runtime process or force a non-zero exit during transcription. | Failure is surfaced, app returns to Idle/Error, and retry is safe; no stale result is applied. | **NOT VERIFIED** |
| STT-06 | Win/Linux/macOS | P1, P2 | Speak silence/noise only until endpoint or stop. | Empty/no-speech result is handled without empty paste; status explains the outcome. | **NOT VERIFIED** |
| STT-07 | Win/Linux/macOS | P1, P2, P3 | In Advanced, run Auto, Rolling preview, and Final text only against the same utterance; repeat once in Playground. Capture the committed/tentative overlay states and first-partial latency. | Auto and Rolling use bounded batch preview only for the primary native model; Final text only and Playground emit no partials. Tentative text stays in the overlay, corrections do not backspace another app, and the final result replaces the preview once. No model advertises native streaming. | **NOT VERIFIED** |
| STT-08 | Win/Linux/macOS | P1, P2, P3 | Change Auto/GPU/CPU-only acceleration preference where supported; run fixture on each available mode. | Auto resolves to a health-validated device, explicit CPU is honored, and unavailable GPU fails clearly without silent fallback. Record resolved backend/device and errors. | **NOT VERIFIED** |
| STT-09 | Windows | P1, P3 | Record the exact sherpa-onnx v1.13.4 and streaming Zipformer prerequisites; attempt the evidence harness only after a native package, pinned model, shared corpus, and Phase 7 comparator exist. | Any missing measurement remains NO-GO. A second logical handler appears only if every first-partial, 30% improvement, RTF, cancellation, WER, lifecycle, crash, memory, and platform threshold passes. | **NOT VERIFIED / CURRENT NO-GO** |

## Output, clipboard, and target safety

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| OUT-01 | Windows | P1, P2, P3, P4, P7 | Put sentinel text on clipboard; focus browser/editor; run HK-01 with auto-insert enabled. | Final text is pasted once into the original field and clipboard is restored when configured. Verify target text and clipboard sentinel manually. | **NOT VERIFIED** |
| OUT-02 | Linux X11 | P1, P2, P3, P4, P6, P7 | Repeat OUT-01 on X11. | Clipboard + paste automation works where Enigo permits; capture output status and clipboard before/after. | **NOT VERIFIED** |
| OUT-03 | Linux Wayland | P1, P2, P3, P4, P6, P7 | Repeat OUT-01 on Wayland. | If synthetic paste is blocked, final text is copied only with an explicit notice; no paste into an unrelated field. | **NOT VERIFIED** |
| OUT-04 | macOS | P1, P2, P3, P4, P6, P7 | Grant Accessibility, repeat OUT-01; revoke permission and repeat. | Granted path inserts once; denied path falls back to clipboard/actionable error without focus theft. | **NOT VERIFIED** |
| OUT-05 | Win/Linux/macOS | P1, P2, P3, P4 | Begin dictation in target A, switch focus to unrelated target B before completion. | On Windows, no synthetic key is sent and final text remains copied because the captured target no longer matches. On Linux/macOS, current behavior is conservatively copy-only. Never accept text pasted into B as PASS. | **NOT VERIFIED** |
| OUT-06 | Win/Linux/macOS | P1, P2, P3, P4 | Close target app during finalization; then change clipboard from another app before restoration delay expires. | No unrelated paste; final text is recoverable from clipboard/status; restoration does not overwrite an independently changed clipboard. | **NOT VERIFIED** |
| OUT-07 | Win/Linux/macOS | P1, P2, P3 | Disable auto-insert; transcribe fixture and use Copy Transcript. | No synthetic key input occurs; explicit copy places the final transcript on clipboard. | **NOT VERIFIED** |

## Downloads, install, settings, privacy, and recovery

| ID | Platform | Prereq | Steps | Expected result/evidence | Status |
| --- | --- | --- | --- | --- | --- |
| DL-01 | Win/Linux/macOS | P1, disposable data dir, network | Start a recommended model download; cancel halfway; resume. | Progress is truthful; partial artifact is retained/resumable or the UI explains why resume is unavailable. Whisper files already require exact size/SHA-256; resumable/atomic activation remains Phase 9 work. | **NOT VERIFIED** |
| DL-02 | Win/Linux/macOS | P1, disposable data dir, network | Replace a downloaded artifact with a truncated file; attempt activation. | Corrupt/incomplete model is rejected and never shown as runnable. | **NOT VERIFIED** |
| DL-03 | Win/Linux/macOS | P1, disposable data dir, network | Interrupt extraction/installation; restart app. | Staging is cleaned or safely quarantined; previous known-good install remains usable. | **NOT VERIFIED** |
| DL-04 | Win/Linux/macOS | P1, disposable data dir | Install runtime, remove it while no transcription is running, then try a model. | Runtime status transitions are visible; removal does not delete unrelated models; repair action is offered. | **NOT VERIFIED** |
| SET-01 | Win/Linux/macOS | P1, P7 | Edit hotkey, recording mode, active model, microphone, performance mode, theme, and auto-insert; restart. | Values persist with no silent reset; invalid values are rejected or salvaged. | **NOT VERIFIED** |
| SET-02 | Win/Linux/macOS | P1, P7 | Copy the config aside, corrupt one field/file, launch, then inspect backup/recovery. Repeat with a valid legacy flat config. | Valid fields survive; corrupt input receives a timestamped backup; legacy input receives a pre-migration backup; future root/section/install fields survive the rewrite. | **NOT VERIFIED** |
| PRIV-01 | Win/Linux/macOS | P1, P7 | Inspect current data directory after successful, no-speech, overflow, and failed transcription. | Normal microphone and Playground paths create no WAV and release in-memory PCM after consumers finish. Only stale WAVs from older builds or the private transitional compatibility bridge may exist; durable history/audio retention is a future feature. Record actual files without exposing transcript content. | **NOT VERIFIED** |
| PRIV-02 | Win/Linux/macOS | P1, P7 | If a later build exposes history modes, exercise Off, Transcript only, and Transcript + audio; pin one entry and clear unpinned. | Audio is off by default, pinned entries survive cleanup, and delete-audio does not delete transcript metadata. | **NOT VERIFIED** |
| RECOV-01 | Win/Linux/macOS | P1, P2, P3 | Force microphone, runtime, output, and config failures one at a time; start a fresh dictation after each. | Each failure has a user-facing stage/message, no stale result leaks into the next run, and Idle is recoverable. | **NOT VERIFIED** |
| RECOV-02 | Win/Linux/macOS | P1, P3 | If retry/history exists, fail a transcription with retained audio then retry. | Retry uses the retained recording and does not create duplicate output/history entries. | **NOT VERIFIED** |

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

### Completion rule

This living matrix remains valid only while each implemented phase updates its
automated checkpoint and any affected manual steps. Platform/release sign-off
requires a dated PASS/FAIL/BLOCKED result with evidence in the owning test
report. Do not change a manual row to PASS based solely on compilation or a unit
test. Rows that cannot be run on a platform remain **NOT VERIFIED** and are
listed as release risks until an explicit support decision is made.
