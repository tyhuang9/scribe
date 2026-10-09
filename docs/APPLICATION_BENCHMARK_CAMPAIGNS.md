# Application-path benchmark campaigns

`--benchmark-campaign` measures one explicit CPU or GPU lane through Scribe's
ordinary `TranscriptionService`, without opening the UI. Unlike the Windows
capture observer, it uses normal routing, worker supervision, model admission,
and warm-model residency. It does not link a GPU provider into the desktop.

## Run a lane

Use the exact built-in GGUF model ID and artifact filename from Scribe's model
catalog, and independently verify the input SHA-256 values before invoking the
command. All input and output paths must be absolute. All options below are
required and single-use:

```text
local-transcriber --benchmark-campaign <fixture.wav> --fixture-sha256 <lowercase-sha256> --model <built-in-gguf-id> --model-path <catalog-filename.gguf> --model-sha256 <lowercase-sha256> --acceleration <cpu|gpu> --output <new-report.json>
```

The model must still satisfy the application's existing trusted catalog
admission. A caller-supplied hash does not authorize an arbitrary model. The
fixture and model are validated before measurement; input paths are not emitted.
An existing output is never overwritten.

Run CPU and GPU as separate lane invocations with the same inputs. `auto` is
intentionally unavailable: this tool must not depend on, modify, or enable Auto
qualification. GPU mode keeps the production rule that a GPU must be used or
the request must fail. The normal GPU priority chooses the backend/device; this
command is not a backend override or a way around pack verification/quarantine.

The command uses an in-memory default configuration, not saved user settings.
It does not save settings or history. Normal runtime GPU security/health records
may be updated by ordinary verified pack resolution; this is not a state-free
identity command.

## Schedule and measurement boundary

Each invocation executes:

1. Five cold requests, each using a fresh service that is shut down before the
   next service starts.
2. One fresh service's priming request, reported separately and excluded from
   measured warm statistics.
3. Twenty warm requests on that same retained service, followed by bounded
   shutdown.

Cold and priming requests must not report warm reuse. Every measured warm
request must report reuse. A lost warm model, changed backend/device/pack,
unexpected CPU resolution in GPU mode, transcription failure, or cancellation
aborts the campaign; it is not replayed or silently re-primed. Service teardown
also runs when a request or report write fails.

The timer measures **application transcription latency**: normal transcription
dispatch through returned outcome. Input hashing, WAV decoding/preparation,
transcript hashing, report serialization, and final service shutdown are outside
that timer. This is not microphone-to-UI end-to-end latency, a purged filesystem
cache measurement, or a native-kernel-only measurement.

Separate lanes preserve the normal one-active-worker rule. Alternating CPU and
GPU while retaining both models would measure a different residency policy.
Control and record run order and host conditions when comparing lanes; the
command does not manufacture a paired qualification cohort from them.

## Report and limitations

Reports contain metadata, actual resolved identities, per-run timings, and
normalized transcript SHA-256 digests. Normalization folds case and whitespace
only: punctuation differences remain differences. Raw audio, transcript text,
worker diagnostics, stdout/stderr, and input paths are not included. Digests and
hardware identities can still be identifying; keep reports local unless their
publication is separately approved.

Reports are explicitly unsigned and unqualified, with Auto and release approval
false. They are not the paired qualification evidence schema and must not be
relabelled or padded into it. Completing a campaign establishes acquisition and
its checked invariants, not performance qualification or transcript correctness.
The existing single-request `--benchmark` command is unchanged.

Host controls, representative hardware/power/lifecycle coverage, telemetry and
provenance integration, strict CPU/GPU transcript parity, and candidate-installer
acceptance remain required by the
[Windows GPU performance contract](WINDOWS_GPU_PERFORMANCE_CANDIDATES.md).
