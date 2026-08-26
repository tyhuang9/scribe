# Scribe technical overview

This guide is the engineering companion to the end-user [README](../README.md). It describes the current checked-in design at a high level; detailed release and verification claims belong in the linked implementation records.

## Architecture

```text
egui UI / tray / global hotkey
            |
            v
 session coordination and settings
            |
            +--> cpal microphone capture
            |       -> bounded audio preparation, metering, VAD, endpointing
            |
            v
  TranscriptionService (runtime-neutral interface)
            |
            v
 InferenceWorkerSupervisor
            |
            v  private anonymous stdin/stdout pipes (SCIF v3)
 hidden child: Scribe --scribe-inference-worker
            |
            +--> worker-local RuntimeRouter
            |       +--> transcribe-cpp GGUF
            |       +--> legacy GGML compatibility
            |       +--> sherpa-onnx models
            |
 separate child: Scribe --scribe-vad-worker
        (VAD only; never an STT runtime)
            |
            v
 final transcript -> overlay, clipboard, optional Windows insertion, history
```

The UI does not select or invoke a runtime directly. The application-facing `TranscriptionService` owns only the runtime-neutral dispatch boundary and worker supervisor in the desktop process. Native GGUF/GGML model objects, transcribe-cpp sessions, sherpa-onnx recognizers, and native FFI handles are constructed only by the persistent hidden `--scribe-inference-worker` child. The desktop process never constructs those native objects.

The inference child is the single persistent STT process: it directly owns the worker-local router and all three native runtime families (GGUF, legacy GGML, and sherpa-onnx). VAD is intentionally a separate worker instance launched with `--scribe-vad-worker`; it has no STT commands. The two workers communicate only through private anonymous stdin/stdout pipes using the SCIF v3 framed protocol. Worker stdout is protocol-only and diagnostics go to stderr. There is no localhost listener, TCP/HTTP inference transport, or nested ONNX worker.

Normal GGUF remains local, native, CPU-only in the currently verified package, and Python-free, but it is now process-isolated rather than in-process. The exceptional verified legacy CLI remains an external fallback only after native bootstrap failure and CLI hash verification. Installation smoke runs in a fresh disposable worker process and is not the normal dictation worker.

## Data, output, and privacy boundaries

- Microphone PCM is processed in bounded native memory; a routine dictation session does not create a WAV file.
- Settings are stored locally as versioned JSON with migration and atomic replacement behavior.
- History uses a local SQLite store and is controlled through the product’s retention settings.
- Only finalized transcript text can reach copy, history, the overlay, or optional Windows focused-app insertion. Insertion is disabled by default and fails back to the clipboard when safety checks cannot preserve the original target.

## Models and installation

The normal **Models** experience accepts trusted GGUF installations and compatible local GGUF imports. Managed downloads use pinned source facts, resumable transfers, size and SHA-256 checks, a native smoke test, and atomic activation. Local imports remain in place: Scribe fingerprints and validates them without copying, uploading, moving, or deleting the source file.

All normal catalog models are currently Experimental. The exact catalog, runtime details, compatibility boundaries, and known legacy migration paths are maintained in the [embedded STT and models record](EMBEDDED_STT_AND_MODELS.md).

## Platforms and release status

Windows x64 is the current release target. Linux and macOS retain source-build paths but are not release-qualified. Global hotkeys are opt-in on Linux, and output is clipboard-only outside Windows. macOS can require Microphone and Input Monitoring permissions.

Do not promote an implemented code path to a release-support claim without the required manual evidence. Consult these records before changing a runtime, package, model claim, or platform-support statement:

- [Project status](https://tyhuang9.github.io/scribe/project-status/)
- [Embedded STT and model management](EMBEDDED_STT_AND_MODELS.md)
- [Runtime-consolidated implementation report](SCRIBE_REVAMP_IMPLEMENTATION_REPORT.md)
- [Manual test matrix](MANUAL_TEST_MATRIX.md)

## Contributor checks

Run the relevant verification for the change you make. The baseline application checks are:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --all-features
```

The documentation site lives in `website/` and is checked independently:

```bash
cd website
npm ci
npm run check
npm run build
```

Runtime, packaging, model-catalog, and platform changes have additional evidence requirements. Follow the linked records rather than treating these baseline commands as release qualification.
