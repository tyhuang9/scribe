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
 InferenceWorkerRegistry (release CPU route; opt-in Vulkan dev route)
            |
            v  private anonymous stdin/stdout pipes (SCIF v5)
 dedicated child: scribe-inference-worker --scribe-inference-worker
            |
            +--> worker-local RuntimeRouter
            |       +--> transcribe-cpp GGUF
            |       +--> native Sherpa ONNX receipt-backed bundles
            |
 separate child: local-transcriber --scribe-vad-worker
        (VAD only; never an STT runtime)
            |
            v
 final transcript -> overlay, clipboard, optional Windows insertion, history
```

The UI does not select or invoke a runtime directly. The application-facing `TranscriptionService` owns only the runtime-neutral dispatch boundary and worker supervisor in the desktop process. Native GGUF model objects, transcribe-cpp sessions, Sherpa ONNX recognizers, and native FFI handles are constructed only by the persistent hidden `--scribe-inference-worker` child. The desktop process never constructs those native objects.

The adjacent `scribe-inference-worker` executable is the single persistent STT process: it directly owns the worker-local router and both native runtime families (GGUF and receipt-backed Sherpa ONNX). Before every launch the desktop verifies the exact canonical sibling, rejects links/reparse points, hardlinks, ADS/case aliases and out-of-root paths, and compares the file identity and SHA-256 against the release-compiled trust anchor. Release builds fail closed if that worker-first SHA-256 anchor is absent. VAD intentionally remains a separate instance of the desktop executable launched with `--scribe-vad-worker`; it has no STT commands. The workers communicate only through private anonymous stdin/stdout pipes using SCIF v5. Hello is the first command and occurs exactly once; its capability exchange binds a fresh random challenge, immutable desktop and role-specific worker build revisions, the worker image SHA-256, ABI, role, compiled provider, and supported artifact targets to the process generation. The child validates the build/protocol expectation; the parent exclusively owns exact-path, file-identity, and final-image digest verification and validates the digest echoed by the child. Worker stdout is protocol-only and diagnostics go to stderr. Worker launch clears the environment and restores only required OS variables, and Windows DLL loading is restricted before native runtime initialization. There is no localhost listener, TCP/HTTP inference transport, nested ONNX worker, Python runtime, dynamic runtime package, or CLI fallback.

Official Stage 2 packaging contains only the CPU route. `Auto` and `CPU` use it; explicit `GPU` returns a clear error and cannot silently fall back to CPU. Windows developers may build an adjacent Vulkan worker and matching desktop with `scripts/build-vulkan-worker-dev.ps1`; that opt-in build advertises Vulkan, permits explicit GPU for GGUF, and is not a release package or an Auto-qualification claim. CUDA, Metal, signed pack manifests, health quarantine, and GPU Auto qualification remain later stages. Unix release worker launch is intentionally unsupported until process creation can be bound to the already verified executable descriptor; source/debug behavior remains available for development only.

Normal GGUF remains local, native, CPU-only, and Python-free, but it is process-isolated rather than in-process. Receipt-backed ONNX inference uses the same private persistent STT child. Installation smoke runs in a fresh disposable worker process and is not the normal dictation worker.

## Data, output, and privacy boundaries

- Microphone PCM is processed in bounded native memory; a routine dictation session does not create a WAV file.
- Settings are stored locally as versioned JSON with migration and atomic replacement behavior.
- History uses a local SQLite store and is controlled through the product’s retention settings.
- Only finalized transcript text can reach copy, history, the overlay, or optional Windows focused-app insertion. Insertion is disabled by default and fails back to the clipboard when safety checks cannot preserve the original target.

## Models and installation

The normal **Models** experience exposes five checked-in Experimental entries: `whisper_cpp_tiny_en`, `whisper_cpp_base_en`, `whisper_cpp_small_en`, `whisper_cpp_medium_en`, and the receipt-backed `moonshine-tiny-en-int8-onnx` bundle. The four Whisper entries use static pinned GGUF artifacts through `transcribe-cpp`; Moonshine uses native Sherpa ONNX. Managed downloads use pinned source facts, resumable transfers, exact size and SHA-256 checks, a native subprocess smoke test, and atomic activation. Moonshine is CPU-only and final-text-only. Local GGUF imports remain in place: Scribe fingerprints and validates them without copying, uploading, moving, or deleting the source file. Legacy user files are preserved but are no longer recognized or executed by a production inference route.

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

On Windows, all-feature checks require the pinned Vulkan SDK. They compile the
developer backend but do not create a release bundle. Use
`scripts/build-windows-release.ps1` for the official CPU-only package, or
`scripts/build-vulkan-worker-dev.ps1` for adjacent debug Vulkan binaries.

The documentation site lives in `website/` and is checked independently:

```bash
cd website
npm ci
npm run check
npm run build
```

Runtime, packaging, model-catalog, and platform changes have additional evidence requirements. Follow the linked records rather than treating these baseline commands as release qualification.
