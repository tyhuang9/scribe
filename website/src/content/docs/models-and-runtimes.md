---
title: Models and runtimes
description: Understand local model selection, runtime discovery, and storage.
---

## Current catalog and status

The normal Models experience exposes package-free GGUF models. It starts with one pinned `tiny.en` fallback and can discover trusted, public `handy-computer` GGUF variants whose exact repository revision, filename, size, and SHA-256 are resolved before installation. You can also validate a local GGUF in place; Scribe fingerprints and smoke-tests it without copying, uploading, or deleting it.

All normal catalog and imported models are **Experimental** and the Supported count is **zero**. A model must pass load, fixture transcription, cancellation, unload/reload, acceleration, and platform checks before promotion.

Three static GGML records (`base.en`, `small.en`, and `medium.en`) and faster-whisper, Vosk, sherpa-onnx, Moonshine, and Parakeet adapters remain only for private configuration/artifact migration. They are not available for new normal UI installation and are not Supported models.

## One runtime boundary

The application has one logical runtime kind. Only the private `RuntimeRouter` selects it; UI, history, output, settings, and model management use runtime-neutral contracts. Model-format and compatibility distinctions stay below that boundary instead of becoming application handlers.

The normal GGUF path uses the safe `transcribe-cpp` 0.1.3 API with a statically linked CPU backend in-process. It has no downloaded runtime package, CLI, localhost service, or Python dependency. Models can remain loaded for warm reuse.

The pinned whisper.cpp v1.9.1 package at commit `f049fff95a089aa9969deb009cdd4892b3e74916` is retained for older GGML compatibility and as a narrowly scoped bootstrap fallback when the primary native GGUF adapter cannot initialize. Its DLL route is in-process and a hash-verified CLI is the last compatibility fallback, not the normal route. `OnnxSpeechRuntime` is absent because the named streaming candidate has not passed its evidence gate.

## Storage and performance

Managed model files live under Scribe's app-data `models` directory. Trusted installs use pinned manifests, resumable partials, exact size/hash checks, safe staging, native smoke tests, and atomic activation. Local imports remain at their source path and are rechecked against their stored fingerprint.

The embedded GGUF adapter is CPU-only. `Auto` resolves to CPU, `CPU` requests it explicitly, and `GPU` reports that no verified accelerator is available. Linux and macOS can compile conservative app fallbacks, but their desktop/model combinations are not release-qualified.

For packaging and benchmark details, consult the [repository README](https://github.com/tyhuang9/scribe#models-and-runtime). It is the detailed implementation reference.
