# Windows x64 CUDA build-input provenance

The `cuda.production_inventory` in
`runtime-manifests/gpu-worker-toolchain-windows-x64.json` authenticates the
complete SDK assembly used to build the CUDA worker, not just the DLLs shipped
in a pack. Its 1,727 files total 1,167,359,432 bytes. Only the worker's reviewed
runtime dependency closure and runtime-component notices belong in the pack;
the compiler, headers and other SDK build inputs are not installer payloads.

This provisions build-input hashes. It does not provision a pack-signing key,
change release policy, trust a fixture pack, qualify hardware, or enable Auto.
No paid Windows Authenticode certificate is needed to record these inputs.

## Supplier archives

The four Windows x64 entries were checked against NVIDIA's
[CUDA 12.8.1 redistribution metadata](https://developer.download.nvidia.com/compute/cuda/redist/redistrib_12.8.1.json).
All archive bytes were downloaded from the listed NVIDIA HTTPS origin, with
redirects disabled, then checked against both the exact size and SHA-256 below.
Each archive was authenticated again on its retained read handle before entry
inspection. No SDK executable was run to generate the inventory.

| Component | Version | Archive bytes | Archive SHA-256 |
| --- | --- | ---: | --- |
| `cuda_nvcc` | `12.8.93` | 120,912,792 | `9fdc70b4271ed9aad4d64cd7076a7d96ec36512d074b9995fe638de669197391` |
| `cuda_cudart` | `12.8.90` | 3,037,735 | `4a39058fd8519444a81cfc7ae055d136f48d1a31ffa41ae255b35b2edd61e13b` |
| `cuda_cccl` | `12.8.90` | 2,911,311 | `bd8548fa1ae82f92910bebc3079e14bd58c5a92aa64596d46bd610a478cb39d7` |
| `libcublas` | `12.8.4.1` | 563,660,944 | `57a470112cec7e112c95253dde8b3c7184d795dbd92b0bde77a4cb7f8c94c8aa` |

Exact archive URLs:

- [CUDA compiler](https://developer.download.nvidia.com/compute/cuda/redist/cuda_nvcc/windows-x86_64/cuda_nvcc-windows-x86_64-12.8.93-archive.zip)
- [CUDA runtime](https://developer.download.nvidia.com/compute/cuda/redist/cuda_cudart/windows-x86_64/cuda_cudart-windows-x86_64-12.8.90-archive.zip)
- [CUDA C++ libraries](https://developer.download.nvidia.com/compute/cuda/redist/cuda_cccl/windows-x86_64/cuda_cccl-windows-x86_64-12.8.90-archive.zip)
- [cuBLAS](https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.8.4.1-archive.zip)

## Reproducible assembly mapping

For each size/hash-authenticated archive:

1. Require its exact `<component>-windows-x86_64-<version>-archive/` root.
2. Strip that root from every entry, preserving physical filename case.
3. Relocate a root `LICENSE` or `LICENSE.txt` to
   `licenses/<component>/<original-license-name>`.
4. Reject absolute/traversal/alternate-stream/reserved-device paths, reparse points,
   nonregular file types, case collisions and file/directory conflicts.
5. Materialize file payloads and their parent directories only. Empty archive
   directories are not part of this SDK assembly. Do not modify file bytes.
6. Require the entire assembled file set to equal the checked-in inventory,
   including hashes; do not accept an ordinary full-toolkit installation with
   extra files merely because `nvcc --version` matches.

The assembly root must be named `v12.8` for the existing toolchain contract.
The compiler payload includes the literal filename `bin/cudafe++.exe`.
Required DLL names may be matched case-insensitively, but the authenticated
inventory retains `bin/cublasLt64_12.dll` with its original case.

Verification on 2026-09-24 UTC independently hashed every authenticated ZIP
file entry, matched all local SDK bytes, rejected extra files/streams/reparse points,
and projected the same ordinally ordered relative-path/hash pairs into the
toolchain manifest. Original assembler SHA-256:
`e5624823bd67fdcb844cde83bd211994630ab3ad85a237ec16d3a5e3f3bb9aaf`.
The metadata-only verification report was 397,488 bytes, SHA-256
`45c293a829b87f20875446ab60fcffe9edb82c78b79c8a5640f4bd739d44300b`.
These identify the local capture, not a remotely signed attestation. Reproduce
the inventory from the pinned supplier archives and mapping above; no private
operator path or report file is required by CI or production.

The temporary archives (690,522,782 bytes total) were removed after verification.
Archive size is not installer compressed size; CI's pack-size report remains
the authority for actual installer payload measurements.

## Runtime DLLs and notices

| SDK-relative DLL | Bytes | SHA-256 |
| --- | ---: | --- |
| `bin/cublas64_12.dll` | 113,716,224 | `9513540e4ec4c51ee9e7304138c2cc255c29a8c181f9e80c38efa25738becd99` |
| `bin/cublasLt64_12.dll` | 674,667,520 | `b199d1ff892a81b7fd3d57ba1781549609b41500b36008fef326038393ad46c7` |
| `bin/cudart64_12.dll` | 573,952 | `c2c9a9c22a9bcba90e261825968836787b331038047a26770cffb7a583c28344` |

Prepared and production dependency copies bind to these inventory hashes, in
addition to the existing AMD64 and normal/delay-import closure checks. A DLL
changed after initial SDK validation must fail at copy time.

The four component archives each contain a 63,021-byte `LICENSE` with SHA-256
`e2c71babfd18a8e69542dd7e9ca018f9caa438094001a58e6bc4d8c999bf0d07`.
CUDA packs preserve the complete runtime-component notices unchanged:

| SDK source | Pack destination |
| --- | --- |
| `licenses/cuda_cudart/LICENSE` | `licenses/cuda-cudart/LICENSE` |
| `licenses/libcublas/LICENSE` | `licenses/cuda-cublas/LICENSE` |

These notice files participate in the authored pack inventory and hashes.
The [archived CUDA 12.8.1 license](https://docs.nvidia.com/cuda/archive/12.8.1/eula/index.html)
lists runtime/BLAS redistributables and filename variants in Attachment A;
Attachment B includes third-party notices for cuBLAS. Preserving those texts
does not by itself establish compliance with every redistribution condition or
replace maintainer review of the complete distribution.

## Verification and remaining gates

Ordinary contract tests must use synthetic SDK trees and fake payloads, not
download NVIDIA archives, execute CUDA, depend on a local SDK, or require keys.
The production empty-inventory rejection remains covered with an explicitly
empty test contract, even though the real manifest is now populated.

The fast synthetic check is
`pwsh -NoProfile -File scripts/test-windows-cuda-pack-inputs.ps1`. The existing
`scripts/test-windows-gpu-worker-pack-tools.ps1` suite invokes it and additionally
verifies the checked-in inventory and notice inclusion in an authored fixture
manifest. Coverage includes canonical DLL case, missing or mismatched inputs,
ADS/reparse rejection, incomplete runtime pin sets, fresh destinations, and a
late notice-directory conflict that must not partially copy the first notice.

Local verification of this change passed the full worker-pack suite,
`scripts/test-windows-vulkan-pack-loader.ps1` (34 checks), and
`scripts/test-windows-release-packaging.ps1`. A separate operator check accepted
all 1,727 real SDK inputs, resolved the three pinned DLLs and two notices,
passed Prepared toolchain-only preflight, and copied both notice files
byte-identically to their fixed pack destinations. No worker build, GPU
inference, production signing, installed-package test or Auto qualification was
performed by these checks. Disposable notice copies were removed afterward.

On a separately provisioned build machine, validate the complete SDK through
`Assert-AuthenticatedCudaSdkInventory` and the builder's
`-Backend Cuda -SigningMode Prepared -ToolchainCheckOnly` preflight. The latter
checks the pinned toolchain and runs compiler version inspection only; it does
not build a worker, create a pack, sign anything or perform GPU inference.

Signed pack assembly, actual installed CUDA/Vulkan worker tests, hardware and
performance qualification, and explicit approval of release policy remain
separate gates. Auto stays default-denied. Reverting this input-provisioning
change returns the manifest to its previous empty-inventory fail-closed state;
it does not require deleting a user's installed SDK.
