# Windows GPU release qualification

This contract evaluates separately acquired Windows x64 CUDA and Vulkan GPU
evidence without loading a provider, running a model, changing trusted pack
state, or editing the runtime Auto manifest. It is the review boundary between
hardware acquisition and a later, explicit Auto-policy change.

The checked-in plan is production-shaped but has no hardware lanes. The
independent production authority has no approved plan or capture key, and
`gpu-auto-qualification-windows-x64.json` remains the exact zero-entry
`default_deny` manifest. Consequently this stage does not qualify a GPU,
provision pack trust, or enable Auto.

Pull-request CI uses synthetic fixture evidence only. `-AllowFixture` must be
present to evaluate a fixture, a passing fixture always reports
`auto_eligible: false`, and `-RequireEligible` returns exit status 2 for that
valid ineligible decision. Fixture data cannot be relabeled as production:
every production plan digest and its exact P-256 capture public key must
already be listed in the fixed checked-in production authority.

## Bound review inputs

A canonical schema-v2 plan fixes the exact evaluator, Windows worker toolchain,
and Auto manifest SHA-256 digests. It also fixes exactly five cold and twenty warm
CPU/GPU pairs, the inclusive 110 percent p95 boundary, the required scenario
set, whether reviewers have established complete coverage of an Auto runtime
bucket, and every required lane with a digest of its complete evidence object.
The evidence document binds the exact plan digest. The plan also binds a
256-bit nonzero campaign nonce, `p256:<sha256-of-SPKI-DER>` capture key ID, and
the exact raw-frame/capture policy. A fixture plan alone may carry a fixture
SPKI and still requires `-AllowFixture`; a production SPKI resolves only from
the fixed production authority.

Each lane binds:

- Windows build and x86_64 architecture;
- CUDA or Vulkan backend/provider and a signed-pack ID, version, digest,
  security epoch, and runtime ABI;
- distinct CPU and GPU worker digests, build IDs, providers, protocol 5, and
  ABIs;
- model, audio, expected-transcript, and inference-options digests;
- stable PCI, Windows LUID, or UUID device identity—never an enumeration
  index—plus exact driver, vendor, class, memory model, and minimum total and
  available memory;
- opaque machine and acquisition-batch identities, CPU topology, thread and
  affinity facts, power plan, AC benchmark conditions, thermal/background-load
  controls, and a recomputed digest of the complete stable device inventory;
- the telemetry source, worker/selected-device scope, and bounded sampling
  interval; and
- exact installer, catalog, and clean-machine-image digests.

Device-set members are strictly sorted stable identities. The selected GPU
must be present, and its enumerated vendor and device class must exactly match
the lane's selected-device identity. The `mixed_gpu` fact must agree with
actual vendor or device-class diversity rather than merely the device count.
At least one mixed-device lane is required before the overall evidence can
pass.

Every worker generation has one signed capture containing the exact base64 SCIF
v5 Hello request and Ready response bytes. The evaluator validates the 26-byte
`SCIF` header, control kind `1`, bounded exact body length (at most 256 KiB),
session/request `0/0`, absence of trailing bytes, strict UTF-8 JSON, exact field
names, and the request's 64-lowercase-hex random challenge with an exact Ready
echo. It validates the Rust wire schema rather than a normalized summary:
`app_build`, `worker_build`, `bundled_worker_sha256`, `abi`, inference role,
provider, ordered GGUF then ONNX ASR `windows-x86_64` artifact targets, GPU pack
expectation, and each device's `stable_device_identity`, `process_index`,
`display_name`, `driver_version`, class, vendor, and total/available memory.

CPU captures omit the pack and advertise no devices. One separate
`provider_discovery` launch advertises the complete reviewed provider-eligible
list. A `selected_device` launch advertises exactly the selected stable device.
Measured runs bind only CPU or selected-device captures, never discovery. Five
distinct cold captures per target prove fresh worker/model generations; all
twenty warm measurements per target bind one retained, once-primed capture.
Transient indexes need only be unique, not contiguous, and are never persistent
identity.

CUDA lanes require a bounded canonical `windows-display:` version. Vulkan
lanes may use that Windows display form or the exact provider runtime identity
`vulkan:<vendor-id>:<driver-id>:<driver-version>:<driver-uuid>`, with fixed
lowercase hexadecimal widths. Runtime vendor IDs are cross-bound as
`10de` = NVIDIA, `1002`/`1022` = AMD, and `8086` = Intel. Cross-backend, cross-vendor,
and truncated driver identities are rejected.

## Paired performance and parity

The evaluator consumes exactly 50 measured records per lane: five cold CPU and
GPU pairs followed by twenty warm CPU and GPU pairs. Odd pairs are CPU then GPU;
even pairs are GPU then CPU. Every cold measurement names a fresh worker/model
generation for each target and pair. Warm measurements name one retained
generation per target after exactly one unmeasured priming run. Session, pair,
order, reset state, machine, batch, options, Windows build, device set, worker,
raw capture, protocol, ABI, pack, model, driver, and stable device are checked on
every record.

Each successful record includes end-to-end and backend milliseconds, peak
worker-process memory, peak dedicated or shared device memory, available
device memory before and after the request, and a transcript digest. CPU
records must report no GPU memory. Discrete GPU records require plausible
dedicated VRAM and zero shared-device memory. Integrated/unified records use
shared-host-memory telemetry and zero dedicated VRAM. Failed records remain in
the report and prevent correctness and reliability equivalence; they are never
dropped to improve a percentile.

The projected minimum total memory equals the observed lane total; schema v2
does not generalize one device's result to a smaller adapter. The projected
minimum available memory equals the lowest availability actually exercised by
a successful GPU run or successful Auto-to-GPU scenario. Every successful GPU
start must meet that emitted floor. A lower plan-asserted threshold is rejected
instead of turning untested memory capacity into an Auto promise.

The evaluator recomputes nearest-rank p50 and p95 using integers. The cold p95
is rank 5 of 5; the warm p95 is rank 19 of 20. Both cold and warm pass only
when `gpu_p95 * 100 <= cpu_p95 * 110`. Overflow-safe integer arithmetic is
used. Every one of the 50 transcript digests must equal the plan-bound expected
digest, and every record must succeed.

## Required Windows scenarios

Every lane carries a separately hashed canonical artifact for:

- clean-machine installation from the exact bound installer;
- device loss, followed by CPU selection for the next request;
- a disabled selected device, followed by unavailable classification and CPU
  recovery on the next request;
- driver change, with the final driver matching the lane identity;
- insufficient available VRAM, denied before GPU startup and followed by CPU;
- mixed-GPU deterministic selection by stable identity, proven by before/after
  selected-device captures in which the same stable ID remaps across fresh
  challenges and different transient process indexes;
- Auto selection on AC;
- Auto power policy on battery; and
- suspend/resume with next-request reevaluation.

All scenarios prohibit active-request migration and partial-output replay,
require selection reevaluation, and require recovery on the next request.
For a discrete lane, Auto must select CPU on battery. Integrated or unified
GPUs remain eligible on battery in the runtime policy, but schema v2 collects
its 5/20 performance pairs on AC only. Therefore schema v2 rejects
`runtime_bucket_complete: true` when any lane is integrated or unified. A
future schema must add paired battery performance before those runtime buckets
can be activated.

## Capture attestation and inventory

Every lane carries an ECDSA NIST P-256 signature in 64-byte IEEE-P1363 form.
Its key ID is `p256:` followed by the lowercase SHA-256 of the exact canonical
SPKI DER. SPKI and signatures use canonical standard base64. The signed record
contains exactly the capture-contract projection digest, campaign nonce, lane
and acquisition IDs, canonical unsigned-lane digest, and canonical digest of a
strictly stable-path-sorted artifact inventory. The capture-contract projection
includes all plan policy and checked-in contract bindings plus the complete
ordered required-lane identity matrix, while deliberately excluding final
evidence digests to avoid a signature/plan cycle.

The signing preimage is exactly:

```text
ASCII("SCRIBE-WINDOWS-GPU-QUALIFICATION-LANE-ATTESTATION-V1\0")
|| UInt64LE(canonical_record_byte_length)
|| canonical_record_bytes
```

ECDSA signs SHA-256 of that preimage. The evaluator authenticates this record
before opening any referenced artifact. It then opens every signed inventory
member through retained handles, checks its digest and limits, and only then
parses the acquisition, 50 runs, nine scenarios, and 15 raw captures. Every
inventory member must be consumed exactly once.

## Filesystem and JSON boundary

Plan, evidence, authority, checked-in contracts, and artifact bytes are read
through retained Windows handles with bounded size, no write/delete sharing,
identity checks before and after the read, and strict checks for physical
ancestors, reparse points, hardlinks, and alternate data streams. Artifact
paths are lowercase bounded relative data components; absolute paths,
backslashes, colons, parent traversal, case collisions, duplicate paths, and
duplicate digests are rejected. Nothing from evidence is executed or
dot-sourced.

Input JSON must be strict UTF-8, printable ASCII, canonical compact JSON with
one LF, bounded integers only, no comments or trailing commas, and no duplicate
or case-colliding fields. Every object uses an exact field allowlist. Each
acquisition, run, scenario, and raw SCIF capture artifact is a canonical versioned
envelope whose record must exactly equal the digest-bound evidence record. The
global limits are 64 lanes, 4096 artifacts, 16 MiB per file, and 512 MiB total
artifacts.

The evaluator writes only one canonical decision to stdout. It has no apply,
activate, output-file, trust, catalog, state, or Auto-manifest mutation mode.
Decisions contain aggregate identities and metrics but no audio, transcript
text, user path, or raw diagnostic data.

The protected production capture signer and campaign-nonce ledger required to
prevent key misuse and cross-campaign replay are not implemented in this stage.
The production authority is intentionally empty. Therefore real production
qualification is a NO-GO even though the synthetic fixture contract passes.

## Decision and activation separation

A passing lane produces a diagnostic projection using the current Windows Auto
schema. The projection includes only vendor and device class, not adapter
family or a narrower device constraint. One physical machine must never be
treated as proof for that broad runtime bucket. `runtime_bucket_complete` may
be set only in a production plan after reviewers establish representative
coverage for the entire projected backend/vendor/class/driver/model/pack
bucket; the exact reviewed plan must then be approved by the production
authority.

Even complete approved evidence is not Auto-eligible until its projections
match the checked-in Auto entries exactly one-for-one. This evaluator never
performs that later policy edit. Pack trust and release signing remain separate
gates.

The decision distinguishes structural rejection (exit status 1), a valid
ineligible decision (exit status 0, or 2 with `-RequireEligible`), and a future
eligible decision (exit status 0). A structurally valid but slow, incorrect,
unreliable, or scenario-failing lane is reported ineligible with deterministic
reasons.

## Commands

Run the synthetic contract suite on Windows:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-qualification.ps1
```

Evaluate an externally reviewed bundle without mutating repository or runtime
state:

```powershell
pwsh -NoProfile -File .\scripts\qualify-windows-gpu-evidence.ps1 `
  -PlanPath C:\reviewed\plan.json `
  -EvidencePath C:\reviewed\evidence.json `
  -ArtifactRoot C:\reviewed\artifacts `
  -RequireEligible
```

The caller may capture stdout as review evidence using its own protected
create-new publication boundary. Do not redirect over an existing decision.
Before any production evaluation, reviewers must approve artifact provenance,
the complete hardware/runtime bucket, clean-installer execution, acquisition
controls, exact pack/model inputs, the plan digest, the protected capture key,
and the nonce-ledger record. The current empty authority and absent protected
signer/ledger deliberately make that operation a NO-GO.
