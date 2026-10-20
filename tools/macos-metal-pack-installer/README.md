# Protected macOS Metal installer — source admission

This is an isolated, **admission-only source component**, not a shipping installer
or privileged service. It has no install command and is not bundled, invoked by
the desktop, or assembled into a package. Neither command writes the protected
store, installs an application, launches a worker, signs anything, accesses a
Keychain, updates security records, or changes Auto eligibility.

## Current commands and authority

`describe` validates and reports compiled release metadata. On macOS,
`verify-source --source-pack ABSOLUTE_PATH` verifies one candidate against the
compiled native Metal release entry. There are no runtime options for a key,
release document, digest, destination, epoch, architecture, or build identity.
Other hosts can run portable source tests and `describe`, but cannot invoke
production source admission.

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
Reverification checks bytes as well as directory identity. Future copying must
retain this permit/lease, reverify, read through retained handles, hash copied
readback independently, and separately admit the protected destination. A caller
must never turn a successful `verify-source` process exit into later authority
for a reopened path. The current command deliberately drops its lease on exit.

## Verification

After fetching the locked dependencies, the canonical source check is:

```
bash scripts/verify-macos-metal-installer-source.sh
```

It checks formatting, strict Clippy, exact discovery, all tests, and release
compile rejection of missing/malformed revision plus one valid synthetic
revision. It runs offline, clears ambient release-document overrides, and never
starts a production installer or worker. Expected totals are 37 on Windows,
37 on Linux, and 51 on macOS, including 10/14/16 admission tests respectively.
These include the reused production verifier/compatibility tests and Mac's
shared read-only filesystem tests.

Fixtures use only an in-memory test key, private temporary directories, synthetic
worker bytes and test binaries. Production trust rejects the fixture key. Tests
cover rejected install/runtime-authority options, every release-entry field,
signature/build/provider/platform/ABI/epoch rejection, exact inventory/size,
source tampering/replacement/links and bounded
FIFO rejection. Mac-only fixtures exercise the actual descriptor-relative source
walk and empty-release rejection before candidate access. No audio, models,
driver, installed application, actual protected root, or private key is used.

Windows/Linux checks are not native Mac or hardware acceptance. The existing
Intel/ARM structural CI runs this same command without installing anything.

## Next boundary, not implemented here

The copy-only root writer must independently verify a fresh sealed destination
under the fixed `/Library/Application Support/Scribe/WorkerPacks` hierarchy,
permanently lock, publish without replacement, preserve older versions and never
clean a possibly published final directory. This tool currently cannot create
that counterpart. Trusted installer bootstrap, package identity/signing,
application replacement, rollback/update/removal, actual installation, native
acceptance and hardware qualification remain separate gates. A user-writable
extraction tree is not an authenticated root bootstrap.
