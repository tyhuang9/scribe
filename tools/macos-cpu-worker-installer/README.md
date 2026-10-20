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
source checks: 21 portable, 17 additional POSIX, and four additional Darwin tests.
The same command is used by CI. Fixtures are private and unprivileged. Windows
and Linux checks do not establish native Mac acceptance.

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
