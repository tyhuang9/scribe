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
Hello lifetime and terminates/reaps the whole process group on failure.

Future GPU packs retain their reserved FHS location under
`/usr/lib/scribe/workers/packs/<id>/<version>/<digest>/`, but this delivery does
not enable Linux GPU discovery, add release trust keys, or make GPU `Auto`
nonempty.

Run the isolated verification suite on Linux without Cargo:

```sh
./scripts/test-linux-worker-launch.sh
```

The suite intentionally uses `rustc` directly because the reviewed Linux
Sherpa archive is not yet present. CI runs it on Ubuntu 22.04 and 24.04.
