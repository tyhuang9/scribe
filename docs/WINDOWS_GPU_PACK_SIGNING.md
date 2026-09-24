# Maintainer-approved Windows GPU pack signing

This is the selected production signing design. It does **not** require a
Windows service, HSM, commercial certificate, or Authenticode-signed installer.
It authenticates the CUDA/Vulkan pack manifests using a project-owned Ed25519
key. Windows installer signing and GPU pack verification are separate concerns.

## Trust boundaries

1. An unprivileged default-branch build produces a complete unsigned CUDA/Vulkan
   pair and canonical handoff. It never receives the signing key.
2. A separate, secret-free preflight checks a **completed successful** producer
   run, its immutable artifact identity and digest, the source/workflow revision,
   the ordered pair, current signing policy, and a separately pinned signer.
3. A fresh GitHub-hosted Windows job waits for approval of the
   `windows-gpu-pack-signing` environment. It revalidates the same inputs, supplies
   the key only to the trusted signer over standard input, and publishes only a
   fully signed and reverified pair.

The trusted signer bundle is not produced by the GPU candidate build. Its source
revision and byte digests are independently reviewed and pinned. The bundle
contains the GPU-free tool and trusted orchestration wrapper, not a worker or
provider DLL. The protected job does not check out candidate source, compile it,
or run candidate executables. Packs are parsed and hashed as untrusted data.

Approval is bound to exact repository/ref/source, workflow, run/attempt, artifact
ID/digest, handoff and release-set digests, pack version, toolchain, both manifests,
signer identity, and policy digest. The signer derives the application/worker
build IDs from the approved app version and source revision; it does not require
the separately pinned tool to have been built from the candidate revision.

## Initial setup (maintainer action, not performed by the PR)

1. Review and merge the necessary implementation stack through normal PRs. This
   document is not merge approval. Keep the ordinary release CPU-only and Auto's
   qualification manifest empty during bootstrap.
2. Generate a unique Ed25519 key using a trusted offline utility which emits the
   PKCS#8 v2 DER format accepted by `ring::signature::Ed25519KeyPair`. Record the
   public key and its SHA-256 separately. Do not use the deterministic test key.
   Never paste private key bytes into an issue, PR, chat, terminal transcript,
   build log, repository file, or artifact.
3. Add only the public key and key ID to
   `runtime-manifests/worker-pack-production-trust.json`, and bind the same key
   in `runtime-manifests/windows-gpu-signing-policy.json`, through a reviewed PR.
   The trust file is minified canonical JSON; one final LF is accepted. Its
   entries contain only `key_id` and lowercase 32-byte `public_key_hex`. Pretty
   formatting, unknown fields, and duplicate IDs are rejected. Never add fixture
   keys to production trust.
   A tool/app with the empty initial trust root intentionally cannot authenticate
   production packs. Rebuild the trusted tool after the public-key change.
4. Build the trusted signer bundle through its unprivileged default-branch
   workflow. Review the exact source revision and downloaded bundle bytes;
   configure the resulting immutable artifact and file pins. Do not trust a
   digest supplied by the unsigned candidate or a mutable branch/tag alone.
5. Create the `windows-gpu-pack-signing` GitHub environment with a required
   maintainer reviewer, a deployment rule for the default **branch only**, and
   administrator bypass disabled. Protect that branch against force-push and
   unreviewed policy/workflow changes. Enable prevention of self-review if a
   second maintainer is available; a sole maintainer must otherwise be able to
   approve their own release deliberately.
6. Store the base64 encoding of the private PKCS#8 DER as an **environment
   secret**, never a repository secret. Configure the protected environment's
   independently reviewed signer pins. Do not copy the key into the GPU builder
   or any pull-request workflow. Keep an offline backup under maintainer control.
7. Perform a small approved production run and examine its logs and exact output
   inventory. Until this succeeds, production signing is **unverified**. Signing
   a pair does not by itself authorize installer inclusion or Auto eligibility.

GitHub releases environment secrets only after its required approval. The
workflow's `environment:` declaration does not configure reviewers or branch
rules by itself. See [GitHub environment protections](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)
and [secure workflow guidance](https://docs.github.com/en/actions/reference/security/secure-use).

Configure these nonsecret pins as repository variables for preflight and with
the **same exact values under their separate approved names** in the protected
signing environment. Do not create the approved names at repository scope. The complete
pin-set digest is checked across approval; changing a pin requires a new
preflight, not reuse of an older approval.

| Repository variable | Protected environment variable | Reviewed value |
| --- | --- | --- |
| `SCRIBE_GPU_SIGNER_SOURCE_SHA` | `SCRIBE_GPU_APPROVED_SIGNER_SOURCE_SHA` | Full 40-character signer source commit |
| `SCRIBE_GPU_SIGNER_RUN_ID` | `SCRIBE_GPU_APPROVED_SIGNER_RUN_ID` | Successful signer-build run ID |
| `SCRIBE_GPU_SIGNER_RUN_ATTEMPT` | `SCRIBE_GPU_APPROVED_SIGNER_RUN_ATTEMPT` | Exact successful run attempt |
| `SCRIBE_GPU_SIGNER_ARTIFACT_ID` | `SCRIBE_GPU_APPROVED_SIGNER_ARTIFACT_ID` | Immutable signer-bundle artifact ID |
| `SCRIBE_GPU_SIGNER_ARTIFACT_SHA256` | `SCRIBE_GPU_APPROVED_SIGNER_ARTIFACT_SHA256` | Lowercase artifact SHA-256, without prefix |
| `SCRIBE_GPU_SIGNER_BINARY_SHA256` | `SCRIBE_GPU_APPROVED_SIGNER_BINARY_SHA256` | Lowercase SHA-256 of `scribe-worker-pack-tool.exe` |
| `SCRIBE_GPU_SIGNER_WRAPPER_SHA256` | `SCRIBE_GPU_APPROVED_SIGNER_WRAPPER_SHA256` | Lowercase SHA-256 of `invoke-windows-gpu-approved-signing.ps1` |

The protected environment secret is `SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64`.
Artifact expiry requires publishing and independently verifying a replacement
tool bundle and updating its pins. It must never trigger an automatic fallback
to a different artifact or a freshly built candidate-provided tool.

## Running a release candidate

1. For signer bootstrap or an intentional tool update, dispatch
   `windows-gpu-signer-tool.yml` on `main`. It has no source override and no
   signing secret. Review and pin the resulting two-file bundle as above.
2. Dispatch `windows-gpu-pack-promotion.yml` on `main` with `operation=prepare`
   and the chosen immutable `pack_version`. Wait for the whole run to succeed;
   record its run ID, attempt, and `windows-gpu-unsigned-<run ID>` artifact ID.
3. Dispatch the same workflow on `main` with `operation=sign`,
   `producer_run_id`, `producer_run_attempt`, and `unsigned_artifact_id` from
   that completed run. Preflight validates the pair without the key.
4. Read the preflight summary before approving the protected environment. Check
   the source, policy, signer pins, artifact, and both backend manifest/digests
   against the intended release. Reject unexpected or stale inputs.
5. After success, retain the complete `windows-gpu-signed-<run ID>-<attempt>`
   artifact, including `windows-gpu-pack-signing-receipt.json`. This is a signed
   candidate, not a published installer or an Auto qualification decision.

Unsigned and signed pair artifacts expire after seven days; the trusted-tool
artifact expires after 90 days. Do not use the download of an expired/missing
artifact as authorization to substitute another build.

## Including an approved pair in the Windows installer

After the setup and a real protected signing run have succeeded, the existing
Windows installer workflow can consume the signed pair without receiving the
private key. Ordinary pull requests, pushes, tags, and manual runs without GPU
inputs remain CPU-only (or fail when an official GPU-required release has no
eligible inputs). Nothing in this adapter provisions trust or changes Auto.

1. Keep the installer checkout at the exact source revision recorded in the
   signed receipt. If `main` has advanced beyond that candidate, prepare and
   sign a new candidate; the workflow will not silently select an older source.
2. Through maintainer-controlled repository configuration, select
   `SCRIBE_GPU_PACK_RELEASE_POLICY=gpu_packs_required`. Keep the independently
   reviewed public signer pins configured as described above. This document and
   its PR do not change that repository setting or authorize publication.
3. Dispatch `release.yml` on `main` with all three string inputs:
   `gpu_signing_run_id`, `gpu_signing_run_attempt`, and
   `gpu_signed_artifact_id`. These identify the **completed signing run** and its
   `windows-gpu-signed-<run ID>-<attempt>` artifact, not the unsigned producer.
   Start with `publish_release=false` to validate an installer without creating
   a GitHub Release. Partial inputs, candidate branches, and other repositories
   are rejected. GPU-required tag releases do not infer an artifact automatically.
4. Require the entire workflow to pass, including native verification of the
   complete signed pair, both staged pack identities, per-pack size reporting,
   installer maintenance checks, and portable/installer payload parity. The job
   reports GPU inclusion only after the generated catalog contains the exact
   verified CUDA/Vulkan pair. A failed second backend cannot produce a successful
   GPU installer or silently change the request to CPU-only packaging.
5. Perform the clean-machine and real-hardware acceptance checks before an
   explicitly authorized publishing run. Build/test success does not establish
   CUDA/Vulkan performance qualification or grant Auto eligibility.

The controller validates both the chosen signing run/artifact and, independently,
the original unsigned producer identities inside the receipt. These run IDs and
artifact IDs are deliberately different. It rejects rerun/stale attempts,
expired or mismatched artifacts, changed policy or signer pins, and incompatible
source/toolchain identities. Only fixed `cuda` and `vulkan` roots returned after
whole-pair native verification reach the existing pack staging path. No downloaded
worker/provider is executed during input verification. Staging still invokes
the compiled desktop verifier before and after copying each pack. Fresh current
policy/provenance checks also precede asset upload and release publication.

The keyless native command is `verify-signed-windows-set --signed-root <path>
--policy <path> --toolchain-manifest <path>`. Its production entry point has no
fixture-key override. Offline tests inject private test trust only inside the
Rust test module; mocked workflow/process tests are not production-signing or
clean-machine hardware evidence.

Rollback: return the repository policy to the explicit
`temporary_cpu_only_stage4` setting and omit all GPU artifact inputs, or revert
the focused adapter PR. Do not revoke pack signatures, lower security epochs,
or enable Auto as part of disabling new installer inclusion.

## Epochs, retries, and stale approval

Each candidate's security epoch must **equal** the currently reviewed policy
epoch for its exact pack ID. Neither an older epoch nor an unreviewed future
epoch is accepted. A policy change must retain both pack identities and never
decrease the global floor or either pack's epoch. CI checks against an immutable
base commit using `scripts/test-windows-gpu-signing-policy.ps1`.

The authoritative default-branch policy is fetched again after approval and
before publication. A changed policy invalidates that approval; begin a new
preflight instead of silently rebinding it. Branch protection is part of this
trust model: repository administrators who can rewrite policy history or replace
the protected secret/pins remain trusted administrators.

Retries of identical approved inputs into a fresh output location are allowed.
Ed25519 signatures are deterministic. This design does not claim global one-time
signing, reserve a release in a remote ledger, or forbid another release within
the same security epoch. A security epoch is a revocation floor, not a release
counter. The client's embedded minimum and persistent per-pack epoch floor
provide additional installation/rollback enforcement.

## Failure behavior and recovery

- Wrong source, artifact, signer, policy, key, app/worker identity, protocol, ABI,
  backend/provider, or epoch: reject; do not publish a signed pair.
- Corrupt, missing, extra, linked, traversal/ADS, or changed payload: reject. The
  existing bounded physical inventory and signature verifier remain mandatory.
- Failure while signing the second pack: no complete publication. Never upload
  an intermediate staging directory or a lone signed backend.
- Existing output: do not overwrite. Retry with a clean, fresh job/output.
- Key compromise: stop approvals, remove the affected secret, rotate public
  trust through review, raise the appropriate security epochs, and rebuild the
  app/signer. Deleting a GitHub secret does not revoke already issued signatures.
- Disablement: stop production signing dispatches. CPU stays usable; leave GPU
  release and Auto qualification policies unchanged until separately verified.

Do not log decoded secrets or forward child diagnostics indiscriminately. The
private key is never a process argument, file in the pack tree, or uploaded
artifact. In-memory handling limits exposure; it is not an HSM and does not
protect against a compromised approved runner or trusted signer.

PowerShell bootstrap path checks and retained leaf handles do not provide
atomic no-follow opens or pin path ancestors. Preflight, signing, and the
installer input controller rely on fresh trusted hosted jobs, no candidate
code execution during input verification, and no untrusted
concurrent writers. Do not move these steps onto a persistent/shared builder.
The native signer's bounded physical inventory verification remains mandatory;
the PowerShell checks are not a substitute for it.

## Verification and scope

Run the locked GPU-free Rust tests, existing promotion contract tests, policy
transition tests, and approved-signing orchestration tests. Fixtures use test-only
trust and do not establish production key custody or GitHub environment setup.

The previous `tools/windows-gpu-promotion-broker` proof and fixture-only
`scripts/promote-windows-gpu-worker-packs.ps1` remain unprivileged regression
fixtures. They are not the selected production path and must not be provisioned
to use this workflow. No background service or paid signing product is needed.

Hardware qualification, production installer execution, Auto enablement, Linux/macOS
rollout, and Authenticode signing remain separately gated. Keep temporary
fixture output short-lived and remove only owned scratch directories; retain
the actual source-bound candidate packs until their acceptance work is complete.
