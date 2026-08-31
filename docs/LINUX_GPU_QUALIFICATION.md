# Linux GPU release qualification

Stage 7E adds a deterministic evidence gate for future Linux CUDA and Vulkan
Auto eligibility. It does not add a GPU pack, trust key, discovery entry, or
Auto allowlist entry. The checked-in release plan has no representative lanes,
`gpu-auto-qualification-linux-x86_64.json` remains the exact canonical empty
default-deny manifest. The independent checked-in production authority also
contains no approved plan digest.

Ordinary pull-request CI validates only synthetic fixtures. It never runs a
performance threshold against shared runners and never represents fixture
measurements as physical-hardware results. A fixture plan and fixture evidence
require `--allow-fixture`; even a complete passing fixture produces
`auto_eligible: false`. `--require-eligible` returns a nonzero status for that
decision.

## Reviewed evidence contract

A release qualification starts with a canonical plan. The plan fixes:

- the exact Linux runtime, toolchain, and empty Auto-manifest digests;
- Ubuntu 22.04 or 24.04, x86_64, kernel, glibc, backend, provider, pack,
  model, workload, stable PCI device, driver, memory, vendor, and device class
  for every representative lane;
- one explicit CPU baseline worker SHA, build ID, provider, protocol, and ABI,
  plus the distinct GPU worker SHA, build ID, provider, protocol, ABI, pack,
  and stable-device identity;
- one acquisition protocol and harness SHA, batch and opaque machine identity,
  CPU model, physical/logical/NUMA topology, host memory, CPU/GPU thread counts
  and affinity digests, AC power, performance governor, fixed GPU power profile,
  isolated background load, and no-throttling observation;
- exactly five cold and twenty warm runs for both the CPU baseline and GPU
  candidate;
- the 110 percent cold and warm end-to-end p95 boundaries; and
- required device-loss, driver-change, and suspend/resume observations.

Every required lane in the plan contains the SHA-256 digest of the complete
canonical lane evidence object. The evidence document binds the exact plan
digest. A result cannot be substituted, removed, relabeled, or edited after
review without invalidating one of those bindings.

CPU and GPU measurements are paired inside that same machine and batch. The
protocol alternates CPU-first and GPU-first order. Every cold pair uses fresh
CPU and GPU worker generations with fresh model state. All twenty warm pairs
reuse one worker generation per target after exactly one unmeasured priming
run. Session IDs, pair IDs, order, reset/priming state, machine, batch, worker
generation, worker identity, and challenge-bound Hello digest are validated for
every record. This prevents unrelated or intentionally slow CPU results from
being substituted as a favorable baseline.

Every run contains a contiguous sequence number, source-artifact path and
digest, outcome, categorized failure, end-to-end time, backend time, peak
process memory, peak VRAM, peak shared device memory, and transcript digest.
Discrete GPUs require dedicated-memory identity, at least 256 MiB total and
qualified memory, and at least 16 MiB measured peak VRAM on successful runs.
Integrated and unified GPUs instead require an explicit shared-host-memory
identity, zero dedicated VRAM, and at least 16 MiB measured shared device
memory. CPU executions require the admitted CPU worker/provider, `cpu:host`,
and zero GPU memory. A GPU record advertising the CPU backend, CPU worker,
wrong provider, reused Hello, or wrong stable device is rejected.

Acquisition, run, and lifecycle artifacts are canonical versioned JSON
envelopes whose record must exactly equal the reviewed evidence record. Paths
are data only and are never executed. The validator rejects absolute and parent
paths, symlinks, hardlinks, missing files, case collisions, duplicate paths,
duplicate digests and Hello attestations across all lanes, oversized files,
more than 64 lanes, more than 4096 artifacts, more than 512 MiB of artifacts,
and digest mismatches. Non-fixture evaluation is Linux-only and opens each path
component relative to retained directory descriptors with `O_NOFOLLOW`; it
checks the file identity before and after its bounded read.

The three lifecycle records supply independently hashed source artifacts and
bind the before/after driver, stable device, observed failure category,
selection reevaluation, active-request migration, partial-output replay, and
next-request recovery facts. Driver-change evidence must end at the lane's
exact driver. Device-loss evidence must be categorized as device loss.

Canonical JSON uses sorted object keys, compact separators, printable ASCII,
and one trailing LF. Duplicate or unknown fields, booleans in integer fields,
unsupported identifiers, extra or missing lanes, extra or missing runs, and
noncanonical documents are rejected before a decision is produced.

## Deterministic decision

For each cold/warm and CPU/GPU run set, the tool recomputes nearest-rank p50
and p95 for:

- end-to-end milliseconds;
- backend milliseconds;
- peak process-memory bytes; and
- peak VRAM and shared-device-memory bytes.

No floating-point arithmetic is used for percentile selection or the
performance boundary. For both cold and warm run sets, the GPU candidate passes
performance only when its end-to-end p95 multiplied by 100 is at most the CPU
p95 multiplied by 110. The boundary is inclusive.

Correctness is equivalent only when all fifty run records succeed and every
transcript digest equals the plan's expected transcript digest. Reliability is
equivalent only when all fifty records succeed; categorized failures remain in
the deterministic report. Each required lifecycle record must pass, show
selection reevaluation, prohibit active-request migration and partial-output
replay, and show recovery on the next request.

`auto_eligible` is true only for a non-fixture bundle with at least one required
representative lane, exact complete coverage, and every lane passing
performance, correctness, reliability, and lifecycle checks. A valid but
failing bundle produces a canonical ineligible decision. Structurally invalid,
unbound, missing, or altered evidence is rejected.

For every passing lane the tool derives the exact current runtime Auto entry:
pack, model, backend, provider, vendor, class, qualified minimum memory, exact
driver, run counts, recomputed warm p95 values, and cold/warm/parity evidence
digests. Auto eligibility additionally requires an exact one-to-one set match
between those projections and the checked-in Linux Auto manifest—no missing,
duplicate, or extra entry. The manifest is empty in this stage, so
`activation_manifest_complete` remains false.

Candidate input cannot promote itself by changing `fixture_only`. Every
non-fixture plan digest must already appear in the fixed checked-in production
authority loaded by the tool; the candidate cannot supply an alternate
authority path. Fixture approval is never accepted by this boundary. The
authority is empty in this stage, so real evaluation is deliberately
impossible. Provisioning one protected approval digest is separate activation
work requiring review of the exact plan, acquisition provenance, and resulting
Auto projection.

## Release workflow

The source-artifact digests and reviewed Git plan prevent undetected mutation;
they do not prove that a claimed physical run actually occurred. Release
reviewers must establish artifact provenance, approve the representative
hardware matrix, and record the protected acquisition process before changing
the empty plan. No production evidence or hardware result is checked in by
this stage.

Run the fixture-only validation suite on either supported Ubuntu lane:

```sh
python3 scripts/test-linux-gpu-qualification.py
```

To evaluate a separately reviewed real bundle without changing production
state:

```sh
python3 scripts/qualify-linux-gpu-evidence.py \
  --plan /reviewed/qualification-plan.json \
  --evidence /reviewed/qualification-evidence.json \
  --artifact-root /reviewed/source-artifacts \
  --output /new-path/qualification-decision.json \
  --require-eligible
```

The output parent must already exist and the output path must not. Publication
uses an exclusive temporary file, fsync, and an atomic same-directory hardlink
that cannot replace a concurrently created destination. A decision is
non-authoritative review evidence: runtime activation still requires a later
change that adds the exact one-to-one Auto projections and separately reviewed
GPU pack trust. Production
Linux trust, catalogs, and Auto remain NO-GO until real hardware evidence and
the release-signing prerequisites are available.
