# Verified GPU worker-pack infrastructure

Stage 3 establishes a dormant security and persistence boundary for future
external GGUF GPU workers. It does not ship a production CUDA, Vulkan, or Metal
worker, enable downloads, change `Auto`, or make a GPU provider discoverable.
The production trust root and registry are intentionally empty.

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

`scripts/stage-verified-worker-packs.ps1` accepts only prebuilt, pre-signed pack
roots. It invokes the compiled production verifier before and after copying,
stages the immutable layout, writes the bounded catalog, reports installed and
compressed sizes, and generates the installer preflight allowlist. Every pack
file is also included in the top-level bundle inventory, preserving portable
and installer parity. The normal catalog is empty.

Repository tooling does not sign packs and contains no production private key.
Publication must remain disabled until a persistent production public key is
reviewed into the application and the matching private key is supplied only
through an explicit external signing path or masked CI secret. Test keys are
fixture-only and must never be promoted.

## Stage 4 launch binding prerequisite

Stage 4 must add one typed launch descriptor that carries the verified pack
ID/version/digest, backend/provider, runtime ABI, target OS/architecture, and
stable device identity through `WorkerExecutableResolver`. The same facts must
be challenge-bound into the worker `Hello` exchange and compared with the
reverified executable and final process image before the worker can advertise a
capability. This binding is required before any production trust root or catalog
may be provisioned. Merely verifying a pack directory or adding a public key is
not sufficient to make a provider discoverable or launchable.

The compile-time seam is `ResolverHelloBindingBridge` followed by
`VerifiedPackLaunchBinding::try_from_resolver_hello_bridge`. The opaque binding
can be created only from an `Arc<VerifiedPackLease>` retained by the resolver,
and only when its descriptor exactly agrees with the worker Hello pack ID,
version, digest, runtime ABI, backend, and provider and the Hello supplies a
canonical stable device identity. Stage 4 must retain that lease through exact
worker/dependency handle launch and final image/Hello validation; it must never
reconstruct launch authority from `VerifiedPack::root`. Production discovery
must obtain those bindings from a concrete
`discover_production_pack_launch_bindings` path and pass only them to
`ProductionPackRegistry::from_launch_bindings`; it cannot insert a raw
`VerifiedPack`. Stage 3 implements neither the bridge nor discovery path and
constructs only `ProductionPackRegistry::empty()`.

Windows is the first intended production target: its future resolver must keep
the verified directory/file lease alive through exact-image launch and the
challenge-bound Hello check. Unix production remains fail closed. Before a
Unix catalog or trust root can become nonempty, the resolver bridge must also
produce an opaque `UnixPackExecAuthority` containing an already-open executable
FD, an anchored dependency-root directory FD, and the exact same
`Arc<VerifiedPackLease>` from which both descriptors were opened. The future
`open_unix_pack_exec_authority_from_verified_pack_lease` constructor must live
on the provider resolver path, open both descriptors relative to that lease,
and be the only production constructor; independently opened FDs cannot be
combined with a lease after the fact. The opaque binding checks lease identity,
and the launch path must consume those authorities through
`execveat`/`fexecve`-equivalent execution and retain the dependency-root and
lease authorities through Hello validation. `PathBuf`,
`Arc<VerifiedPackLease>`, and `Command::spawn` are not Unix execution authority
and cannot satisfy the provisioning guard. Unsupported Unix variants must
remain fail closed until an equivalent descriptor-relative execution primitive
is implemented and tested. The architecture guard scopes this prohibition to
the verified-pack provider launch function; unrelated process launches do not
satisfy or trip the gate.
