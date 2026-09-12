# Windows Vulkan policy loader

This is Scribe's narrowly patched Vulkan loader, not a system installation or an
upstream default. The source manifest pins the official loader and header
archives by immutable revision, size, and SHA-256. The patch and resulting three
source files are also pinned. Build dependencies are acquired separately; the
native build must not download anything.

## Build-host trust boundary

Run the builder only in a trusted, exclusive build job. The checkout, manifests,
scripts, toolchain, source archive directory, and fresh build parent must not
have concurrent untrusted writers. The builder retains read-only-sharing leases
on authenticated input archives and the patch, but the extracted source tree is
not an immutable sandbox and is not leased throughout compilation. The output
digest prevents admitting unexpected binaries; it does not prevent code
execution by someone who can already modify build inputs or the build account.
Runtime worker-pack verification and installation ACLs are separate boundaries.

The shared toolchain preflight rejects ambient compiler and SDK overrides. This
builder also rejects ambient `CMAKE_*`, `CTEST_*`, and native flag overrides
before invoking a fresh configure, including compiler/linker launchers. The
check does not change the caller's environment. Run the fast offline input
checks with `pwsh -NoProfile -File scripts/test-windows-vulkan-policy-loader.ps1`.

## Artifact and runtime policy

The output DLL is pinned as well. Its current digest was reproduced in two
fresh directories with the reviewed MSVC 19.44.35227 toolchain. Other compiler
payload profiles must reproduce that digest or undergo a separately reviewed
repin; an unexpected output must never be admitted merely because it compiled.

The compile-time policy returns empty explicit/implicit layer inventories and
inactive loader settings before any corresponding manifest or registry lookup.
This prevents settings-forced layers and settings-added drivers from bypassing
the policy. Normal Windows ICD discovery remains necessary and enabled. Vendor
driver code still executes in the inference worker; this is not a claim of
complete process isolation from drivers, EDR, or operating-system injection.

The fixed `VK_LOADER_LAYERS_DISABLE=~all~` worker environment value is only
defense-in-depth. It cannot replace this policy: upstream loader settings can
force layers on despite that environment filter.

## Delivery gates

This change is in development. A standalone native build is not a releasable
worker pack. Before delivery, integrate the DLL and licenses into the signed
exact pack inventory, require its pinned identity before Vulkan initialization,
and repeat verified-pack SCIF/model smoke tests with module-inventory evidence.
CPU, CUDA, developer Vulkan builds, production trust, and Auto qualification
must remain unchanged. Test settings injection with an isolated registry shim,
never by changing the developer's actual Vulkan registry configuration.

Updating this dependency requires reviewing upstream security/driver changes,
repinning the archives and policy patch, rerunning hostile-configuration tests,
and requalifying supported GPU lanes. Pack security epochs must prevent replay
of any previously released vulnerable policy. Rollback must retain the existing
exact-app-build and security-epoch checks. Disabling Vulkan packs leaves Auto's
CPU fallback intact; explicit GPU mode still cannot silently use CPU.
