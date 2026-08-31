# Linux GPU release qualification

Stage 7E adds a deterministic evidence gate for future Linux CUDA and Vulkan
Auto eligibility. It does not add a GPU pack, trust key, discovery entry, or
Auto allowlist entry. The checked-in release plan has no representative lanes,
the checked-in evidence document has no results, and
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
- exactly five cold and twenty warm runs for both the CPU baseline and GPU
  candidate;
- the 110 percent warm end-to-end p95 boundary; and
- required device-loss, driver-change, and suspend/resume observations.

Every required lane in the plan contains the SHA-256 digest of the complete
canonical lane evidence object. The evidence document binds the exact plan
digest. A result cannot be substituted, removed, relabeled, or edited after
review without invalidating one of those bindings.

Every run contains a contiguous sequence number, source-artifact path and
digest, outcome, categorized failure, end-to-end time, backend time, peak
process memory, peak VRAM, and transcript digest. The validator opens every
source artifact under one explicit root component by component, rejects
absolute and parent paths, symlinks, hardlinks, missing files, case-colliding
paths, oversized files, duplicate evidence digests, and digest mismatches.
Artifact paths are data only and are never executed.

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
- peak VRAM bytes.

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

The output path must not already exist and is published by a same-directory
atomic replacement. A passing report is evidence for a later, separately
reviewed Auto-manifest change; it is not itself runtime authority. Production
Linux trust, catalogs, and Auto remain NO-GO until real hardware evidence and
the release-signing prerequisites are available.
