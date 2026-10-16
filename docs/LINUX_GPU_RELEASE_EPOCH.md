# Linux GPU release-epoch reader contract

This is a read-only prerequisite for future Linux GGUF CUDA/Vulkan pack
admission on `x86_64-unknown-linux-gnu`. It does not activate Linux GPU packs,
authenticate a pack, modify the package inventory, or enable GPU Auto. The
existing CPU path does not depend on this authority.

## Fixed authority and canonical record

The future privileged installation contract must commit the authority at:

```text
/var/lib/scribe/gpu-release-security-epoch-v1
```

The exact ASCII record is the prefix
`scribe-linux-gpu-release-security-epoch-v1:x86_64-unknown-linux-gnu:`,
followed by exactly twenty decimal digits and one LF. There is no JSON,
whitespace tolerance, alternate target, environment override, or caller-supplied
production path. Both the committed floor and the authenticated candidate must
be between 1 and 9,007,199,254,740,991, inclusive.

The candidate must equal the already committed floor. A lower candidate is a
downgrade; a higher candidate is an uncommitted advance. Neither is admitted.
The reader never initializes, repairs, appends, advances, or lowers the floor.
An absent authority is not an epoch-zero bootstrap opportunity.

## Descriptor-bound admission snapshot

`LinuxReleaseEpochLease::open_production(authenticated_candidate_epoch)` opens
from `/` through fixed components using the existing Linux `openat2`
beneath/no-symlink/no-magiclink pattern. Every ancestor must be root:root and
not group- or other-writable. The floor is opened read-only, nonblocking,
close-on-exec and no-follow; it must be a single-link regular file with safe
ownership, permissions and an exact bounded record length before it is read.
In particular, a FIFO must not block admission before type rejection.

The lease retains descriptors and snapshots. `recheck()` validates the retained
objects, rebinds the fixed path, and compares identities, relevant metadata,
exact bounded contents and candidate equality. A same-content replacement is
still a changed authority. Missing, inaccessible, malformed, oversized,
linked, special, writable or changed authority fails closed with categorized,
path-free errors. It does not quarantine a GPU or edit application health data.
Bounded positional reads keep concurrent rechecks from sharing a file cursor.

The future caller must first authenticate the candidate's release/pack epoch
and perform the required recheck before launch. Possessing this lease alone
grants no pack, signature, ABI, executable, driver or device authority. This
unit deliberately adds no runtime call-site wiring.

## Limits and remaining activation prerequisites

This is an inspected admission snapshot, not a linearizable updater lock or a
hard kernel-I/O deadline. Privileged monotonic advancement, writer/reader
coordination, initial provisioning, retention across uninstall/reinstallation,
and a Linux-specific authenticated release-authority schema remain separate
prerequisites. An ordinary user-owned cache cannot substitute for them. A
privileged administrator and a compromised kernel remain outside the reader's
protection boundary.

Only after those prerequisites, pack/launcher integration and native
qualification exist may production discovery consume this API. No protected
runner, service, root state or OS configuration is provisioned by this change.
Future ONNX execution providers still require their separately reviewed
provider namespace and qualification series.

## Verification

The existing standalone command remains the Linux verification entry point:

```sh
./scripts/test-linux-worker-launch.sh
```

It includes portable parsing/policy/metadata tests and Linux descriptor
fixtures on the supported Ubuntu 22.04 and 24.04 lanes. Fixture paths and
ownership seams are test-only; unprivileged fixture success does not prove
production root ownership. The new suites require their complete expected test
inventory, and the reader suite has a finite timeout to contain a blocking
special-file regression. Windows can execute the portable core and source
guards, but cannot establish Linux syscall behavior. Actual Linux results must
be reported only after the native lane runs; queued jobs are not passing
evidence. This suite is not GPU hardware or performance qualification.
