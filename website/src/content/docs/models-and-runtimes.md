---
title: Models and runtimes
description: Understand local model selection, runtime discovery, and storage.
---

## Current catalog and status

The normal Models experience always includes seven static catalog entries:
Experimental GGUF models `whisper_cpp_tiny_en`, `whisper_cpp_base_en`,
`whisper_cpp_small_en`, and `whisper_cpp_medium_en`, plus the receipt-backed
Experimental `moonshine-tiny-en-int8-onnx`, `moonshine-base-en-int8-onnx`, and
`parakeet-tdt-06b-v2-en-int8-onnx` bundles. The GGUF artifacts are pinned by
repository revision, filename, size, and SHA-256; the ONNX bundles are installed
only as their verified receipt-backed bundles. Moonshine Base is a
286,930,831-byte converted five-file INT8 artifact (including its MIT / Useful
Sensors 2024 license file); its source and converter revisions are unrecorded.

When a trusted catalog response is available, Models can also discover and
display additional non-duplicate remote GGUF variants. Those variants are not
bundled, are not static catalog entries, and may be unavailable. A remote
listing becomes usable only after its exact source facts are verified and its
artifact is installed; listing it does not authorize execution or make it
Supported.

The seven static entries are all **Experimental** and the Supported count is
**zero**. A model must pass load, fixture transcription, cancellation,
unload/reload, acceleration, and platform checks before promotion. You can also
validate a local GGUF in place; Scribe fingerprints and smoke-tests it without
copying, uploading, or deleting it.

Moonshine Base's scoped Windows gate used Sherpa 1.13.5 and passed child
load/health/silence, normalized known-WAV equality, and unload/reload in 140.40
seconds total. That duration is diagnostic elapsed time, not a latency or
quality claim. It remains a CPU-only English final-text model with no timestamps
or native streaming; cancellation, supervisor restart recovery, latency,
resource use, accelerators, and non-Windows behavior are not yet measured.

Parakeet's scoped Windows/Sherpa 1.13.5 gate passed child load/health/silence,
exact normalized known-WAV equality, unload/reload, and activation. It is a
661,190,513-byte (~631 MiB), CPU-only, English final/batch-text bundle with no
native streaming or timestamps; cancellation, restart recovery, accelerators,
non-Windows support, latency, RAM, and other resource use remain unverified.
It retains CC-BY-4.0 attribution to NVIDIA Corporation, the
[license legal URL](https://creativecommons.org/licenses/by/4.0/legalcode), and
notice that this sherpa-onnx int8 conversion is not the unmodified NVIDIA
checkpoint and has no recorded source/converter revision.

Legacy user configuration and artifact files are preserved for migration, but
they are no longer recognized or executed by a production inference route. An
installer does not need to remove those files for the current catalog to work.

## One runtime boundary

The application has one logical runtime kind. Only the private `RuntimeRouter` selects it; UI, history, output, settings, and model management use runtime-neutral contracts. Model-format and compatibility distinctions stay below that boundary instead of becoming application handlers.

Normal GGUF inference uses the safe `transcribe-cpp` 0.1.3 API with a statically
linked backend in Scribe's private persistent inference child. Default source
builds and published releases include the CPU backend only. Source developers
can opt into a statically linked Vulkan backend with the
`vulkan-acceleration` Cargo feature. The receipt-backed ONNX bundles use native
Sherpa ONNX in that same child and remain CPU-only. Neither path requires
Python, a localhost service, a dynamic runtime package, a GGML/DLL route, or a
CLI fallback. Models can remain loaded there for warm reuse.

The desktop process uses runtime-neutral contracts; only the private inference
child owns concrete model and recognizer state. VAD has a separate production
worker/process path and is not an STT runtime.

## Storage and performance

Managed model files live under Scribe's app-data `models` directory. Trusted installs use pinned manifests, resumable partials, exact size/hash checks, safe staging, native smoke tests, and atomic activation. Local imports remain at their source path and are rechecked against their stored fingerprint.

The embedded GGUF adapter advertises GPU support only in a source build compiled
with `--features vulkan-acceleration`. In that build, `Auto` asks the native
runtime to try a compatible GPU first and fall back deterministically to CPU;
the reported device is the device the loaded model actually uses. `CPU` is a
strict CPU request. `GPU` is a strict Vulkan request and fails if Vulkan cannot
be initialized instead of silently running on CPU. Moonshine ONNX remains
CPU-only in every build. Linux and macOS can compile conservative app fallbacks,
but their desktop/model combinations are not release-qualified.

For packaging, model-validation, and benchmark details, consult the repository’s [technical overview](https://github.com/tyhuang9/scribe/blob/main/docs/TECHNICAL_OVERVIEW.md) and the linked implementation records.
