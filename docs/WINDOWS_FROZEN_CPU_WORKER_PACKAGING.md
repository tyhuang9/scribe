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
by this slice.

The [performance-candidate roadmap](WINDOWS_GPU_PERFORMANCE_CANDIDATES.md)
still requires qualified acquisition controls and admission semantics,
authenticated capture and nonce custody, candidate-policy construction,
actual-installer qualification, and explicit unchanged-artifact promotion.
Those stages must preserve the exact measured worker artifacts rather than
silently rebuilding them.

Disablement is simply to omit `-FrozenCpuWorkerRecordPath` and use the unchanged
normal builder. Preserve frozen artifacts while they are needed; cleanup is an
explicit local decision, not part of fallback or validation failure handling.
