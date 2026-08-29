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
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" "/DAppVersion=$version" installer\scribe.iss
Copy-Item "dist\Scribe-Setup-$version.exe" dist\Scribe-Setup.exe -Force
.\scripts\verify-windows-release-package.ps1 -BundlePath dist\portable -InstallerPath dist\Scribe-Setup.exe
```

The Inno Setup compiler first writes `dist\Scribe-Setup-<version>.exe`; the
normalized release asset is `dist\Scribe-Setup.exe`. Do not distribute a bare
`local-transcriber.exe` or `scribe-inference-worker.exe`: the installer must
include the complete staged payload, with both executables adjacent.
The scripts download the exact pinned runtime/model sources and verify their
sizes and SHA-256 values before they are staged.

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
- **Manual publication is skipped:** rerun from the repository default branch
  and explicitly enable `publish_release`; disabled dispatches only validate.
- **Tag or release already exists:** do not overwrite it. Confirm the existing
  release is valid or increment `Cargo.toml` to a new version and publish that.
  If it is an orphan tag from a failed manual publication, use the recovery
  checks above before a maintainer deletes that specific tag and reruns.
- **Pages does not deploy:** confirm Pages is set to GitHub Actions and that the
  documentation change has reached `main`.
