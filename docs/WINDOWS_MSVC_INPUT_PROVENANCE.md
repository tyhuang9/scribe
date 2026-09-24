# Windows MSVC 14.44.35229 build-input provenance

The Windows worker toolchain admits the exact `msvc-14.44.35229-hosted`
four-tool profile alongside the existing 35227 and 35228 profiles. The compiler
family, toolset directory `14.44.35207`, Windows SDK, static runtime and exact
filename/version/hash checks remain unchanged. A mixed or unrecognized tool
set is still rejected before `vcvarsall` or the compiler is run.

This is a build-input compatibility update, not GPU pack trust, release
qualification, an installer release or Auto eligibility. It installs nothing.

## Independently authenticated supplier package

The observed CI hashes were independently compared with the signed parts of
this fixed [Microsoft compiler VSIX](https://download.visualstudio.microsoft.com/download/pr/aa209763-134c-427b-9960-9ca54ccc1734/f4ad3cee4ef18e9f36382399261de27be6910ab3467178d7e687377ec841306d/Microsoft.VC.14.44.17.14.Tools.HostX64.TargetX64.base.vsix).

- Actual archive length: **26,607,271 bytes**.
- SHA-256: `f4ad3cee4ef18e9f36382399261de27be6910ab3467178d7e687377ec841306d`.
- Signed `/manifest.json` identity:
  `Microsoft.VC.14.44.17.14.Tools.HostX64.TargetX64.base`, version
  `14.44.35229`, type `Vsix`, chip `x64`.
- Exactly one OPC package signature, with 58 signed parts out of 59 package
  parts; only the signature part itself is outside that signed inventory.
- Signature time: `2026-08-27T17:25:09Z`.
- Signer: `CN=Microsoft Corporation, OU=OPC, O=Microsoft Corporation,
  L=Redmond, S=Washington, C=US`.
- Signer certificate thumbprint:
  `DD6E7B7522E2CC300982AC84BF0CD0CC59CB7310`.
- Chain root thumbprint:
  `3B1EFD3A66EA28B16697394703A72CA340A05BD5`
  (Microsoft Root Certificate Authority 2010).

Verification on 2026-09-24 independently downloaded the bounded package from
the fixed HTTPS URL with redirects disabled, required the exact length/hash,
then opened the same in-memory bytes using Windows PowerShell 5.1's
`WindowsBase` assembly. `PackageDigitalSignatureManager.VerifySignatures(false)`
returned `Success`. A separate `X509Chain.Build` succeeded with no status
errors, online revocation and Code Signing application policy OID
`1.3.6.1.5.5.7.3.3`. The verifier required the exact signer and root above,
the signed manifest identity, and explicit signature coverage of every tool
below before comparing hashes. No downloaded executable was run or saved.

Do not substitute checking the archive hash alone for that supplier-signature
verification when admitting a new profile. The archive hash pins the reviewed
bytes; it does not establish Microsoft's authorship by itself. Certificate
and revocation status here are a dated observation, not a promise that future
online validation will succeed.

## Tool mapping

All four signed parts are under
`/Contents/VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64/`.
The file versions below were observed by the CI builder on the same
hash-identical executable bytes.

| File | File version | SHA-256 |
| --- | --- | --- |
| `cl.exe` | `19.44.35229.0` | `fe251ef50a1545b1b0835ee17b1e785459712b38d79e45b5c1d3d28970a36619` |
| `link.exe` | `14.44.35229.0` | `a364af801a8539e4324d9489313dbf001d959128451fc99a27b24676bbac058f` |
| `lib.exe` | `14.44.35229.0` | `fde148a275981855689c45f932dda56b10dc9289a0290826a0fb93478da34929` |
| `nmake.exe` | `14.44.35229.0` | `ce6b50c2d8e6704b671ec116e52ce9d2fbc1d20dd28c5b895b5421adddf94ebb` |

The upstream discovery catalog was used only to locate the package URL.
Its advertised catalog hash/size did not match the retrieved catalog, and
its advertised VSIX size (26,662,704) differed from the actual signed archive.
Those metadata checks **did not pass** and are not claimed as provenance.
Admission instead rests on the independently verified Microsoft OPC signature,
trusted certificate chain, signed inner identity and signed executable bytes.

This profile authenticates the existing builder's four-tool boundary, not
every Visual Studio installation file, library, SDK or `vcvarsall` script.
It does not expand that boundary or remove any other toolchain checks.

## Verification and rollback

`pwsh -NoProfile -File scripts/test-windows-msvc-payload-profiles.ps1` exercises
the production matcher without installed compilers or network access. It
covers each approved profile, every tool's filename/version/hash mismatch,
mixed sets from different approved profiles, duplicate/ambiguous profiles,
missing tools and profile-count limits. The existing worker-pack tools suite
invokes it and additionally checks the actual installed toolchain.

The qualification plan must bind the updated toolchain manifest digest;
rebinding that input neither approves the plan nor enables GPU Auto.
Historical prepared packs and measurements remain bound to their original
toolchain digest and must not be relabeled.

Rollback removes only the new profile and updates that plan binding through
review. Existing 35227/35228 installations remain accepted; machines with
35229 then fail closed again. A green hosted run verifies the selected
runner's toolchain, not every Windows hardware/driver lane.
