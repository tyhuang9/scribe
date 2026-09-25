# Windows GPU performance-policy candidates

This is an offline, pre-installer evidence boundary. It derives proposed Auto
policy bytes from authenticated measurements of a frozen set of workers. It
does **not** approve an installer, change runtime policy, enable production Auto,
sign packs, or publish a release.

The independent campaign authority is initially empty. Production campaign
approval, capture-key custody, nonce consumption and final release approval
remain unprovisioned. GPU-pack signing authority does not grant these powers.

## Why this phase is separate

The full qualification schemas in
[`WINDOWS_GPU_QUALIFICATION.md`](WINDOWS_GPU_QUALIFICATION.md) bind an installed
package and include scenario observations in their memory floor. They cannot
also be the input used to construct that same package. Committing a completed
qualification entry creates another cycle: changing the source revision changes
the exact worker/pack identities that the entry approves.

The intended release sequence is:

1. Freeze source revision R and the exact CPU and GPU worker artifacts.
2. Approve an installation-independent performance capture contract.
3. Capture and authenticate performance evidence; derive candidate policy M.
4. Separately authorize candidate construction; build only the desktop using M
   and the existing workers from R.
5. Test the actual candidate installer, including Auto selection and failure
   scenarios, against the frozen policy and memory floor.
6. Approve and publish that exact installer without rebuilding it.

This delivery implements the evidence boundary in steps 2–3, not the protected
approval service, candidate builder, final qualification or publication flow.
In particular, the current release builder still rebuilds its CPU worker; that
must change in the later fixed-artifact candidate integration.

A real acquisition/writer is also still missing. The existing Windows Vulkan
evidence script and ignored hardware tests produce fixture-only, metadata-only
timing reports. They rebuild workers, use a different execution schedule, and do
not retain the complete per-run telemetry and raw handshake artifacts required
here. Only the synthetic test constructors currently write complete performance
bundles. Do not convert an old timing report into qualification evidence or
invent its missing observations.

The future collector must consume frozen worker artifacts, execute the exact
paired schedule, observe actual power/memory/process/device state and handshake
frames, and write the bounded canonical artifacts and unsigned lane inventory.
Protected capture signing and campaign/nonce custody remain separate; signing
arbitrary caller-supplied JSON is not proof that a capture occurred. Collector
integration and candidate-installer integration are distinct remaining stages.

## Versioned inputs

The existing `qualify-windows-gpu-evidence.ps1` entrypoint dispatches by an exact
plan kind. Full qualification schemas 2 and 3 retain their existing shapes,
authority and behavior. Performance documents instead use schema 1 and distinct
kinds:

- `windows_gpu_performance_candidate_plan`
- `windows_gpu_performance_candidate_evidence`
- `windows_gpu_performance_candidate_decision`

The performance plan contains `fixture_only`, `source`, Windows/x86_64 target,
the fixed 5-cold/20-warm/110-percent rules, `contract_bindings`,
`capture_contract`, `capture_authority`, `authorization`, and ordered
`required_lanes` with exact identity/evidence digests. `source` names
`tyhuang9/scribe`, `refs/heads/main`, a full frozen revision and the application
version. Production evaluation requires that revision to match a clean checkout;
fixture identities are not production provenance.

The checkout is a trusted build input. Local Git checks do not independently
prove GitHub repository ownership, protected-branch membership or a hosted
workflow's identity. A future protected controller must authenticate those
facts and the caller's integrity pins before using a performance decision.

The contract bindings are `evaluator_sha256`, `toolchain_contract_sha256`, and
`base_auto_manifest_sha256`. They pin the current evaluator, Windows worker
toolchain, and **base** checked-in Auto manifest, not the not-yet-derived
candidate policy. Updating an evaluator requires a new capture contract; schema
compatibility does not authorize rebinding old evidence to new code.

Each lane uses the schema-3 acquisition, CPU/GPU worker, pack, model, workload,
driver, device and power identities, with two deliberate exclusions:

- No installation identity, scenarios or mixed-device remapping captures.
- No declared qualified minimum total or available memory. Observed total memory
  and successful performance-run availability determine those values.

All desktop/worker build identities must use the frozen source revision and
application version. Lanes share one exact CPU baseline, and a backend cannot
mix different packs or GPU worker artifacts into one candidate.

## Separate campaign approval

`runtime-manifests/windows-gpu-performance-authority.json` is a canonical,
checked-in trust document with schema 1, kind
`windows_gpu_performance_campaign_authority`, `minimum_policy_epoch: 1` and
initially `keys: []`. It is separate from both pack trust and the full
qualification authority. Future approval-key entries contain `key_id` and
`public_key_spki_base64`; IDs use
`performance-approval-p256:<canonical SPKI SHA-256>`. Capture and approval keys
must be different.

The plan's capture authority contains the campaign nonce, capture key ID and
canonical P-256 capture SPKI. Its authorization envelope contains the approval
key ID, `ecdsa-p256-sha256-ieee-p1363` scheme, signature, and a signed record
binding:

- `source_revision`, `performance_contract_sha256`, and `campaign_nonce`;
- `policy_epoch`, `issued_at_unix_seconds`, and `expires_at_unix_seconds`;
- schema 1 and kind `windows_gpu_performance_campaign_authorization`.

The signed contract is the complete ordered performance identity matrix,
source, target, fixture flag, fixed rules, tool/contract hashes, approval key ID,
capture contract, capture key/SPKI and nonce. It excludes final lane evidence
hashes, signatures, future policy bytes and installer identity. The external
approval can therefore authorize an exact capture contract before captures
exist, without committing post-capture plan digests into source.

Authorization uses the distinct preimage:

```text
ASCII("SCRIBE-WINDOWS-GPU-PERFORMANCE-AUTHORIZATION-V1\0")
|| UInt64LE(canonical_record_length)
|| canonical_record_bytes
```

P-256/SHA-256 signatures use the fixed 64-byte IEEE-P1363 representation. Lane
attestations have their own
`SCRIBE-WINDOWS-GPU-PERFORMANCE-LANE-ATTESTATION-V1\0` domain and
`windows_gpu_performance_lane_attestation` record kind, binding the approved
contract, authorization-envelope digest, campaign, exact unsigned lane and exact
inventory. Full-qualification signatures are not interchangeable with these
signatures.

Authenticate approval and lane attestation before opening inventory-selected
artifacts. Production performance evaluation additionally requires caller-pinned
`-ExpectedAuthorizationSha256` and `-ExpectedCampaignNonce`. Those pins provide
integrity binding, not independent authentication of the caller; the future
protected workflow must authenticate where they came from.

Require an epoch at least the authority floor and
`issued_at <= now < expires_at`, with a positive validity interval of at most
seven days. Production uses the real UTC clock. Only fixture documents admitted
with `-AllowFixture` may supply a fixture approval key and
`-FixtureNowUnixSeconds`. Full qualification rejects all new performance-only
arguments. Approval expiry governs evaluation/build/publication, not the
behavior of an already installed application.

Identical evidence may be evaluated repeatedly. This stateless evaluator does
not consume nonces or prove one-time capture. Protected capture and promotion
ledgers remain separate prerequisites.

## Performance and candidate bytes

Discrete GPUs require AC performance. Integrated/unified GPUs require separate
AC and battery acquisitions using system-managed power. Each power has exactly
five cold and twenty warm CPU/GPU pairs and thirteen captures: ten cold, two
retained warm, and one provider discovery. Inventories contain 64 artifacts for
AC-only lanes or 128 for paired-power lanes. Power, acquisition, session,
generation, challenge, device and raw-SCIF bindings remain mandatory.

Each required power must independently pass success, transcript parity and both
cold/warm end-to-end p95 limits. Timings are never pooled. The available-memory
floor is the maximum of the per-power minima actually exercised by successful
GPU performance starts. Total memory is the observed total. Later scenario
observations must not recompute these candidate values: a future final gate must
instead reject successful Auto GPU starts below the frozen floor.

The displayed warm CPU/GPU p95 pair comes from the worse same-power ratio, with
AC winning ties. Evidence digests remain power-separated. A candidate cannot
contain two entries matching the same pack, model, backend/provider, vendor,
device class and exact driver, even with different memory floors: those
lower-bound memory rules would overlap. Emit no policy if any required lane
fails.

The decision's `candidate_policy` is a **JSON string** containing exact runtime
manifest bytes: runtime field order, compact JSON, ordinally sorted serialized
entries, and one trailing LF. Its SHA-256 hashes the UTF-8 bytes of the decoded
string, including that LF. Do not reserialize it through the evaluator's sorted
outer-document formatter. This transport digest differs from the runtime's
internal manifest fingerprint, which omits the optional trailing LF.

The outer performance decision uses stable insertion-order compact JSON with
one trailing LF. The embedded policy's LF is escaped in that outer JSON and
must survive decoding unchanged. The legacy sorted `StrictJson` formatter
rejects decoded control characters and is intentionally not used for this new
output. Existing qualification inputs and outputs retain their original
canonicalization rules; future candidate consumers must implement the new
decision format explicitly.

The decision also binds source revision, authorization, performance contract,
plan and evidence digests. `auto_eligible` and `release_approved` are always
false, even for a passing production performance campaign. A failed performance
decision has no candidate policy or policy digest. A candidate is proposed data,
not proof of complete hardware coverage, installer reliability or publication
permission.

The existing global artifact, lane, file, aggregate-byte and SCIF size bounds
still apply. This evaluator writes only its bounded decision to stdout; it does
not activate policy or replace caller files.

## Verification and use

The canonical offline Windows test command remains:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-qualification.ps1
```

For a shorter implementation checkpoint, add `-PerformanceSmokeOnly`. That
invokes the actual evaluator on an integrated-GPU fixture, checks the exact
candidate bytes with the existing Auto-manifest report validator, verifies that
`-RequireEligible` still refuses release eligibility, and rejects an invalid
approval signature. It is not a substitute for the full suite above.

`-PerformanceOnly` runs the expanded candidate-contract group while skipping
the older full-qualification cases. CI uses neither shortcut and runs the full
suite.

The full suite exercises both the full qualification contract and the separate
candidate contract. Synthetic signatures and captures are test evidence, not hardware
qualification. Test-owned scratch is removed; historical source-bound release
evidence and useful build caches are not disposable test files.

Production use requires a separately reviewed approval key, protected capture
key and nonce custody, a clean matching checkout, and authenticated expected
pins. The current empty authority deliberately prevents such evaluation. No
production signing command or automatic provisioning is supplied here.

Structural rejection returns exit 1. A valid performance decision returns exit
0, or exit 2 with `-RequireEligible`, because this phase can never approve a
release. Later candidate embedding and final promotion must independently
validate the exact policy bytes, workers, source and unchanged installer.
