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

`build-linux-release-package.sh` refuses links, hardlinked inputs, an existing
output, a desktop that does not contain the exact CPU-worker SHA-256 anchor, or
any GPU pack that cannot pass the Rust production verifier and immutable
`PackStore`. It writes an exact sorted file inventory, normalizes timestamps
from `SOURCE_DATE_EPOCH`, uses root ownership in the archive, and publishes the
completed `.deb` by one same-directory rename. The adjacent canonical size
report records installed and compressed package bytes. The pack size reporter
first invokes either fixture-only or production Rust verification, labels the
trust mode, and computes reproducible installed and compressed sizes.

`verify-linux-release-package.sh` inspects the data archive before extraction,
rejects unsafe names, duplicates, links and nonregular entries, and then checks
the exact inventory, modes, sizes, hashes, FHS paths, empty pack tree, canonical
catalog, reviewed release contract, and CPU-worker anchor. This makes the
installed CPU worker compatible with the descriptor-bound `openat2` plus
sealed-`memfd` launcher while leaving no mutable path fallback.

Run the native package and attack suite on Ubuntu 22.04 or 24.04:

```sh
./scripts/test-linux-release-packaging.sh
```

The suite builds the CPU-only package twice and requires byte-identical `.deb`
and size reports. It rejects worker tampering, unexpected files, catalog and
contract mutation, symlinks, overwrite attempts, fixture trust in production,
and partial publication. The standalone Rust tool tests additionally cover
exact manifest/signature/inventory validation, immutable staging, no-replace
publication, interruption recovery, previous-pack retention, rollback, epoch
floors, ancestor swaps, and hostile filesystem entries.

This stage is suitable for reviewing and publishing the packaging pull request,
but it is not approval to ship Linux GPU packs. Production GPU delivery still
requires reviewed Linux Sherpa/transcribe-cpp artifacts, a separately reviewed
public key and protected signer, clean-machine installation evidence, and the
hardware/performance qualification stage.
