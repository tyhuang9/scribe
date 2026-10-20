# Protected macOS Metal writer — source component

This is an isolated, **copy-only installer source component**, not a shipping
installer or privileged service. It is not bundled, invoked by the desktop, or
assembled into a package. No command installs an application, launches a worker,
signs anything, accesses a Keychain, updates security records, activates a pack,
or changes Auto eligibility. Source checks never invoke the production mutation
entry or touch the actual protected store.

## Current commands and authority

`describe` validates and reports compiled release metadata. On macOS,
`verify-source --source-pack ABSOLUTE_PATH` verifies one candidate against the
compiled native Metal release entry. There are no runtime options for a key,
release document, digest, destination, epoch, architecture, or build identity.
Other hosts can run portable source tests and `describe`, but cannot invoke
production source admission or copying. The sole mutation entry,
`install --source-pack ABSOLUTE_PATH`, requires effective UID 0 before accessing
the candidate. It is source for a future authenticated installer, not permission
to execute it. It never elevates itself. An empty compiled release rejects the
candidate before opening it or the protected hierarchy.

Release builds require a valid `SCRIBE_BUILD_REVISION`. The isolated package
version must equal the application's literal package version; compilation fails
if they drift. Shared worker identities therefore still describe the application
release, not an unrelated installer version. The source-build toolchain and
reviewed build inputs remain trusted; this is not a hermetic root bootstrap.

Build-only `SCRIBE_GPU_PACK_CATALOG` and `SCRIBE_GPU_PACK_RELEASE_AUTHORITY` freeze
bounded release documents into the executable. When absent, the existing empty
catalog and epoch-zero default-deny authority are compiled. No project signing
key or production trust file is changed. A malformed compiled pair fails closed.
The shared application parser validates canonical authority, exact catalog hash,
entry-by-entry inclusion, build/protocol identity, epoch and Keychain namespace.
The verifier independently authenticates canonical Ed25519 manifests using the
existing production **public** trust and checks complete hashes and inventory.

An opaque, module-private permit binds the admitted lease to exactly one native
Metal entry: canonical pack ID/version/digest, matching provider, ABI, epoch,
platform, worker identity, installed size and three-file release inventory:

```
pack-manifest.json
pack-manifest.sig
worker/scribe-inference-worker
```

A valid signature is insufficient without exact release inclusion. Ambiguous
native entries, auxiliary payloads and foreign builds are rejected. The compressed
size is positive release-bound metadata, not a runtime performance measurement.
The native source path is walked from `/` through retained no-follow directory
handles, rejecting symlink ancestors and noncanonical spellings without
canonicalizing away aliases. Regular-file reads are nonblocking so a FIFO cannot
stall verification before type validation.

The lease retains content authority, not protected execution authority. Source
directories need not be root-owned or APFS: they are untrusted candidate bytes.
Reverification checks bytes as well as directory identity. The copy transaction
retains this permit/lease, reverifies, reads through retained handles, hashes copied
readback independently, and separately admits the protected destination. A caller
must never turn a successful `verify-source` process exit into later authority
for a reopened path. The current command deliberately drops its lease on exit.

## Copy-only protected transaction

The destination is fixed; ID, version and digest come only from the private
release permit, never a runtime destination argument:

```
/Library/Application Support/Scribe/WorkerPacks/<pack-id>/<version>/<digest>
```

Every retained and named ancestor must pass the shared root-owned, local
ownership-enforcing APFS, ACL, link and mode checks. Missing Scribe/WorkerPacks
and admitted ID/version ancestors use sealed private-directory, no-replace
publication. Existing unsafe nodes fail without repair. A permanent single-link
0600 `.installer.lock` beneath WorkerPacks is held nonblockingly through copying,
cleanup and confirmation; it is never unlinked.

A fresh 0700 stage contains only the closed three-file inventory. Inputs are
reopened under the admitted source lease. Every new output is exclusive, bounded
per file, synced, given its final mode (0644 metadata, 0755 image), closed writable,
then independently reopened and hashed. The complete sealed destination is
reverified with the production signature verifier and original copy-entry hashes,
including the exact signature envelope. A different valid same-sized envelope
is not an interchangeable copy. Directories are sealed 0755 and synced before
`renameatx_np(RENAME_EXCL)` publication.

Existing destinations and collision winners require independent full inspection;
they are never overwritten. Publication uncertainty is recorded before rename.
Confirmation errors retain the final directory for verified retry. Cleanup proves
the original temporary basename, retained identities, complete inventory and
all children **before its first unlink**. Unknown/replaced/unproven nodes retain
the stage with an error. Cleanup is flat, never targets a digest directory, and
never adopts or sweeps abandoned stages. Older versions are retained. Shared
installer-only transaction/filesystem mechanics are also used by CPU, whose
literal ancestor restrictions and 512-MiB image bound remain separate.

## Verification

After fetching the locked dependencies, the canonical source check is:

```
bash scripts/verify-macos-metal-installer-source.sh
```

It checks formatting, strict Clippy, exact discovery, all tests, and release
compile rejection of missing/malformed revision plus one valid synthetic
revision. It runs offline, clears ambient release-document overrides, and never
starts a production installer or worker. Expected totals are 52 on Windows,
52 on Linux, and 90 on macOS, including 13/17/19 admission tests respectively.
These include the reused production verifier/compatibility tests and Mac's
shared read-only filesystem tests. Native discovery separately requires all
16 Metal writer tests and eight ancestor-bootstrap tests.

Fixtures use only an in-memory test key, private temporary directories, synthetic
worker bytes and test binaries. Production trust rejects the fixture key. Tests
cover rejected invalid install/runtime-authority options, every release-entry field,
signature/build/provider/platform/ABI/epoch rejection, exact inventory/size,
source tampering/replacement/links and bounded
FIFO rejection. Mac-only fixtures exercise the actual descriptor-relative source
walk and empty-release rejection before candidate access. Private native writer
fixtures cover mutation after source reopening, copied readback corruption,
partial writes, permanent locks, unsafe nodes/ACLs, collision/reuse, interrupted
stages, ambiguous rename, confirmation failure and cleanup-before-unlink proof.
UID/root relaxation and mutation hooks exist only in test code. No audio, models,
driver, installed application, actual protected root, or private key is used.

Windows/Linux checks are not native Mac or hardware acceptance. The existing
Intel/ARM structural CI runs this same command without installing anything.

## Remaining boundaries, not implemented here

The writer is source-tested, not an installed or qualified release. Native Mac
execution of this new transaction, real power-loss durability, and clean-machine
acceptance require separate evidence. Root must not be a concurrent hostile
writer; this does not defend against compromised root. Trusted installer
bootstrap, package identity/signing,
application replacement, rollback/update/removal, actual installation, native
acceptance and hardware qualification remain separate gates. A user-writable
extraction tree is not an authenticated root bootstrap.
