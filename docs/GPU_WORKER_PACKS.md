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
launchable path can be passed to Stage 2's exact-path, file-identity, digest,
and final-process-image checks.

## Immutable storage and activation

Pre-signed input is first verified, copied with no-follow opens into a random
sibling staging directory, and fully reverified there. Only then is it durably
renamed to:

```text
workers/packs/<pack-id>/<version>/<pack-digest>/
```

An existing digest directory is never overwritten or repaired in place. A
private app-data activation record atomically stores current and previous
`VerifiedPack` descriptors. Activation and rollback reverify the complete pack
and require the descriptor root to derive exactly from the immutable store.
A per-pack-ID security-epoch high-water record is durably raised before
activation; rollback can select a lower version only at or above that floor.
Corrupt activation state, interrupted staging, or an invalid pack produces no
GPU candidate and cannot disable the compiled CPU route.

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
