# Protected macOS CPU writer — source component

This isolated tool is installer **source**, not a shipping installer, privileged
service, runtime download mechanism, or permission to install it. The desktop
never starts it. It installs only the shared CPU inference/VAD worker image, not
the application or Metal workers, and does not change Auto eligibility.

## Authority and transaction

Release compilation requires `SCRIBE_BUNDLED_WORKER_SHA256` (the existing desktop
anchor for the final signed universal CPU image) and `SCRIBE_BUILD_REVISION`.
`describe` reports that compiled metadata without mutation. The only mutation
entry is `install --source-worker ABSOLUTE_PATH`, restricted to effective UID 0.
The candidate supplies bytes, never a root, digest, manifest, or trust decision.
There is no runtime authority override, elevation attempt, or signing key.

The fixed destination is:

```
/Library/Application Support/Scribe/CPU/<compiled SHA-256>/scribe-inference-worker
```

The writer retains and rechecks no-follow descriptors through the fixed root,
requires ownership-enforcing local APFS and the shared ACL/mode/link policy, and
creates only missing Scribe/CPU directories. Each missing ancestor uses an
independently verified empty private stage, sealed 0755 and synced before
no-replace publication. Existing nodes are never repaired.
A permanent root-owned single-link 0600 `.installer.lock` provides nonblocking
mutual exclusion; it is never unlinked. Ancestors remain user-traversable.

A new version is copied into a fresh private stage, independently reopened and
hashed against compiled authority, closed to writable image handles, sealed
0755, synced, and published with Darwin `renameatx_np(RENAME_EXCL)`. An existing
version is reusable only after exact one-file inventory, mode, identity, length,
and digest verification. Neither collisions nor errors overwrite a version.
Older directories remain available; there is no pointer, downgrade state,
pruning, activation sweep, or previous-version deletion.

Publication is marked uncertain **before** rename. Failures after publication
retain the admitted directory for an independently verified retry. Cleanup is
flat and restricted to this invocation's proven staging inode and file. In
particular, the original staging name must still identify that inode before
anything is deleted through its retained descriptor. If identity is uncertain,
cleanup retains the stage and reports the failure rather than risking a final
directory. A process crash may leave an unadmitted stage; this tool never sweeps
such entries.

## Verification

After fetching the locked dependencies, the canonical source check is:

```
bash scripts/verify-macos-cpu-installer-source.sh
```

It checks formatting, strict Clippy, exact discovery, every tool test, and
release-build rejection of missing/malformed digest and build identity. A final
compile check uses fixed synthetic test identities; it does not link or start
an installer. The check runs offline and never invokes the production install
entry. Portable tests
cover phase failures, partial writes/ENOSPC, readback tampering, size changes,
collision, ambiguous rename, cleanup failure, confirmation failure, and retry.
Native Mac tests exercise the real Darwin writer only in exclusively created
unprivileged APFS fixtures. They cover permanent-lock contention and malformed
locks, exact reuse without inode replacement, no-replace publication, malformed
existing inventory/modes/links, source special files, and ambiguous cleanup.
The same transaction ordering also governs ancestor creation. Private native
fixtures cover interruption before/after sealing and retry, competing-installer
collisions with legitimate descendants, unsafe existing ancestors, ambiguous
publication, confirmation failure, and unproven/replaced/nonempty stage residue.
No model, audio, driver, real protected root, signing material, or user cache is
used. CI runs the same check on Intel and ARM.
Exact discovery requires 21 portable tests or 42 on macOS. Transaction, bounded
byte and native filesystem/bootstrap mechanics are installer-only shared source;
the CPU adapter still enforces literal Scribe/CPU ancestors and its 512-MiB
compiled-image bound. The desktop and worker do not import these write helpers.

### Native CPU admission before package construction

The structural CI job runs the unchanged protected CPU health/shutdown contract
immediately after constructing and verifying the universal application, before
constructing the unsigned CPU entry/package. The contract consumes only that
app, independently verifies it again, and copies its final CPU image into a
private unprivileged fixture. It never consumes the unsigned package.
Exact discovery then runs the two inference/VAD role contracts: authenticated
CPU identity and Ready, health, cross-role rejection, shutdown and child reaping,
with no model or audio.

Default success gating remains in force: a failed app build/verification cannot
start these tests; a CPU contract failure prevents subsequent package work and
artifact publication. Conversely, a later package-format failure no longer hides
the preceding CPU result. This changes diagnostic priority, not independence of
the two contracts or any release/signing gate. Commands, timeouts, permissions
and fixture cleanup are unchanged. Actual native results are still required;
passing private fixtures would not establish protected-path installation,
package authentication, production qualification or Auto eligibility.

## Non-installing development component

An opt-in source-authoring step now joins the finalized CPU application and a
freshly compiled writer without creating an installer or invoking `install`:

```
bash scripts/build-macos-release.sh --output-directory dist-macos \
  --pack-version 0.1.0 --signing-mode adhoc --prepare-protected-cpu-component
```

This requires macOS, Python 3.13+ (authoring only), the reviewed Rust 1.96
toolchain with both Apple targets, and previously fetched locked writer
dependencies. The toolchain, PATH and Cargo cache/configuration must themselves
be reviewed source-build inputs; this is not a hermetic or authenticated root
bootstrap. No arbitrary prebuilt writer can be supplied. Ambient compiler
wrappers, target linker/flag overrides, GPU/loader and signing variables are not
passed to the writer build. Cargo uses the repository working directory and
fresh targets, offline and locked, with the same frozen SHA and revision as the
desktop. Only CPU-only, ad hoc, epoch-zero development builds are eligible.

`dist-macos/protected-cpu-component` has exactly three regular, single-link files:

```
component.json
payload/scribe-inference-worker
tools/scribe-macos-cpu-worker-installer
```

The worker is copied from the final signed app and independently rehashed.
Both component assembly and package-entry construction share one bound-input
admission path. It snapshots hashes and file identities for the six fixed app
inputs (worker, main executable, signature record, Info.plist, catalog and release
authority), copies both executables, and verifies the complete original app and
its main executable **in the original bundle context**. Source inputs must stay
unchanged and independently reopened copied bytes must match. Only that bound
desktop copy receives structure/marker inspection: it is not treated as a
standalone signed tool or re-signed. Copied workers and writers still require
strict standalone signature verification. Copies and source context are checked
again after inspection; failure cannot proceed to a writer/entry build.

These checks retain the trusted source/toolchain and non-hostile same-user/root
build assumption. They do not defeat hostile same-UID swap-and-restore or grant
installer/root authentication. The latest shared CI signature-failure label did
not identify its artifact, so the bundle-context defect is a source diagnosis,
not a claim that the original failure has been reproduced or fixed natively.
Both worker/writer architectures, minimum OS, signatures, native dependencies
and frozen writer identities are checked. Only the freshly built native writer
is probed via its read-only `describe` command, without stdin, loader overrides
or signing material. Output, deadline and process-group cleanup are bounded;
uncertain cleanup retains its private workspace and fails closed. Other-arch
inspection is structural, not an execution claim. The JSON is an integrity
inventory and explicitly **not installer authority**. Ad hoc signing is not
publisher authentication. This development component must never be run as root
or treated as a shipping package.

Existing components can be inspected on macOS using independently known frozen
inputs, without executing any component executable:

```
python3 build_support/macos_cpu_component.py inspect \
  --component dist-macos/protected-cpu-component \
  --worker-sha256 <final-signed-worker-sha256> --build-revision <app-build-revision>
```

Inspection takes a bounded private snapshot before invoking system structural
tools. Extra files, aliases, links, inconsistent records or hashes fail closed.
Publication never overwrites an existing component. A failed component build
preserves the finished application; cleanup removes only its proven private
workspace when child cleanup is confirmed. Executable workspaces require a
non-other-user-mutable ancestor chain, including Darwin ACL and sticky-directory
policy; inherited mutation ACLs are rejected too. The app ZIP remains unchanged and
does not contain the component. Structural CI assembles and inspects it, but
does not upload it, install it, or invoke the production installer entry.

Run `python3 -B build_support/test_macos_cpu_component.py` for exact-discovery
source checks: 40 portable, 19 additional POSIX, and six additional Darwin tests.
The same command is used by CI. Fixtures are private and unprivileged. Windows
and Linux checks do not establish native Mac acceptance.

The additional Darwin regression compiles tiny inert universal fixture images,
signs worker/main/outer app in release order using ad hoc signatures (no private
key), and inspects original and independently copied bytes. It never executes
the fixture images, worker or installer. Its path-free report observes whether
the loose main verifies; neither possible outcome bypasses mandatory bundle and
bound-input checks. This isolates signing context, not Scribe's complete resource
policy. The existing full structural build exercises the real app verifier and
the same helper; actual Mac results remain required.

Full build identities have explicit immutable byte arrays in macOS images.
The CPU layout consumers also retain the fixed root and their compiled revision
and digest (the worker itself intentionally has no self-digest anchor).
Runtime string construction
alone does not promise contiguous bytes after optimization. The shared macro
uses compiler `used` plus an ordinary Mach-O data section with `no_dead_strip`;
it contains no pointers, executable code or initializer. This follows the
[Rust compiler/linker distinction](https://doc.rust-lang.org/reference/abi.html#the-used-attribute),
[LLVM section attributes](https://github.com/llvm/llvm-project/blob/main/llvm/lib/MC/MCSectionMachO.cpp)
and [Apple linker retention policy](https://github.com/apple-oss-distributions/ld64/blob/main/src/ld/parsers/macho_relocatable_file.cpp).
The native regression includes these actual production definitions in inert
Rust fixtures, with optimization, fat LTO, symbol stripping and linker dead
stripping. Both thin architectures must contain every complete marker; a second
revision/digest must replace the previous markers. No fixture is executed.
These bytes are provenance sanity checks, not signature or admission authority;
all existing signature, hash, source-context and CPU-only checks remain required.
Missing-marker errors expose only closed role/architecture labels and zero-based
marker indices, never marker values, hashes, paths or raw tool diagnostics.

Nonzero native tool exits report a closed operation label and numeric status
(including signal termination on POSIX), not a command path, argv, environment,
stdout or stderr. Labels distinguish app verification, copied-worker/in-bundle
desktop/writer signature checks, structural inspection,
each architecture's writer/entry build, combination, development-only signing
and the read-only writer description. Unknown labels reject before spawning.
This diagnoses which operation failed without publishing sensitive diagnostics;
it does not infer the underlying native/compiler error. Process bounds, exact
child/group retirement, signature checks and failure policy are unchanged.
Uncertain child cleanup takes precedence over an ordinary exit diagnostic and
retains its workspace. POSIX tests exercise real failing children with sentinel
arguments/environment/streams and check that none appears in the rendered error.

Failure cleanup is confirmed only after reaping the owned leader and a final
signal-zero probe reports the process group absent. At that point an earlier
failed termination attempt is resolved; the original output-bound/deadline
rejection still propagates, never success. Failed waits, denied/unknown probes,
surviving groups and lost child ownership remain `CleanupUncertain` and retain
the private workspace. No delivery signal is sent after reap or ownership loss;
the two-second cleanup wait and normal-exit checks are unchanged. Portable
mocked syscall-order tests cover this matrix without launching or signaling a
real process, while the existing immediate-exit overflow tests remain intact.
This rule does not infer why a particular OS denied a signal, or add containment
for descendants that leave the process group.

## Dedicated native package-entry source

The non-default `installer-package-entry` Cargo feature builds a **different
entry mode**, intended to become the package's native `postinstall` executable.
The default `describe | install --source-worker` command parser is compiled out;
package mode has no source, root, digest, revision, key or environment authority
override. It launches no sibling executable, shell or service. This mode is not
assembled into a PKG, signed, installed or executed by the source check.

Build-only `SCRIBE_MACOS_CPU_PACKAGE_WORKER` selects the already-finalized signed
universal worker from a reviewed private build snapshot. Release compilation
requires that input and the existing frozen SHA/revision. The build checks a
bounded regular input (including native no-follow/link/type checks), copies it
exclusively, independently hashes the reopened output, and embeds that snapshot.
Directive/control characters in build paths reject before Cargo metadata output.
The build toolchain and Cargo output remain trusted; this is not a hermetic build
or a signature/Mach-O verifier. The future package author must independently
verify the final worker and supply the exact same frozen inputs as the desktop.
Source tests may compile an empty debug snapshot, which cannot pass admission.

The runtime obtains its actual effective UID and accepts only the Installer
operation context: package-path argument, install location `/`, live volume `/`,
and an optional system-root argument that must also be `/`. Three or four
arguments are accepted; aliases, alternate/offline volumes and CLI modes reject.
The package path is never opened or used as authority. Invocation/UID checks
constrain scope; **they do not authenticate an Installer caller**. After those
checks, the embedded bytes are bounded and hashed against the compiled anchor
before constructing the existing protected CPU transaction. Its closed source
enum retains all original file-candidate checks in the default mode; package
mode uses only a read-only cursor, not an external image path.

Apple's current [packaging guidance](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution)
describes signed PKGs as protecting included files and using Installer for custom
installation code. Its [archived install-operation contract](https://developer.apple.com/library/archive/documentation/DeveloperTools/Conceptual/SoftwareDistribution4/Install_Operations/Install_Operations.html)
allows binary operation executables and documents these arguments. The archive
is retired: direct native `postinstall` invocation, exact current argument
semantics and protected execution of the exact authenticated entry remain
explicit native acceptance requirements. Root, Installer, the authenticated
signed package and build toolchain form the intended trusted boundary; an
Installer-shaped command line or an ad hoc development component is not proof.

The canonical source check also runs this feature's exact suite: 32 Windows,
33 Linux or 57 Mac tests, including all four invocation tests and three native
embedded-copy fixtures on Mac. It checks release rejection of missing payload,
unsafe paths, missing/nonregular input and mismatched digest, plus a compile-only
synthetic positive. No production entry runs. The existing default 21/42 tests
still run independently. Native fixtures cover embedded copy/reuse, corruption,
collisions and ambiguous-publication cleanup only—not caller authenticity.

Both architectures must embed the **same complete final universal worker**.
A universal entry therefore holds approximately two worker copies before
compression. Do not thin/re-sign the frozen worker to reduce this overhead;
actual compressed/installed package size must be reported when package authoring
is implemented. No PKG authoring, certificate provisioning, signing/notarization,
application installation, replacement or update/removal is supplied by this unit.

### Internal entry construction

`build_support/macos_cpu_pkg_entry.py` supplies an internal `build_entry`
function for the future PKG author, not a public command or installer. It accepts
the finalized app, an absolute author-owned workspace outside that app, and the
independently frozen worker SHA/build revision. On an unprivileged Mac only, it
applies the existing CPU-only app policy, copies and verifies the signed desktop
and worker snapshots, then uses fresh per-architecture Cargo directories with
locked/offline release builds and only `installer-package-entry` enabled.

Both slices embed the same complete worker snapshot. Each thin result must have
its expected architecture; the combined candidate is inspected without execution
for exactly ARM64/x64, macOS 13.0, system-only CPU dependencies and frozen markers.
Input signature verification remains mandatory; the separate structural check
on the fresh candidate is not signature or publisher authentication. No entry
invocation, signing, package assembly, extraction or installation occurs here.

Each slice is bounded by the 512 MiB worker allowance plus 32 MiB for code and
alignment; the combined entry is bounded by 1,088 MiB. These are rejection bounds,
not measured package sizes. The ordinary development writer remains limited to
64 MiB. The function exclusively publishes `postinstall` in the caller's workspace
and returns its **pre-sign** hash/size. The future author must finalize/sign,
remeasure and reverify those bytes; the returned identity grants no authority.
Existing or ambiguously published finals are never removed. Only owned temporary
files are cleaned after confirmed child retirement; uncertainty retains them.

Run `python3 -B build_support/test_macos_cpu_pkg_entry.py`, also used by Mac CI:
17 portable tests plus two POSIX tests. All native tools/builds are mocked, even
on macOS. These cover frozen inputs, both builds, environment isolation, missing,
linked, oversized and wrong-architecture outputs, tool failures, publication
collisions and uncertain cleanup. They do not establish actual native
construction, package acceptance or installation. Trusted source/toolchain/Cargo
configuration and non-hostile same-user/root writers remain build assumptions.

The dedicated native `native_macos_cpu_pkg_entry.py` construction contract
now runs after the finalized universal app is built on each Intel/ARM structural
lane. The CI step supplies the app's independently measured worker SHA and exact
build revision. It calls the real two-architecture builder in an owned private
workspace, requires one regular/single-link/executable `postinstall`, independently
checks returned hash/size and both architectures, and requires repeat construction
to reject without changing the entry's bytes or identity. It then calls the same
private packaging tail as `build_component`, using **that retained entry**, not
a second successful build or a caller-supplied prebuilt executable. The actual
system `pkgbuild --nopayload` output must pass the strict streaming inspector;
unsupported native metadata is a real failure, never a skip or guessed default.
The driver independently rehashes the package and checks entry/node/inventory,
app inputs and the package identity again through final admission. Worker,
desktop, signature record and app metadata remain unchanged; the app is
reverified too.

Neither the candidate entry nor its unsigned PKG is executed, signed, installed
or uploaded. The exact `scripts/postinstall` directory contains only that entry;
package output and scratch remain outside `scripts`. Everything stays outside
`dist-macos` and is removed only as an owned test fixture after
confirmed child cleanup. Uncertainty retains the entire fixture, including on
the repeated-build and packaging paths. The existing step retains its 50-minute
bound, dependency gates, permissions and upload paths; absent inputs, tools or
either target fail rather than skip. Its successful log report contains only
construction status, expected architectures and byte counts, including package
constructed/not-executed flags, not local paths or hashes. Its report prefix and
required CLI inputs are unchanged. Running the driver on Windows/Linux rejects
before input access.

`python3 -B build_support/test_native_macos_cpu_pkg_entry.py` checks the driver's
assertions and failure handling using mocked builders/tools: exactly 19 portable
and one additional POSIX test. The original fast entry suite remains mocked.
Two cases exercise the real shared packaging tail with only native tools mocked,
including unsupported metadata rejection. Incorrect helper results, entry/package
mutation or same-byte replacement, extra operations, phase ordering and uncertain
packaging cleanup are covered. Actual native entry/package construction is not
established until the exact-head jobs pass. Even then, this proves construction,
not package authenticity, Installer execution, installation or release qualification.

### PRE-SIGN CPU entry from the complete GPU-enabled release

`build_support/macos_full_release_cpu_entry.py` is a distinct internal constructor
for the CPU entry from the **same full app** used by Metal construction. The old
CPU-only constructor, development component and finalization APIs still require
an empty catalog/default-deny authority; they are not relaxed or given an
`allow_gpu` mode. Stripping GPU resources into a second app is not same-release
admission.

Both fixed constructors now open their own `macos_full_release.owned_release`
scope. It owns the exact bounded full-tree copy, frozen hashes/node identities,
original app/bundle-context verification and a freshly built native Rust verifier.
Both production packs and their app/build/protocol/size joins must verify before
any entry compilation. `FrozenInputs` remains import-compatible through the
Metal entry module. A yielded private lexical workspace is never accepted by a
public constructor as a caller-supplied receipt, callback or trust boolean.

The new CPU path uses only its exact private universal CPU worker and the same
fixed CPU compilation tail, CPU size bounds, identifiers/markers and non-GPU
dependency checks as the old path. It receives no Metal payload build variables.
After compilation, full source/private inventories, bytes, nodes and verifier
identity must remain unchanged before candidate byte/node checks and exclusive
publication. Retained snapshot baselines are immutable tuples, not refreshed
from current files. Normal scope completion rechecks again; failure after
publication reports failure and retains the final output, not atomic rollback.
An exceptional body exit never runs that normal check and cannot mask a
`CleanupUncertain` error or permit deletion of a possibly live workspace.

`python3 -B build_support/test_macos_full_release_cpu_entry.py` discovers exactly
11 portable tests plus one POSIX cleanup test. All native children are mocked.
They cover verification ordering/either-pack failure, every retained original
and private file's mutation/same-byte replacement, unchanged CPU-only denial,
CPU bounds/environment, publication ambiguity and cleanup-error preservation.
These are source orchestration tests, not authentic pack/signature admission or
native CPU-from-full-release construction evidence.

This result is PRE-SIGN path/hash/size only. The existing CPU finalizer/package
owner remains CPU-only and does not consume this new output. The separate
full-release owner below constructs/finalizes it itself. Eventual product
composition must retain one complete app baseline across both component builds:
`FrozenInputs` alone does not pin every desktop/Info/profile byte across separate
calls. No publisher activation, package signing/authentication, product assembly,
Installer invocation, installation or Auto enablement is added here.

### Inactive full-release CPU finalization and component owner

`build_support/macos_full_release_cpu_pkg_final.py` constructs an unsigned CPU
component from its **own freshly built** full-release CPU entry. The source-fixed
publisher policy denies before caller input access or native tools. No CLI,
prebuilt-entry input, signing callback, publisher override or key discovery exists.
The legacy CPU-only owner and its empty-catalog/default-deny admission are unchanged.

The new owner retains one complete inventory and immutable content/node snapshot
before application-publisher verification. It calls the full-release constructor,
which independently admits the app and both production packs. That same baseline
remains bound across construction, entry signing and packaging; it is never
refreshed. The six-file CPU-only packaging tail is deliberately not used.

Only the sole expected private `postinstall` can be finalized with the fixed CPU
identifier, hardened runtime and timestamp request. Existing independent Apple
Developer ID publisher/role checks, both-slice properties, no-entitlement grammar,
CPU markers/dependencies/deployment target and CPU size limits remain required.
The owner measures **post-sign** bytes and pins their node before building the
fixed CPU `--nopayload` package. The bounded streaming CPU inspector must find
exactly those bytes. Package identity is captured before inspection, rechecked
after measurement, and rechecked with full source, policy and retained-entry
identity immediately before exclusive publication. Neither an archive checksum
nor the public signing-key selector grants publisher or installation authority.

`python3 -B build_support/test_macos_full_release_cpu_pkg_final.py` discovers
exactly 17 portable tests plus one POSIX cleanup test. All native children,
including signing, remain mocked. Tests cover full-source changes and same-byte
replacement, pre/post-sign and packaged-byte binding, cross-role/version rejection,
policy changes, malformed or missing packages, collisions and uncertain cleanup.
Publication ambiguity retains the final; cleanup uncertainty retains the owned
workspace and error. This is not transactional source/package rollback.

The result is still an **unsigned component package**, not authenticated installer
input. Real full-release construction/signature qualification, component and
outer-product authentication, app placement/replacement and Installer behavior
remain separate work. Publisher policy, release routes and Auto stay default-deny.
No actual signing, installation or protected-directory writes are needed to run
these source tests.

### Native package-format prerequisite

#### Combined app/CPU/Metal product observation

`python3 -B build_support/test_macos_product_pkg_profile.py` discovers 20 portable
tests, one additional POSIX cleanup test, and one additional native test on each
Mac lane. The source-only CLI `python3 -B build_support/macos_product_pkg_profile.py`
accepts **no arguments**. It generates one tiny constant inert app payload and
two distinct comment-only operation components, then an unsigned outer product
using system `pkgbuild`/`productbuild`. The executable-mode text has no interpreter
and is never run. No real app, worker, key, signing tool or Installer is involved.

This is a format prerequisite, not a product author, acceptance codec or
authentication decision. The existing scripts-only CPU codec cannot simply be
reused for app `Payload`/`Bom` or an outer Distribution. Bounded in-memory
observation records complete member-tree shapes, app PackageInfo controls,
Distribution shapes, payload compression class and BOM presence/header class.
Unsupported encodings remain opaque/unobserved; they do not trigger extraction
or successful byte-comparison claims. A BOM header observation is not BOM
validation, and decoded CPIO fixture equality is not installed-file qualification.

Nested components may be raw XAR files or flattened member trees. The report
distinguishes these forms instead of assuming raw inner package preservation.
It compares fixed known decoded members to the original fresh component members;
raw-archive equality is reported only when that archive is actually embedded.
Transformations/mismatches remain explicit observations, not silently normalized
or accepted. Input component files, fixture directories and source files retain
their original content/node/inventory identities through assembly and reporting.

No archives are extracted, published or uploaded. Reports contain only closed
labels, flags, bounded counts/sizes and redacted XML shapes—not raw names, paths,
identities, code, metadata values, digests or XML. Global archive/member/depth,
decoded-byte and report bounds supplement the per-stream/XML limits. Ordinary
success removes the owned temporary workspace; uncertain child cleanup preserves
it and the original error. Both existing diagnostic lanes emit the report, and
structural lanes run the exact-discovery suite before Rust setup/heavy builds.

Native results are required before freezing app/product format acceptance. They
do not prove signatures, fixed app destination/non-relocation, upgrades, Installer
operation ordering, cross-component atomicity or rollback. Eventual composition
still needs one complete release baseline across fresh app/CPU/Metal construction
and the correct outer-product authentication. Existing publisher and Auto gates
remain unchanged/default-deny.

#### Scripts-only component observation

Before implementing a real PKG author/validator, run the same source command as
CI: `python3 -B build_support/test_macos_cpu_pkg_profile.py`. Exact discovery is
57 portable tests, plus two native tests on each Mac lane. The native tests use
system `pkgbuild --nopayload` separately on a fixed ASCII comment and a fixed
inert universal Mach-O named `postinstall`. The comment has no interpreter; the
native fixture is compiled from the constant `int main(void) { return 0; }`
for exactly arm64 and x86_64 using absolute system `xcrun`/`lipo` paths. Source
and intermediate outputs stay outside the sole-file scripts directory. Both
fixtures have executable mode to model the archive; **neither is executed**.
No real worker/application, signing tool, key or Installer is used by this probe.

CI runs this command once in each structural architecture lane, immediately
after checkout/Python setup and before Rust setup or other heavy work. A failed
profile prevents that lane's build, CPU/PKG contracts and artifact upload. Its
peer can proceed independently; the unchanged official job still requires
**both complete structural lanes** plus its protected manual/default-branch gate.
The structural probe remains mandatory and lane-local, with no cross-architecture
profile barrier. A healthy lane can do heavy work and upload its existing ad hoc
structural-evidence ZIP even if its
peer profile fails; that is not official signing or release acceptance. The
profile step has a four-minute deadline to include bounded native compilation.
An independent `package-diagnostics` matrix runs the same paired probe through
`python3 -B build_support/macos_cpu_pkg_profile.py`, without Rust or an app build,
so its terminal logs can be read independently of the full structural job. It
has a ten-minute job/four-minute step cap, no dependency edges, no uploads and
no environment, secrets or permission override. Official signing still needs
only the complete structural matrix and its existing protected gate. These two
extra runner starts may compete for queue capacity; no faster scheduling or
total CI time is promised. Deadlines do not bound queue delay. The packaging
source check requires unique structural, diagnostic and official boundaries
inside its unprivileged secret scan; missing boundaries cannot become empty
successes. External required-check configuration needs review before merge.

The structural matrix uses explicit `macos-15` for Apple Silicon and
`macos-15-intel` for Intel. The separately gated official job's source also
uses `macos-15`; changing its runner does not dispatch it or authorize signing.
GitHub's [macOS 14 retirement notice](https://github.com/actions/runner-images/issues/13518)
announces retirement on November 2, 2026 and warns of longer queues during
deprecation. This migration addresses that documented runner dependency, not a
proven cause of any particular queue delay or a promise of faster scheduling.
That runner migration did not change deployment target 13.0, tests, artifacts,
permissions or signing gates. Review fresh expanded profiles and full native
results on the new image; old-image success is historical evidence, not new-image
acceptance or proof of runtime compatibility on older macOS versions.

Only these freshly generated unsigned packages are read in bounded memory.
The probe has no archive/path/options input and never extracts an archive. It
reuses the existing private workspace and bounded process/reaping helper, rejects
root, and invokes only absolute system compilation and metadata-tool paths. Every
tool has bounded time/output and receives the sanitized system-only environment
plus a private TMPDIR and working directory. Its own temporary package and source
are removed after confirmed child cleanup; uncertain
cleanup retains that private workspace. Nothing is uploaded as an artifact.

After a native run completes, `SCRIBE_NATIVE_CPU_PKG_PROFILE` schema **7** records
actual outer member inventory, modes, encodings/checksum labels, bounded sizes,
fixed PackageInfo identity/operation reference, and a sanitized Scripts inventory.
The paired CLI emits the same two reports in a fixed envelope. Each report labels
its fixture kind and includes closed operation shapes: direct/global scripts and
postinstall counts, shallow container/operation records with order and duplicates,
child tags/counts, text/tail presence, attribute counts and fixed value classes.
Only `file`, `type` (binary/shell), `timeout` and `component-id` (the fixed fixture
identity) have observation labels; other names are counts and other values are
`other`. The literal postinstall timeout is reported as an integer only for
canonical positive ASCII decimal text in 1–86400; zero retains its separate
closed class. Signs, leading zeros, whitespace, Unicode digits, fractions,
exponents and out-of-bound values remain `other`, without a numeric value.
The neutral `numeric_value` field makes no unit or consumer-semantics claim.
This cap only bounds observation/output: it is not constructor acceptance or an
Installer execution limit. These labels are not Apple's schema or an allowlist.
Invalid reference errors retain only the same closed diagnostic. The
schema-six paired native reports from both architectures isolate the extra
literal `timeout` attribute in both fixtures. Every report has exactly one
postinstall referencing `./postinstall`, two attributes (file and timeout), no
children/text/tail, and no unknown attributes. The timeout was classified `other`,
which proves only that its spelling is not exactly `0`, not that it is numeric
or positive. The text/native-binary hypothesis is not supported by a difference.
Both real retained-entry constructors still failed the operation predicate.
The subsequent schema-seven reports establish canonical `600` in all four
text/universal-Mach-O fixtures across both architectures. Independently of the
observer, the private constructor now accepts only the legacy attribute set
`{file: ./postinstall}` or that exact set plus `timeout="600"`. No other value,
numeric range, normalization, attribute, operation or observer import is added.

The [vendor pkgbuild manual, reproduced in the Xcode man-page mirror](https://keith.github.io/xcode-man-pages/pkgbuild.1.html)
describes named preinstall/postinstall files under `--scripts` as top-level
operations, and directs scripts needing more than ten minutes to component-specific
configuration. Together with the observed literal, that supports this narrow
trusted-producer construction inference. It is not a modern flat-PackageInfo
schema or proof of units, timeout enforcement, cancellation, rollback or completion
on supported Installer versions. Bundle-specific timeout keys are not imported
into this no-bundle package. All other operation, identity, inventory, retained-byte,
streaming, publication and cleanup checks remain. Actual retained-entry/package
construction must pass independently on the new head; no installation or runtime
authority is granted by the fixture or this compatibility rule.

After that correction, the native ARM protected-CPU contract passed but real
construction stopped at an empty `bundle-version` direct child. That first-error
diagnostic does not establish the remaining siblings, multiplicity or significant
tails. Schema 8 therefore adds `package_info_children`: every direct child in
order, including duplicates, with a closed tag label, child/attribute counts and
significant-text/tail booleans. Known labels are `scripts`, `payload`, `bundle`,
`bundle-version`, `upgrade-bundle`, `update-bundle`, `atomic-update-bundle`,
`strict-identifier` and `relocate`; all other names are `unrecognized`. No arbitrary
names, attributes, values or recursive subtrees enter this report. Existing XML
byte/node/depth/fanout bounds remain; the existing paired diagnostics need no new
workflow, input or tool. These labels are observation vocabulary only, not a
consumer schema or constructor acceptance. That diagnostic-only change left
production parsing unchanged. No Installer execution,
privileged destination behavior, installation or release readiness is established.

The completed schema-8 Intel and ARM text/universal-Mach-O pairs report all six literal
controls once, in the order `bundle-version`, `upgrade-bundle`, `update-bundle`,
`atomic-update-bundle`, `strict-identifier`, `relocate`, followed by the sole
scripts container. Every control has zero attributes/children and false
significant-text/tail observations. The separate source compatibility rule now
supports either legacy absence or exactly one of **each** of those six empty
controls. Partial groups, duplicates, populated/nested controls, unknown same-shape
tags, case/namespace aliases and bundle references still reject. The literal set
is defined independently in the constructor, not imported from the observer.

Only ASCII XML whitespace (`space`, `tab`, `CR`, `LF`) is permitted in the new
controls' text/tails. The observer's generic whitespace booleans do **not** prove
that stricter spelling; new-head real construction must independently confirm it.
Control order is intentionally not an authority decision in this private profile;
source tests exercise permutations without claiming Installer-order equivalence.
The all-or-none grouping is our narrow compatibility policy, not an Apple schema
requirement. Both new-head real constructors remain separate pending native gates.
No arbitrary archive/root admission is added: the
fixed no-payload invocation, exact physical inventory, sole operation, retained
entry bytes, checksums, source binding, publication and cleanup rules still apply.
No consumer, installation, authentication or release qualification is inferred.

Earlier schema-3 native reports identified the literal `FinderCreateTime` field
on both architectures. Full native construction then reached a creation-time
spelling rejection; those reports did not reveal the spelling itself.
The XAR header layout follows [Apple's source](https://github.com/apple-oss-distributions/xar/blob/main/xar/include/xar.h.in).
Both basic Intel/ARM reports established the ODC prefix; complete native record
metadata still requires the expanded reports, not synthetic assumptions.

The private ODC decoder follows [Apple's CPIO format documentation](https://github.com/apple-oss-distributions/libarchive/blob/a03e74621efd47371aaf72468d24711aa763323b/libarchive/libarchive/cpio.5):
76-byte fixed ASCII-octal headers, NUL-inclusive name lengths and **no** name/data
alignment padding. The text-fixture archive remains bounded to 64 KiB; the native
fixture has a separate 256-KiB body cap and 8-KiB archive-overhead allowance. TOC,
PackageInfo and other metadata remain bounded to 64 KiB. Both archives have at most 16 records
plus one trailer, and each name to 256 bytes including NUL. Every numeric field,
name terminator and complete data range is checked before slicing. Missing or
truncated trailers, extra archives and nonzero post-trailer data reject. Terminal
zero-byte padding is counted without presuming a 512-byte block convention.

A separate fixed-fixture check requires exactly one regular `postinstall` or
`./postinstall` record containing exactly its own retained fixture bytes, optionally
one empty `.` or `./` directory, with no extras or duplicate aliases. Actual order,
root spelling, modes, link counts, offsets and trailer metadata are observed.
Zero/multiple link counts and permission flags are **not** accepted as safe or
frozen into a shipping profile; no links are resolved and no data is shared or
extracted. Only closed fixture names/types and bounded numeric observations are
reported. Raw payload prefixes (which can expose device/inode fields), identities,
owners, raw timestamps, data and hashes are omitted. This private report-schema change
does not change a worker protocol, pack manifest, runtime or installer authority.

Portable fixtures now encode valid ODC independently, with a literal golden
header, every-field malformed-octal and truncation coverage, odd name/data sizes,
root/order variants, aliases/types/partial payloads, link metadata, record/length
bounds and trailer/tail cases. Preserve both actual expanded native reports
before freezing a real PKG author/validator profile; no optional Bom/Payload
member or native tool compatibility is inferred from these synthetic fixtures.

Outer member `mode` is observation metadata: an absent field is reported as JSON
`null`, never defaulted to permissions or treated as safe/executable. A present
field still requires one bounded leaf scalar; duplicate, nested, empty or unsafe
values reject. Apple's [XAR stat source](https://github.com/apple-oss-distributions/xar/blob/main/xar/lib/stat.c)
writes this property conditionally. This reporting rule does not relax required
member names/types, data bounds or encoding checks, and grants no extraction or
installation authority. Portable fixtures cover missing modes independently and
together. A native failure at mode lookup alone cannot distinguish missing from
duplicate/nested metadata; only the corrected native report can establish that.

The report retains schema 3's sorted direct-child metadata shapes for each member:
a closed tag label, child count, attribute count and text-presence boolean.
Duplicates remain separate records. Unknown tag names become `unrecognized`;
no scalar values, attribute names/values, nested names, identities, owners,
timestamps, hashes or payload bytes are included. Closed labels may name stat
properties, but cannot supply their values or grant acceptance. Apple's
[XAR stat writer](https://github.com/apple-oss-distributions/xar/blob/main/xar/lib/stat.c)
can emit `inode` and `deviceno`; that is not evidence that either caused a
particular native failure. The real constructor's existing unsupported-metadata
error now distinguishes duplicate and unsupported counts using only a numeric
member index. Earlier native profiles established scalar `inode`/`deviceno` and
one redacted two-child container on Scripts; actual construction then rejected
three unsupported fields with no duplicates. Later schema-3 reports established
the container's label, but not complete constructor compatibility.

Schema 4 additionally observes the character shape of the exact literal
`FinderCreateTime/time` leaf. Missing or ambiguous containers/leaves, attributed
or nested time leaves, empty text and significant surrounding text produce only
closed status labels. A usable leaf produces at most 64 masked characters and a
truncation flag: ASCII digits become `d`, only `T`, `Z`, `-`, `:`, `.` and `+`
remain literal, and every other codepoint becomes `?`. Nanoseconds and other
properties are not inspected by this observer. The mask intentionally reveals
bounded character positions and selected separators, not raw date/digit values,
attribute data, identities, arbitrary names or paths. It describes formatting,
never timestamp validity, acceptance or authority.

The public Darwin attribute writer below uses bare `%FT%T`, while native
construction rejected the former strict bare-time grammar. That generic error
does not establish a particular suffix or replacement grammar. The observer
remains useful for bounded serialization diagnosis, but does not define the
constructor's acceptance policy or grant authority. Full retained-entry/package
construction must still pass independently. The report adds no workflow lane,
input option or runtime diagnostic.

Schema 5 retains those observations and adds exactly nine fixed PackageInfo
root-attribute records. The five existing construction labels and four candidates
(`overwrite-permissions`, `relocatable`, `postinstall-action`, `generator-version`)
form an observation vocabulary only. The latter names occur in
[Mozilla's own package template](https://github.com/mozilla-firefox/firefox/blob/main/python/mozbuild/mozpack/apple_pkg/PackageInfo.template),
which is not Apple's schema or evidence of current native output. Identity,
version and location values are redacted. Other fields expose only key-specific
closed classes: absent, expected literal root/2/true/false/none, other, or bounded
printable generator text (1–128 ASCII characters). No generator text is emitted.
Unknown and namespaced names contribute only to a numeric count; their names
and values never enter the report. The fixed record count bounds output even
when the already bounded XML has many unknown attributes; nothing is truncated
or selected from unknown fields.

The terminal ARM observation established `overwrite-permissions="true"`,
`relocatable="false"`, `postinstall-action="none"` and bounded printable generator
text in the fixed fixture, with no unknown root attributes. The separate real
constructor rejected the same four extra names; its error did not expose values.
The private unsigned constructor now permits those three controls only at their
exact observed literals, optionally absent for legacy minimal metadata. A present
generator is ignored 1–128-character printable ASCII, never a path, command,
identity, compatibility decision or diagnostic value. The constructor allowlist
is explicit and independent of the observation vocabulary. Unknown/namespaced
attributes still reject and expose only a numeric count.

[Apple's archived component-package guide](https://developer.apple.com/library/archive/documentation/DeveloperTools/Conceptual/SoftwareDistribution4/Component_Packages/Component_Packages.html)
describes directory-permission overwrite for directories in the component
hierarchy, not the installation destination itself. Combined with the exact
physical `PackageInfo` plus `Scripts` inventory (**no Payload or Bom**), this
supports a narrow no-payload construction inference under the trusted-producer
assumptions above. An optional payload metadata marker must still be exactly
zero files/zero KiB; an absent marker remains supported. This legacy guidance
does **not** establish modern Installer, privileged execution or destination
permission behavior. Both new-head native construction lanes remain required;
package authentication, actual installation and release qualification are separate.

The sole exact `./postinstall` operation, retained-entry hash/size, app/source
binding, archive ranges/checksums, link restrictions and cleanup remain unchanged.
Other direct-child constructs still reject at the same predicate. Their error
uses only fixed tag labels (otherwise `unrecognized`), bounded child/attribute
counts and a text-presence boolean, never arbitrary names, values or nested text.
Source fixtures cover all optional-field combinations with/without the zero
marker, opposite/noncanonical controls, generator bounds/redaction, and valid
controls combined with forbidden authority, payload, operation and archive changes.
The shared-tail driver fixture exercises the full observed profile. Nothing here
runs Installer, an operation or a worker, or grants installation/root authority.

The fresh-output constructor now permits only three additional optional stat
properties on **Scripts**, not PackageInfo. `inode` and `deviceno` use canonical
signed-64 and signed-32 decimal text respectively, matching the serialization in
[Apple's XAR configuration](https://github.com/apple-oss-distributions/xar/blob/main/xar/configure.ac)
and macOS's device type, rather than treating a serialized inode as an unsigned
filesystem identity. Values are never compared with this host, reinterpreted as
identity, recorded in diagnostics or an identity cache, or used as authority.

Only the literal `FinderCreateTime` container is supported, following
[Apple's Darwin attribute writer](https://github.com/apple-oss-distributions/xar/blob/main/xar/lib/darwinattr.c):
exactly `time` and `nanoseconds` leaves, no attributes, nested/extra/duplicate
children or significant surrounding text. The `time` leaf is opaque text:
1–128 printable ASCII characters, like the other archive timestamps. It is
discarded, not parsed as a date, compared with this host, used in a path/command,
recorded in diagnostics or treated as identity, validity or authority. This
deliberately removes date-format conformance, not a content-integrity check;
zone/fraction spellings and non-date printable text are equally inert.
Nanoseconds remain canonical decimal 0–999999999. This narrowly scoped policy
for this invocation's fresh output is not arbitrary-archive admission or proof
of native construction/installation compatibility. Every other unknown property
still rejects even with the same two-child shape. Inventory, types/links,
signatures, checksums, retained entry bytes, complete heap coverage, source
binding and cleanup rules are unchanged.

This probe validates only its fixed fixture's Scripts inventory, not checksums,
signatures, shipping-package operations or package authenticity. It is not a
reusable arbitrary-archive inspector, trusted installer, installation test or
shipping format validator. Actual package entry
composition, full hostile-archive validation, signed-package authenticity,
Installer binary execution and install/update/removal/rollback remain separate
gates. Portable checks do not establish native observations; review completed
preflight reports separately from full structural results.

### Internal unsigned CPU component package author

`build_support/macos_cpu_pkg.py` adds an internal, unprivileged constructor for
the CPU-only app admitted by the existing verifier. It is not a CLI, arbitrary
archive inspector or installation entry. It calls the existing fresh universal
CPU entry constructor into a private, exact one-file `scripts/postinstall`
directory, retains the measured entry SHA-256/size/node identity, then invokes
only the absolute system `pkgbuild --nopayload` with a source-fixed identifier,
the verified app version and install location `/`. It never executes that entry
or a worker, signs anything, installs, accesses keys, or accepts GPU-positive apps
through the CPU-only verifier.

Both expanded format observations have been reviewed: Intel on `macos-15-intel`
and Apple Silicon on `macos-15`. They establish the narrow outer members and ODC
record layout, not every PackageInfo attribute or native author acceptance. The
constructor's explicit PackageInfo policy therefore rejects unqualified fields,
references and operations. Its synthetic positive fixtures are not claimed to be
the full native PackageInfo output. An unsupported native construct must be
investigated and reviewed before extending that policy; it must not be silently
accepted as an inferred default. Current-head CI readiness remains a separate
gate from format-design observations.

The private inspector reads only this invocation's fresh, regular output. The
package size is bounded relative to the measured entry; all file reads and
decompressor output calls are at most 64 KiB, without loading the potentially
over-1-GiB entry into memory. Metadata has independent 64-KiB bounds. It checks
the XAR header, exact two-member inventory, complete nonoverlapping heap coverage,
complete bounded compression streams, declared sizes and actual container
checksums. Apple's [XAR writer](https://github.com/apple-oss-distributions/xar/blob/main/xar/lib/archive.c)
hashes the **compressed** TOC and stores its SHA-1 in the heap; its
[member hashing implementation](https://github.com/apple-oss-distributions/xar/blob/main/xar/lib/hash.c)
distinguishes archived and extracted checksums. These SHA-1 checks establish only
format integrity, never project or installer trust.

The Scripts member must contain exactly one matching regular `./postinstall`
ODC record (optional observed `.` directory, exact modes/link counts), one trailer
and bounded zero padding, with no extra actions, aliases or links. Every entry
byte is streamed into an independent SHA-256 comparison against the retained
entry. All compression input is consumed, including after the trailer; truncated,
concatenated or trailing streams reject. Nothing is extracted. The app snapshot
and retained entry are rechecked before no-overwrite publication as
`cpu-worker.pkg`, returning only an **unsigned, pre-sign** path/hash/size. Child
cleanup uncertainty retains the owned stage; collision or ambiguous publication
never removes an existing final output. Reviewed source/toolchain and non-hostile
same-user/root writers remain assumptions, not a new hostile-author sandbox.

`python3 -B build_support/test_macos_cpu_pkg.py` discovers exactly 46 portable
tests and two additional POSIX tests. Native tools and entry construction are
mocked; an independent XAR/ODC encoder supplies actual checksums. Cases include
chunk/size boundaries, mutations, archive/operation identity, malformed XML,
range/checksum errors, truncation/tails, links, source replacement, collisions
and uncertain cleanup. Exact timeout tests cover absence/600, noncanonical and
other values, unknown/namespaced keys, missing/wrong file references and extra or
nested operations. Existing payload, entry, link/checksum and source-mutation
negatives run with and without the permitted timeout. This fast suite is always
mocked. The existing native
construction contract now calls the same private packaging tail on its retained
entry, without rebuilding or publishing the package as an artifact. Actual native
author acceptance is not established until that exact-head contract passes;
final entry signing/repackaging, package authentication,
notarization, product composition/order and Installer execution remain unproven.

### Inactive finalized-entry authoring boundary

`build_support/macos_cpu_pkg_final.py` is a separate internal owner, not a CLI,
release option or installer. It preserves the existing PRE-SIGN constructor and
unsigned development path. The source-fixed
`runtime-manifests/macos-installer-entry-signing-policy.json` is **default-deny**
with an empty publisher. It rejects before app/workspace access, construction or
native tools. No environment, caller-supplied policy, path, signing callback,
prebuilt entry or displayed certificate identity can provision trust.

A future independently reviewed positive policy must pin a Developer ID
Application publisher/team. Application, CPU-entry and reserved Metal-entry code
identifiers are distinct source constants. The fixed requirement combines the
Apple generic anchor, Developer ID issuer and Application certificate OIDs,
publisher Team ID and exact role identifier, as described in
[Apple TN3127](https://developer.apple.com/documentation/technotes/tn3127-inside-code-signing-requirements).
The selector is only a canonical public certificate SHA-1 fingerprint, never
authority or a private key. A wrong publisher, development/ad hoc certificate,
Installer certificate, role or identifier must fail independent all-architecture
verification. No production publisher or actual key use is selected here.

The owner verifies the CPU-only app and publisher, constructs its own fresh entry
in an owned private stage, and requests the fixed identifier, hardened runtime
and secure timestamp. It then independently verifies the publisher requirement,
checks both slices' bounded display properties and absent entitlements or one
exact empty XML plist dictionary (no extra values, attributes or text),
and reinspects structure and frozen markers. Displayed identity is not a trust
source. Only the exact codesign property command can return its already-bounded
stderr to this private in-memory parser; failures still expose only closed
phase/status. The parser is intentionally fail-closed on unknown required-field
formats, not a stable Apple metadata schema or native-signature qualification.

The finalized bytes are independently reopened/hashed and retained by node
identity. App inputs and source policy must remain unchanged. The existing
streaming package tail receives the **post-sign** digest/size, validates every
packaged entry byte, and publishes without overwrite. The output remains an
**unsigned component package**, not authenticated root input. Confirmed failures
publish nothing; uncertain child cleanup retains the owned stage, and ambiguous
publication retains the final. Trusted reviewed build tools and non-hostile
same-user/root writers remain assumptions; signatures plus markers do not prove
arbitrary transformed code came from this build.

`python3 -B build_support/test_macos_cpu_pkg_final.py` discovers exactly 18 mocked
tests on every host. Synthetic policy and tools exist only in test-private
patches/fixtures. They establish default denial, closed orchestration,
post-sign byte binding, both-slice checks, rejection paths and publication
behavior—not real signatures, publisher trust or Mac acceptance. CI invokes only
this mocked suite and checks that the public policy remains unprovisioned.
No release/signing path, protected environment, key, installation or Auto gate
is activated. Actual signature/native-tool acceptance, production policy,
final package authentication and product composition remain separate gates.

Apple distinguishes executable and package signing identities and requires
signing nested contents before their containers; see
[creating distribution-signed code](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/)
and [packaging Mac software](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution).
This CPU-first source stage does not replace the planned Metal installer work.

## Not yet established

The enclosing installer must authenticate this executable and its compiled
build before running it as root. A user-writable extraction tree is **not** a
trusted bootstrap. Trusted package assembly, protected entry-script/extraction
rules, signing/notarization, application replacement, full rollback policy,
real installation/update/removal, and clean-machine acceptance are separate
delivery work. Root itself must not be a concurrent hostile writer; this is not
a defense against a compromised root account.

Private fixtures exercise the real fixed-ancestor creation helper, but never
the production /Library root walk or installation entry. Interruption before
ancestor publication leaves only an unadmitted temporary name; retry creates a
new stage instead of adopting or sweeping an earlier one. A published ancestor
already has its final traversal permissions. Existing unsafe nodes still fail
closed without repair. Native execution, actual power-loss durability and
installer-level recovery acceptance remain unverified until qualified.

Native CI, filesystem sync behavior under power loss, actual signed-image
admission, and installer-level durability/rollback require their own evidence.
Portable tests are not Mac acceptance or release qualification.
