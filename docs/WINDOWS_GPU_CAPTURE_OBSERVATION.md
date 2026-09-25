# Windows GPU capture observations

This development-only observer is the first real acquisition primitive for the
[performance-candidate pipeline](WINDOWS_GPU_PERFORMANCE_CANDIDATES.md). It is
not a completed qualification campaign. Do not use its output as signed
evidence or release approval.

## Scope and invariants

The observer consumes existing verified CPU/GPU workers and hash-pinned GGUF
model and WAV inputs. It must not build, download, replace, activate or sign
workers. All ordinary pack verification and the desktop's CPU-worker digest
anchor remain required. An arbitrary path plus a supplied digest is not launch
authority. The currently empty production trust still prevents production GPU
pack use until the separate approved-signing setup is completed.

The initial scope is one CPU request and one GPU request, executed serially,
with actual handshake, request timing and native memory/power observations,
including request-bound raw provider-memory snapshots.
It does not implement the five-cold/twenty-warm campaign, production capture
signatures, campaign authorization or nonce consumption. Normal application
routing, saved settings, health records and Auto policy must remain unchanged.
The optional collector must not enable the `inference-worker` feature or link
CUDA/Vulkan inference providers into the desktop process. Existing CPU-only
voice-activity support is unchanged.

Observation-only raw-frame buffers, retained handshake state, duplicated
process handles and lease APIs are compiled out of ordinary builds. The
existing IPC frame format and protocol version are unchanged.

Only validated Hello/Ready wire frames may be retained, bound to their actual
worker generation. Capturing arbitrary IPC would expose audio, model paths or
transcripts. Do not retain PCM, transcription responses, native stderr, user
paths, raw audio or transcript text in the report. Transcript comparison uses
digests computed in memory. Outputs must be bounded and published without
replacing an existing result.

## Negotiated provider observations

The collector negotiates version 1 of the runtime-observation extension on
**both** authenticated worker generations before decoding audio or issuing a
model request. Negotiation does not sample memory. An unsupported worker fails
this preflight; the collector does not fall back, rebuild a worker or weaken
pack verification. Existing Hello, Ready and RuntimeTranscript shapes and
SCIF protocol 5 / worker ABI 1 remain unchanged. The extension is additive on
the private, exact-build-bound connection, not compatibility with arbitrary
older workers. Ordinary workers implement the responder; only the opt-in
collector initiates it.

Immediately before each individual CPU/GPU batch, `BeginRuntimeObservation`
binds the next GGUF model digest and captures a fresh provider enumeration.
The GPU sample is not a cached reading from before the preceding CPU run.
The worker permits one observation at a time and binds it to the actual
batch session and begin/end request IDs. A successful batch captures the
loaded model's fresh device observation. `FinishRuntimeObservation` must
match the original observation request and consumes that result once.
Cancellation, failure, unload, invalid correlation or worker replacement
invalidates the observation. Unrelated or cross-role commands also invalidate
the pending result; read-only worker health checks are allowed between phases.
No observation request extends a model lifetime through keepalive inference or
silently re-primes an expired warm model.

The bounded typed snapshots contain no paths, audio, transcript text or native
error messages. Available GPU snapshots must match the authenticated backend,
provider, stable device and total memory, independently of the volatile process
index. The collector checks this binding before sending the model or audio,
and checks the after snapshot again before reporting it. CPU snapshots are
`not_applicable`; an unreported total or failed loaded-model query can be
explicitly `unavailable`, never fabricated zero memory. Existing device-binding
checks may reject an observation before it reaches that unavailable result;
unknown measurements must never relax those checks. Available raw values retain
`value_semantics: native_backend_defined` and
`admission_validity: unestablished` on both backends. These are measurement
reports, not evidence that a model fits or a backend qualifies for Auto.

## Measurement meanings

Worker process observations must use the retained child process handle, not a
caller-supplied PID. Sampling stops when the lease is invalidated or the worker
exits; a replacement generation cannot inherit earlier measurements.

- `elapsed_ms` is the collector's instrumented request window, including
  sampling setup and observation-control overhead, but excluding sampler
  finalization after the observed request returns. It is not yet a complete
  cold/warm qualification timing contract. The full campaign must establish
  consistent boundaries and measure instrumentation overhead before using
  these observations to compare ordinary application latency.
- Process memory is current private commit (`PrivateUsage`), sampled over the
  individual observation window. The reported maximum is a sampled maximum,
  not a guaranteed instantaneous peak or a process-lifetime high-water mark.
- GPU process memory uses the selected adapter and explicit **local** and
  **non-local** memory segments. Keep those names. Do not automatically relabel
  them dedicated VRAM and shared host memory, especially on integrated GPUs.
- Windows memory budget is not provider free memory. Do not substitute budget
  minus process usage for a CUDA/Vulkan allocator's free-memory observation.
- A native provider's `memory_free` field is also backend-defined. In the
  pinned transcribe-cpp 0.1.3 native implementation, CUDA uses `cudaMemGetInfo`,
  while Vulkan uses heap budget minus heap usage when its memory-budget
  extension is available and otherwise returns total heap capacity. The public
  wrapper does not expose which Vulkan path was taken. Record such values as
  `provider_reported_memory_free_bytes`, not verified physical free memory.
  A fresh snapshot alone therefore cannot establish Vulkan memory admission
  or a qualification memory floor; native provenance remains required.
- The public wrapper also permits zero for an unreported free-memory value.
  Preserve a reported zero when total memory is known; it might mean exhaustion
  or an unreported value. Mark admission validity as unestablished rather than
  inventing that distinction. A zero total is unavailable, and a reported free
  value greater than total is invalid; never clamp it to produce a plausible
  successful observation.
- Preserve unknown or unavailable measurements. Do not replace them with zero,
  claim no throttling, or infer inference-thread count from OS process threads.
- Record power at both request endpoints; unknown or differing readings
  invalidate the observation. These endpoint checks do not establish that an
  AC-to-battery-to-AC transition was absent between readings. Do not migrate
  or replay a request to make it qualify.

The native implementation uses `D3DKMTQueryVideoMemoryInfo`, using an adapter
opened from an independently matched LUID and a retained process handle with
`PROCESS_QUERY_INFORMATION`. Ambiguous adapter mappings and unsupported linked
adapter configurations must be rejected rather than guessed. A physical-node
index inside an already identified adapter is not a persistent device identity.

The initial Windows mapping accepts canonical `native:luid:<16 lowercase hex>`
and the worker's zero-domain PCI identities, such as `native:0000:01:00.0`
or `native:pci:00000000:01:00.0`, without rewriting their stable identity.
UUID-only identities are rejected: the collector cannot yet independently map
a provider UUID to a Windows adapter. Nonzero PCI domains and linked
physical adapters are also unsupported. This limits which verified workers the
observer can measure; it does not change normal application device selection.

Microsoft documents the relevant definitions:

- [Process private commit and lifetime peak counters](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex).
- [Process- and adapter-bound video-memory query](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/d3dkmthk/ns-d3dkmthk-_d3dkmt_queryvideomemoryinfo).
- [Dedicated and shared memory on both GPU classes](https://devblogs.microsoft.com/directx/gpus-in-the-task-manager/).
- [Known inaccurate GPU Process Memory performance counters](https://learn.microsoft.com/en-us/troubleshoot/windows-client/performance/gpu-process-memory-counters-report-wrong-value).

A development read-only probe on 2026-09-25 checked the installed Windows SDK's
x64 layout and successfully queried both segments on three enumerated adapters
for the probe's own process. That process had no GPU workload and reported zero
usage. This establishes local API availability only: it does not verify
selected-worker mapping, linked-adapter handling, nonzero usage accuracy,
sampling overhead or behavior under inference load. No other process was
sampled or altered, and no persistent probe artifact was created.

## Remaining integration

The [performance-only contract](WINDOWS_GPU_PERFORMANCE_CANDIDATES.md) records
sampled private commit and separate local/non-local segments without forcing an
unused pool to zero based on GPU class. Its acquisition records the actual
native-default thread request of zero, with resolved count null, while retaining
host/affinity and worker/native-source bindings. The pinned library does not
expose its resolved count; OS process threads are not a substitute. This keeps
the application's scheduling unchanged without requiring a native fork just
for that field. Full-qualification schemas 2/3 keep their legacy shapes and
rules; neither old evidence nor this unsigned report may be relabelled as a
new authenticated capture.

Raw provider observations remain separate from the performance contract's
admission inputs. The completeness of separately supplied numeric admission
values is not proof of their measurement semantics. Null admission inputs must
prevent a candidate floor or policy; the raw values in this report cannot supply
them automatically. Reviewed, pinned acquisition code and protected capture
custody are still required to establish valid production admission observations.

Provider-memory provenance is a separate prerequisite for safe Auto memory
admission. Existing startup snapshots cannot substitute for fresh before/after
measurements, nor can a fresh provider value establish its own memory
semantics. Raw observations here do not satisfy that admission prerequisite.
An independent worker-only Vulkan memory-budget query can provide a future
path without modifying the native library: match the exact physical device by
PCI/LUID/UUID, verify budget-extension support and record the actual heap scope.
That is budget-headroom evidence, not physical free VRAM or proof of which
internal native branch was taken. Missing support or ambiguous matching must
remain unavailable. Qualification and runtime admission must use consistent
measurement semantics before such evidence can affect Auto.

The later paired warm campaign needs one retained CPU worker and one retained
GPU worker, executing inference serially. Keep this exception private to the
collector, bound it to two workers and account for retained-model interference.
Preserve the five-minute idle lifetime and reject an expired warm generation;
do not add keepalive inference or silently prime it again.

Hardware counter validation, complete paired-run acquisition, protected capture
custody, candidate-installer integration and unchanged-artifact promotion remain
separate acceptance gates. Passing deterministic observer tests alone does not
establish GPU performance or production readiness.

Final-installer qualification also needs an explicitly versioned contract that
can represent these actual observations. Legacy full-qualification schemas 2/3
still require their older threading and memory shapes; neither transforming
new observations into those shapes nor rebinding old evidence is a valid way
to qualify the final installer.

## Development interface

The opt-in feature is `windows-gpu-capture-observation`.
It is excluded from ordinary release builds. The collector command is
`--scribe-windows-gpu-capture-observation` with exactly one value for each of:
`--model`, `--model-sha256`, `--wav`, `--wav-sha256`, `--gpu-pack-id`,
`--gpu-backend`, `--gpu-device` and `--output`. Paths must be absolute, hashes
lowercase SHA-256, and the backend `cuda` or `vulkan`. The worker paths are not
caller inputs: CPU resolution remains digest-anchored and GPU discovery remains
signature-verified.

`scripts/run-windows-gpu-capture-observation.ps1` is a convenience wrapper around
an independently trusted collector executable. `CollectorPath` and
`CollectorSha256` identify that build; the remaining named parameters forward
the inputs above. Keep the executable and its parent directories in a trusted,
operator-controlled location. The wrapper's supplied digest does not establish
build provenance or grant pack trust. It performs no build, download or signing.

The schema-2 report kind is `windows_gpu_capture_observation`, with
`unsigned:true`, `unqualified:true`, `auto_eligible:false` and
`release_approved:false`. It is not accepted as a qualification evidence bundle.
Each worker has a `provider_memory` pair with `before` and `after` typed raw
observations; this replaces the schema-1 top-level provider-free-memory
placeholder. Resolved inference-thread count and thermal state remain explicit
unavailable observations, not fabricated successful facts. There is no legacy
report conversion or qualification adapter: old observations must not be
relabelled as new captures.

The canonical offline verification entry point is:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-capture-observation.ps1
```

It uses locked, offline Cargo commands: formatting, ordinary desktop, collector
and independent CPU-worker production checks, strict lint, positive test
discovery and five test groups: collector, native telemetry, supervisor
observation controls/leases, provider-memory snapshots and architecture guards.
`-ScriptOnly` provides the fast inner-loop check: script parsing and
twelve prelaunch argument/file-rejection cases without invoking any
executable. The three tiny owned fixture files are removed after use.
It does not replace the full command above.
Both strict lint configurations include `ui-harness`, matching the existing
release checks' shared UI-route coverage; one excludes the observer and one
includes it. The production checks do not enable `ui-harness`, and no check in
this command enables a GPU inference provider.
Dependencies and the reviewed native archive must already be available, as in
the existing Windows CI setup. Optional `CargoTargetDirectory` and
`NativeArchiveDirectory` select local caches without downloading or copying
them. CI calls this same script. A passing run establishes deterministic code
and contract checks, not real GPU counter accuracy, performance qualification,
production pack trust or release approval.
