# Linux release packaging contract

Stage 7D assembles a deterministic Ubuntu x86_64 Debian package from already
built desktop and CPU-worker executables. It does not build or download native
inference dependencies. The package installs the desktop at
`/usr/bin/local-transcriber`, the CPU worker at
`/usr/lib/scribe/scribe-inference-worker`, and reserves immutable GPU packs
under `/usr/lib/scribe/workers/packs/<id>/<version>/<digest>/`.

The checked-in release contract and package catalog are canonical JSON. The
catalog is exactly `{"schema_version":1,"packs":[]}`. Production Linux trust
contains no public key, so every nonempty CUDA or Vulkan pack input is verified
with `ProductionTrustRoot` and rejected before publication. Fixture signing is
available only to tests and size-report evidence; production assembly and
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

`verify-linux-release-package.sh` inspects the data archive before extraction,
rejects unsafe names, duplicates, links, nonregular entries, non-root ownership,
and unsafe archive modes without normalizing them during extraction. It then
checks ELF identity, the exact inventory, deterministic regular-file byte sum,
modes, sizes, hashes, FHS paths, empty pack tree, canonical catalog, reviewed
release contract, and CPU-worker anchor. This makes the
installed CPU worker compatible with the descriptor-bound `openat2` plus
sealed-`memfd` launcher while leaving no mutable path fallback.

Run the native package and attack suite on Ubuntu 22.04 or 24.04:

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
standalone Rust tool tests additionally cover
exact manifest/signature/inventory validation, immutable staging, no-replace
publication, interruption recovery, previous-pack retention, rollback, epoch
floors, ancestor swaps, and hostile filesystem entries.

This stage is suitable for reviewing and publishing the packaging pull request,
but it is not approval to ship Linux GPU packs. Production GPU delivery still
requires reviewed Linux Sherpa/transcribe-cpp artifacts, a separately reviewed
public key and protected signer, clean-machine installation evidence, and the
hardware/performance qualification stage.
