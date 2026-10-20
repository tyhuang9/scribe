# Protected macOS CPU writer — source component

This isolated tool is installer **source**, not a shipping installer, privileged
service, runtime download mechanism, or permission to install it. The desktop
never starts it. It does not install the application, VAD, or Metal workers and
does not change Auto eligibility.

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
creates only missing Scribe/CPU directories. Existing nodes are never repaired.
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
No model, audio, driver, real protected root, signing material, or user cache is
used. CI runs the same check on Intel and ARM.

## Not yet established

The enclosing installer must authenticate this executable and its compiled
build before running it as root. A user-writable extraction tree is **not** a
trusted bootstrap. Trusted package assembly, protected entry-script/extraction
rules, signing/notarization, application replacement, full rollback policy,
real installation/update/removal, and clean-machine acceptance are separate
delivery work. Root itself must not be a concurrent hostile writer; this is not
a defense against a compromised root account.

Private fixtures bypass production ancestor bootstrap. A first-install crash
between creating a missing ancestor privately and sealing it user-traversable
can leave that root-owned ancestor inaccessible. A retry deliberately refuses
to repair existing nodes; installer-level recovery must be designed and tested
before shipping. No automatic permission repair is authorized by this tool.

Native CI, filesystem sync behavior under power loss, actual signed-image
admission, and installer-level durability/rollback require their own evidence.
Portable tests are not Mac acceptance or release qualification.
