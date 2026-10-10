# Protected Windows GPU qualification CI

Protected GPU CI is the selected qualification trust model. The controller,
GPU acquisition runner, capture signer and durable replay ledger must be separate
boundaries. GPU-pack signing permission does not grant qualification authority.
The implemented prerequisites are a **secret-free provenance preflight** and
**pre-acquisition signed-contract admission**, not a qualified capture producer,
signer, nonce ledger or Auto rollout.

## Implemented boundary

`windows-gpu-qualification-preflight.yml` runs the same offline contract tests
for pull requests and main-branch pushes. Its separate manual job runs only in
`tyhuang9/scribe`, on `refs/heads/main`, after those tests. It uses a fresh
GitHub-hosted Windows runner and the `windows-gpu-qualification-preflight`
environment, with only `contents: read` and `actions: read`. It never schedules
a GPU runner or accesses signing secrets. Repository administrators must
configure environment reviewers and branch restrictions separately; merely
naming an environment in YAML does not establish those protections.

The script authenticates:

1. The fixed controller repository (including numeric ID), workflow, manual
   event, workflow revision and main revision. The workflow also verifies that
   its checkout matches `GITHUB_SHA` before running the script.
2. The current main policy, fetched through GitHub's fixed HTTPS API at that
   immutable revision. A moved main requires a fresh dispatch. Local checkout
   policy, caller-selected policy paths and acquisition-artifact policy are not
   accepted as authority.
3. An exact reviewed campaign entry matching **all** independently supplied
   pins: source revision, producer run/attempt, artifact ID/digest, performance
   contract digest, campaign nonce and authorization-envelope digest. Campaign
   approvals have a maximum seven-day interval, are strictly nonce-sorted and
   cannot overlap a nonce, artifact or producer attempt.
4. The producer's protected-main ancestry, fixed workflow path, exact workflow
   bytes at the approved revision, repository identity, manual event, successful
   terminal result and latest matching attempt. A successful old attempt cannot
   stand in for a newer rerun.
5. Immutable artifact metadata: exact ownership, source, attempt-specific name,
   digest, positive size of at most 512 MiB and unexpired lifetime. Creation must
   fall within the completed attempt's start/update window; artifact metadata
   has no run-attempt field, so neither a matching run ID nor a name alone is
   sufficient.
6. The same policy and producer observations again, and main once more before
   emitting the receipt. Pins are never replaced with values from a later read.

API requests are GET-only, restricted to the fixed repository and enumerated
metadata/content endpoints. Redirects and cookies are disabled, requests time
out after 30 seconds, responses are bounded to 4 MiB, and error messages do not
include HTTP bodies or transport exceptions. JSON preserves integer/string
types, rejects ambiguous fields and invalid UTF-8, and has a depth bound.
Reviewed policy bytes must be sorted, compact canonical JSON with one LF.

## Policy and independent pins

`runtime-manifests/windows-gpu-qualification-ci-policy.json` has schema 1, kind
`windows_gpu_qualification_ci_policy`, fixed repository/ref/producer bindings
and initially `approved_campaigns: []`. **Every production request is denied.**
The reserved producer path is
`.github/workflows/windows-gpu-performance-capture.yml`; no acquisition workflow
is supplied or approved by this change. Neither ordinary PR test results nor
the existing diagnostic observer is an approved producer.

A future independently reviewed campaign entry contains exactly:

- `artifact_id`, `artifact_sha256` and `authorization_sha256`;
- `campaign_nonce` and `performance_contract_sha256`;
- `issued_at_unix_seconds` and `expires_at_unix_seconds`;
- `producer_run_id`, `producer_run_attempt` and `producer_workflow_sha256`;
- `source_revision`.

IDs are canonical positive decimal strings; digests and the nonzero nonce are
lowercase hexadecimal. Times are integer UTC Unix seconds. This policy is a
separate, short-lived **metadata inspection approval**, not the evaluator's
campaign-signature authority and not permission to execute or sign a capture.
Because inspection follows acquisition, an entry can bind an earlier immutable
producer revision without rebuilding workers at the later policy revision.

Manual dispatch supplies only run ID, attempt and artifact ID. The remaining
pins come from separately reviewed environment variables:

- `SCRIBE_GPU_CAPTURE_SOURCE_REVISION`;
- `SCRIBE_GPU_CAPTURE_ARTIFACT_SHA256`;
- `SCRIBE_GPU_CAPTURE_CONTRACT_SHA256`;
- `SCRIBE_GPU_CAPTURE_CAMPAIGN_NONCE`;
- `SCRIBE_GPU_CAPTURE_AUTHORIZATION_SHA256`.

Those variables alone cannot approve anything: every pin must also match the
current reviewed policy. This change creates no environment, variables, runners,
keys, campaign approval or production state, and does not dispatch the workflow.

## Pre-acquisition signed-contract admission

The evaluator also has a mutually exclusive `CaptureAdmission` parameter set.
It checks approval before capture evidence exists, using the exact existing
`windows_gpu_performance_capture_contract` projection and the same P-256,
source/identity, rule and checked-in contract validators as evidence evaluation.
It does not synthesize a plan with placeholder evidence hashes.

The canonical request has exactly schema 1, kind
`windows_gpu_performance_capture_admission_request`, `contract` and
`authorization`. `contract` is the existing 14-field signed projection,
including ordered `required_lane_identities`, not `required_lanes` or final
evidence digests. `authorization` is the existing campaign authorization
envelope. Independent contract, authorization and nonce pins are mandatory:

```powershell
pwsh -NoProfile -NonInteractive -File .\scripts\qualify-windows-gpu-evidence.ps1 `
  -CaptureAdmissionPath <canonical-request.json> `
  -ExpectedPerformanceContractSha256 <reviewed-contract-sha256> `
  -ExpectedAuthorizationSha256 <reviewed-authorization-sha256> `
  -ExpectedCampaignNonce <reviewed-campaign-nonce>
```

Admission rejects empty, duplicate or unsorted lane matrices, incompatible
worker/pack/provider identities and altered source, evaluator, toolchain or base
Auto-policy bindings. It verifies the separate approval authority, distinct
capture/approval keys, domain-separated signature, epoch and at-most-seven-day
validity window, then checks expiry again before output. Production also
requires a clean checkout at the exact frozen source. This local Git check is
not independent GitHub protected-main provenance; the future controller must
combine the separate boundaries, not trust either receipt as a credential.

Signed lane acquisition/control declarations are **requirements for the future
producer to enforce and compare with actual observations**, not measured facts
established by admission. This does not establish isolation, no throttling,
instrumentation accuracy, memory-admission semantics or frozen input custody.

The bounded canonical stdout result has kind
`windows_gpu_performance_capture_admission`, the exact digests, authority digest,
nonce, source and lane count. `authorization_valid` describes signature/contract
validation only. `acquisition_started`, `capture_authenticated`,
`nonce_consumed`, `signing_authorized`, `auto_eligible` and `release_approved`
are all false. The result is replayable data, not execution or signing authority;
an executor must reauthenticate and claim a durable nonce before acquisition.
No executor or signer consumes it yet.

Evidence/artifact paths, review records and `RequireEligible` cannot be combined
with this parameter set. No capture-selected artifact is opened and no worker,
download, signer, ledger or GitHub mutation is invoked. Rejections after request
binding emit one fixed error, without input contents, private paths or native
diagnostics. Existing evidence-mode behavior and diagnostics remain separate.

The existing fixture switches remain restricted to explicitly fixture-only
contracts; fixture results identify their trust as `fixture_only`. Supplying a
fixture clock for production is rejected, and the empty checked-in campaign
authority continues to deny all production approvals. No key is provisioned.

## Data-only provenance receipt

After all checks, the script emits one bounded canonical JSON document to
stdout. It writes no file, downloads or extracts no artifact, executes no
candidate, and mutates no GitHub state. The receipt binds the exact policy
digest, controller revision, approved pins and observed producer metadata.
It carries these explicit false claims:

`archive_contents_verified`, `capture_authenticated`, `nonce_consumed`,
`signing_authorized`, `auto_eligible`, and `release_approved`.

Artifact metadata is not evidence-content validation. Matching contract and
authorization pins does not prove that an archive contains them, that its
authorization signature is valid, or that a capture happened. A receipt can be
replayed and is deliberately not a signature, credential or approval for a
later job. Consumers must check successful process exit and complete JSON, and
independently reauthenticate current policy/provenance before exercising any
future authority. They must authenticate downloaded bytes and exact inventories
before interpreting contents. No signer consumes this receipt today.

The workflow logs only this metadata receipt, not private hardware observations,
audio, transcript text, user paths or raw diagnostics. Public artifact metadata
or these public policy pins must never be treated as secret capabilities.

## Remaining protected-CI implementation

The next acquisition unit must implement reviewed measurement controls and
source-defined admission semantics using frozen workers and authenticated
inputs. Existing unsigned diagnostic observation/campaign reports must not be
converted, padded or relabeled as qualification evidence. A separate protected
controller must authenticate the acquisition origin before any signer is used.
Pre-acquisition admission removes the future-evidence-hash dependency; it does
not implement those measurements or authorize skipping their remaining gates.

Later units must add durable atomic one-time campaign claim/consumption with
crash and retry behavior; an independently pinned, GPU-free capture signer;
protected ephemeral GPU-runner and environment provisioning; and the real
hardware campaigns. Ordinary Actions artifacts/caches and job concurrency are
not a durable nonce ledger. Signing arbitrary supplied JSON is not acquisition.
No ledger service, cloud account, paid runner or key custody is provisioned here.

Candidate construction, exact final-installer qualification and separately
approved release publication remain later gates. Existing punctuation-exact
parity, five cold/twenty warm runs per power and inclusive 110% CPU-relative p95
limits are unchanged. Runtime Auto and both qualification/key authorities remain
empty/default-deny. This infrastructure does not resolve the outstanding native
startup or transcript/performance defects.

## Verification and rollback

The canonical credential-free inner-loop and CI command is:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-qualification-preflight.ps1
```

It executes the real provenance core with mocked API responses and time, and
the real HTTP/parser boundary with an in-memory HTTP handler. No network,
credentials, GPU, executable fixture, persistent output or production state is
used. Positive fixtures do not populate the checked-in policy. Before readiness
also run the existing full evaluator contract suite:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-qualification.ps1
```

That full suite also executes the pre-capture admission group. Its faster
inner-loop command is the same script with `-CaptureAdmissionOnly`; it uses
synthetic signatures and requests without creating capture artifacts or changing
production authorities. This shortcut is not the full regression gate.

Real protected-environment approval, runner isolation, acquisition and signing
are not established by these tests. No production dispatch can succeed with the
checked-in empty policy. Disable future inspection by emptying its reviewed
campaign list or removing the manual workflow; neither action changes installed
Auto policy, pack trust or inference behavior. No migration or rollback of user
data is involved.

GitHub's primary references describe the [artifact metadata API](https://docs.github.com/en/rest/actions/artifacts),
[workflow-run API](https://docs.github.com/en/rest/actions/workflow-runs), and
[self-hosted runner security risks](https://docs.github.com/en/actions/reference/security/secure-use).
