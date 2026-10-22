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
source checks: 35 portable, 18 additional POSIX, and six additional Darwin tests.
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

### Native package-format prerequisite

Before implementing a real PKG author/validator, run the same source command as
CI: `python3 -B build_support/test_macos_cpu_pkg_profile.py`. Exact discovery is
42 portable tests, plus one native test on each Mac lane. That native test uses
the system `pkgbuild --nopayload` on one fixed, tiny ASCII comment file named
`postinstall`, with no interpreter or program content. The file has executable
mode only to model an operation archive; **nothing executes it**. Neither the
real entry nor any worker/application is built, signed, packaged or launched.

CI runs this command once in each structural architecture lane, immediately
after checkout/Python setup and before Rust setup or other heavy work. A failed
profile prevents that lane's build, CPU/PKG contracts and artifact upload. Its
peer can proceed independently; the unchanged official job still requires
**both complete structural lanes** plus its protected manual/default-branch gate.
No command, permission, secret or artifact destination is added. Profile logs
remain readable before the long native build finishes.

This removes two job startups and the cross-architecture profile barrier; it
does not promise shorter runner queues or total CI time. A healthy lane can now
do heavy work and upload its existing ad hoc structural-evidence ZIP even if its
peer profile fails; that is not official signing or release acceptance. The
separate profile job's ten-minute cap is removed. The profile step retains its
two-minute deadline and existing process bounds inside the structural job's
unchanged lifecycle; none of these limits bounds queue delay. The packaging
source check requires both unique structural/official boundaries inside its
unprivileged scan before rejecting secrets, so a missing boundary cannot turn
the scan into an empty success. The former standalone profile check names are
removed; any external required-check configuration must be reviewed before merge.

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

Only this freshly generated unsigned fixture is read, entirely in bounded memory.
The probe has no archive/path/options input and never extracts an archive. It
reuses the existing private workspace and bounded process/reaping helper, rejects
root, and invokes only absolute system `pkgbuild` and `xar -tf` paths. Its own
temporary package and source are removed after confirmed child cleanup; uncertain
cleanup retains that private workspace. Nothing is uploaded as an artifact.

After a native run completes, `SCRIBE_NATIVE_CPU_PKG_PROFILE` schema **5** records
actual outer member inventory, modes, encodings/checksum labels, bounded sizes,
fixed PackageInfo identity/operation reference, and a sanitized Scripts inventory.
Earlier schema-3 native reports identified the literal `FinderCreateTime` field
on both architectures. Full native construction then reached a creation-time
spelling rejection; those reports did not reveal the spelling itself.
The XAR header layout follows [Apple's source](https://github.com/apple-oss-distributions/xar/blob/main/xar/include/xar.h.in).
Both basic Intel/ARM reports established the ODC prefix; complete native record
metadata still requires the expanded reports, not synthetic assumptions.

The private ODC decoder follows [Apple's CPIO format documentation](https://github.com/apple-oss-distributions/libarchive/blob/a03e74621efd47371aaf72468d24711aa763323b/libarchive/libarchive/cpio.5):
76-byte fixed ASCII-octal headers, NUL-inclusive name lengths and **no** name/data
alignment padding. It bounds the decoded archive to 64 KiB, at most 16 records
plus one trailer, and each name to 256 bytes including NUL. Every numeric field,
name terminator and complete data range is checked before slicing. Missing or
truncated trailers, extra archives and nonzero post-trailer data reject. Terminal
zero-byte padding is counted without presuming a 512-byte block convention.

A separate fixed-fixture check requires exactly one regular `postinstall` or
`./postinstall` record containing exactly the inert fixture bytes, optionally
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

The actual constructor's unsupported-attribute error now adds only sorted
recognized extra labels and a numeric unrecognized count. Its five-attribute
allowlist is unchanged: every candidate remains rejected, even a false/none value.
PackageInfo controls Installer behavior, so neither a zero-payload archive nor
the retained postinstall bytes prove permission/relocation/postinstall flags inert.
Review actual native observations and consumer semantics before qualifying any
additional construction field. This diagnostic does not fix or bypass the
current native PackageInfo rejection, and never runs Installer or an operation.

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

`python3 -B build_support/test_macos_cpu_pkg.py` discovers exactly 36 portable
tests and two additional POSIX tests. Native tools and entry construction are
mocked; an independent XAR/ODC encoder supplies actual checksums. Cases include
chunk/size boundaries, mutations, archive/operation identity, malformed XML,
range/checksum errors, truncation/tails, links, source replacement, collisions
and uncertain cleanup. This fast suite is always mocked. The existing native
construction contract now calls the same private packaging tail on its retained
entry, without rebuilding or publishing the package as an artifact. Actual native
author acceptance is not established until that exact-head contract passes;
final entry signing/repackaging, package authentication,
notarization, product composition/order and Installer execution remain unproven.

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
