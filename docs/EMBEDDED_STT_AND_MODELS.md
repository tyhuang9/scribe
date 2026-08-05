# Embedded STT and model management

**Record status:** Phase 1 safe-runtime implementation plus migration inventory. Updated from the checked-out source and targeted native-fixture verification on 2026-08-05. This is a living implementation record, not a claim that every item in the target specification is complete.

## How to read this record

- **Current fact** is supported by the checked-out source, a checked-in manifest, or a supplied command result.
- **Planned work** is a required migration outcome from the embedded-runtime brief. It is not evidence of an implementation.
- **Unverified** means neither this documentation-only slice nor the available repository evidence proves the assertion. It must not be converted into a compatibility, platform, performance, or security claim.

Where this record differs from older prose, the current source and pinned manifests are authoritative. A reference to a “sidecar” must identify whether it is the default dictation path, an explicit compatibility fallback, or an isolated installation-smoke helper.

## Phase 0 baseline

### Repository and verification baseline

Scribe is a Rust 2024 native desktop application built with `eframe`/egui. Its desktop/native boundary is Rust-only; there is no Tauri, Electron, React, or frontend-to-native IPC boundary. Audio capture uses `cpal`; settings are local JSON; the UI coordinates hotkeys, recording, overlay, history, and output.

The following baseline was supplied for this documentation slice. The commands were **not rerun** merely to edit this file.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo check --all-targets --all-features` | PASS |
| strict Clippy, all targets and features | PASS |
| debug build | PASS |
| release build | PASS |
| `cargo test --all-targets --all-features` | 533 passed, 0 failed, 6 fixture-gated tests ignored |

The six ignored tests require a local runtime/model/fixture environment. A passing compile or fixture-gated test does not prove a packaged desktop build, microphone behavior, physical GPU use, or macOS/Linux support.

### Timing instrumentation and measurement status

**Current fact:** `src/app.rs::LatencyTrace` and `src/diagnostics.rs` capture hotkey-to-overlay, capture start, first meter update, model load, first partial, recording duration, capture finalization, final-text, and paste/output timestamps. `src/benchmark.rs` records fixture preparation, model-load, and backend-processing timings through `TranscriptionService`.

**Current fact:** speech-onset and a separate post-processing interval are intentionally emitted as unavailable when no verified measure exists. This prevents meter cadence or a wall-clock decode duration from being misreported as VAD onset or audio timeline duration.

**Unverified for this Phase 0 record:** a fresh, comparable before/after measurement on one machine, model class, acceleration setting, and audio fixture. Do not infer latency, memory, real-time-factor, accuracy, or GPU improvement from the instrumentation alone.

## Current architecture

### Normalized dictation route

```text
egui / tray / hotkey
        |
        v
SessionCoordinator + LocalTranscriberApp
        |
        +--> cpal capture -> bounded native audio preparation / VAD / endpointing
        |                         |
        |                         +--> optional rolling batch-preview publisher
        |
        v
TranscriptionService (runtime-neutral API)
        |
        v
private RuntimeRouter -> bounded dedicated "scribe-native-runtime" worker
        |
        +--> `.gguf` -> private EmbeddedRuntime -> safe `transcribe-cpp` 0.1.3 API
        |                       |
        |                       +--> statically linked native CPU backend in-process
        |
        +--> legacy `.bin` -> private TranscribeCppRuntime -> C shim -> dynamically loaded whisper.dll
                                |
                                +--> hash-verified CPU ggml backend DLL selected in-process
        |
        v
final Transcript -> overlay/history/output (only finalized text can paste)
```

**Current fact:** `src/transcription.rs` owns the application-facing `TranscriptionService`, `SpeechEngine`, optional `StreamingSpeechEngine`, `SpeechStream`, normalized `Transcript`, `TranscriptionOptions`, `RuntimeCapabilities`, acceleration preference, and session/request correlation types. The architecture-guard tests prevent UI/application modules from naming `RuntimeRouter`, `TranscribeCppRuntime`, or model-family terms.

**Current fact:** `RuntimeRouter` is the only runtime selector and currently has exactly one private `RuntimeKind`: `TranscribeCpp`. The router serializes the native context with a mutex. `RuntimeWorker` uses a capacity-one synchronous command channel, performs native load/decode/unload on the named worker, and uses a lock-free cancellation generation. The model is retained for five minutes of inactivity (`WARM_MODEL_TTL`), unloaded after that timeout, on an explicit unload, or at shutdown. A failed native decode discards the context so the next request cannot be falsely reported as warm.

**Current fact:** normal live text is not a claim of native streaming. The primary model capabilities say `native_streaming: false`; `Auto` and `Rolling` can run the bounded rolling *batch* preview scheduler in `src/streaming.rs`. The stabilizer emits committed and tentative text to the overlay, with session, request, model, and sequence correlation. Finalized text alone reaches output.

### Default route versus processes and servers

The distinction below is deliberate and must remain explicit in later reports.

| Mechanism | Current role | Is it the default normalized dictation route? | Notes |
| --- | --- | --- | --- |
| Safe `transcribe-cpp` static CPU backend | In-process primary inference for the default trusted GGUF model | **Yes** | It has no downloaded runtime package, CLI, localhost service, or Python dependency. |
| `whisper.dll` plus allowlisted `ggml*.dll` files | In-process compatibility inference for retained GGML models | **No** | A dynamically loaded native library is in-process; it is not a localhost service or sidecar process. |
| `whisper-cli` | Compatibility fallback only after a native bootstrap/ABI/native-library-load failure and only after CLI hash verification | **No** | It is an external process, but it is not a server. It must be retired once packaged native bootstrap reliability has parity evidence. |
| faster-whisper, Vosk, sherpa-onnx/Moonshine/Parakeet adapters | Private legacy configuration/artifact compatibility bridge | **No** | `src/stt/*` invokes short-lived processes; several require Python environments. They are retained migration debt, not normalized UI/service support. |
| `--scribe-install-smoke` child mode | Isolated health/load/decode/unload/reload validation before activation | **No** | This is a bounded installer helper, not a dictation sidecar and not an inference server. |
| Local HTTP listeners | Test fixtures only | **No** | Loopback listeners in `src/installations.rs` are behind `#[cfg(test)]`; source inspection found no production STT listener, localhost setting, or health-polling client. |

**Current default-path statement:** with the installed default `whisper_cpp_tiny_en` GGUF model, Scribe transcribes through the safe in-process Rust-owned adapter. It does not install or start a runtime package, Python, a localhost server, or an inference executable. Retained GGML models still use the compatibility native package; CLI and process bridges remain non-default migration debt.

`README.md` still uses the phrase “bundled sidecar” in one requirements sentence. That wording does not accurately distinguish the in-process primary DLL from the compatibility CLI and should be corrected in a separate documentation-consistency change.

## Runtime implementation and platform constraints

### Current embedded adapter

| Item | Current fact | Required follow-up / limitation |
| --- | --- | --- |
| Safe GGUF adapter | `src/embedded_runtime.rs` uses `transcribe-cpp = "=0.1.3"` directly, with `default-features = false`. It owns a retained `Model` and `Session`, initializes backends once, maps only owned neutral data above the private router, and has no application-owned `unsafe` code. | Current static build is CPU-only. `Gpu` requests fail explicitly until a packaged, smoke-tested GPU feature/backend is added. The checked-in default `whisper_cpp_tiny_en` is a pinned trusted GGUF and does not require a runtime package. |
| Legacy adapter | `src/runtime_router.rs`, `native/whisper_shim.c`, and vendored v1.9.1 headers implement the existing `.bin` route. Rust uses opaque native handles, copies callback text into owned `String`s, and confines FFI to the router. | It remains temporary compatibility code until catalog and packaging migration lets the safe GGUF route become the product default. |
| Native source/version | The primary package and vendored headers are whisper.cpp v1.9.1, commit `f049fff95a089aa9969deb009cdd4892b3e74916`. The package manifest identifies logical runtime `transcribe-cpp`. | This identifies the checked-in package, not a claim that a future `transcribe-cpp` crate wraps the same ABI. |
| Runtime package | `runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json` pins a 7,982,101-byte archive, archive SHA-256, 13 allowlisted files, individual sizes/hashes, native entrypoint, and compatibility CLI entrypoint. | Packaged-release smoke evidence remains platform-specific. |
| Backend discovery | The package dynamically loads only the hash-verified CPU allowlist and selects the highest scored CPU backend. | Device enumeration and support beyond the packaged CPU profile are not proven by this record. |

### Packaged backend matrix

| Target | Current package behavior | Current status |
| --- | --- | --- |
| Windows x86_64 | One pinned whisper.cpp v1.9.1 CPU package. `Auto` and `Cpu` resolve to CPU; `Gpu` returns a structured unsupported-GPU error. The release bundler invokes only `bundle-whisper-runtime.sh`. | **Current fact for source/manifests; packaged desktop smoke remains unverified here.** |
| macOS | No pinned primary package in the checked-in manifest. No Metal package is verified. | **Unavailable / unverified; do not claim CPU or Metal release support.** |
| Linux | No pinned primary package in the checked-in manifest. No Vulkan or other GPU package is verified. | **Unavailable / unverified; do not claim CPU or GPU release support.** |

The desktop shell may compile for Linux/macOS, but that is separate from a verified STT package. The target state is CPU plus verified Metal on supported macOS builds and CPU plus a packaged verified GPU backend on supported Windows/Linux builds; that is **planned work**, not the current matrix.

## Current catalog and installation inventory

### Normalized catalog

`src/model_catalog.rs` remains the checked-in normalized catalog used for managed installs. Its default `whisper_cpp_tiny_en` is the tested `handy-computer` GGUF pin and uses the package-free safe route; the remaining compatibility entries are pinned `ggerganov/whisper.cpp` GGML artifacts. `src/huggingface_catalog.rs` supplies backend-owned dynamic discovery/cache using existing `ureq`; the Models page asynchronously renders its trusted, cache-aware model cards without direct frontend HTTP or URL construction. Trusted dynamic GGUF variants now use a stable source-derived ID and can be installed through the same verified transaction; they remain Experimental rather than being claimed as Supported.

| ID | File format and file | Exact size | Current compatibility |
| --- | --- | ---: | --- |
| `whisper_cpp_tiny_en` | GGUF `whisper-tiny.en-Q4_K_M.gguf` from `handy-computer/whisper-tiny.en-gguf` revision `becb8bcb804405dc97b380a523d9975888820986` | 43,545,248 bytes | Experimental |
| `whisper_cpp_base_en` | GGML `ggml-base.en.bin` | 147,964,211 bytes | Experimental |
| `whisper_cpp_small_en` | GGML `ggml-small.en.bin` | 487,614,201 bytes | Experimental |
| `whisper_cpp_medium_en` | GGML `ggml-medium.en.bin` | 1,533,774,781 bytes | Experimental |

Each artifact has a checked-in SHA-256 and is English, batch-capable, CPU-only, and explicitly not native-streaming. The model catalog exposes zero `Supported` models; no roles are assigned before the complete evidence gate. The source now implements both the initial trusted `handy-computer` GGUF default and backend-only trusted discovery policy.

### Legacy inventory

`src/models.rs` still constructs compatibility entries for one Vosk model, seven faster-whisper models, and sherpa-onnx Zipformer, Moonshine, and Parakeet. They are retained for configuration/artifact migration below the normalized service/UI boundary. `src/runtime_catalog.rs` still records their runtime-pack information, but its legacy model artifacts have no pinned version or SHA-256. They are not evidence-backed normal catalog candidates.

| Legacy family | Current adapter/format | Intended disposition |
| --- | --- | --- |
| faster-whisper | Python/runner process; CTranslate2 model directory | Conditional migration only after a named compatible primary artifact passes contracts; otherwise retire. |
| Vosk | Python/runner process; Vosk directory | No target primary handler is selected; retire from the production catalog unless new evidence justifies it. |
| sherpa-onnx / Moonshine / Parakeet | Python/runner process; ONNX-family directories | Keep private only for migration evaluation. Do not add a second logical runtime unless a named evidence gate passes. |
| Existing unmanaged paths | Readable through compatibility/config migration | Never call them managed installs or delete them during normal cleanup. Import only after validation is designed and tested. |

### Current installer behavior

**Current fact:** `managed_downloads.rs` resolves a caller-supplied `ModelId` against the local normalized catalog, then obtains a checked-in pinned URL, revision, expected size, and SHA-256. The UI does not provide an arbitrary model URL to this API.

**Current fact:** `installations.rs` streams to `*.partial`, supports cancellation and validated HTTP Range resume, rejects malformed or ignored range behavior safely, validates size and SHA-256, quarantines invalid partials, and atomically promotes a verified model. Runtime archives are extracted into a staging directory with exact allowlisted paths/files, then activated with a durable journal and previous-known-good rollback handling. A staged native health/load/decode/unload/reload smoke runs before activation.

**Current implementation:** `src/huggingface_catalog.rs` is a Rust discovery service. It queries only public, non-gated `handy-computer` automatic-speech-recognition repositories carrying `gguf` and `transcribe.cpp` tags, follows only validated same-query Hugging Face `Link` continuations with loop and page-limit protection, deduplicates repository cards, resolves a full commit SHA, enumerates its repository tree, accepts only safe `.gguf` LFS files with a size and SHA-256, applies `resources/model_metadata_overrides.json`, and returns typed `RemoteModel`/`RemoteModelVariant` data. It persists a versioned 24-hour cache through the project’s durable atomic writer, uses stale cache on a refresh failure, and finally exposes a pinned bundled fallback. `app.rs` starts discovery only from the Models page on a named background thread and receives a typed result event; the egui code has no Hugging Face client or URL construction.

**Current installer slice:** the checked-in GGUF default and each selected typed `TrustedArtifact` use the existing resumable, size/hash-verified downloader and activation journal. Staged GGUF validation passes `runtime_package_root: None` through the isolated safe-adapter smoke; no runtime archive is downloaded or activated for GGUF. After smoke validation and atomic activation, `src/installed_manifest.rs` writes a per-model JSON manifest through the same replacement and journal-recovery transaction. It captures explicit normalized-catalog, trusted-Hugging-Face, or local-import provenance; an explicit verification level (`pinned_source_digest` or `locally_observed_fingerprint`); source facts; local absolute path/size/SHA-256; safe-runtime version; resolved acceleration; smoke timings; the loaded model's `general.architecture`; runtime-observed capabilities; and verification time. The service consumes a v4 receipt only when its model ID and canonical artifact path match; imported receipts must also declare local-import provenance. Missing, stale, or legacy receipts retain conservative runtime defaults.

**Dynamic activation/configuration:** a remote artifact is never activated from a UI URL. The card supplies a backend-owned typed catalog artifact, `managed_downloads.rs` derives a fixed Hugging Face resolution URL and an opaque source ID, and settings persist the exact repository, full revision, filename, expected size/SHA-256, presentation metadata, and app-managed destination only after activation succeeds. `TranscriptionService` resolves that record into the same package-free embedded GGUF route and checks the stored integrity facts before use. Each remote artifact has its own directory below the immutable revision, so Remove stages the data and its installed-manifest sidecar together without deleting a sibling quantization.

**Local GGUF import:** the Models page accepts a user-supplied local `.gguf` path. Scribe rejects links/reparse points and non-regular files, canonicalizes the source, cancellably streams a SHA-256 and size snapshot, assigns `local-<full-sha256>`, and runs the same isolated embedded-runtime smoke before recording the model. It then re-fingerprints the source to fail closed if its bytes changed. Scribe does not copy, move, upload, rename, or delete the source file. The app-owned receipt is stored under its model storage's `imported-receipts/` directory; Remove deletes only that receipt and the Scribe configuration record. The fingerprint is locally observed integrity data, not a trusted upstream checksum, and every later runtime load rechecks the configured size and hash.

**Models behavior:** experimental cards now offer Install; installed cards remain usable even when the remote catalog is unavailable, and expose Use and Remove. The catalog has session-only controls for name/language search, exact-installed-variant, recommended, multilingual, and smallest-variant size-tier filtering, plus recommended/size/name sorting. Non-default sorts order the complete matching result set rather than a Recommended/Experimental presentation group. Accessible status text reports the filtered result count. It states when trusted catalog data does not include download counts, update timestamps, or native-streaming capability rather than offering unsupported controls. Before each pinned model download, Scribe checks the destination volume for remaining bytes plus a 1 GiB safety headroom; an insufficient or unverifiable volume disables the card action and the installer rechecks before network I/O. A later revision for the same repository/file is marked Update available and installs as a new source-pinned artifact; the known-good version remains intact until the user explicitly switches to the validated new model. Dynamic results remain Experimental; no trusted remote model is claimed as Supported.

**Normal-path boundary:** `TranscriptionService::model_descriptors()` now exposes only package-free embedded GGUF entries to the Models page, Downloads view, and Playground selector. The static GGML descriptors remain resolvable only by explicit ID for legacy configuration migration; they cannot be newly installed or maintained through the normal UI. The Models page also treats installed embedded GGUF files as runtime-ready directly, and no longer shows the runtime-package maintenance section for a fresh/default configuration.

## Migration inventory

| Existing backend/path | Current call sites | Replacement or target | Keep temporarily? | Removal criteria |
| --- | --- | --- | --- | --- |
| Safe GGUF adapter | `embedded_runtime.rs`, `runtime_router.rs`, `transcription.rs` | Make this the sole normalized primary route after trusted GGUF catalog/install and packaged target evidence exist. | Yes | The catalog can install trusted GGUF artifacts and every supported target has load/decode/cancel/unload/package evidence. |
| Native normalized whisper.cpp DLL | `runtime_router.rs`, `transcription.rs`, `model_catalog.rs`, `runtime-manifests/` | Compatibility path for existing `.bin` installs while the product migrates to the safe GGUF adapter. | Yes | The safe GGUF route is catalog-default and existing installations have a tested migration/retention policy. |
| Compatibility `whisper-cli` | `runtime_router.rs` fallback verification; `stt/whisper_cpp.rs`; compatibility bridge | Remove default fallback once native bootstrap/package reliability meets parity; no user-facing runtime pairing. | Yes, non-default | Native package load, model load, decode, cancellation, restart, and package smoke are reliable on each supported target. |
| faster-whisper Python runner | `stt/faster_whisper.rs`, `stt/mod.rs`, legacy catalog/runtime catalog | Curated primary-runtime artifact only if it passes the common contract; otherwise retire. | Yes, private migration bridge | Config/artifact migration plan and replacement evidence for affected users. |
| Vosk Python runner | `stt/vosk.rs`, `stt/mod.rs`, legacy catalog/runtime catalog | Retire from normal product catalog. | Yes, private migration bridge | Explicit user migration/removal behavior; no remaining config path needs execution. |
| sherpa/Moonshine/Parakeet runners | `stt/sherpa_onnx.rs`, `stt/mod.rs`, legacy catalog/runtime catalog | Retire unless a concrete, measured candidate earns an optional private compatibility handler. | Yes, private migration bridge | The complete benefit, lifecycle, cancellation, memory, platform, and compatibility gate passes—or the models are retired. |
| Static model catalog and direct pinned downloads | `model_catalog.rs`, `managed_downloads.rs`, `installations.rs`, Models UI in `app.rs` | Rust `HuggingFaceCatalogService` plus curated overlay and trusted variant installer. | Yes, until catalog parity | Discovery, versioned cache/fallback, strict variant filtering, typed download resolution, dynamic validation/activation, persisted source pins, Use/Remove, and revision-aware update installation now exist. |
| Legacy model-path validation | `config.rs` backend-name branches | Installed manifests and runtime-derived capability validation. | Yes | Imported and managed artifacts can be classified without application-level backend branching. |
| Runtime package installer | `runtime_catalog.rs`, `managed_downloads.rs`, `installations.rs` | Retain only as app-packaged native dependency distribution; never install a Python or arbitrary executable through the model installer. | Yes for primary package | Per-platform self-contained package tests and removal of legacy runtime-pack UI. |

## Security, privacy, and trust decisions

### Current controls supported by source

- **Pinned artifact trust:** normalized model URLs, full revisions, exact sizes, and SHA-256 values are checked in. Runtime package archives and every extracted runtime file are likewise allowlisted and hash-checked.
- **Native-only normal audio route:** capture PCM is retained in native Rust workers; the normalized transcription route has no HTTP STT request. Current artifact HTTP is for model/runtime bytes only.
- **Download integrity:** HTTPS is required in production; test-only loopback HTTP is permitted behind `cfg(test)`. Before managed model network I/O, a canonicalized destination-volume probe reserves the remaining pinned bytes plus 1 GiB headroom and fails closed if unavailable; the downloader repeats this check before its HTTP request. Download bytes stream to disk, Range and `Content-Range` are validated, and full size/hash verification precedes activation. A concurrent writer can still exhaust storage after the check, so write failures remain transactional errors rather than a false guarantee.
- **Filesystem safety:** staging/activation reject unsafe relative paths, path overlap, symbolic links/reparse points in protected runtime paths, and unallowlisted archive contents. Activation journals preserve or restore a known-good artifact when the settings commit is interrupted.
- **Runtime execution safety:** GGUF validation verifies model bytes and loads through the safe static adapter without any runtime package. Retained GGML validation verifies both its native package and model bytes. Downloaded model artifacts are data; the normal model installer does not execute them.
- **Boundary/privacy safety:** diagnostics store allowlisted identifiers, capability/timing fields, and structured outcomes rather than transcript or PCM by default. Final text, not tentative preview text, is eligible for output.

### Open security work and decisions not yet implemented

- **Implemented:** the Rust-side Hub service obtains only public, non-gated trusted-organization repositories; the egui frontend has no Hugging Face client.
- **Implemented:** it resolves a full Hub commit SHA and validates selected safe GGUF LFS filenames against the repository tree before exposing variants.
- **Implemented:** Hugging Face artifact requests disable automatic redirects and manually validate no more than five hops. Every redirect must be credential-free HTTPS and remain below the `huggingface.co` or `hf.co` suffixes, which covers Hub, LFS CDN, Xet, and CDN endpoints while rejecting lookalike hosts, downgrade attempts, and unusual ports.
- **Implemented for normalized and typed remote GGUF installs:** a versioned per-model manifest is atomically staged, committed, rolled back, and startup-reconciled with its GGUF model transaction. It records a `pinned_source_digest` verification level, source pin, expected and observed file facts, runtime/version, resolved acceleration, loaded-model architecture and capabilities, smoke timings, and verification time. These runtime fields flow from the isolated post-load smoke result; they are not inferred from model filenames or the catalog overlay.
- **Implemented for local GGUF imports:** only a user-selected regular local file is fingerprinted and smoke-validated; Scribe stores a content-addressed configuration record and an app-owned receipt while leaving the original bytes and path untouched. Its `locally_observed_fingerprint` level explicitly prevents the locally calculated digest from being reported as trusted remote verification.
- **Planned:** no Hugging Face token storage for public discovery. If gated models ever become a requirement, use the OS credential store and redact tokens from logs.

## Evolving required final report headings

The following headings must remain in this document through implementation. Their current state is intentionally explicit so that planned work is not reported as complete.

| # | Required final-report heading | Current record status |
| ---: | --- | --- |
| 1 | Repository baseline and previous runtime architecture | Recorded above; legacy subprocess paths remain. |
| 2 | Exact embedded runtime crate/native versions and enabled features | Safe crate 0.1.3 is active for `.gguf`, built with `default-features = false`; CPU fixture evidence is recorded below. Packaged GPU and non-Windows support remain unverified. |
| 3 | Final runtime and model-management diagrams | Current runtime diagram recorded; final Hugging Face management diagram is planned. |
| 4 | Packaged backend behavior per operating system | Windows x64 CPU manifest recorded; macOS/Linux packages unverified. |
| 5 | Model worker and warm-load lifecycle | Current worker, five-minute TTL, cancellation, and unload behavior recorded. |
| 6 | Common transcription contracts and module boundaries | Current service contracts and architecture guard recorded. |
| 7 | Every old sidecar/server/backend call site removed or migrated | Inventory above; compatibility adapters/fallback remain. |
| 8 | Hugging Face catalog query/filter rules | Implemented in the backend service and displayed through the Models page; selected typed variants install through the backend-owned transaction. |
| 9 | Catalog cache and offline fallback behavior | Implemented: 24-hour versioned cache, stale-cache fallback, then a conservative pinned bundled fallback. |
| 10 | Curated metadata overlay format | Implemented: versioned `resources/model_metadata_overrides.json`. |
| 11 | Model/variant DTOs and frontend commands/events | Backend DTOs are rendered by the Rust-native Models page through a typed background result event. Dynamic cards dispatch only typed source-pinned installation actions and share operation IDs/progress with the transactional installer. |
| 12 | Install, resume, verify, validate, update, rollback, and remove flows | The default and selected typed GGUF variants download/resume/verify/validate/activate without a runtime package. Their model and installed manifest share rollback/commit/startup reconciliation. Revision updates install separately and do not overwrite the previous known-good source in place. User-selected local GGUF files are fingerprinted, smoke-validated, rechecked, and referenced externally; Remove leaves the source untouched. |
| 13 | Installed manifest format | Implemented for normalized, typed remote, and local imported GGUF installers: schema v4, explicit source provenance and verification level, source and observed file facts, safe-runtime evidence, resolved acceleration, runtime-reported architecture/capabilities, smoke timings, and verification timestamp. Imported receipts are stored only in Scribe-owned storage. |
| 14 | Security and trust decisions | Current controls and remaining decisions recorded above. |
| 15 | Supported, upstream-compatible, experimental, incompatible, and gated model handling | Zero Supported; four Experimental; legacy entries are migration-only; public/non-gated trusted-Hub policy is implemented. |
| 16 | Automated tests and exact commands to run them | Baseline and safe-adapter results are recorded below; catalog/installer commands remain to be added. |
| 17 | Manual platform tests completed | None newly completed by this documentation slice. |
| 18 | Before/after cold and warm latency measurements | Instrumentation exists; no new comparable measurement is recorded here. |
| 19 | Remaining limitations, each as a concrete tracked task | Listed below; track ownership/status in the project task system. |
| 20 | Whether any default-path sidecar, Python runtime, server, or external executable remains | No default normalized sidecar/Python/server/executable route; non-default CLI and legacy process paths remain. |

## Safe adapter verification (2026-08-05)

The safe adapter was built with the Visual Studio 2022 C++ environment and its Visual Studio CMake executable because the `transcribe-cpp-sys` build needs a Windows-native CMake generator. This is a build prerequisite only; Scribe does not modify a user's environment at runtime.

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --all-targets --all-features -- -D warnings` | PASS |
| `cargo test --all-targets --all-features` | PASS: 594 passed, 0 failed, 9 ignored |
| `installed_manifest::tests` and `installations::tests` | PASS: manifest provenance and verification level distinguish pinned source digests from locally observed fingerprints; runtime-observed architecture/capabilities, atomic replacement, and activation-journal recovery coverage pass. |
| `config::tests::trusted_remote_gguf_survives_normalization_only_at_its_pinned_path`, `managed_downloads::tests::trusted_gguf_download_uses_only_a_pinned_huggingface_resolution_url`, and `installed_manifest::tests::remote_gguf_manifest_preserves_the_dynamic_pinned_source` | PASS: verifies fixed app-managed paths, typed pinned Hub resolution, and persisted dynamic provenance. |
| `installations::tests::trusted_huggingface_redirect_*` | PASS: local-server coverage rejects a disallowed redirect; unit coverage accepts current official Hub LFS/Xet/CDN suffixes and rejects insecure, credentialed, non-standard-port, and lookalike targets. |
| `app::layout_tests::normal_models_page_does_not_expose_runtime_package_maintenance` and `embedded_gguf_model_is_ready_without_a_runtime_package` | PASS: the normal UI does not surface runtime-package controls and installed embedded GGUF models do not require a package to become Ready. |
| `config::tests::imported_gguf_stays_external_and_is_never_classified_as_remote_or_managed`, `installed_manifest::tests::local_import_receipt_is_app_owned_and_never_sidecars_the_source_file`, `transcription::tests::imported_gguf_uses_the_embedded_installation_binding`, and local-import app tests | PASS: local imports use a full content-addressed ID, reject sources inside Scribe storage, recheck same-size byte changes before persistence, stay outside app-managed model paths, use the embedded package-free route, keep their receipt in Scribe storage, and Remove never deletes the external source. |
| `huggingface_catalog::tests::paginated_discovery_filters_each_page_and_deduplicates_repository_cards` and `catalog_pagination_rejects_untrusted_and_repeated_continuations` | PASS: discovery follows validated trusted pagination, applies repository filters on every page, emits one card per repository, and rejects unsafe or cyclic continuation URLs. |
| `app::layout_tests::remote_catalog_browse_filters_and_sorts_only_from_available_metadata` and `normal_models_page_does_not_expose_runtime_package_maintenance` | PASS: the Models page filters and sorts only on persisted/received metadata, and browse controls remain accessible without exposing runtime-package maintenance. |
| `disk_space::tests` and `installations::tests::disk_space_preflight_accounts_for_resumable_partial_bytes` | PASS: model-download preflight keeps a 1 GiB reserve, fails closed for probe/overflow errors, and accounts for retained resumable or oversized partial bytes without touching them. |
| `cargo build --release --all-features` | PASS with the Visual Studio 2022 developer environment and its Windows-native CMake executable. |
| `embedded_runtime::compatible_gguf_loads_and_reports_runtime_capabilities` | PASS with a local trusted fixture; the safe wrapper reported strict CPU backend. |
| `embedded_runtime::compatible_gguf_transcribes_canonical_audio_in_process` | PASS with a local trusted fixture and canonical WAV input; no CLI, server, or external inference process was started. |
| Release app installation smoke, `--scribe-install-smoke whisper_cpp_tiny_en <fixture> - cpu` | PASS: safe model health/load/decode/reload completed in 111/39/212/119 ms with strict CPU and no runtime package root. |

The tested model fixture was `handy-computer/whisper-tiny.en-gguf` at revision `becb8bcb804405dc97b380a523d9975888820986`, file `whisper-tiny.en-Q4_K_M.gguf`, 43,545,248 bytes, SHA-256 `3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b`. This same pin is now the checked-in default catalog artifact; the fixture remains separate local test evidence rather than an installed user artifact.

## Remaining tracked work

1. Retain the private C shim only while `.bin` compatibility is required; define an explicit migration or retirement decision for legacy non-GGUF files. Local GGUF import is implemented.
2. Produce pinned, self-contained, smoke-tested primary packages for each intended macOS/Linux/Windows target and add release CI coverage.
3. Remove the native-bootstrap CLI fallback only after equivalent failure, recovery, and packaged-release evidence exists.
4. Add richer per-card dynamic installation detail beyond the current progress, error, cancel, and resume controls, such as per-stage validation timing and offline-source diagnostics.
5. Expose persisted runtime validation details in model diagnostics and define an automatic cleanup policy for superseded revisions.
6. Migrate or explicitly retire each legacy adapter and delete its runtime install/process-management UI only after its users have a tested path.
7. Run the remaining production-only packaged-release and manual platform suites; publish only measured latency/compatibility results.

## Current conclusion

Scribe now has a real safe, in-process `transcribe-cpp` 0.1.3 GGUF default with retained model/session lifecycle, bounded-worker routing, cancellation, explicit option/acceleration errors, and isolated install-smoke evidence. Its installer does not stage a runtime archive for GGUF and atomically persists an installed-model provenance record with observed architecture and capabilities. Trusted backend discovery/cache, strict variant resolution, destination-volume preflight, and Rust-native Models-page catalog cards can now install, validate, activate, use, update, and remove typed dynamic GGUF variants. Users can also validate and reference an external local GGUF without Scribe copying or deleting it; its locally observed fingerprint is kept distinct from trusted remote provenance. macOS/Linux packages, richer dynamic-card operation state, and legacy-process retirement remain incomplete.

No statement in this record promotes those planned capabilities, the four Experimental models, or platform packaging to Supported status without the required evidence.
