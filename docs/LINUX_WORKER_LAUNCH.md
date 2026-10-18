# Linux packaged worker launch contract

This contract applies only to `x86_64-unknown-linux-gnu`. Linux releases install
the desktop at `/usr/bin/local-transcriber` and the CPU inference worker at
`/usr/lib/scribe/scribe-inference-worker`. The desktop path is not part of
process-creation authority; a package may manage `/usr/bin/local-transcriber`
separately, including as a symlink.

The desktop admits the worker only from the compile-time `/usr/lib/scribe`
authority. Every ancestor and the install root must be root-owned and not
group- or other-writable. The worker must have the exact reviewed name, be a
single-link regular executable, and match the release SHA-256 embedded when the
desktop is built. Runtime environment variables and path overrides cannot move
this authority.

Admission and recheck use retained descriptors and `openat2` with
`RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS`. The child is
created with a fixed descriptor set, a private process group, parent-death
signal, `no_new_privs`, descriptor-rooted working directory, `close_range`, and
`execveat(AT_EMPTY_PATH)`. A CLOEXEC error pipe makes setup and exec failures
bounded and observable. The parent retains the authority through the worker
Hello lifetime. An internal creator/guardian thread performs `fork` and remains
alive for the worker lifetime so `PR_SET_PDEATHSIG` is not tied to the bounded
supervisor's short-lived launch helper. The process wrapper terminates/reaps the
whole process group on failure, including after the group leader exits. The
actual execution descriptor is a private `memfd_create` snapshot whose length
and SHA-256 are verified during copying and which is sealed against writes,
growth, shrinkage, and further seal changes before `fork`. Fixed descriptor 3
deliberately survives `execveat`; the worker verifies its anonymous memfd
identity and exact seals, hashes a duplicate for its one Hello capability, and
then closes the inherited image descriptor. Descriptor cleanup remains bounded
above fixed descriptor 6.

The current Ubuntu host prerequisite is executable-memfd allowance. When the
host exposes `/proc/sys/vm/memfd_noexec`, its value must be `0`. Older kernels
without that policy use their legacy executable-memfd behavior. A denial at
`execveat` is reported as a categorized `EACCES` error; newer kernels may reject
the memfd earlier and surface a generic OS error. Every denial fails closed,
does not fall back to the mutable source inode, and does not require newer
`MFD_EXEC` support, preserving compatibility with kernel 5.15.

The deterministic Debian package described in
`docs/LINUX_RELEASE_PACKAGING.md` now publishes this CPU worker path with its
exact SHA-256 embedded in the desktop and an exact installed-file inventory.
Package verification rejects partial trees, links, mode changes, unexpected
files, and worker-anchor mismatches before installation evidence is accepted.

Future GPU packs retain their reserved FHS location under
`/usr/lib/scribe/workers/packs/<id>/<version>/<digest>/`, but this delivery does
not enable Linux GPU discovery, add release trust keys, or make GPU `Auto`
nonempty.

Run the isolated launcher and authority verification suite on Linux:

```sh
./scripts/test-linux-worker-launch.sh
```

That suite uses direct `rustc` kernel fixtures and locked, offline Cargo tests
for the portable authority tool. It remains separate from actual application
linking. CI runs both kinds of checks on Ubuntu 22.04 and 24.04.

For the actual CPU-worker contract, fetch the main crate's locked dependencies
and prepare the already-reviewed Linux x64 Sherpa archive, then run:

```sh
cargo fetch --locked
bash scripts/prepare-linux-release-inputs.sh --output-directory "$RUNNER_TEMP/scribe-reviewed-linux-native"
SHERPA_ONNX_ARCHIVE_DIR="$RUNNER_TEMP/scribe-reviewed-linux-native" bash scripts/test-linux-native-cpu-worker.sh
```

The input preparer downloads only the fixed build-time archive, caps its size,
and checks its reviewed length and SHA-256 before atomic publication. The
vendor build independently verifies the same pins before extraction. This is
CI/developer build preparation, not an application runtime download.
`scripts/test-linux-release-inputs.sh` checks short/oversized downloads, wrong
digests, transport failure and existing/linked output rejection offline using a
mock transport; it never substitutes synthetic bytes for successful admission.

The bounded native script links the actual release CPU worker, embeds that
worker's digest in the actual release desktop and test parent, requires exactly
one ignored native test to be discovered, and runs it through the ordinary
descriptor-bound launcher. Its temporary test-only authority relaxes ownership
for the fixture; it does not change production `/usr/lib/scribe` authority.
The test rejects the wrong digest before launch, authenticates SCIF v5 Ready
and exact CPU/build/ABI identity, sends model-free Health and Shutdown, and
confirms exit and reap before removing the fixture. The script then assembles,
independently verifies and size-reports the actual pair's CPU-only Debian
package without installing it. Owned temporary build/test outputs are removed
on exit.

The packaged CPU recipe explicitly sets the upstream-supported
`TRANSCRIBE_USE_SYSTEM_BLAS=OFF` for the worker, desktop and test parent, after
clearing both ambient CMake argument overrides. The current package owns no
BLAS payload or dependency contract. Automatic host BLAS discovery previously
enabled CBLAS calls whose absolute link dependency was not propagated by the
native Rust wrapper, producing an actual release-worker link failure. This
profile avoids inheriting an unowned build-host runtime; it is not a dummy
worker or a skipped link check. The harness checks the regenerated CMake cache
and link manifest, then both deliverable ELFs for unexpected BLAS requirements.
The native ggml CPU backend/threadpool is unchanged. The optional host-decoder
CBLAS fast path is disabled and may be slower; CPU transcription/performance
remains unqualified. Adding optimized BLAS later requires an explicitly owned
link/runtime inventory and separate qualification, not an ambient `-lblas` fix.

Use the bounded script rather than invoking the ignored Rust test directly:
its outer deadline also bounds blocking protocol reads. Reaping confirms that
the worker is no longer running; the process API does not expose its exit code.

No model or audio is loaded. This verifies native linking, startup, IPC,
shutdown and package assembly, not transcription correctness, clean-machine
installation, Linux GPU activation or hardware/performance qualification.
