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
source checks: 27 portable, 18 additional POSIX, and four additional Darwin tests.
The same command is used by CI. Fixtures are private and unprivileged. Windows
and Linux checks do not establish native Mac acceptance.

Nonzero native tool exits report a closed operation label and numeric status
(including signal termination on POSIX), not a command path, argv, environment,
stdout or stderr. Labels distinguish app verification, structural inspection,
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
to reject without changing the entry's bytes or identity. Worker, desktop,
signature record and app metadata remain unchanged; the app is reverified too.

The candidate is never executed, signed, assembled into a PKG or uploaded. It
stays outside `dist-macos` and is removed only as an owned test fixture after
confirmed child cleanup. Uncertainty retains the entire fixture, including on
the repeated-build path. The step has a 50-minute bound; absent inputs, tools or
either target fail rather than skip. Its successful log report contains only
construction status, expected architectures and byte counts, not local paths
or hashes. Running the driver on Windows/Linux rejects before input access.

`python3 -B build_support/test_native_macos_cpu_pkg_entry.py` checks the driver's
assertions and failure handling using mocked builders/tools: exactly 13 portable
and one additional POSIX test. The original fast entry suite remains mocked.
Actual native construction is not established until the new exact-head jobs
pass. Even then, this proves construction, not package authenticity, Installer
execution, installation or release qualification.

### Native package-format prerequisite

Before implementing a real PKG author/validator, run the same source command as
CI: `python3 -B build_support/test_macos_cpu_pkg_profile.py`. Exact discovery is
16 portable tests, plus one native test on each Mac lane. That native test uses
the system `pkgbuild --nopayload` on one fixed, tiny ASCII comment file named
`postinstall`, with no interpreter or program content. The file has executable
mode only to model an operation archive; **nothing executes it**. Neither the
real entry nor any worker/application is built, signed, packaged or launched.

Only this freshly generated unsigned fixture is read, entirely in bounded memory.
The probe has no archive/path/options input and never extracts an archive. It
reuses the existing private workspace and bounded process/reaping helper, rejects
root, and invokes only absolute system `pkgbuild` and `xar -tf` paths. Its own
temporary package and source are removed after confirmed child cleanup; uncertain
cleanup retains that private workspace. Nothing is uploaded as an artifact.

`SCRIBE_NATIVE_CPU_PKG_PROFILE` in completed native CI logs records a path-free
observation: actual outer member inventory, modes, encodings/checksum labels,
bounded header/TOC/member sizes, fixed PackageInfo identity/operation reference
and decoded Scripts prefix. The header layout follows [Apple's XAR source](https://github.com/apple-oss-distributions/xar/blob/main/xar/include/xar.h.in).
No CPIO dialect, optional Bom/Payload member or native tool compatibility is
presumed from synthetic fixtures. Review both actual Mac reports before freezing
the supported PKG profile and implementing full bounded Scripts parsing.

This probe does not validate checksums, signatures, exact operation inventory or
package authenticity. It is not a reusable arbitrary-archive inspector, trusted
installer, installation test or shipping format validator. Actual package entry
composition, full hostile-archive validation, signed-package authenticity,
Installer binary execution and install/update/removal/rollback remain separate
gates. Until the native jobs finish, only the portable observation code is tested.

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
