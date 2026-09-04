# Windows GPU promotion broker contract

This independently locked workspace defines the unprivileged intent accepted
by a future separately privileged Windows service or remote HSM broker. The
canonical `PromotionIntent` contains the fixed
`scribe-windows-gpu-production-v1` policy namespace and all provenance,
artifact, digest, version, security-epoch, and replay-policy fields. It contains
no intake path, publication path, endpoint, key, or state location. Its identity
is SHA-256 over the exact canonical JSON prefixed by
`scribe-windows-gpu-promotion-intent-v1\0`.

The non-serializable `ClientInvocation` keeps the existing CLI compatible by
pairing that intent with process-local `PathBuf` values. Those paths do not
change the intent bytes or digest, do not enter receipts or the ledger, and
cannot choose protected publication names. The test broker derives the staging
and final names as `.staging-<release-set-digest>` and
`<release-set-digest>` respectively. Its `--output-root` value is only the
test-local publication parent and is not authority input.

On Windows, the normal `scribe-windows-gpu-promotion-client` validates the
invocation in memory and contacts only the fixed
`\\.\pipe\ScribeGpuPromotionBroker.v1` endpoint. It authenticates the server as
the session-zero, restricted LocalService process carrying the exact
`ScribeGpuPromotionBroker` service SID before writing any request bytes. The
client opens the pipe with identification-only security quality of service.
After the bounded read, the service impersonates only long enough to compare
the client's exact `TokenUser` SID with its startup policy snapshot. Group
membership and elevation do not authorize a client. Checked reversion completes
before decoding or replying. There is no caller-configurable endpoint or
service identity.

The corresponding `scribe-windows-gpu-promotion-service` binary runs only
through the Windows Service Control Manager. It refuses to create the pipe
unless its own token is the expected restricted LocalService token and a fixed
64-bit `HKLM\SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization` policy
fully verifies. The policy contains exactly DWORD `SchemaVersion=1` and a
canonical `AuthorizedClientSid`, no subkeys or extra values, SYSTEM ownership,
and a protected noninheriting DACL with exactly SYSTEM/Administrators full
control plus service-SID read. The service snapshots the SID for its lifetime;
registry mutation requires restart and never broadens a running instance. A
valid orphan SID can lock every client out.

The local-only, first-instance-only, message-mode, bounded pipe DACL contains
exactly service-SID generic-all and the configured client SID with mask
`0x00100183`. The client never receives generic write,
`FILE_CREATE_PIPE_INSTANCE`, `WRITE_DAC`, or `WRITE_OWNER`. An authorized
canonical path-free request can receive only a correlated typed
`NotProvisioned` response. After validating it, the client sends a bounded
request-and-response-correlated acknowledgement. Neither process opens the
handoff or output paths.

An elevated administrator can create the absent policy once with:

```powershell
$nonceBytes = [byte[]]::new(32)
[Security.Cryptography.RandomNumberGenerator]::Fill($nonceBytes)
$nonce = [Convert]::ToHexString($nonceBytes).ToLowerInvariant()
pwsh -NoProfile -File .\scripts\provision-windows-gpu-broker-client-policy.ps1 -AuthorizedClientSid 'S-1-5-21-...-1000' -InvocationNonce $nonce
```

The provisioner accepts a SID, never an account name, and conservatively
accepts only canonical `S-1-5-21` account SIDs with RID 1000 or greater. This
rejects broad/built-in identities, SYSTEM, LocalService, NetworkService, all
service SID forms, and the broker service SID. It creates rather than updates,
supplies the final SYSTEM-owned protected descriptor atomically with key
creation, verifies that protection before its first value write, retains an
incomplete marker until values verify, and refuses any pre-existing policy. It
also opens the fixed 64-bit ancestor chain without following registry links,
refuses ancestors writable by untrusted principals, and atomically protects any
missing Scribe-specific ancestor before creating the leaf. It enumerates the
complete raw DACL rather than projected `RegistryAccessRule` entries. A
non-qualified or non-Allow/Deny ACE fails closed; denies are non-granting and
inspected raw Allow ACEs without mutation bits remain acceptable. Every mutating
raw Allow requires an exact trusted SID except for at most one standard
explicit, non-callback `CommonAce` on exact case-sensitive `SOFTWARE`: AceType
and qualifier AccessAllowed, SID `S-1-3-0`, mask `0x000f003f`, AceFlags exactly
ContainerInherit, and no opaque bytes. It does not rewrite the root ACL.
Descendants, path/case variants, inherited, callback, object, or duplicate
template ACEs, actual account SIDs, and altered mask or flags still fail the
normal untrusted-mutation check. On success it writes
one JSON record bound to the supplied correlation nonce and lists only ancestors
for which that invocation received `REG_CREATED_NEW_KEY`.
The native helper captures the caller token's exact prior `SeRestorePrivilege`
state and restores it before removing the incomplete commit marker. Restore or
handle-close failure retains the token and captured state for the outer
`finally` boundary to retry. A persistent retry failure terminates through
`Environment.FailFast` rather than returning to a long-lived host with uncertain
privilege state; a successful retry preserves the original provisioning error.
Success JSON is emitted only after restoration and token closure succeed.
The nonce is not a credential. Automation must verify the record's version,
nonce, SID, fixed policy path, and ordered ancestor inventory before claiming
test cleanup ownership.

The hostile-input copier, fixture Ed25519 authority, chained replay/epoch
ledger, signed receipt, recovery state machine, authorizer, and atomic publisher
are under `cfg(test)` only. They prove the intended broker contract but are not
deployable production authority. In particular, the tests do not establish:

- production service installation, immutable binary/ancestor ACLs, and update
  policy;
- durable production service/client installation and update lifecycle;
- no-follow opening and retained ancestor authority for the workflow client
  executable (the workflow retains only a standard .NET leaf stream);
- DLL/loader policy for a future nontrivial broker client;
- NT handle-relative traversal for every input component;
- non-resettable replay or security-epoch storage;
- a production key, trust root, CUDA inventory, or release catalog.

The fixture receipt and ledger use incompatible v2 schemas and domains so an
old or mixed record is rejected. Each receipt embeds the complete path-free
intent and its recomputed digest. The v2 fixture ledger intentionally starts
fresh because it is test-only. A production migration must preserve every v1
used-release reservation and security-epoch high-water mark before accepting v2
traffic; that migration is deferred with the production broker itself.

Policy provisioning alone does not provision production authority. Production
promotion must stay disabled until those controls are implemented and
independently reviewed. The elevated repository harness temporarily installs
the zero-authority service and policy. It creates one disabled, one-hour local
standard account with an in-memory cryptographic `SecureString`, stages
exact-hash client and fixed-probe copies beneath a protected machine directory,
then enables the account and launches those copies with its primary credentials,
`LoadUserProfile=false`, and a cleared minimal environment. The policy and
staging ACL bind only that account's canonical machine SID (RID 1000 or
greater); the elevated runner remains a wrong-identity fixture. The harness
proves invalid-policy and wrong-SID denial, exact access-mask and snapshot
behavior, and performs identity-safe cleanup without creating a profile or
persisting credential material. Account cleanup validates its exact SID, name,
unique marker, and expiry, disables it, deletes only by SID, and drops ownership
immediately after confirmed deletion. The harness
validates and deletes each owned registry object through one
no-follow handle opened with `DELETE`; handle-bound `NtDeleteKey` prevents a
same-name replacement from redirecting cleanup after validation. Ownership is
dropped immediately after each successful deletion, before observing the path
again, so a replacement cannot be deleted by a later cleanup retry. The
elevated adversarial test renames the validated object, creates a replacement at
the fixed path, and proves the retained handle deletes only the renamed original.
Neither
release binary contains installation, policy provisioning, or console-mode
behavior. Run the crate proof on Windows with:

```powershell
cargo test --locked --offline -- --test-threads=1
```

One test invokes the repository's existing PowerShell promotion harness, which
uses the independently locked worker-pack author to generate both prepared
packs and the canonical handoff/request. The broker then consumes those exact
generated bytes so schema compatibility is not established only by a second
Rust fixture generator.
