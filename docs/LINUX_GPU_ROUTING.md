# Linux GPU identity and routing contract

This contract applies only inside an admitted `x86_64-unknown-linux-gnu` GPU
worker. The desktop does not link CUDA, Vulkan, or their discovery libraries.
Linux production pack discovery, release trust, activation, packaging, and
`Auto` eligibility remain disabled in this delivery unit.

The Linux parent has one provider-free invalidation prerequisite. It reads a
bounded inventory of driver-bound display PCI functions from
`/sys/bus/pci/devices`, driver module versions from `/sys/module`, and the
kernel release from `/proc/sys/kernel/osrelease`. It never reads NVIDIA UUID
facts, loads CUDA or Vulkan, initializes a provider, or enumerates provider
devices. Directory iteration is bounded before entries are retained. Two
independently read samples are canonicalized by PCI identity and must agree on
the complete sorted PCI, vendor, driver, and kernel-release witness. Empty,
duplicate, malformed, oversized, added, removed, or changing facts fail closed.
Only a genuinely missing module-version file permits the built-in-driver kernel
fallback; permission or other inspection failures reject the sample.
The kernel release remains part of the witness even when a module supplies its
own version, so an OS/kernel change invalidates the parent device-set witness.
This prerequisite detects invalidation only; it does not enable Linux pack
discovery, execution, packaging, release trust, or `Auto`.

When a future approved Linux Auto pack is present, desktop admission reads only
bounded kernel facts from `/sys/class/power_supply`. A confirmed online `Mains`
supply is AC; a confirmed discharging **system** battery is battery. Peripheral
`Battery` supplies marked `scope=Device` are ignored. Simultaneous online mains
and a discharging system battery, or absent, unreadable, oversized, malformed,
or otherwise ambiguous facts are `Unknown`. Unknown is fail-closed for discrete
and unclassified GPUs while integrated and unified GPUs remain eligible for
their separate signed qualification checks. No command,
environment override, telemetry, or calibration is used.

Every device that reaches the worker Hello is identified as
`native:pci:dddd:bb:dd.f`, using lowercase fixed-width PCI domain, bus, device,
and function fields. Nonzero PCI domains are preserved. A provider registry
index is accepted only as a current-process selector and is remapped from the
stable PCI function on every worker start; it is never used as durable device
identity.

The worker starts from the same bounded kernel PCI sample, then privately
enriches it with provider-specific proof. For an optional NVIDIA alias it reads
`/proc/driver/nvidia/gpus`; CUDA and Vulkan proof is obtained only inside the
corresponding admitted worker. It does not run `nvidia-smi`, consult environment
path overrides, or match a device by display name or enumeration order. Two
complete enriched fact samples must agree before routing, so device, binding,
driver, UUID, or provider changes fail the Hello instead of producing a stale
selection.

CUDA devices may present either canonical PCI identity or one bounded physical
`GPU-...` UUID. A UUID is reconciled privately against the NVIDIA proc fact for
one validated sysfs PCI function. It is not returned in Hello, persisted, or
included in diagnostics. The worker also loads the fixed CUDA driver SONAME and
requires the current CUDA device UUID for that PCI function to equal the
physical proc UUID. This rejects a single MIG slice even when the provider
presents only its shared physical BDF. MIG identities, missing physical proof,
and multiple logical devices resolving to one PCI function are rejected.

Vulkan devices must present canonical PCI identity. The Vulkan worker also
requires `VK_EXT_pci_bus_info` for the exact provider-claimed physical device
and binds a bounded Vulkan driver UUID/version witness to the sysfs driver
witness. A driver-bound display controller that is absent from the chosen
Vulkan ICD remains a kernel fact but does not invalidate an unrelated exact
provider mapping. Missing, ambiguous, conflicting, duplicate, or changing
claimed mappings fail closed. There is no name-only, UUID-only, or ordinal
fallback.

Run the dependency-light Linux suite with:

```sh
./scripts/test-linux-worker-launch.sh
```

The suite exercises parent witness sampling and worker routing with injectable
fake fact sources and runs source guards on Ubuntu 22.04 and 24.04. A separate
locked, source-only type-check crate compiles both real Linux feature branches,
including the pinned `ash` Vulkan API, without linking or downloading Sherpa or
other native payloads. Real CUDA/Vulkan worker and package smoke evidence
remains separate qualification work. The reviewed Linux Sherpa pin is present,
and the native CPU contract now prepares it for actual release linking,
authenticated startup and CPU-only package verification. That CPU evidence
does not supply signed Linux GPU packs or GPU hardware/performance results.
