# Protected macOS Metal writer — source component

This is an isolated, **copy-only installer source component**, not a shipping
installer or privileged service. It is not bundled, invoked by the desktop, or
assembled into a package. No command installs an application, launches a worker,
signs anything, accesses a Keychain, updates security records, activates a pack,
or changes Auto eligibility. Source checks never invoke the production mutation
entry or touch the actual protected store.

### Embedded copy-content source

The transaction supports a private embedded source adapter, now consumed by the
separate default-off native package-entry feature described below. The default
writer retains its filesystem CLI; package mode compiles that CLI and its
production path-based source factory out. Neither mode adds an end-user install
step. No bytes are staged into a temporary candidate tree to manufacture a
filesystem lease.

Both filesystem and embedded admission use the same bounded canonical signature,
manifest and compatibility check. That check authenticates metadata **only**;
it cannot prove payload correctness, a protected destination or execution
authority. Filesystem admission still verifies its exact tree, every payload
hash, retained handles and launch-time identity. Embedded admission additionally
requires the compiled native release permit, exactly three nonempty bounded
slots (manifest, exact signature envelope and worker), the signed worker size
and hash, same-build compatibility and exact installed byte count. Production
trust/release inputs are compiled, not supplied by a runtime argument.

A closed leased/embedded source union provides retained file readers or cursors
over immutable borrowed slices. The entire embedded source stays alive through
the existing transaction; it grants no launch authority and creates no fake
root or verified lease. Boundary rechecks hash all three original slots. The
materialized protected tree is independently verified using the ordinary
filesystem verifier and must equal every original copy-entry hash, including
the signature envelope. Another valid, same-size envelope is not interchangeable.
Protected ancestry, publication, collision, cleanup and confirmation rules are
unchanged. Native fixtures exercise both sources without executing the
production entry or writing the actual protected root.

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
starts a production installer or worker. Expected totals are 58 on Windows,
58 on Linux, and 100 on macOS, including 13/17/19 filesystem admission tests
respectively and exactly six embedded-admission tests on every host.
These include the reused production verifier/compatibility tests and Mac's
shared read-only filesystem tests. Native discovery separately requires all
20 Metal writer tests (four embedded-source contracts) and eight ancestor-bootstrap tests.

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

## Dedicated native package entry (default off)

`installer-package-entry` compiles a different entry mode for the future
authenticated PKG's native operation executable. It embeds the exact manifest,
signature envelope and native worker bytes, rather than opening candidate paths
or launching siblings from a writable extraction tree. It is not assembled into
a PKG, signed, installed or executed by this source work.

The entry and CPU package entry share the installer-only invocation predicate
and its four tests. Actual effective UID must be root, the package-context path
must have the same bounded canonical spelling, and all destination/root arguments
must be `/`. The context path is never opened. These checks restrict scope but
**do not authenticate an Installer caller**; the authenticated package, Installer,
root and reviewed compiler remain trusted. A second actual-UID check precedes
`admit_embedded`, whose complete production-signature and compiled-release
checks must pass before any protected-store access.

Release feature compilation requires explicit build-only inputs:

- `SCRIBE_BUILD_REVISION` (existing valid frozen revision).
- `SCRIBE_GPU_PACK_CATALOG` and `SCRIBE_GPU_PACK_RELEASE_AUTHORITY` (absolute
  regular inputs, with nonempty/positive-epoch build shape).
- `SCRIBE_MACOS_METAL_PACKAGE_PACK` (one closed three-file source tree).
- `SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256` (independently frozen, lowercase
  SHA-256 of the exact finalized manifest bytes).

The snapshot helper rejects noncanonical/control-bearing paths, symlink/reparse
ancestors, hard-linked/special files, Windows stream paths and extra inventory.
Directory enumeration stops after the expected count plus one. It reads only
the manifest (256 KiB), signature envelope (4 KiB) and worker (2 GiB) with
nonempty bounds aligned by tests to the runtime limits. Copies are exclusive,
synced, closed and independently reopened/hashed. The manifest must match its
frozen SHA; the worker must match the manifest-declared size/hash. The manifest
must claim Metal/macOS, one fixed worker path and **Cargo's target architecture**
(`CARGO_CFG_TARGET_ARCH`, not the build host). Each eventual universal entry
slice must therefore embed its own architecture's pack.

All snapshots finish before generated outputs are published. Partial publication
aborts compilation; no old-output fallback is permitted. Failed build stages may
remain in Cargo's trusted output, never adopted as a successful input. The source
toolchain/Cargo output and non-hostile same-user/root build assumption remain
explicit; this is not a hostile same-UID build sandbox or root bootstrap.

These are **structural and frozen-byte build checks only**. The small positive
release-input shape check deliberately does not mirror runtime canonical release
validation or authenticate a signature. The future author must independently
verify finalized inputs before assembly. Runtime still runs the complete compiled
release permit, production Ed25519 verification, compatibility and exact payload
checks before touching protected ancestry. Debug source tests may compile empty
slots, which can never pass that admission. No project trust override is added.

The canonical source check also requires exactly 69 feature tests on
Windows/Linux or 111 on Mac: four shared invocation tests, seven snapshot tests
and the empty-compiled-source rejection, alongside existing admission/transaction
tests. It checks ten invalid release-input cases and one compile-only positive.
Its tiny checked-in ASCII fixtures are intentionally **not authentic packs**:
their envelope and release documents cannot pass runtime admission. The positive
does not execute or link an installer. Both explicit architecture claims are
tested without mutating the test process environment. Mac's real embedded copy,
collision and uncertain-cleanup fixtures still run independently.

Actual native feature compilation/fixtures, authenticated package composition,
Installer invocation, installation and cross-component update/rollback remain
unverified until their own gates pass. No production entry is run in CI.

## Internal universal entry construction (source only)

`build_support/macos_metal_pkg_entry.py` exposes an internal function, not a CLI
or shipping package workflow. An unprivileged Mac author supplies a finalized
application, a fresh output workspace outside it, and frozen revision, CPU-worker,
catalog, authority and exactly two architecture-keyed manifest hashes. The
existing full application verifier is mandatory, including the reviewed Keychain
namespace and profile policy. The empty reviewed namespace still prevents an
actual positive Metal release from passing. CPU-only admission is unchanged.

The constructor copies the exact application context and both closed three-file
packs into its owned private workspace, binding independent readback hashes to
source hashes and node identities. Catalog and authority bytes must equal their
compiled bytes (no terminal LF normalization). It builds the existing author
tool fresh for the host, offline and locked, then runs only its production Metal
content verifier on each private native pack. It independently joins the
descriptors, release entries, build identity, provider, layout and measured sizes.
A descriptor is not a retained lease or release, native or execution authority.
The existing Rust signature and compatibility validators remain authoritative;
there is no Python cryptography or runtime key override.

Separate fresh target directories compile both `installer-package-entry` slices
with the same frozen release documents and only the corresponding native pack.
The combined entry must contain exactly ARM64 and x64 slices, target macOS 13.0
and load only trusted system libraries, with no GPU dependency. Embedded Metal
worker bytes do not make the outer entry a GPU-linked process. Each entry slice
is bounded by the existing 2 GiB worker, 256 KiB manifest, 4 KiB signature, two
512 KiB release documents and 32 MiB code/alignment allowance. CPU bounds do not
change. Source and private snapshots are rechecked before exclusive publication.
Only a PRE-SIGN path/hash/size is returned; uncertain child cleanup retains the
private workspace and an ambiguous publication never deletes the final path.

The reviewed source, Cargo configuration/cache/toolchain, finalized app and
non-hostile same-user/root writers remain build assumptions, not a hermetic
bootstrap. Its source suite is
`python3 -B build_support/test_macos_metal_pkg_entry.py` (21 portable, 23 POSIX
tests), with all native builds/tools mocked. These synthetic positives are not
authentic packs, Mac construction acceptance, signature qualification or
installation evidence. CI adds only this source suite under the unchanged
profile/structural gate; it does not build, upload, sign or execute a real Metal
entry.

## Inactive finalized-entry owner

`build_support/macos_metal_pkg_final.py` adds a separate, internal build-only
owner for its own freshly constructed Metal entry. It has no CLI, prebuilt-entry
input, signing callback, caller-selected policy, release switch or installation
route. The same source-fixed publisher policy as CPU finalization remains
default-deny and rejects before caller inputs, workspaces or native tools.
No publisher, production key or signing identity is provisioned by this work.

If separately reviewed policy were activated, the owner would verify the app
against its independent Apple Developer ID/publisher/application requirement,
then call the existing full Metal constructor. The constructor still performs
fresh Rust production pack verification for both native packs; a structural
snapshot is not crypto admission, and CPU-only admission is never substituted.
The complete app/pack inventory, frozen release and policy remain bound through
finalization. The owner's fresh entry alone is signed with the reserved Metal
identifier, hardened runtime and timestamp request. A public certificate SHA-1
selector chooses a key but supplies no publisher authority. Both slices must
meet the independent Metal signature requirement, runtime/timestamp properties,
empty-entitlement rule and existing non-executing structural checks.

Hash/size are remeasured after signing using the Metal entry bound, not the
smaller CPU bound. Unchanged source/policy/inventory and final node/bytes are
required before exclusive publication. The result is only a finalized entry,
not a PKG, authenticated root input, installed worker or enabled release option.
The unchanged PRE-SIGN constructor still returns only PRE-SIGN bytes. The CPU
owner, CPU admission and production trust policies are unchanged.

`python3 -B build_support/test_macos_metal_pkg_final.py` discovers exactly 14
mock-only tests on Windows and POSIX. They exercise the full owned constructor
with synthetic signatures/tools, not real key use, native signature acceptance
or installation. CI runs only this mocked source suite. Actual Metal
construction/finalization, publisher provisioning, package authentication and
hardware qualification remain separate gates.

## Remaining boundaries, not implemented here

The writer is source-tested, not an installed or qualified release. Native Mac
execution of this new transaction, real power-loss durability, and clean-machine
acceptance require separate evidence. Root must not be a concurrent hostile
writer; this does not defend against compromised root. Trusted installer
bootstrap, package identity/signing,
application replacement, rollback/update/removal, actual installation, native
acceptance and hardware qualification remain separate gates. A user-writable
extraction tree is not an authenticated root bootstrap.
