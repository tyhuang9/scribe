# Linux release packaging contract

Stage 7D assembles a deterministic Ubuntu x86_64 Debian package from already
built desktop and CPU-worker executables. It does not build or download native
inference dependencies. The package installs the desktop at
`/usr/bin/local-transcriber`, the CPU worker at
`/usr/lib/scribe/scribe-inference-worker`, and reserves immutable GPU packs
under `/usr/lib/scribe/workers/packs/<id>/<version>/<digest>/`.

The CPU-only payload also includes
`/usr/lib/scribe/gpu-pack-release-authority.json`: the exact reviewed Linux
epoch-zero, empty authority, with its source transport newline removed. The
assembler and independent verifier compare it with fixed reviewed bytes and
the exact empty catalog. Recomputing an unsigned inventory cannot authorize
different authority bytes, a positive epoch or GPU entries.

The checked-in release contract and package catalog are canonical JSON. The
catalog is exactly `{"schema_version":1,"packs":[]}`. The shared project public
key can authenticate compatible CUDA/Vulkan packs with `ProductionTrustRoot`,
but the key bootstrap supplies no Linux packs or release qualification. The
assembler still rejects every nonempty GPU pack set, including one that passes
signature verification. Its unchanged release contract's
`"production_trust":"empty"` field is a legacy CPU-only stage marker, not the
shared key inventory. Fixture signing is available only to tests and size-report
evidence; production assembly and
production size reporting never accept it. Linux GPU discovery, the runtime
registry, and Auto qualification remain empty/default-deny.

`build-linux-release-package.sh` refuses symlink arguments, hardlinked inputs,
non-ELF or non-x86_64 executables, an existing output, a desktop that does not
contain the exact CPU-worker SHA-256 anchor, or any GPU pack that cannot pass
the Rust production verifier and immutable `PackStore`. It writes an exact
sorted file inventory, sets every directory and executable to `0755`, sets all
metadata to `0644`, normalizes timestamps from `SOURCE_DATE_EPOCH`, uses root
ownership in the archive, and publishes the completed `.deb` by one
same-directory rename. The adjacent canonical size report records the sum of
regular installed-file bytes and compressed package bytes. Package size
reporting consumes the package verifier's machine-readable result and rejects
a stale sidecar. Pack-size reporting first invokes either fixture-only or
production Rust verification and labels the trust mode.

`verify-linux-release-package.sh` independently parses the outer `ar` archive,
requires exactly the three ordered Debian members, and bounds both compressed
members and their XZ expansion before any tar processing. It requires the exact
control field set and the independently authorized six-file CPU payload, then
rejects unsafe names, duplicates, case collisions, links, nonregular entries,
non-root ownership, and unsafe archive modes without normalizing them during
extraction. It also checks ELF identity, the exact inventory, deterministic
regular-file byte sum, modes, sizes, hashes, FHS paths, empty pack tree,
canonical catalog, reviewed release contract, and CPU-worker anchor. Extending
the payload for GPU packs requires separate authenticated release authority;
the unsigned inventory cannot authorize additional files. This makes the
installed CPU worker compatible with the descriptor-bound `openat2` plus
sealed-`memfd` launcher while leaving no mutable path fallback.

On supported Linux x86_64 GNU, desktop GPU discovery consumes its embedded
authority through the typed validator against the canonical empty catalog.
Reviewed epoch-zero metadata reports that production Linux GPU support is
disabled; invalid or positive metadata reports a separate rejection. Both
paths return zero packs and no catalog generation. This data-only check does
not read installed metadata, access a security floor or health cache, or start
a provider. Unsupported targets retain their unsupported-platform result.

Build-time Linux policy and packaging independently require the same reviewed
empty document. This is artifact-integrity checking, not publisher
authentication: the package verifier does not execute the desktop and cannot
prove the provenance or embedded authority of an arbitrary supplied ELF. No
synthetic linker marker or new digest-grep proof substitutes for that missing
provenance. Positive authenticated release inclusion remains separate work.

Run the native package and attack suite on Ubuntu 22.04 or 24.04:

Prerequisites are the system C/Debian/Python packaging tools and, for the Rust
pack-verifier cases, the pinned Rust toolchain with locked dependencies fetched
separately using
`cargo fetch --locked --manifest-path tools/worker-pack-author/Cargo.toml`.
Verification uses the cached dependencies offline; it does not provision tools
or start services.

```sh
./scripts/test-linux-release-packaging.sh
```

Within each supported Ubuntu CI lane and its pinned release-tool versions, the
suite builds the CPU-only package under caller umasks `077`, `022`, and `002`
and requires byte-identical `.deb` and size reports. It does not claim identical
compression across different `dpkg-deb` or compression-tool versions. It
rejects unsafe directory modes, invalid ELF inputs, worker tampering, unexpected
files, stale size reports, catalog and contract mutation, symlink arguments,
overwrite attempts, fixture trust in production, and partial publication. The
authority-specific cases also reject missing, changed, linked or duplicated
authority files, positive metadata and self-consistently rehashed tampering.
Compiler-generated fixture executables test packaging, not the actual desktop
binary or its native inference behavior. The
standalone Rust tool tests additionally cover
exact manifest/signature/inventory validation, immutable staging, no-replace
publication, interruption recovery, previous-pack retention, rollback, epoch
floors, ancestor swaps, and hostile filesystem entries.

This stage is suitable for reviewing and publishing the packaging pull request,
but it is not approval to ship Linux GPU packs. Production GPU delivery still
requires reviewed Linux Sherpa/transcribe-cpp artifacts, a separately reviewed
public key and protected signer, clean-machine installation evidence, and the
hardware/performance qualification stage.
