# Scribe runtime-consolidated revamp: implementation report

> **Historical record (superseded 2026-08-28):** This report preserves the
> original dated implementation evidence verbatim. It is not the current
> architecture or qualification contract. Use the checked-in source,
> [technical overview](TECHNICAL_OVERVIEW.md), and live
> [manual test matrix](MANUAL_TEST_MATRIX.md) for current truth.

Initial report date: 2026-08-04
Evidence updated through: 2026-08-05
Integration base: `536a85f`
Final stacked implementation branch: `revamp/phase-11-diagnostics-hardening`
Release decision: **NO-GO pending manual and compatibility evidence**

## Delivered architecture

The application-facing path is runtime neutral:

```text
egui / coordinator / history / output
                 |
        TranscriptionService
                 |
           RuntimeRouter
                 |
      TranscribeCppRuntime
```

The final application contains exactly **one logical runtime handler**:
`TranscribeCppRuntime`. `RuntimeRouter` is the only application-level component
that selects it. `OnnxSpeechRuntime` was not added because the named
sherpa-onnx v1.13.4 Zipformer candidate lacks the complete native-package,
shared-corpus, WER, lifecycle, crash, memory, cancellation, platform, and
first-partial evidence required by the gate. This is a concrete NO-GO, not
simulated streaming support.

`TranscriptionService` owns model resolution, dedicated worker lifecycle,
cancellation, sequencing, streaming/final requests, and the private router.
The UI, session coordinator, recording, settings, history, output, model views,
and benchmark path use only runtime-neutral IDs, descriptors, capabilities,
options, updates, results, and diagnostics.

Microphone PCM remains in Rust. A fixed-capacity SPSC ring separates the CPAL
callback from the native preparation worker. Downmixing, 16 kHz resampling,
normalization, levels, VAD, endpointing, pre/post-roll, rolling snapshots, and
final prepared audio never cross a React, JavaScript, webview, or general UI IPC
boundary. Tentative text stays in the native overlay; the finalized transcript
is passed once through the target-safe output path.

## Runtime and model compatibility

Primary runtime pin:

- Package: whisper.cpp v1.9.1
- Source commit: `f049fff95a089aa9969deb009cdd4892b3e74916`
- Verified package in this run: Windows x64 CPU, exact 13-file allowlist plus
  manifest
- Resolved acceleration: `Auto -> CPU`; explicit CPU is available; GPU is
  unavailable because no verified accelerator package ships
- Native streaming capability: false
- Preview mode: shared bounded rolling batch preview, not a second handler

Verified **Supported** models: **0**.

The normalized catalog exposes these models as **Experimental**:

| Model ID | Role/capability state | Current evidence |
| --- | --- | --- |
| `whisper_cpp_tiny_en` | Experimental; batch plus shared rolling preview | Historical CPU JFK smoke only |
| `whisper_cpp_base_en` | Experimental; batch plus shared rolling preview | Current Windows x64 CPU load/decode/rolling/cancel evidence |
| `whisper_cpp_small_en` | Experimental; batch plus shared rolling preview | Historical CPU JFK smoke only |
| `whisper_cpp_medium_en` | Experimental; batch plus shared rolling preview | Historical CPU JFK smoke only |

The earlier tiny/base/small/medium figures of 644/1,205/3,980/11,857 ms are
process/load/transcribe smoke observations, not end-to-end latency or Supported
status. None of the four models has passed the complete acceleration, platform,
crash-recovery, memory, unload/reload, cancellation, and common fixture suite.

Legacy faster-whisper, Vosk, offline sherpa-onnx, Moonshine, and Parakeet
download/selection paths are not part of normalized application routing or
release packaging. Unreachable runner-download helpers and their model-family
URL branches were removed. Private legacy adapters, configuration aliases,
development scripts, and existing user artifacts are preserved for migration
compatibility; they do not create logical runtime handlers or support claims.

## Working vertical slice

The implemented native vertical slice is:

```text
global shortcut
-> capture original target
-> show pre-created non-focusable overlay
-> begin capture and model load concurrently
-> publish throttled audio levels
-> produce committed/tentative rolling text
-> stop/endpoint and run the complete final utterance
-> revalidate the target and paste the final text once, or copy safely
-> save history according to privacy settings
```

The authoritative coordinator rejects illegal transitions, stale session IDs,
and out-of-order request sequences. Explicit stop outranks inferred silence.
The final pass is immutable once accepted. Cancellation and no-speech produce no
paste. Windows revalidates HWND, PID, process creation identity, foreground
activation, and clipboard generation before one synthetic paste. Unsafe target
or paste failures and unavailable, unsupported, or erroneous clipboard
snapshots become explicit copy-only output. If another app changes the
clipboard during the transaction, Scribe sends no synthetic keys, does not
overwrite that newer clipboard content, and keeps the final text in Scribe.
Linux and macOS remain conservatively copy-only.

Settings are versioned and sectioned, preserve unknown fields, salvage valid
fields, back up corrupt/legacy input, and use debounced atomic replacement.
History uses bundled SQLite with Pending/Completed/Failed lifecycle, keyset
pagination/search, pin/delete, optional bounded audio, explicit fresh-target
repaste, retry only when audio exists, retention, and startup reconciliation.
Defaults are transcript-only, at most 20 unpinned entries, audio Off, and coarse
application identity Off.

## Diagnostics, hardening, and privacy

Scribe retains at most 50 allowlisted session-diagnostic records in memory.
Records contain neutral model/backend/capability context, phase durations,
outcome, and failure stage. They contain no transcript, audio, clipboard data,
target title, process path, source/output path, secret, or raw error chain.
Explicit export uses create-new semantics, synchronizes the file, and applies
owner-only mode on Unix; Windows inherits the selected directory ACL.

The local `--benchmark` command executes through `TranscriptionService` and
emits allowlisted JSON containing platform/CPU, neutral model and resolved
backend, capability/streaming configuration, audio duration, and timings. It
omits transcript/audio samples, paths, stdout/stderr, and raw runtime errors and
refuses to overwrite an existing report.

During Phase 11, two Windows access-violation dialogs were observed in release
test executables. The cause was a shutdown race: rolling-preview and runtime
worker `Drop` implementations could time out or lose a `try_send`, detach a
native decoder/runtime thread, and allow DLL/process teardown while that thread
was still running. Shutdown now closes admission, requests cooperative
cancellation, and bounds command admission, unload acknowledgement, and worker
completion within one two-second process-exit budget. Completed and panicked
workers are joined. A worker still live at the deadline triggers an immediate
hard abort, which skips Rust/DLL teardown; it is never detached and cannot keep
the UI hanging indefinitely. Deterministic regressions cover active decoder
ownership, stuck command deadlines, panic/disconnect, and concurrent last-owner
drops. The exact release rolling fixture then completed all 25 iterations. A
prolonged physical Windows desktop shutdown/restart soak remains required before
GO.

Automated architecture guards assert the one-handler count, router-private
selection, runtime/model-family-neutral UI, family-logic allowlist, native-only
PCM shape, absence of webview/JS/IPC audio transport, and final-only output.

## Measured latency

All comparable figures use the same Windows machine, 11-second JFK fixture,
`whisper_cpp_base_en` artifact, and resolved CPU backend. Median/p95 are shown.

| Measurement | Phase 0 before | Phase 11 after | Change |
| --- | ---: | ---: | ---: |
| Cold process/load/transcribe total | 1,282.8 / 1,333.1 ms | 1,182 / 1,197 ms | -7.9% / -10.2% |
| Repeated process total vs retained warm total | 1,279.5 / 1,452.8 ms | 846 / 926 ms | -33.9% / -36.3% |
| Cold model load | not separated comparably | 308 / 330 ms | no claim |
| Warm model load | repeated process included load | 0 / 0 ms | retained-model observation |
| Cold real-time factor | not separately recorded | 0.107 / 0.109 | no claim |
| Warm real-time factor | not separately recorded | 0.077 / 0.084 | no claim |
| Rolling preview, cold | not available | 2,042 / 2,077 ms | no claim |
| Rolling preview, warm | not available | 1,783 / 1,849 ms | no claim |

The retained-warm comparison includes both eliminating repeated process setup
and retaining the loaded model. It is not an accuracy or end-to-end desktop
claim. One final CLI privacy-report smoke recorded 1 ms preparation, 403 ms
model load, 838 ms backend execution, and 1,243 ms total; a single run is not a
median/p95 result.

Desktop hotkey-to-overlay, hotkey-to-capture, first-meter, first-partial,
stop-to-final, final-to-paste, total session duration, memory, and idle CPU are
instrumented but **not verified on a real desktop session**. Diagnostics will
record them during the required manual run. No fabricated number is reported.

## Verification result

The final automated gate covers formatting, all-target/all-feature check,
strict Clippy, all-target/all-feature tests, debug and release builds, the Rust
and Python architecture boundaries, release runtime packaging, primary service
fixture, cancellation, rolling preview, diagnostics privacy, benchmark privacy,
accessibility contrast/semantics, and source diff hygiene. Exact final command
results are recorded in `docs/SCRIBE_REVAMP.md` and
`docs/MANUAL_TEST_MATRIX.md`.

The final full suite discovered 623 tests: 614 passed, 0 failed, and 9
environment-gated tests were ignored.

Specialist reviews found no Critical, High, or Medium security issue. The
accessibility re-review is GO, with measured heatmap contrast floors of 13.18:1
in light mode and 8.77:1 in dark mode. CodeRabbit reviewed earlier stacked
snapshots and its findings were addressed; its CLI was unavailable for a
dedicated final-snapshot rerun. Local specialist, compiler, test, security,
accessibility, performance, and final integration review cover the final tree.

## Remaining release gates

Implementation phases are complete, but release is **NO-GO** until all of the
following have saved evidence:

1. The dated Windows 11 manual matrix, including real shortcut, microphone,
   pre-created no-focus overlay, target identity, clipboard race, exactly-once
   paste, history/privacy, install recovery, multi-monitor/DPI, device restart,
   and standard/elevated target cases.
2. A physical desktop shutdown/restart soak confirming that the native worker
   join fix eliminates the observed access violation.
3. Comparable desktop median/p95 latency, memory, and idle-CPU measurements.
4. The complete compatibility suite before any model becomes Supported.
5. The native-streaming Definition of Done. The absent second handler remains a
   truthful NO-GO unless the exact Zipformer evidence gate passes.
6. Conservative macOS/Linux build and copy-only fallback exercises where those
   environments are available.

No unverified model, runtime, platform behavior, latency improvement, or native
streaming capability is labelled Supported.
