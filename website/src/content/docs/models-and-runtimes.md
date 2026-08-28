---
title: Models and runtimes
description: Understand local model selection, runtime discovery, and storage.
---

## Current catalog and status

The normal Models experience always includes five static catalog entries:
Experimental GGUF models `whisper_cpp_tiny_en`, `whisper_cpp_base_en`,
`whisper_cpp_small_en`, and `whisper_cpp_medium_en`, plus the receipt-backed
Experimental `moonshine-tiny-en-int8-onnx` bundle. The GGUF artifacts are
pinned by repository revision, filename, size, and SHA-256; the ONNX model is
installed only as its verified receipt-backed bundle.

When a trusted catalog response is available, Models can also discover and
display additional non-duplicate remote GGUF variants. Those variants are not
bundled, are not static catalog entries, and may be unavailable. A remote
listing becomes usable only after its exact source facts are verified and its
artifact is installed; listing it does not authorize execution or make it
Supported.

The five static entries are all **Experimental** and the Supported count is
**zero**. A model must pass load, fixture transcription, cancellation,
unload/reload, acceleration, and platform checks before promotion. You can also
validate a local GGUF in place; Scribe fingerprints and smoke-tests it without
copying, uploading, or deleting it.

Legacy user configuration and artifact files are preserved for migration, but
they are no longer recognized or executed by a production inference route. An
installer does not need to remove those files for the current catalog to work.

## One runtime boundary

The application has one logical runtime kind. Only the private `RuntimeRouter` selects it; UI, history, output, settings, and model management use runtime-neutral contracts. Model-format and compatibility distinctions stay below that boundary instead of becoming application handlers.

Normal GGUF inference uses the safe `transcribe-cpp` 0.1.3 API with a statically
linked CPU backend in Scribe's private persistent inference child. The
receipt-backed ONNX bundle uses native Sherpa ONNX in that same child. Neither
path requires Python, a localhost service, a dynamic runtime package, a
GGML/DLL route, or a CLI fallback. Models can remain loaded there for warm reuse.

The desktop process uses runtime-neutral contracts; only the private inference
child owns concrete model and recognizer state. VAD has a separate production
worker/process path and is not an STT runtime.

## Storage and performance

Managed model files live under Scribe's app-data `models` directory. Trusted installs use pinned manifests, resumable partials, exact size/hash checks, safe staging, native smoke tests, and atomic activation. Local imports remain at their source path and are rechecked against their stored fingerprint.

The embedded GGUF adapter is CPU-only. `Auto` resolves to CPU, `CPU` requests it explicitly, and `GPU` reports that no verified accelerator is available. Linux and macOS can compile conservative app fallbacks, but their desktop/model combinations are not release-qualified.

For packaging, model-validation, and benchmark details, consult the repository’s [technical overview](https://github.com/tyhuang9/scribe/blob/main/docs/TECHNICAL_OVERVIEW.md) and the linked implementation records.
