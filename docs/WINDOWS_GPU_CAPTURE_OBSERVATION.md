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
with actual handshake, request timing and native memory/power observations.
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

## Measurement meanings

Worker process observations must use the retained child process handle, not a
caller-supplied PID. Sampling stops when the lease is invalidated or the worker
exits; a replacement generation cannot inherit earlier measurements.

- Process memory is current private commit (`PrivateUsage`), sampled over the
  individual observation window. The reported maximum is a sampled maximum,
  not a guaranteed instantaneous peak or a process-lifetime high-water mark.
- GPU process memory uses the selected adapter and explicit **local** and
  **non-local** memory segments. Keep those names. Do not automatically relabel
  them dedicated VRAM and shared host memory, especially on integrated GPUs.
- Windows memory budget is not provider free memory. Do not substitute budget
  minus process usage for a CUDA/Vulkan allocator's free-memory observation.
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

The unpublished performance contract currently requires an unused memory pool
to be zero based only on GPU class. That assumption cannot describe general
Windows observations. Establish the collector's actual measurement APIs first,
then correct performance-specific semantics and regenerate digest-bound test
fixtures. Preserve the legacy full-qualification schemas 2/3; never relabel old
evidence as newly captured observations.

The worker also needs trustworthy resolved inference-thread and fresh provider
free-memory observations before it can satisfy the full acquisition contract.
The current native default thread setting is not an observed positive count.
Existing startup device snapshots are not fresh before/after measurements.

The later paired warm campaign needs one retained CPU worker and one retained
GPU worker, executing inference serially. Keep this exception private to the
collector, bound it to two workers and account for retained-model interference.
Preserve the five-minute idle lifetime and reject an expired warm generation;
do not add keepalive inference or silently prime it again.

Hardware counter validation, complete paired-run acquisition, protected capture
custody, candidate-installer integration and unchanged-artifact promotion remain
separate acceptance gates. Passing deterministic observer tests alone does not
establish GPU performance or production readiness.

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

The schema-1 report kind is `windows_gpu_capture_observation`, with
`unsigned:true`, `unqualified:true`, `auto_eligible:false` and
`release_approved:false`. It is not accepted as a qualification evidence bundle.
Unavailable provider free memory, resolved inference-thread count and thermal
state are explicit unavailable observations, not fabricated successful facts.

The canonical offline verification entry point is:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-capture-observation.ps1
```

It uses locked, offline Cargo commands: formatting, ordinary desktop and
collector production checks, strict lint, positive test discovery and four
test groups: collector, native telemetry, supervisor leases and architecture
guards. `-ScriptOnly` provides the fast inner-loop check: script parsing and
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
