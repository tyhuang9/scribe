# Verified GPU worker-pack infrastructure

Stage 4 implements bundled Windows x64 CUDA and Vulkan GGUF worker packs behind
the verified Stage 3 boundary. A pack becomes an explicit-GPU candidate only
after signed catalog discovery, retained no-follow verification, a bounded
provider probe, challenge-bound SCIF Hello reconciliation, and authoritative
per-device Windows driver mapping. `Auto` is governed by Stage 5 release
qualification evidence and remains default-denied to GPU until an approved
entry is added. The checked-in production trust
root is still empty, so ordinary releases remain CPU-only and a requested
nonempty GPU release fails closed until a separately reviewed public key and
protected trusted signing workflow are provisioned. The candidate-ref release
workflow never receives signing authority. Official releases additionally require the
reviewed `SCRIBE_GPU_PACK_RELEASE_POLICY` repository variable. Its temporary
Stage 4 value is `temporary_cpu_only_stage4`; once production trust is
provisioned it may be changed to `gpu_packs_required` only with a separate
trusted workflow that signs fixed verified unsigned artifacts and returns both
packs for publication.

## Current Stage 4 behavior

- Windows x64 discovers at most eight immutable packs and at most sixteen
  bounded devices per provider probe. It produces one opaque launch binding per
  stable device identity and sorts CUDA before Vulkan, then stable identity.
- Explicit `GPU` tries at most four verified GPU routes, advances only after a
  pre-output provider/worker failure, and never falls back to CPU. Cancellation,
  invalid input, model corruption, decode/content failure, and partial output
  are never replayed.
- The current process index is remapped from stable PCI, Windows LUID, or device
  UUID identity on every actual start. Hello must agree with the
  parent-observed backend, provider, vendor, class, identity, current index,
  driver, and bounded memory snapshot.
- CUDA uses the provider PCI identity plus the matching bounded Windows
  SetupAPI driver version. Windows Vulkan drivers commonly omit
  `VK_EXT_pci_bus_info`, so the verified Vulkan worker performs a second
  extension-free loader query and correlates the provider enumeration to
  `VkPhysicalDeviceIDProperties` in the same live process. It prefers the
  Windows LUID, falls back to device UUID, and binds the Vulkan driver ID,
  version, and driver UUID. Duplicate or unmatched facts fail closed. The
  desktop never loads the Vulkan loader or provider. Missing stable device or
  driver identity makes the candidate incompatible instead of producing an
  unbound health key.
- One registry-wide route owns the sole inference worker/model. CPU/GPU and
  GPU-device switches retire the previous worker; failed fallback workers are
  retired; only the winner retains the existing five-minute warm model.
- Repeated explicit-GPU requests reuse the verified catalog while a cheap
  signed-catalog generation plus SetupAPI device/driver fingerprint is
  unchanged. A changed fingerprint retires the warm worker before re-probing.
- GPU health uses the exact pack/runtime/OS/driver/device/model key described
  below.

## Stage 5 Windows Auto qualification

`runtime-manifests/gpu-auto-qualification-windows-x64.json` is a compact,
canonical, deny-unknown policy embedded in both desktop and worker builds. It
currently has `mode: default_deny` and zero entries, so it is an explicit
release-safe CPU default rather than an implicit promise that every discovered
GPU is suitable for Auto.

Before a provider is loaded, Auto compares a verified pack's backend, provider,
ID/version/digest, security epoch, runtime ABI, and the requested GGUF digest
with an entry. Only matching packs may be probed. After the challenge-bound
Hello, it performs a second exact comparison of vendor, device class, and
driver identity while enforcing the entry's minimum total-memory threshold.
Free VRAM is live diagnostics only and is
not qualification input. The worker then applies the existing battery policy
and private health quarantine before deterministic CUDA → Vulkan ordering; CPU
is appended as Auto's final fallback. Explicit `GPU` deliberately bypasses this
Auto evidence gate and never falls back to CPU.

Every future entry must carry immutable evidence ID and SHA-256 digests for
cold, warm, and transcript-parity evidence, at least five cold and twenty warm
runs, correctness and reliability assertions, and GPU p95 no more than 110% of
the matching CPU p95. The application never benchmarks a user's device. The
report script validates this schema and emits its deterministic evidence summary
in Windows CI. A malformed, noncanonical, wrong-platform, unknown-field, or
nonmatching entry denies Auto GPU use.

## Signed envelope and digest

Each pack root contains exactly two reserved metadata files:
`pack-manifest.json` and `pack-manifest.sig`. Every other regular file must
appear exactly once in the manifest's strictly path-sorted payload inventory,
with byte size and lowercase SHA-256. Both JSON files use the compact canonical
serialization accepted by the Rust structs; unknown fields, duplicate or
ambiguous serialization, and noncanonical bytes are rejected.

The detached Ed25519 signature authenticates the exact manifest bytes before
the verifier opens any inventory-selected payload path. A verified manifest
binds schema, pack ID/version, security epoch, desktop and worker protocol,
runtime ABI, distinct desktop/worker build identities, backend/provider,
target OS/architecture, worker relative path, and the complete inventory.

`pack_digest` is SHA-256 of the domain separator
`scribe-gpu-worker-pack-digest-v1\0` followed by canonical JSON containing all
identity/compatibility fields and the sorted payload inventory. It excludes the
`pack_digest` field itself and the reserved manifest/signature envelope, so the
definition is non-circular while every payload byte remains transitively bound.

Verification rejects traversal, absolute paths, dot segments, backslashes,
colons/ADS, invalid or reserved Windows names, trailing dots/spaces,
case-collisions, excessive depth/count/name/file/aggregate size, missing or
unexpected files/directories, nonregular entries, symlinks, junctions/reparse
points, hardlinks, incompatible metadata, unknown keys, bad signatures, and
digest mismatches. The complete tree is verified again immediately before a
borrowed `LaunchableWorker` target can be passed to Stage 2's exact-path,
file-identity, digest, and final-process-image checks. The target cannot outlive
its `VerifiedPackLease`.

Installed-pack verification is authorized by a retained `VerifiedPackLease`,
not by a descriptor path. The store acquires each configured absolute root from
the filesystem root one lexical component at a time without canonicalizing or
following it, then opens each pack-ID/version/digest directory the same way.
Missing roots are created only beneath an already retained non-reparse parent.
The complete ancestor chain and verified payload handles remain alive. Windows opens
reject reparse points and omit delete sharing so an ancestor or payload cannot
be renamed out from under verification. Unix opens use `openat` with
`O_DIRECTORY|O_NOFOLLOW` for ancestors and handle-relative payload access.
Linux directory enumeration opens an independent `.` descriptor relative to
the retained directory descriptor. Darwin instead duplicates the already
validated retained descriptor with `F_DUPFD_CLOEXEC`; because that duplicate
shares the open-file-description offset, Darwin scans are process-serialized
and call `rewinddir` before each scan. Both paths pass only the disposable
descriptor to `fdopendir`, verify its directory identity, and classify each
name with `fstatat(AT_SYMLINK_NOFOLLOW)`; neither treats `/proc/self/fd` or
`/dev/fd` as a portable directory namespace.
Directory identities are checked again before the lease is returned and before
launch handoff; unsupported platforms fail closed.

Pack ID and version are stricter than general signed identifiers because they
become immutable-store directory names. They are bounded lowercase ASCII
components that start and end with an alphanumeric character and otherwise use
only alphanumerics, `.`, `_`, or `-`; separators, colons, dot-only values,
Unicode/case aliases, and Windows device names including extension forms are
rejected. Persisted activation, journal, and epoch state is checked again before
the store constructs an exact three-component path beneath its canonical root.

## Immutable storage and activation

Pre-signed input is first verified through a retained source-root lease. The
store copies only the two reserved envelope files and the exact signed inventory
with explicit file/count/directory/aggregate bounds and streaming SHA-256
checks; it never recursively copies the mutable source namespace. The source is
fully reverified before and after the bounded copy, and the random sibling
staging directory is fully reverified as well. Only then is it durably
published with a native atomic no-replace operation to:

```text
workers/packs/<pack-id>/<version>/<pack-digest>/
```

An existing digest directory is never overwritten or repaired in place. A
private app-data activation record atomically stores current and previous
`VerifiedPack` descriptors. Activation and rollback reverify the complete pack
and require the descriptor root to derive exactly from the immutable store.
A per-pack-ID security-epoch high-water record is durably raised before
activation; rollback can select a lower version only at or above that floor.
All store read-modify-write transitions are serialized across processes with a
private no-follow OS-backed lock. The lock is taken against the retained parent
directory authority before the state root is opened: Unix locks the retained
directory inode, while Windows retains a non-delete-sharing private lock-file
handle. A state-root rename or junction swap therefore cannot split the lock
from the reads and replacements it protects. Epoch-raising activation first writes a
bounded pending-activation journal containing the verified target and exact
prior/next state witnesses. Recovery reverifies the target and completes a
transaction interrupted after the journal, epoch, or activation write without
lowering the security floor. Corrupt transaction state, interrupted staging,
or an invalid pack produces no GPU candidate and cannot disable the compiled
CPU route.

## Private health quarantine

Health records are keyed exactly by pack digest, runtime ABI, OS/architecture,
driver version, stable device identity, and model digest. App build and the
device-set digest are envelope witnesses; a witness or exact-key change
invalidates the affected state. Persisted values are limited to a categorized
failure code, a count saturated at three, timestamps, an idle-probe count, and
a bounded one-shot retry grant. Paths, audio, transcripts, raw errors, and raw
diagnostics are never persisted.

The first failure quarantines for 15 minutes, the second for 6 hours, and the
third and later failures for 7 days. Explicit retry grants one immediate launch
attempt for ten minutes without erasing the streak. A failed retry or probe
escalates normally. Two consecutive successful idle probes delete the record
and clear history early. Selection sees only an exact-context quarantine
projection and applies it only to matching GPU candidates.

Only provider-attributable worker crash, hang, provider initialization, driver,
device-loss, out-of-memory, and protocol categories may update quarantine.
Invalid input, artifact/model corruption, content/decode failure, cancellation,
and partial output never do. Mutations take a private OS-backed lock and reload
state before replacement, so separate app instances cannot lose updates or
consume one retry twice. A truly missing first-run cache is available; corrupt,
unreadable, noncanonical, app-build-mismatched, or device-set-mismatched state is
explicitly invalid/unprobed for GPU while CPU remains eligible. Two successful
idle probes atomically replace that state and restore availability.

## Packaging and key provisioning

Linux x86_64 uses the same Rust manifest verifier and immutable `PackStore` as
the other platforms. `scripts/build-linux-release-package.sh` accepts prebuilt
CPU executables and optional pre-signed pack roots, but the production Linux
trust root is empty. Consequently CPU-only `.deb` assembly succeeds with an
exact empty catalog while every nonempty CUDA/Vulkan input fails before
publication. Fixture keys are confined to author/verifier tests and explicitly
labeled size evidence. The package verifier requires the canonical FHS tree,
exact inventory, desktop-to-CPU-worker digest binding, and an empty immutable
pack directory. See `docs/LINUX_RELEASE_PACKAGING.md`.

`scripts/stage-verified-worker-packs.ps1` accepts only prebuilt, pre-signed pack
roots. It invokes the compiled production verifier before and after copying,
stages the immutable layout, writes the bounded catalog, reports installed and
compressed sizes, and generates the installer preflight allowlist. Every pack
file is also included in the top-level bundle inventory, preserving portable
and installer parity. The normal catalog is empty until production trust is
provisioned. When a release includes packs, the same catalog is inside the
portable payload and the installer copies that exact tree; CI emits a separate
per-pack installed and compressed size report from the verified catalog.

Repository tooling contains no production private key. The candidate-ref
workflow contains no production-key secret reference, accepts no GPU-pack
dispatch request, and always creates the CPU-only portable/installer payload.
Official publication fails when the reviewed repository policy is absent or
unknown; the temporary Stage 4 policy is explicitly CPU-only, while
`gpu_packs_required` remains unavailable until a separate protected trusted
workflow can sign fixed verified unsigned artifacts. That signer must verify the
approved source/revision and complete unsigned-artifact digests before receiving
authority; candidate-ref scripts must not run with the key. The authoring tool
still verifies that a supplied key's public half exactly matches the separately
reviewed key embedded in `ProductionTrustRoot`. Because no production public key
or trusted signer exists today, every nonempty production release fails closed.
The deterministic seed and key ID used by tooling tests and local hardware smoke
are fixture-only and cannot verify under production trust.

## Windows Vulkan hardware evidence

On 2026-08-29, a clean fixture-only pack built from `10d4ec2` with the pinned
Rust/MSVC/CMake contract and Vulkan SDK 1.4.357.0 completed the ignored
challenge-bound SCIF/model smoke on an NVIDIA GeForce RTX 4080 SUPER. The pack
was `scribe-vulkan-windows-x64` version `0.1.0-fixture7`, digest
`563e1cf17db85bf02c40dda7d074e981c589931aa890f986446df70428aad62b`.
It contained three files totaling 98,017,192 installed bytes; the worker payload
was 98,016,256 bytes, and the same optimal ZIP method used by staging measured
31,801,892 compressed bytes.

The worker reported stable identity `native:0000:01:00.0`, bounded SetupAPI
driver identity `windows-display:32.0.16.1088`, and 16,824,401,920 total-memory
bytes. It transcribed the pinned `whisper-base.en-Q8_0.gguf`/JFK fixture through
the verified explicit-GPU route, contained the expected `ask not` phrase,
reported `warm_reused=true` on the second request, and launched no CPU worker.
These numbers describe the fixture pack only; they are not CUDA sizes or a
production installer measurement.

The toolchain-selection repair was reverified from clean commit `94ba0ff` after
the hosted Windows runner began preferring a Visual Studio 18 shell. The build
accepted the shell only after locating the reviewed v143 compatibility
component, then activated MSVC toolset `14.44.35207` and Windows SDK
`10.0.26100.0` through `vcvarsall`. CMake reported compiler
`19.44.35227.0` from the exact pinned directory and used the exact hashed
`cl.exe`, `link.exe`, `lib.exe`, and `nmake.exe` payloads. The resulting
fixture pack `0.1.0-fixture-toolchain2` had digest
`edd7cc74481720c19c21decfa4676af8c7b2dfb32abb50e2c5ba9a56c88fd306`
and the same 98,016,256-byte worker payload. It passed the ignored explicit-GPU
SCIF/model smoke on the same RTX 4080 SUPER with stable device/driver identity,
the expected `ask not` phrase, warm reuse, and zero CPU launches. This evidence
verifies compiler-payload selection across Visual Studio shells; it does not
replace remote CI, CUDA, production-signing, installer, or Auto qualification.

CUDA was not built or run locally because CUDA Toolkit/nvcc 12.8.93 is absent.
Fixture mode checks that exact developer-toolkit version. Production mode also
requires a complete canonical CUDA Toolkit inventory with exact SHA-256 values;
the checked-in inventory is intentionally empty, so same-version modified inputs
cannot become production-trusted packs. Production signing and packaging remain
intentionally unverified and fail closed because no reviewed production public
key or protected trusted signer exists.
Auto enablement, five-cold/twenty-warm performance qualification, driver/device
loss qualification, and portable/installer hardware execution remain later
release evidence rather than claims established by this smoke.

The passing test used the following command shape with absolute local fixture
paths and the recorded hashes above:

```powershell
$env:SCRIBE_GPU_FIXTURE_PACK_ROOT = '<fixture7-pack-root>'
$env:SCRIBE_GPU_FIXTURE_MODEL = '<whisper-base.en-Q8_0.gguf>'
$env:SCRIBE_GPU_FIXTURE_MODEL_SHA256 = '3b46ca40bccbf7609c68d88a36d96077a04ca7c87f2060ede06f129fac3e7652'
$env:SCRIBE_GPU_FIXTURE_WAV = '<pinned-jfk.wav>'
$env:SCRIBE_GPU_FIXTURE_WAV_SHA256 = '59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e'
$env:SCRIBE_GPU_FIXTURE_EXPECTED_TRANSCRIPT = 'ask not'
$env:SCRIBE_GPU_FIXTURE_STABLE_DEVICE_ID = 'native:0000:01:00.0'
cargo test --features inference-worker verified_vulkan_fixture_pack_scif_model_hardware_smoke -- --ignored --nocapture
```

## Stage 4 launch binding

Stage 4 adds a typed launch descriptor that carries the verified pack
ID/version/digest, backend/provider, runtime ABI, target OS/architecture, and
stable device identity through `WorkerExecutableResolver`. The same facts must
be challenge-bound into the worker `Hello` exchange and compared with the
reverified executable and final process image before the worker can advertise a
capability. Merely verifying a pack directory or adding a public key is not
sufficient to make a provider discoverable or launchable.

The compile-time seam is `ResolverHelloBindingBridge` followed by
`VerifiedPackLaunchBinding::try_from_resolver_hello_bridge`. The opaque binding
can be created only from an `Arc<VerifiedPackLease>` retained by the resolver,
and only when its descriptor exactly agrees with the worker Hello pack ID,
version, digest, runtime ABI, backend, and provider and the Hello supplies a
canonical stable device identity. The Windows resolver retains that lease through exact
worker/dependency handle launch and final image/Hello validation; it must never
reconstruct launch authority from `VerifiedPack::root`. Production discovery
obtains those bindings from the concrete
`discover_production_pack_launch_bindings` path and pass only them to
`ProductionPackRegistry::from_launch_bindings`; it cannot insert a raw
`VerifiedPack`.

Windows and macOS are implemented Stage 4 targets. Their resolvers retain the
verified directory/file lease through exact-image launch and the
challenge-bound Hello check. macOS uses the opaque descriptor-bound
`UnixPackExecAuthority` and `posix_spawn` `/dev/fd` bridge; catalog paths are
not process-creation authority. Production macOS nevertheless remains
fail-closed today because its trust root and Auto qualification manifests are
empty/default-deny. Linux remains fail-closed until an equivalent descriptor
relative execution primitive is implemented and tested. `PathBuf`,
`Arc<VerifiedPackLease>`, and `Command::spawn` are not Unix pack-execution
authority and cannot satisfy that provisioning guard. The architecture guard
scopes this prohibition to the verified-pack provider launch function;
unrelated process launches do not satisfy or trip the gate.

## Stage 6 macOS Metal packaging contract

macOS packages use a universal `Scribe.app`, with universal desktop and CPU
worker Mach-O files in `Contents/MacOS`. The CPU worker remains the only
default path. Metal is available for an explicit GPU request only when a
verified installed Metal pack is declared by
`Contents/Resources/worker-pack-catalog.json` and stored exactly at:

```text
Contents/Resources/workers/packs/<pack-id>/<version>/<digest>/
```

Each standalone Metal worker is built per architecture from the corresponding
pinned `runtime-manifests/gpu-worker-toolchain-macos-*.json` contract. It is
Developer-ID signed with hardened runtime and timestamp before its inventory is
hashed and before its canonical Ed25519 pack manifest is authored. The release
assembler never creates signing keys, accepts secret values as arguments, or
uses `codesign --deep`. It signs the universal CPU worker and desktop binaries,
then the outer application; the dedicated protected release step verifies,
notarizes, staples, and verifies again.

Stage 6 macOS Metal packs have a deliberately narrower payload contract than
the general signed-pack format: the signed payload inventory must contain
exactly one regular executable, the declared worker path. The manifest and
detached signature remain the only two control files outside that signed
payload. Any dylib, framework, resource, helper, or second executable rejects
the pack before launch authority or a GPU route is constructed. Multi-file
macOS packs are deferred until descriptor-bound dependency loading or
ownership-enforced filesystem immutability is separately designed and reviewed.

Metal framework linkage is confined to the per-architecture Metal worker. The
desktop and universal CPU worker use only an IOKit power-source shim plus the
`kern.osversion` witness and must have no Metal load command. Stable
`MTLDevice.registryID` discovery/remapping occurs inside the verified Metal
worker and reaches the parent only through the challenge-bound capability
Hello. Release verification checks these Mach-O linkage boundaries with both
`otool -L` and `otool -l`.

On Windows, before a bundled verified lease can become a route, discovery loads
the private per-platform/backend/pack security-epoch high-water ledger under the
same anchored lock and atomic durable-replace discipline as activation state.
On macOS, the data-protection Keychain release floor is authoritative and the
app-data ledger is advisory only; deleting application data cannot reset the
device floor. The same epoch is accepted, a higher epoch advances the relevant
authority, and a lower epoch, corrupt state, or unavailable authority fails GPU
admission closed. A catalog cache never bypasses this check. CPU routing remains
available, and an empty production trust result does not mutate Windows ledger
state.

The signed desktop is the non-resettable release authority for bundled macOS
packs. Both universal slices embed identical canonical schema-v2 authority bytes
that bind the application version and build revision, SCIF protocol, exact
catalog SHA-256, the outer `release_security_epoch`, the exact Keychain access
group, pack identity/digest/security epoch, runtime ABI, backend/provider,
target, worker path, sizes, and complete inventory. The installed catalog must
match that authority before verification, cache lookup, or device-local
Keychain admission. Deleting or recreating app-data state therefore cannot
admit an older catalog.

The checked-in authority is the canonical default-deny document: schema version
2, the SHA-256 of the exact compact empty catalog bytes, release epoch `0`, an
empty Keychain group, and no entries. Epoch zero is permitted only for that
empty CPU-only release. Every nonempty Metal catalog requires an explicitly set
positive canonical `SCRIBE_MACOS_GPU_RELEASE_SECURITY_EPOCH`; the release
builder passes that same value to every pack author's `--security-epoch` and
writes it as the outer release epoch. A positive epoch may intentionally carry
an empty catalog to revoke prior GPU capability, but it remains a protected
release. The device-local Keychain floor is append-only; no packaging or runtime
path resets, deletes, or lowers it. Release epochs are canonical exact JSON
integers from `0` through `9007199254740991`, avoiding loss of precision in the
packaging toolchain. Runtime admission also requires every authority entry to
carry the outer release epoch.

A protected release (a positive epoch or any Metal catalog) requires
Developer-ID signing, a regular non-symlink distribution provisioning profile,
and `SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP` matching exactly the
non-empty value in the source-reviewed
`runtime-manifests/gpu-keychain-namespace-macos-release.json`. That manifest is
empty by default, so protected releases fail closed until a separate security
review pins the production Team ID and access group. The profile's application
identifier and sole `keychain-access-groups` value must both equal that exact
group, and its team identifier must equal the group's Team ID. The builder
generates protected application, team, and Keychain entitlements from the
checked-in template, embeds the profile before the final signing pass, and
verifies both the desktop executable and outer app. CPU and Metal workers must
not carry the desktop Keychain group. The package verifier rechecks the
authority/catalog binding, reviewed namespace, effective entitlements, profile
authorization, and profile-aware exact inventory.

Discovery checks the device floor before publishing a verified route, including
cached discovery. Because provider probing can take several seconds, the parent
checks the embedded release identity and Keychain floor again immediately before
making a GPU route active. That final check is the request activation boundary:
Auto skips a rejected GPU and reaches CPU, explicit GPU reports a clear error,
and a transcription already active when a newer release advances the floor is
allowed to finish rather than being migrated.

The hosted pull-request lane is deliberately credential-free and builds only
the epoch-zero empty catalog. The protected official lane is also CPU-only at
epoch zero today because the production Metal trust root and provisioning inputs
have not been provisioned. Before enabling a positive-epoch release, the
protected environment must supply the reviewed Developer-ID identity, stable
group, authorized profile path, positive release epoch, and (for Metal) the
reviewed pack-signing key material. These inputs are requirements for a future
protected run, not evidence that production Metal trust exists now.

The production authoring CLI must accept `--backend metal --target-os macos
--target-arch <aarch64|x86_64>` and bind those facts in its signed manifest.
Until that reviewed CLI extension and a persistent production trust root exist,
the build fails closed for non-empty packs and produces the canonical empty
catalog. The checked-in macOS Auto manifests are both canonical zero-entry
`default_deny` documents. No runtime calibration occurs: Auto remains CPU-only
until a separately reviewed release qualification provides five cold runs,
twenty warm runs, parity/reliability evidence, and GPU end-to-end p95 no more
than 110% of the matching CPU p95. ONNX and Sherpa remain CPU-only.

The release builder signs the universal CPU worker before hashing it, then
embeds that final SHA-256 into both desktop slices. It verifies that the final
package still contains that digest and the runtime's challenge-bound CPU-worker
handshake checks the same parent expectation before output is accepted.

## Build and verification commands

`scripts/build-windows-gpu-worker-pack.ps1` builds one deterministic fixture or
production pack from the pinned contract in
`runtime-manifests/gpu-worker-toolchain-windows-x64.json`.
`scripts/prepare-windows-gpu-worker-packs.ps1` is the production-only two-pack
orchestrator used by the opt-in release job. It requires exact Rust 1.96.0,
CMake 4.4.2, MSVC 14.44.35207 tool payloads, Windows SDK 10.0.26100.0, the
reviewed Sherpa archive, Vulkan SDK 1.4.357.0, and CUDA Toolkit/nvcc 12.8.93.
The mutable Visual Studio product-shell version is not the compiler identity:
the build locates a shell containing the reviewed compatibility component,
activates the exact toolset/SDK, verifies tool file versions and SHA-256s, and
exports only the verified build environment. Missing or mismatched payloads
fail with a specific gate; the scripts never download an unapproved SDK. Pack payload outputs use
the ignored `artifacts/gpu-worker-packs` tree. Native Cargo targets must be
fresh direct children of the validated short `LocalApplicationData\sgp` build
root. Each build also receives a separate fresh physical `LOCALAPPDATA` child,
so `transcribe-cpp-sys` never reads, replaces, or reuses the user's shared
`tcs` junction namespace. If the dependency's first CMake configure encounters
its known Windows junction bootstrap failure, the script validates the one
build-local junction and its exact Cargo OUT_DIR, replaces only that fresh
partial `out\build` directory with an isolated short junction, and retries
once. This keeps CMake/MSBuild paths bounded in deep worktrees and prevents
CUDA and Vulkan feature outputs from being confused.

`scripts/test-windows-gpu-worker-pack-tools.ps1` exercises deterministic
fixture authoring plus signature, key, tamper, unexpected-file/DLL, ADS,
junction, and hardlink rejection. The Rust suites cover downgrade floors,
catalog mismatch, challenge/ABI/pack/device identity mismatch, multi-device
remapping, bounded fallback, quarantine privacy/timing, and no-replay rules.
`scripts/test-windows-release-packaging.ps1` and
`scripts/verify-windows-release-package.ps1` enforce exact catalog/inventory,
installer allowlist, portable/installer parity, and hostile filesystem cases.

## Stage 7 Linux GPU contract

Linux GPU support remains default-deny. The only reviewed future worker target is
`x86_64-unknown-linux-gnu` on Ubuntu 22.04 or 24.04, with glibc 2.35 or newer
and kernel 5.15 or newer. The CUDA lane is CUDA 12.8 / nvcc 12.8.93 through
`transcribe-cpp-ggml-cuda` with an NVIDIA driver floor of 570.26. The Vulkan
lane is loader/toolchain 1.4.357.0, API 1.2 or newer, through
`transcribe-cpp-ggml-vulkan`. A future packaged launcher must also prove Linux
`openat2` with beneath/no-link resolution, `execveat` with `AT_EMPTY_PATH`, and
`close_range`; this stage records those primitives but does not launch a pack.
Unsupported distributions, architectures, ABI,
glibc, or kernels deny Linux GPU eligibility only; the CPU path remains
available. `gpu-auto-qualification-linux-x86_64.json` is canonical
`default_deny` with no entries, so this contract does not enable Auto GPU use or
introduce production trust. A Linux release-authority document is intentionally
deferred until the root-owned epoch authority is implemented; the current
schema is macOS Keychain-specific and is not reused as a Linux trust contract.
