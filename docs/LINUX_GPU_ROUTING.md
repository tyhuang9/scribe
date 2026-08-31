# Linux GPU identity and routing contract

This contract applies only inside an admitted `x86_64-unknown-linux-gnu` GPU
worker. The desktop does not link CUDA, Vulkan, or their discovery libraries.
Linux production pack discovery, release trust, activation, packaging, and
`Auto` eligibility remain disabled in this delivery unit.

When a future approved Linux Auto pack is present, desktop admission reads only
bounded kernel facts from `/sys/class/power_supply`. A confirmed online `Mains`
supply is AC; a confirmed discharging battery is battery; absent, unreadable,
oversized, malformed, or otherwise ambiguous facts are `Unknown`. Unknown is
fail-closed for discrete and unclassified GPUs while integrated and unified
GPUs remain eligible for their separate signed qualification checks. No command,
environment override, telemetry, or calibration is used.

Every device that reaches the worker Hello is identified as
`native:pci:dddd:bb:dd.f`, using lowercase fixed-width PCI domain, bus, device,
and function fields. Nonzero PCI domains are preserved. A provider registry
index is accepted only as a current-process selector and is remapped from the
stable PCI function on every worker start; it is never used as durable device
identity.

The worker reads bounded facts only from `/sys/bus/pci/devices`,
`/sys/module`, `/proc/sys/kernel/osrelease`, and, for an optional NVIDIA alias,
`/proc/driver/nvidia/gpus`. It does not run `nvidia-smi`, consult environment
path overrides, or match a device by display name or enumeration order. Two
complete fact samples must agree before routing, so device, binding, and driver
changes fail the Hello instead of producing a stale selection.

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

The suite exercises routing with injectable fake fact sources and runs source
guards on Ubuntu 22.04 and 24.04. A separate locked, source-only type-check crate
compiles both real Linux feature branches, including the pinned `ash` Vulkan
API, without linking or downloading Sherpa or other native payloads. Real
CUDA/Vulkan worker and package smoke evidence remains PR 7D scope because the
reviewed Linux Sherpa archive and signed pack payloads are not present yet.
