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

The frozen build-contract digest also binds the shared native-baseline helper
and both Windows CPU-worker construction scripts. Changing either normal or
frozen CPU-worker assembly therefore requires a new freeze record.

The producer and frozen consumer bind `SCRIBE_BUILD_REVISION` to the actual
checkout revision instead of inheriting an unrelated caller override. Temporary
build environment values are restored when the operation finishes or fails.

Every bundled CPU worker is built in a new, uniquely named Cargo target below
the system temporary directory. Its native build fixes
`TRANSCRIBE_X86_CONSERVATIVE=ON` and `GGML_NATIVE=OFF`, rejects ambient CMake
and compiler-flag overrides, pins Rust to the existing static CRT without host
ISA features, and verifies the single generated transcribe CMake
cache plus the generated `ggml-cpu` compiler flags before the worker is hashed
or copied. This selects the conservative x86-64 CPU floor and can perform less
well than a host-tuned SIMD build. The builder prints the exact retained target
path after validation; it is evidence for the worker and is deliberately not
deleted automatically. Remove it only after retaining the freeze or bundle
record needed for local investigation. The cache/flag check establishes the
build contract; real GGUF execution on the oldest supported Windows x64 CPU is
still the acceptance test for hardware compatibility.

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
packs. It accepts only the repository-pinned Inno Setup 7.1.0 provenance and
rejects an `ISCC.exe` whose filename, size, or SHA-256 differs from that
record. The reviewed x64 compiler removes the previous source-path length
ceiling; both installer templates explicitly select its x86 Setup output to
preserve the current Win32 Pascal ABI. The builder validates the frozen record,
exact CPU-worker bytes, bundle inventory,
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
  -InnoCompilerPath C:\Users\you\AppData\Local\ScribeDev\inno-7.1.0\ISCC.exe `
  -OutputDirectory C:\ScribeLocal\local-frozen-installer
```

The compiler path is an input location, not a trust override: it must match
`installer/inno-setup-7.1.0-provenance.json`. The builder rejects output that
already exists, overlaps an input or source root, has a stale sibling staging
directory, or traverses a reparse point. It never overwrites or removes a
caller-selected output.

The local pre-use guard authenticates only the pinned `ISCC.exe` bytes; it does
not inventory or authenticate every native file installed alongside the
compiler. That upstream native-component review is separate from this local
compiler-path validation.

The frozen record remains bound to its captured source revision. Updating the
Inno compiler does not rebind a previously captured frozen record, bundle, or
installer output to a later checkout; create a new freeze and bundle whenever
the source identity changes.

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

## Installed single-pair GPU observations

To include the existing diagnostic collector, add `-LocalFrozenGpuObservation`
when building the frozen bundle. The switch is rejected without
`-FrozenCpuWorkerRecordPath` and retains the local-only CI restrictions. It adds
only `windows-gpu-capture-observation` to the desktop's `ui-harness` features;
it does not link a CUDA/Vulkan provider into the desktop or rebuild either
worker. Without the switch, desktop build arguments remain unchanged.
The collector's existing `GetProcessMemoryInfo` telemetry imports the Windows
system `psapi.dll`, which is explicitly reviewed by the PE import allowlist.
This does not permit bundling a loose `psapi.dll` or matching DLL name prefixes.

```powershell
pwsh -NoProfile -File scripts/build-windows-release.ps1 `
  -ModelSource C:\ScribeLocal\inputs\whisper-base.en-Q8_0.gguf `
  -BundlePath C:\ScribeLocal\local-observer-bundle `
  -FrozenCpuWorkerRecordPath C:\ScribeLocal\worker-freeze\windows-frozen-cpu-worker-record.json `
  -WorkerPackRoot C:\ScribeLocal\verified-packs\cuda `
  -LocalFrozenGpuObservation
```

The pack root must contain an already signed, compatible pack accepted by the
existing compiled trust policy. A caller-supplied path or digest is not trust.
Building this option with an empty catalog is allowed for CPU-only testing,
but requesting an installed GPU observation without the selected verified pack
fails before installation. The option neither provisions production keys nor
substitutes fixture packs. A positive CUDA run still needs approved trusted
pack artifacts; an unsigned Windows installer does not change that requirement.

Build the LOCAL installer from this bundle with
`scripts/build-windows-frozen-test-installer.ps1`, as shown in the local installer
section above, using this observer bundle as `-BundlePath`. Then
append all six observation arguments to the ordinary installer-verification
command, using its matching bundle, freeze, installer and installer record:

```powershell
  -ObservationWavPath C:\ScribeLocal\inputs\sample.wav `
  -ObservationWavSha256 <lowercase-sha256-of-sample.wav> `
  -ObservationGpuPackId scribe-cuda-windows-x64 `
  -ObservationGpuBackend cuda `
  -ObservationGpuDevice <exact-stable-device-id> `
  -ObservationReportPath C:\ScribeLocal\reports\installed-cuda.json
```

The six arguments are all-or-none. The report's parent directory must already
exist, and the destination must be new and outside the source, bundle, frozen
worker, installer, installation and verifier scratch directories. No collector,
model or worker path override is accepted: the collector and bundled model
paths, sizes and hashes come from the parity-verified installed inventory.

For a local, fail-closed diagnostic campaign only, append
`-ObservationCampaignPower ac` or `-ObservationCampaignPower battery` with the
same six arguments. This keeps the fixed fifteen-minute deadline and requests
the collector's schema-2 five-cold/twenty-warm campaign; it is not a performance
qualification, power-plan change, Auto decision, authorization, or release gate.
The verifier accepts only a completed, cleanup-complete local-only report and
does not publish a final report after any timeout, oversized output, malformed
report, incomplete campaign, uninstall, or cleanup failure.
Campaign stdout and stderr are each bounded to 262144 characters, and the
campaign report is bounded to 32 MiB. These are diagnostic containment limits,
not performance, stability, or hardware qualification evidence.
If a direct collector parent exits while an inherited descendant still holds a
pipe, the verifier cannot prove that descendant was retired; it fails closed at
the bounded post-exit drain deadline and publishes no success report.
Real hardware use requires a newly frozen CPU worker, observer-enabled bundle,
and the exact verified installed pack set for that run; it cannot reuse this
local diagnostic report as production evidence or an Auto/release decision.

By default, after installed payload parity and the CPU smoke pass, the verifier invokes
the installed collector once for one CPU/GPU pair, with individual arguments
and a fifteen-minute process deadline. It checks the bounded (1 MiB), UTF-8
schema-3 report envelope, nonqualification flags, collector revision, input
digests and selected pack/backend/stable-device identity. Worker record fields
and the transcript-digest/parity relationship are checked; embedded handshake
frames and nested telemetry are not independently authenticated or fully
revalidated by this PowerShell adapter. The existing native collector remains
responsible for its worker protocol and provider observations.

`transcript_parity: false` is preserved as a diagnostic result when consistent
with the two transcript digests; it is not a passing correctness qualification.
The report remains `unsigned:true`, `unqualified:true`, `auto_eligible:false`
and `release_approved:false`. This mode does not run the five-cold/twenty-warm
campaign or establish ordinary application latency, power-scheme qualification,
capture authorization, GPU Auto eligibility or release approval.

The verifier retains report bytes only after validation, uninstalls the trusted
test payload, verifies removal and source identity, and cleans its owned
scratch before publishing the report through a same-directory, no-replace
rename. Observer, report-validation or cleanup failure must not publish a final
report or replay inference. An existing or race-created destination is left
unchanged. As with ordinary verification, an installation whose payload parity
was not established is retained rather than executing its untrusted contents.

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

To omit only observation support, leave out `-LocalFrozenGpuObservation` and
the verifier's six observation arguments. To disable frozen packaging entirely,
omit `-FrozenCpuWorkerRecordPath` and use the unchanged normal builder.
Preserve frozen artifacts while they are needed; cleanup is an
explicit local decision, not part of fallback or validation failure handling.
