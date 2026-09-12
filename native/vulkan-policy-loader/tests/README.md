# Scribe Vulkan pack policy tests

These tests cover the Windows x64 loader built from Scribe's authenticated,
pinned Vulkan-Loader source plus `no-layers-or-settings.patch`. They are an
overlay on the upstream loader test framework; they do not write the real
Windows registry or install manifests.

## Scope and expected assertions

`scribe_vulkan_pack_policy_tests` uses the upstream `FrameworkEnvironment` and
Detours shim. The shim redirects loader file and registry reads to temporary,
per-test state. When executed, the suite is designed to assert:

- explicit and implicit manifests in mocked registry, replacement-path, and
  add-path discovery sources enumerate no layers, even with loader enable and
  allow filters;
- a requested explicit layer returns `VK_ERROR_LAYER_NOT_PRESENT`;
- force-on settings injected through both mocked HKCU and HKLM settings keys
  cannot enumerate or activate a layer, including while
  `VK_LOADER_LAYERS_DISABLE=~all~` is set;
- settings cannot add or exclusively select a hidden driver; and
- instance creation and physical-device enumeration still reach the isolated
  mock ICD.

The settings tests assert that the temporary JSON and mocked registry entry
exist before invoking Vulkan. They are not substitutes made only from
environment variables. As a negative control, the same suite should fail
against pristine Vulkan-Loader 1.4.357: its force-on settings tests expose the
layer and its exclusive-driver test selects the settings driver.

`scribe_vulkan_live_probe` does not import Vulkan. It accepts only an absolute
DLL path, loads that exact file with `LoadLibraryExW`, resolves Vulkan through
that module, and is designed to assert:

- loader version is at least 1.3.234;
- instance layer enumeration returns zero;
- a missing sentinel and every supplied installed layer name are rejected;
- a no-layer instance enumerates at least one real physical device; and
- exactly that loader path, no second `vulkan-1.dll`, and none of the forbidden
  module basenames are mapped after enumeration.

The default forbidden basenames are the hooks observed in the affected worker:
`we-graphics-hook64.dll`, `ow-graphics-vulkan.dll`, and `owclient.dll`.
Additional names can be supplied with repeated `--forbid-module` arguments.

## Upstream test overlay

Use only the authenticated pinned revisions:

- Vulkan-Loader source `5f157b62e333c63260d05d81bf66faa216ab0fb8`;
- Vulkan-Headers source `e3b1eec08173d6b825cd3ac88c885a63b621504a`
  (upstream `v1.4.357`);
- GoogleTest `f8d7d77c06936315286eb55f8de22cd23c188571`
  (upstream `v1.14.0`); and
- Microsoft Detours `4b8c659f549b0ab21cf649377c7a84eb708f5e68`.

`dependency-manifest.json` pins the official codeload URLs, archive byte sizes,
SHA-256 digests, and extraction roots for these two test-only dependencies.
Validate both size and digest before extraction; never let the native build
download or update them.

Register this directory after the upstream `tests/framework` targets exist.
For example, append this build-only block to the authenticated loader source's
`tests/CMakeLists.txt` after its existing test target definitions:

```cmake
set(SCRIBE_SOURCE_DIR "" CACHE PATH "Absolute path to the authenticated Scribe checkout")
if(NOT IS_ABSOLUTE "${SCRIBE_SOURCE_DIR}")
    message(FATAL_ERROR "SCRIBE_SOURCE_DIR must be absolute")
endif()
add_subdirectory(
    "${SCRIBE_SOURCE_DIR}/native/vulkan-policy-loader/tests"
    "${CMAKE_CURRENT_BINARY_DIR}/scribe-vulkan-policy-tests")
```

That source-tree registration is build orchestration, not part of the durable
upstream source patch. Do not add the test directory before
`add_subdirectory(framework)`.

With the pinned dependencies already acquired and Vulkan-Headers configured as
an install tree, configure and build from an x64 Visual Studio developer shell:

```powershell
cmake -S <patched-loader-source> -B <build-dir> -G "NMake Makefiles" `
  -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTS=ON -DUPDATE_DEPS=OFF -DLOADER_CODEGEN=OFF `
  -DCMAKE_C_FLAGS=/guard:ehcont -DCMAKE_CXX_FLAGS=/guard:ehcont `
  -DSCRIBE_SOURCE_DIR=<absolute-Scribe-checkout> `
  -DVULKAN_HEADERS_INSTALL_DIR=<Vulkan-Headers-install> `
  -DGOOGLETEST_INSTALL_DIR=<directory-containing-googletest> `
  -DDETOURS_INSTALL_DIR=<detours-source>
cmake --build <build-dir> --config Release --target `
  scribe_vulkan_pack_policy_tests scribe_vulkan_live_probe
ctest --test-dir <build-dir> -C Release `
  -R "^scribe[.]vulkan[.]pack[.]policy[.]" --output-on-failure --no-tests=error
```

The upstream framework is not parallel-safe. The CTest registrations are
marked `RUN_SERIAL`. The test build requires `/guard:ehcont` on its framework
objects as well as its link; do not work around `LNK2047`/`LNK1386` by disabling
the mitigation. The two Scribe targets explicitly retain `/EHsc`.

## Live host qualification

Run the live probe from the same fixed worker environment used by the pack.
Pass the newly built policy DLL, not the system loader. Also pass every known
installed layer name whose explicit-request behavior matters:

```powershell
& <build-dir>/tests/scribe-vulkan-policy-tests/scribe_vulkan_live_probe.exe `
  --loader <absolute-path-to-policy-vulkan-1.dll> `
  --explicit-layer <installed-layer-name> `
  --forbid-module we-graphics-hook64.dll `
  --forbid-module ow-graphics-vulkan.dll `
  --forbid-module owclient.dll
```

Record the executable exit code and the complete output. A passing run ends in
`SCRIBE_VULKAN_LIVE_PROBE=PASS`. Independently verify the policy DLL's expected
SHA-256 and pack-relative path before running; this probe establishes the
mapped path but does not authenticate the file.

## Recorded development verification (2026-09-12)

With the revisions above, CMake 4.4.2, MSVC 19.44.35227, Windows SDK
10.0.26100.0, and the NMake Release recipe:

- All five isolated policy tests passed. Reversing only the three-file policy
  patch in a separate fresh source/build tree made all five tests fail, with
  binary exit code 1. This negative control included both registry hives and
  the settings-exclusive driver case.
- Three fresh native builds produced the pinned 771,072-byte DLL with SHA-256
  `4e8aa4fcdb4299183809fefe89efe12fd68c3a337a604a565ed91fe8da95a14d`.
  Its only direct imports were `advapi32.dll`, `cfgmgr32.dll`, and
  `kernel32.dll`; there were no delay imports.
- The exact-DLL live probe passed on a host with NVIDIA RTX 4080 SUPER and AMD
  Radeon integrated graphics. Both devices remained enumerable, layers were
  empty, and the explicit sentinel/validation layer requests were rejected.
  Neither observed OBS/Overwolf hook nor another Vulkan loader was mapped.
  The run cleared inherited environment, restored the worker's seven Windows
  system/user-path allowlist values, and deliberately set
  `VK_LOADER_LAYERS_ENABLE=*` and `VK_LOADER_LAYERS_ALLOW=*` without a disable
  filter. No real registry setting was changed.

These are development observations, not final signed-pack qualification. Vendor
ICD side components still loaded; the probe did not prove whole-process
isolation. Repeat the live check for the final probe and worker artifact rather
than transferring this result to a later binary by assumption.

## Limits and required qualification

- The isolated suite proves loader behavior against mocked manifests,
  settings, registry keys, layers, and ICDs. It does not exercise vendor
  display drivers.
- The live probe proves only the machine and process observed in that run. Its
  module checks are basename-specific and do not establish a general process
  sandbox or prevent injection outside Vulkan's layer mechanism.
- The upstream test framework loads its mock layer DLLs itself so it cannot use
  process module presence as an activation signal. It instead asserts the mock
  layers never receive an instance handle. Live mapped-module evidence comes
  from the separate probe.
- Qualify the final signed pack on supported NVIDIA and AMD hosts, including a
  machine with the previously observed hooks/settings installed. Repeat the
  existing Scribe Vulkan SCIF/model/warm tests and verify the worker's loaded
  module set.
- These executables do not exercise Scribe's CPU/Auto selection, CUDA, Metal,
  worker protocol, model ABI, signing, or packaging. Those paths remain the
  pack/runtime integration test scope.
