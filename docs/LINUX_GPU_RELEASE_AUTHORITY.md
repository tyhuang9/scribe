# Linux GPU release-authority boundary

This prerequisite gives Linux GGUF worker packs their own embedded release
metadata contract. It does not enable production Linux GPU discovery, launch,
nonempty packages or Auto. The shared project pack-signature verifier and its
keys are unchanged; this document is not a new signing authority.

## Checked-in default and build policy

The reviewed default is
`runtime-manifests/gpu-pack-release-authority-linux-empty.json`. Its canonical
JSON fields, in order, are:

```json
{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[]}
```

The catalog digest binds the exact bytes `{"schema_version":1,"packs":[]}`.
Epoch zero is the empty, GPU-ineligible default, not a rollback-floor bootstrap
value. The document has no Keychain namespace or configurable floor path.

Linux builds select this Linux default. Windows and macOS retain their existing
default selection and override behavior. Unsupported Linux architectures and
ABIs remain CPU-buildable; selecting this document does not make them GPU
eligible.

The existing `SCRIBE_GPU_PACK_RELEASE_AUTHORITY` input and generated output
filename are preserved. Existing transport bounds, UTF-8 checks and at-most-one
trailing LF normalization remain in place. Linux embedding additionally requires
the independently fixed reviewed zero-epoch, empty authority. Positive epochs,
nonempty entries and other overrides fail before authority output publication.
Changing the checked-in default alone cannot bypass this policy.

## CPU-only production consumer and package inventory

Supported Linux x86_64 GNU desktop discovery now validates the embedded bytes
against the canonical empty catalog using the typed Linux validator. Only
epoch-zero, empty metadata produces the production-support-disabled diagnostic;
malformed, mismatched, positive-empty and nonempty metadata are rejected. Both
outcomes return zero leases and no catalog generation. Other Linux targets
remain unsupported. Windows and macOS behavior is unchanged.

The consumer is data-only: it performs no filesystem, floor, health, signature,
provider or process operations. Metadata rejection affects GPU diagnostics,
not the bundled CPU startup path. Structurally valid positive fixture metadata
is still not production admission.

The CPU-only Debian package carries the normalized reviewed empty authority at
`/usr/lib/scribe/gpu-pack-release-authority.json`. Assembly and independent
verification require fixed reviewed bytes, the exact empty catalog, and the
six-file inventory. An unsigned, self-consistent inventory cannot authorize
new metadata. See `LINUX_RELEASE_PACKAGING.md` for the package checks and the
explicit limitation that arbitrary supplied ELF provenance is not verified.

## Typed document and catalog binding

The private Linux validator parses a Linux-specific schema and binds it to the
exact catalog. Unknown or duplicate fields, noncanonical encoding, excessive
input or entry counts, invalid numeric bounds and mismatched digests reject.
Entry correspondence is ordered and exact, rather than a subset match.

Positive fixture documents exercise the future nonempty contract. They bind app
version and revision, protocol and runtime ABI, Linux target, CUDA/Vulkan backend
and provider, pack identity/version/digest/epoch, immutable relative root,
worker path, sizes and complete ordered inventory. Epochs must agree across the
document and entries. Invalid paths and duplicate inventory identities reject.

Those positive fixtures establish a parsing and metadata-binding contract only.
They do not authenticate a pack, verify signatures or file contents, admit a
driver/device, commit a security floor, or make positive production overrides
acceptable. A validated metadata result must not substitute for any of those
separate authorities.

A positive epoch with an exactly empty catalog may be structurally valid future
metadata, but authorizes zero packs. Parsing it performs no revocation or floor
advance and is not approval to publish a revocation release. Production embedding
still rejects it, just as it rejects every other positive epoch.

## Existing safety boundaries and remaining work

The read-only floor lease in `LINUX_GPU_RELEASE_EPOCH.md` is unchanged and is
not called by this validator. The production reader accepts only positive
committed epochs; an empty release document is not a reason to create or access
root state. No privileged state is provisioned or mutated by this change.

Privileged monotonic advancement, reader/writer coordination, provisioning,
uninstall/reinstall retention, authenticated positive release integration,
pack/launcher wiring and native qualification remain required before Linux GPU
activation. The existing nonempty package rejection and Auto default-deny gates
remain independent safeguards. CPU startup remains independent of GPU authority.

## Verification and evidence limits

The existing Linux verification entry point remains:

```sh
./scripts/test-linux-worker-launch.sh
```

It runs the portable parser/build-policy tests through the existing worker-pack
authoring test host, with explicit test discovery, alongside architecture guards
and the existing descriptor/routing suites. Tests cover Linux default selection,
unsupported targets, unchanged Windows/macOS policy, default-to-emitted-byte
validation, positive fixtures and malformed/tampered inputs.

Before verification, provision the pinned Rust toolchain and fetch the authoring
host's locked dependencies with
`cargo fetch --locked --manifest-path tools/worker-pack-author/Cargo.toml`.
Dependency setup is separate from the verification command; its Cargo test hook
runs offline. The supported CI lanes perform that setup explicitly.

Windows portable results do not prove Linux runtime behavior or native artifacts.
Supported Ubuntu 22.04/24.04 CI must run this revision before claiming its native
lane passed. Neither source checks nor fixture metadata establish GPU hardware,
speech parity, performance, production trust or Auto qualification. No live
privileged installation or GPU inference is part of this unit's verification.
