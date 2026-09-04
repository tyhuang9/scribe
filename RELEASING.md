# Releasing Scribe for Windows

Scribe's canonical application version is the `version` field in the root
`Cargo.toml`. A stable GitHub release must use an exact matching tag: application
version `0.2.0` becomes tag `v0.2.0`.

## Build a local installer

Use Windows x64 with the Rust 1.96.0 toolchain, Visual Studio 2022 C++ build
tools, CMake, and Inno Setup 6 installed. From the repository root:

```powershell
$archiveName = 'sherpa-onnx-v1.13.5-win-x64-static-MT-Release-lib.tar.bz2'
$archiveDir = Join-Path $PWD '.ci-native'
$archivePath = Join-Path $archiveDir $archiveName
New-Item -ItemType Directory -Force -Path $archiveDir | Out-Null
curl.exe --fail --location --retry 3 --retry-delay 2 --output $archivePath "https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.5/$archiveName"
if ((Get-Item -LiteralPath $archivePath).Length -ne 120217991) { throw 'Unexpected sherpa-onnx archive size' }
if ((Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant() -ne 'b7080b6f470bac96ef0afe56b25ae9b2f9f0ca82d10dad19bf3a2fc5ffd6cffc') { throw 'Unexpected sherpa-onnx archive SHA-256' }
$env:SHERPA_ONNX_ARCHIVE_DIR = $archiveDir
.\scripts\prepare-windows-release-inputs.ps1 -OutputDirectory .release-inputs
.\scripts\build-windows-release.ps1 `
  -ModelSource .release-inputs\model\whisper-base.en-Q8_0.gguf `
  -BundlePath dist\portable
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches.Groups[1].Value
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" `
  "/DAppVersion=$version" `
  "/DWorkerPackAllowlist=..\dist\worker-pack-allowlist.iss" `
  installer\scribe.iss
Copy-Item "dist\Scribe-Setup-$version.exe" dist\Scribe-Setup.exe -Force
.\scripts\verify-windows-release-package.ps1 -BundlePath dist\portable -InstallerPath dist\Scribe-Setup.exe
```

The Inno Setup compiler first writes `dist\Scribe-Setup-<version>.exe`; the
normalized release asset is `dist\Scribe-Setup.exe`. Do not distribute a bare
`local-transcriber.exe` or `scribe-inference-worker.exe`: the installer must
include the complete staged payload, with both executables adjacent.
The scripts download the exact pinned runtime/model sources and verify their
sizes and SHA-256 values before they are staged.

Every Stage 3 package contains `worker-pack-catalog.json` with an empty `packs`
array. No CUDA, Vulkan, or Metal production pack is shipped. The release builder
accepts future prebuilt, pre-signed roots through `-WorkerPackRoot`, but it runs
the compiled production verifier before and after staging each root into
`workers/packs/<pack-id>/<version>/<digest>/`. Because no production pack public
key is provisioned yet, every non-empty declaration currently fails closed.
Signing is not performed by repository release scripts. Future private signing
material must remain inside a separately reviewed signer or HSM, must never be
passed to source-checkout code, and must match a separately reviewed persistent
public key.

`Promote Windows GPU worker packs` is the fail-closed contract for that future
path. Run it manually from the default branch with `promote` enabled and a
canonical pack version. The unprivileged builder produces exactly one prepared
CUDA pack and one prepared Vulkan pack with no signatures, then uploads a
one-day handoff artifact. The handoff binds repository/ref/source SHA, workflow
ref, run ID and attempt, pack version, pinned toolchain-manifest SHA-256, both
manifest and pack digests, and a release-set digest. GitHub's artifact upload
returns an artifact ID and digest; the digest-pinned download action validates
the transfer before the protected boundary, and the future privileged broker
must bind both values as part of its authorization context.

The protected job requires approval through the
`windows-gpu-pack-signing` environment and a fresh
`scribe-gpu-pack-signer-ephemeral` runner using Actions Runner 2.327.1 or later
(required by the pinned Node 24 artifact action). It performs no checkout or
compile, runs no repository script, and receives no raw private key. The
independently installed executable is an unprivileged broker client, not a
signer. Its digest is pinned in protected environment configuration and the
workflow holds a read-only, no-write/delete handle from hashing through child
exit to prevent hash-to-exec replacement. The client has no key, ledger, state
path, configurable broker endpoint, or fixture mode. A separately privileged
Windows service or remote HSM broker must copy hostile input into broker-owned
storage, enforce the approved toolchain/version/security epoch, reject replay
with independently durable state, sign and verify both packs, and publish only
the complete CUDA+Vulkan pair plus a protected receipt. The
resulting artifact is not activated or included in the normal release
automatically.

The independently locked `tools/windows-gpu-promotion-broker` workspace defines
the exact request schema and a test-only hostile-input state-machine proof. Its
fixture implementation uses retained no-write/delete file handles, no-follow
final opens, exact bounded inventories, a domain-separated signed receipt, a
hash-chained reserve/ready/published ledger, and write-through atomic pair
publication. The fixture seed and ledger code are compiled only under
`cfg(test)` and are absent from the normal client artifact. Windows does not
provide a stable handle-relative traversal API through Rust's standard library;
because this proof cannot establish service ACLs or full NT handle-relative
traversal, it is not production authority.

The checked-in client intentionally attempts no IPC. Connecting it to a fixed,
authenticated service/HSM endpoint and provisioning that service are separate
security-reviewed release work; an endpoint supplied by CLI or environment is
not accepted.

This path is currently a NO-GO: `ProductionTrustRoot`, the privileged broker or
HSM, its independently durable epoch/replay authority, and the CUDA production
inventory are not provisioned. The protected job checks that state and stops
before invoking the client, so a failed run cannot access broker or signing
authority. Validate only the fixture contracts locally with:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-pack-promotion.ps1
cargo test --locked --offline --manifest-path tools/windows-gpu-promotion-broker/Cargo.toml -- --test-threads=1
```

Windows GPU Auto activation also requires the independent offline evidence
gate documented in `docs/WINDOWS_GPU_QUALIFICATION.md`. The checked-in plan has
no representative hardware lanes, `runtime_bucket_complete` is false, the
production approval authority is empty, and the Windows Auto manifest has no
entries. Release validation runs only the synthetic contract suite:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-qualification.ps1
```

The evidence gate requires lane-level ECDSA P-256 capture attestation and one
exact request/Ready SCIF v5 frame pair for every measured worker generation.
The signed raw captures bind the exact app/worker, protocol/ABI, provider/pack,
stable device identities, and transient index mapping. A separate discovery
launch binds the complete provider-eligible device list; measured launches are
CPU-only or narrowed to the selected stable device. Mixed-GPU evidence proves
that the same stable device remaps across fresh challenges and different
process indexes. Enumeration index `0` is never persisted as identity.

A future hardware decision is non-authoritative until its exact reviewed plan
and capture public key are approved, a protected capture signer and nonce
ledger exist, its projected runtime bucket has representative coverage, its
Auto entries are separately reviewed and checked in one-for-one, and production
pack trust/signing is provisioned. Those protected capture services are not
built in this stage, so real production qualification remains a NO-GO. Do not
treat fixture output, a one-machine projection, or successful explicit-GPU
smoke as release qualification.

## Publish a version from a tag

1. Update the root `Cargo.toml` version and any appropriate release notes.
2. Run the local validation and installer build above.
3. Commit and push the version change through the normal review process.
4. Create and push the matching tag:

   ```powershell
   git tag v0.2.0
   git push origin v0.2.0
   ```

5. The `Build Windows installer` workflow validates formatting, clippy, tests,
   downloads verified inputs, builds the full staged payload and Inno Setup
   installer, verifies the installed payload, and uploads `Scribe-Setup.exe`
   and `Scribe-windows-x64.zip`.
   For a matching exact semantic tag, its release job validates the tag against
   `Cargo.toml`, then creates the GitHub Release with generated notes and that
   exact installer and portable ZIP asset pair.

The permanent latest-installer URL is:

<https://github.com/tyhuang9/scribe/releases/latest/download/Scribe-Setup.exe>

Previous versions remain available at:

<https://github.com/tyhuang9/scribe/releases>

## Manual validation and publication

Open **Actions → Build Windows installer → Run workflow**. The
`publish_release` input defaults to `false`; leave it disabled for a
validation-only build. That run performs the complete build and packaging
checks and creates a temporary `windows-release-assets` workflow artifact, but
it cannot create a GitHub Release.

To publish without pushing a tag, select the repository's default branch and
explicitly enable `publish_release`. Publication from any other branch is
blocked. The workflow derives the tag as `v<package version>` from the checked
out root `Cargo.toml` and targets exactly the default-branch commit identified
by the workflow's `GITHUB_SHA`; there is no user-supplied release tag or target.
After validating both assets, the workflow verifies the release-tag protection
prerequisite below. It then atomically creates that tag at the exact commit and
verifies the remote ref before creating the release. Tag-triggered runs also
verify that the pushed tag resolves to the workflow commit. One publication job
runs at a time and GitHub holds up to 100 additional pending jobs; attempts over
that platform queue limit can be canceled. The workflow then creates a stable,
non-draft, non-prerelease GitHub Release containing exactly `Scribe-Setup.exe`
and `Scribe-windows-x64.zip`, so the README's
`releases/latest/download/...` links remain permanent.

### Required release-tag ruleset

Before publishing, a repository administrator must configure this prerequisite
out of band in **Settings → Rules → Rulesets** after receiving explicit approval
for the repository setting change. The workflow only verifies the setting; it
does not create or modify repository rulesets.

Create exactly one repository tag ruleset with this contract:

- Name: `Protect release tags`
- Enforcement status: **Active**
- Target: **Tags**
- Ref-name inclusion: exactly `refs/tags/v*`
- Ref-name exclusions: none
- Rules: **Restrict updates** and **Restrict deletions** enabled; do not restrict
  creation
- Bypass list: empty

This permits creation of a new matching release tag while preventing that tag
from being moved or deleted after creation. Publication fails closed if the
ruleset is missing, duplicated, inactive, ambiguous, unreadable, or differs from
this contract. The repository must be configured separately before the first
publish-enabled run can succeed.

Manual publication refuses to proceed if either the derived tag or its GitHub
Release already exists. It does not replace assets, move tags, or otherwise
clobber a prior release. If a run fails before publication, fix the underlying
validation or packaging problem and rerun it. If it fails after creating a tag
but before publishing the release, the atomic tag can remain as an orphan and a
rerun will intentionally refuse to overwrite it. Inspect the tag and release
state before taking action; prefer correcting the version in `Cargo.toml` and
publishing a new version. To retry the same version, a repository maintainer
must first confirm that no release exists, verify that the orphan tag points to
the intended commit, and obtain explicit approval for a repository administrator
to temporarily change the protective ruleset, delete only that tag, and restore
the exact active ruleset contract before rerunning. Only a repository
administrator should delete an erroneous release or tag as an explicit
rollback, after preserving any needed assets and confirming that no users or
automation depend on that version.

## GitHub Pages

The documentation is deployed by `.github/workflows/docs.yml`. Once per
repository, open **Settings → Pages** and set **Source** to **GitHub Actions**.
The default project URL is <https://tyhuang9.github.io/scribe/> and contains the
same permanent download link.

## Signing

The installer is currently unsigned. Windows may show a SmartScreen or unknown
publisher warning. Do not claim it is signed or add certificate configuration
until a real code-signing identity and secret-management process are approved.

## Common release failures

- **Tag rejected:** use an exact semantic tag such as `v0.2.0`, with the same
  value as `Cargo.toml`.
- **Pinned input verification fails:** do not bypass it; investigate the source,
  size, and SHA-256 mismatch before retrying.
- **Installer payload verification fails:** rebuild the staged `dist\portable`
  directory; the installer must include every item from `bundle-inventory.json`.
- **A declared GPU worker pack is rejected:** do not bypass verification or add
  a fixture key to production. Confirm the pack was externally signed by a
  provisioned production key, then review its canonical manifest, detached
  signature, target/build compatibility, and complete payload inventory.
- **Manual publication is skipped:** rerun from the repository default branch
  and explicitly enable `publish_release`; disabled dispatches only validate.
- **Tag or release already exists:** do not overwrite it. Confirm the existing
  release is valid or increment `Cargo.toml` to a new version and publish that.
  If it is an orphan tag from a failed manual publication, use the recovery
  checks above before a maintainer deletes that specific tag and reruns.
- **Pages does not deploy:** confirm Pages is set to GitHub Actions and that the
  documentation change has reached `main`.

## macOS Metal release packaging

macOS 13 is the minimum supported OS for the release bundle. Build the universal
application with the default deny-empty Metal catalog for a local structural
validation run:

```bash
bash scripts/build-macos-release.sh \
  --output-directory dist-macos \
  --pack-version 0.1.0 \
  --signing-mode adhoc
bash scripts/verify-macos-release-package.sh --app dist-macos/Scribe.app
bash scripts/test-macos-release-packaging.sh
```

This is not a notarized or hardware-qualified release. It uses ad hoc signing
only to exercise the app layout, universal Mach-O, entitlements, catalog, and
hostile-filesystem checks. It must not be distributed.

An official protected macOS job requires the Developer-ID identity and
notarytool keychain profile via `SCRIBE_MACOS_SIGNING_IDENTITY` and
`SCRIBE_MACOS_NOTARY_PROFILE`. If it is authorized to include Metal packs it
also requires `SCRIBE_PACK_SIGNING_PRIVATE_KEY_PATH` and
`SCRIBE_PACK_SIGNING_KEY_ID`; their values are never passed on the command line
or written to artifacts. First build the per-architecture standalone packs,
then assemble the app, and run:

```bash
bash scripts/sign-notarize-macos-release.sh \
  --app dist-macos/Scribe.app \
  --archive-output dist-macos/Scribe-macos-universal.zip
```

Do not use `codesign --deep`. A Metal pack manifest is generated from the final
Developer-ID-signed worker bytes and must be signed by a separately reviewed
Ed25519 production key matching the desktop trust root. There is no provisioned
production key or qualification evidence in this repository, so an ordinary
release must retain the canonical empty catalog and Auto remains CPU-only.
No macOS artifact is added to the existing Windows release publication contract.
