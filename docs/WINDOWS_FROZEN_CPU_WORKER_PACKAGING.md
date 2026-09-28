# Local frozen CPU-worker packaging

This development-only path separates building the Windows CPU inference worker
from building its desktop and portable bundle. It preserves the exact worker
bytes for later capture and candidate-installer work. It does not implement the
complete performance-candidate flow, grant production artifact trust, embed a
candidate Auto policy, sign anything, or authorize publication.

The normal `build-windows-release.ps1` invocation remains unchanged: it builds
the CPU worker first, hashes it, and then builds the desktop with that exact
bundled-worker digest. Frozen mode is explicit and local-only; hosted CI must
not use it as a release input.

## Integrity and authority

`new-windows-frozen-cpu-worker.ps1` builds the CPU worker from the current clean
checkout using the existing locked, offline release command. Its output binds
the actual source revision and build contracts, application version, Windows
target, worker ABI, private-protocol version, desktop and worker build IDs, and
the worker's exact size and SHA-256. These are source/build-contract identities,
not independently observed worker capabilities.

The producer and frozen consumer bind `SCRIBE_BUILD_REVISION` to the actual
checkout revision instead of inheriting an unrelated caller override. Temporary
build environment values are restored when the operation finishes or fails.

The record is **unsigned local integrity data**, not authenticated producer
provenance. A clean Git checkout, a matching hash, or a locally successful
build does not prove protected-branch membership, trusted runner identity, or
permission to publish. This path assumes a trusted local operator and workspace;
it is not an isolation boundary against hostile concurrent filesystem writers.
Do not accept someone else's freeze directory merely because its self-declared
hashes agree.

The consumer resolves only the fixed sibling `scribe-inference-worker.exe`
named by `windows-frozen-cpu-worker-record.json`. There is no separate worker
path, digest, ABI, protocol, or build-ID override. It validates the record and
source context before invoking Cargo, retains the worker input through desktop
construction and staging, embeds its exact digest, and verifies the copied
bytes. Invalid, changed, missing, or incompatible inputs fail rather than
causing a replacement worker build.

The freeze directory has exactly three files: the worker, its JSON record, and
`WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt`. The record is bounded to 64 KiB and
the worker to 2 GiB; these are format limits, not expected artifact sizes.
The local bundle marker retains the consumed record digest and worker/source
identity so later tooling can bind the same integrity data without treating it
as authorization.

## Local use

Use a clean checkout and the repository's existing pinned Windows x64 build
toolchain and offline dependencies. Choose new output directories outside the
checkout and Cargo target trees. Do not use existing release artifacts as
scratch output or remove them automatically to make a command succeed.

```powershell
pwsh -NoProfile -File scripts/new-windows-frozen-cpu-worker.ps1 `
  -OutputDirectory C:\ScribeLocal\worker-freeze

pwsh -NoProfile -File scripts/build-windows-release.ps1 `
  -ModelSource C:\ScribeLocal\inputs\whisper-base.en-Q8_0.gguf `
  -BundlePath C:\ScribeLocal\local-frozen-bundle `
  -FrozenCpuWorkerRecordPath C:\ScribeLocal\worker-freeze\windows-frozen-cpu-worker-record.json
```

The producer builds only the CPU worker. The frozen consumer builds only the
desktop; it does not rebuild or replace CPU/GPU workers. The pinned bundled
model and ordinary verified GPU-pack boundaries remain unchanged. Empty
production pack trust still prevents production GPU-pack use.

The frozen consumer's default installer-allowlist output is transient within
its owned staging directory, so this local operation does not create a sidecar
in the clean source checkout. Explicit output paths must not overlap the freeze
directory. The normal builder's default installer-allowlist output is unchanged.

## Fixed-artifact LOCAL test installer

`build-windows-frozen-test-installer.ps1` consumes a completed frozen bundle;
it never invokes Cargo or rebuilds the desktop, CPU worker, model, or worker
packs. It accepts only the repository-pinned Inno Setup provenance and rejects
an `ISCC.exe` whose filename, size, or SHA-256 differs from that record. The
builder validates the frozen record, exact CPU-worker bytes, bundle inventory,
model manifest/model, legal files, PE imports, worker-pack layout, and all
paths before copying to a private staging directory. It keeps read handles over
the staged payload while Inno runs, validates staging and the original bundle
again afterward, then publishes exactly two sibling files: the installer and
`windows-local-frozen-test-installer-record.json`.

Compilation has a fifteen-minute deadline with process-tree termination and
bounded stream draining. A timeout publishes no output and cleans owned staging;
the caller can retry with the same unused output directory. Installer and smoke
processes keep their separate one-minute deadlines. Numeric integrity fields
require actual JSON integer scalars rather than coercible strings or booleans.

Use a new, non-existent output directory outside the checkout, bundle, and
freeze. The freeze record binds the current clean source revision, so a source
change requires a new freeze and a new bundle; do not rebind a previously
captured PR artifact to a later checkout.

```powershell
pwsh -NoProfile -File scripts/build-windows-frozen-test-installer.ps1 `
  -BundlePath C:\ScribeLocal\local-frozen-bundle `
  -FrozenCpuWorkerRecordPath C:\ScribeLocal\worker-freeze\windows-frozen-cpu-worker-record.json `
  -InnoCompilerPath C:\Users\you\AppData\Local\ScribeDev\inno-6.7.1\ISCC.exe `
  -OutputDirectory C:\ScribeLocal\local-frozen-installer
```

The compiler path is an input location, not a trust override: it must match
`installer/inno-setup-6.7.1-provenance.json`. The builder rejects output that
already exists, overlaps an input or source root, has a stale sibling staging
directory, or traverses a reparse point. It never overwrites or removes a
caller-selected output.

The generated installer has a separate local-only AppId and an immutable,
token-bound default location:

```text
%LOCALAPPDATA%\Scribe\LOCAL-Frozen-Test\<generated-lowercase-token>
```

It rejects `/DIR`, an existing destination, and any reparse-point destination
ancestor. It does not maintain, repair, update, or overwrite a stable Scribe
installation. It has no shortcuts, auto-launch, program-group entry, shared
uninstall registration, or `CloseApplications` behavior, so it does not close
unrelated Scribe instances. This distinguishes program files only: it does
**not** prove that runtime user-data settings, logs, or other per-user state are
isolated from the normal application.

After an actual local build, pass the two sibling outputs to the separate
verifier:

```powershell
pwsh -NoProfile -File scripts/verify-windows-local-frozen-test-installer.ps1 `
  -BundlePath C:\ScribeLocal\local-frozen-bundle `
  -FrozenCpuWorkerRecordPath C:\ScribeLocal\worker-freeze\windows-frozen-cpu-worker-record.json `
  -InstallerPath C:\ScribeLocal\local-frozen-installer\Scribe-LOCAL-Frozen-Test-<token>.exe `
  -InstallerRecordPath C:\ScribeLocal\local-frozen-installer\windows-local-frozen-test-installer-record.json
```

The verifier derives the installation path from the validated local record;
there is no caller-selected stable installation root. It installs silently,
checks exact installed payload parity, runs the installed CPU worker's normal
offline handshake/cancellation smoke, and only then invokes the expected
uninstaller. Inno may finish its foreground uninstaller before its second-phase
directory cleanup, so the verifier polls the derived token-bound root with a
five-second monotonic deadline instead of treating that short delay as a
failure. If parity was not established, it deliberately retains the token-bound
installation for inspection rather than executing an untrusted uninstaller.
Its own bounded temporary log directory is cleaned after use. The real manual
gate must also record refusal of `/DIR`, an existing destination, and a
reparse-point destination before relying on this test installer.

This is a LOCAL test format, not a release format. It adds no new GPU-pack
trust path: a bundle that contains already-verified signed GPU packs must still
pass the existing catalog, inventory, and compiled-descriptor checks. Current
actual testing is CPU-only. The unchanged production release verifier continues
to reject the local-only marker; there is no flag or manifest rewrite that
admits it to a production release.

## Local validation versus release acceptance

A successful local bundle must pass the builder's exact file inventory, PE and
import checks, input/copy digest checks, and existing offline
`--scribe-install-smoke-parent` CPU smoke. That smoke uses the ordinary
authenticated worker handshake and verifies runtime compatibility and
cancellation. Frozen mode must not bypass or mock it during real execution.

The local bundle includes an inventoried
`WINDOWS-FROZEN-CPU-WORKER-LOCAL-ONLY.txt` marker and local-only documentation.
The unchanged production `verify-windows-release-package.ps1` requires its
fixed release allowlist and therefore rejects this bundle. Local validation
success and production rejection are both required; there is no release
verifier switch that admits the local marker. Do not delete the marker or
rewrite inventories to pass a release gate.

The marker is not a cryptographic restriction against a trusted operator who
rewrites artifacts. Its purpose is to keep the supported local path distinct
from the protected release path. A future authenticated controller must verify
producer provenance and authorize candidate construction separately rather
than treating this local record as that authority.

## Verification and remaining work

The canonical offline script suite remains:

```powershell
pwsh -NoProfile -File scripts/test-windows-release-packaging.ps1
```

Synthetic build/process tests establish orchestration and failure contracts;
they are not evidence that a real worker or installer was built. A real local
freeze, desktop build, staged smoke and byte comparison must be recorded
separately. No real GPU inference, hardware performance qualification,
candidate-policy embedding, production signing, or publication is established
by this slice. `test-windows-local-frozen-test-installer.ps1` uses a small
locally compiled `ISCC.exe` process seam (requiring the built-in Windows .NET
Framework C# compiler) to test input binding, output transactions, compiler
failure, staged mutation, source revalidation, bounded process timeout,
actual builder compiler-timeout cleanup/retry (with a shorter fixture-only deadline),
local-record binding, payload parity, exact installed-smoke arguments (including
spaces and Unicode through a real child process), cancellation diagnostics, delayed removal,
and unchanged production rejection. It does not replace the pinned Inno
build/install/uninstall gate.

The [performance-candidate roadmap](WINDOWS_GPU_PERFORMANCE_CANDIDATES.md)
still requires qualified acquisition controls and admission semantics,
authenticated capture and nonce custody, candidate-policy construction,
actual-installer qualification, and explicit unchanged-artifact promotion.
Those stages must preserve the exact measured worker artifacts rather than
silently rebuilding them.

Disablement is simply to omit `-FrozenCpuWorkerRecordPath` and use the unchanged
normal builder. Preserve frozen artifacts while they are needed; cleanup is an
explicit local decision, not part of fallback or validation failure handling.
