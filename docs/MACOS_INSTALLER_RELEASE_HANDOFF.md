# Mac installer authentication and publication

## Status and scope

Proposed source contract: one freshly owned app, CPU worker and Metal pack may
become a shipping installer only after package authentication, final content
readback and notarization succeed under the same retained release baseline.
This contract does not implement or activate that path.

The current [product owner](../build_support/macos_product_pkg.py) constructs
three components and an **unsigned** product under one full admission scope.
Its [private codec](../build_support/macos_product_pkg_codec.py) rejects signature
metadata. The fixed inert native test addresses unsigned codec compatibility,
not signed full-app admission or permission to install. The current
[builder](../scripts/build-macos-release.sh) still produces a ZIP, and the
[notarization script](../scripts/sign-notarize-macos-release.sh) operates on the
app. Those scripts, publisher policies, default-deny settings and Auto
eligibility remain unchanged.

## Separate sources of authority

| Subject | Required authority | Not a substitute |
| --- | --- | --- |
| App and native installer entries | Existing role-specific Developer ID Application requirements, expected publisher and complete code verification | A matching filename, signing selector or package signature alone |
| GPU worker packs | Existing signed pack/catalog authority, inventories, digests and release bindings | Application signing or a caller-supplied verification receipt |
| Installer package | Qualified native package-signature verification, expected Installer publisher and independently reviewed certificate identity | Application code requirements, a package digest, display name or generic Gatekeeper acceptance |

Apple distinguishes Application and Installer certificates; the latter is
required for package signing. See [signing a Mac installer package](https://help.apple.com/xcode/mac/current/en.lproj/deve51ce7c3d.html).
A public signing selector chooses a key but grants no authority. Expected
publisher and certificate policy must be reviewed independently of the package
being checked, agree with the application publisher, and remain unchanged
throughout the operation. Missing policy must deny before caller inputs, key
access or native tools. No environment or command-line override may supply it.

## One owned release operation

The future protected release path must perform the following work in one
unprivileged owner, retaining the original full app/pack/policy baseline:

1. Admit the complete release inputs and retain their identities, copies and
   verifier scope. Authenticate the app and packs using their existing domains.
2. Construct fresh CPU, Metal and app components in the owner's private scope.
   Keep exact inventories, bounds, nodes and byte identities across every tool.
   Authenticate native entry code before it becomes package content.
3. Apply the qualified component authentication rule, then construct the outer
   product from only those retained components and source-generated controls.
   The component rule remains an unresolved gate below, not an assumed pass.
4. Check the complete unsigned product against the original components and
   Distribution. Sign only this owner's fresh candidate with the separately
   selected Installer identity, keeping the unsigned original private.
5. Authenticate the signed candidate and independently bind its complete content
   to the original expected members and controls. A successful signing process
   or signature check cannot replace content readback.
6. In a separately authorized phase of this same retained operation, notarize
   the authenticated shipping container and staple its ticket. A protected
   shipping product requires accepted notarization and a validated staple; an absent ticket or
   deferred notarization is not a release-ready mode.
7. After the last authorized mutation, repeat package authentication, required
   ticket/policy checks and content binding. Measure the final bytes while
   preserving the original expected content baseline. Recheck all retained
   inputs, policies and owned artifacts across tools and scope exits.
8. Publish without replacing an existing final only after all required checks
   and admission cleanup succeed. Return the identity of the final bytes, not
   the identity of a pre-signing or pre-stapling intermediate.

This requires extending ownership through authentication and publication.
Reopening today's returned unsigned package in a separate adapter would not
retain that scope. No prebuilt-package input, transferable receipt, signing
callback or independently refreshed baseline may stand in for it.

Apple's [packaging guidance](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution)
describes signing supported containers inside out, notarizing the distributed
outer container and stapling that container. These operations do not establish
Installer execution order or a transaction across the app and worker stores.

## Candidate native package verifier

The candidate for qualification combines absolute system
`/usr/sbin/pkgutil --check-signature` with an independently reviewed SHA-256
fingerprint of the **signing certificate**, the expected Installer role and
publisher, and trusted timestamp verification. The certificate fingerprint is
not the package digest and must never be learned as an approval from the
candidate package itself. Use a separate `/usr/sbin/spctl --assess --type install`
assessment as an additional machine-policy check, not as sole publisher proof.
Apple documents these tools in [notarization troubleshooting](https://developer.apple.com/documentation/security/resolving-common-notarization-issues)
and [code-signing tasks](https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/Procedures/Procedures.html).

This is a candidate, not an implemented or qualified parser. Before accepting
any result, native qualification must establish which authenticated signer a
reported fingerprint identifies, package-byte binding, certificate class and
trust, timestamp meaning, and a bounded unambiguous output grammar under the
supported toolchain/locale. Unknown, incomplete, overridden or ambiguous results
must deny. Do not infer certificate identity from a company name or the
human-readable, potentially absent originator described by
[SecAssessment](https://github.com/apple-oss-distributions/Security/blob/main/OSX/libsecurity_codesigning/lib/SecAssessment.h).

Keep tool paths, environment, time/output bounds, exact candidate identity and
process cleanup under the owner. Qualification must define bounded network,
cache and prompt behavior; unattended checks must not prompt or modify user
trust settings. Native verification does not authorize key
discovery, remote submission or certificate/policy acquisition. Revocation,
certificate validity and pin rotation need explicit qualification; a trusted
timestamp alone must not be treated as permission to ignore Installer
certificate expiry. Rotation requires independently reviewed public policy,
not automatic acceptance of a newly observed signer.
Report categorized failures without copying raw verifier output, local paths
or credential diagnostics into public CI logs or PR summaries.

## Unresolved implementation gates

- **Component representation and authentication.** Establish how signed
  component packages are represented after `productbuild` flattens them. Do not
  assume their signatures survive, or that an outer signature substitutes for
  component authentication. Private components are not standalone release
  artifacts; any future independent distribution needs its own contract.
- **Signed and stapled content readback.** Establish the exact own-output layout,
  signature/ticket metadata bounds and final member equivalence. Keep the
  unsigned codec's rejection intact until a separately qualified path exists;
  ignoring signature elements or inventing a signed-XAR profile is not a fix.
- **Public publisher policy.** Review the actual publisher and Installer
  certificate identity, validity/rotation requirements and their binding to the
  Application publisher. No public identity is provisioned by this contract;
  private keys or passwords do not belong in source, tests, logs or PRs.
- **Authorized native qualification.** Use the owner's actual signed components
  and full product to qualify the verifier, notarization, stapling and final
  readback. Portable fixtures or a signed third-party archive cannot qualify
  this release path. Credential use and remote submission require their own
  authorization; a source PR is not that authorization.

The next implementation must resolve these gates rather than add another
policy-only adapter or expand arbitrary-package admission. Shipping-script
integration follows a complete authenticated product path, not the reverse.

## Failure behavior and required verification

Any failed admission, signature/content/policy/ticket check, tool error or timeout
must stop publication. Do not retry with weaker checks. Recheck replacements
before and after every tool and final scope exit. Uncertain process cleanup
retains the owned scope; a collision retains the existing final. An error after
publication must never delete or overwrite that final. Preserve the existing
non-hostile same-user/root writer and reviewed-system-tool assumptions; this
contract does not defend against compromised root or grant Installer/root
authority to a user-writable extraction directory.

The implementation verification plan must cover:

- Default denial before input/tool/key access; wrong publisher, certificate pin
  or role; invalid/expired/revoked certificates; missing or untrusted timestamp;
  unsigned, malformed, localized, truncated and ambiguous verifier output.
- App, pack, component, controls, policy or candidate replacement across every
  signing/readback/stapling boundary, including changes that retain a valid
  signature but disagree with the original expected content.
- Missing, rejected or mismatched notarization/tickets; final byte measurement
  after stapling; cancellation, hangs and uncertain child cleanup.
- Failure before publication, collision, publication ambiguity and cleanup
  failure after publication, with no falsely successful result or lost final.
- Exact native signed/stapled full-product readback on supported Intel and ARM
  toolchains, separate from portable tests and inert unsigned compatibility.

Actual installation, app replacement, interruption recovery, cross-component
rollback, update/removal and clean-machine acceptance remain separate lifecycle
work. Neither authentication nor choice-outline order proves those properties.
Revert the future inactive implementation to disable it; this documentation
change itself changes no runtime, trust policy, installer or release behavior.
