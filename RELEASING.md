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

Local builds without GPU roots and ordinary CI runs without GPU artifact inputs
remain CPU-only, with an empty `packs` array in `worker-pack-catalog.json`.
The protected installer workflow includes GPU packs only when an exact, complete
signed pair is supplied; a GPU-required release with missing inputs fails closed.
The release builder accepts prebuilt roots through `-WorkerPackRoot` and runs
the compiled verifier before and after staging each root into
`workers/packs/<pack-id>/<version>/<digest>/`.

The shared project-owned Ed25519 public trust root is now provisioned as
`scribe-pack-ed25519-20260929-v1` (the policy pins public-key SHA-256
`0f4a6638632a5f8b81e9802738e31cc05ec46c31339274ed0bd210444c7672ad`). This
bootstrap enables verification only: it does not configure the protected
GitHub environment, reviewer, signer pins, or secret; trigger a signing run;
sign an installer; publish a release; or enable Auto. No production signing
run is claimed here.

Follow the [canonical Windows GPU pack signing guide](docs/WINDOWS_GPU_PACK_SIGNING.md)
for the protected setup and exact commands. The selected path is:

1. Dispatch `windows-gpu-pack-promotion.yml` on `main` with
   `operation=prepare` and the immutable pack version. After the whole run
   succeeds, dispatch it again on `main` with `operation=sign`, the completed
   producer run ID and attempt, and its exact unsigned artifact ID. Unsigned
   and signed pair artifacts are retained for seven days.
2. Preflight and protected signing use an independently pinned GPU-free signer
   bundle. The sign job runs on a fresh GitHub-hosted Windows runner and waits
   for approval through the `windows-gpu-pack-signing` environment. The key is
   supplied only to the trusted signer; the builder never receives it, and the
   protected job does not check out, compile, or execute candidate code.
3. The handoff, approval, and signed receipt bind the exact repository/ref/source
   revision, workflow/run/attempt, artifact IDs and digests, pack version,
   toolchain, manifests, signer pins, and policy. Mismatched, stale, partial,
   or expired inputs fail closed.
4. A signed pair is still only a candidate. Installer inclusion requires the
   exact three GPU inputs to `release.yml`, the reviewed `gpu_packs_required`
   policy, and complete pair re-verification. Start with `publish_release=false`
   to validate the installer without creating a GitHub Release. Publication
   remains a separately authorized action subject to the existing release gates.
   Windows GPU Auto remains separately gated by offline qualification
   evidence and its checked-in default-deny manifest.

Local diagnostic signing under exact approval grants no production signing,
merge, release-publication, or Auto authority.

The previous `tools/windows-gpu-promotion-broker` implementation and
fixture-only `scripts/promote-windows-gpu-worker-packs.ps1` path are
regression fixtures only. Do not provision them or treat their service,
ledger, receipt, or output as production authority. Their existing contract
checks are:

```powershell
pwsh -NoProfile -File .\scripts\test-windows-gpu-pack-promotion.ps1
pwsh -NoProfile -File .\scripts\test-windows-gpu-broker-transport.ps1
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
Auto entries are separately reviewed and checked in one-for-one, and protected
production pack signing has completed successfully. Those protected capture
services are not built in this stage, so real production qualification remains
a NO-GO. Do not treat fixture output, a one-machine projection, or successful
explicit-GPU smoke as release qualification.

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
  a fixture key to production. Confirm the pack was signed through the approved
  signing path by a provisioned project pack key, then review its canonical
  manifest, detached signature, target/build compatibility, and complete payload
  inventory.
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
Ed25519 production key matching the shared desktop trust root. The shared public
trust root is provisioned, but macOS signing, pack release authority, and Metal
qualification remain separately gated; `gpu-pack-release-authority-macos-empty.json`
is still empty, so an ordinary release must retain the canonical empty catalog
and Auto remains CPU-only.
No macOS artifact is added to the existing Windows release publication contract.
