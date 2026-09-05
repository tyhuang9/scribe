[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$AuthorizedClientSid,

    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9a-f]{64}$')]
    [string]$InvocationNonce
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$policyPath = 'SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization'
$policyRoot = 'SOFTWARE'
$policyAncestors = @(
    'SOFTWARE\Scribe',
    'SOFTWARE\Scribe\GpuPromotionBroker',
    'SOFTWARE\Scribe\GpuPromotionBroker\v1'
)
$serviceSid = 'S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137'
$systemSid = 'S-1-5-18'
$administratorsSid = 'S-1-5-32-544'
$schemaValue = 'SchemaVersion'
$clientSidValue = 'AuthorizedClientSid'
$provisioningValue = 'ProvisioningState'
$successRecordKind = 'scribe-windows-gpu-broker-client-policy-provisioning-success-v1'
$createdAncestorPaths = [Collections.Generic.List[string]]::new()

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-DedicatedAccountSid([string]$Value) {
    try { $sid = [Security.Principal.SecurityIdentifier]::new($Value) }
    catch { throw 'AuthorizedClientSid must be an explicit canonical SID, never an account name.' }
    Assert-True ($sid.Value -ceq $Value) 'AuthorizedClientSid is not in canonical round-trip form.'

    $parts = $Value.Split('-')
    Assert-True ($parts.Count -eq 8 -and ($parts[0..3] -join '-') -ceq 'S-1-5-21') 'AuthorizedClientSid must be a dedicated local or domain account SID.'
    foreach ($component in $parts[4..7]) {
        $parsed = 0u
        Assert-True ([uint32]::TryParse($component, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$parsed)) 'AuthorizedClientSid contains a noncanonical subauthority.'
        Assert-True ($component -ceq $parsed.ToString([Globalization.CultureInfo]::InvariantCulture)) 'AuthorizedClientSid contains a noncanonical subauthority.'
    }
    Assert-True ([uint32]$parts[7] -ge 1000) 'AuthorizedClientSid must not identify a built-in or reserved account.'

    $dangerous = @(
        'S-1-1-0',       # Everyone / WD
        'S-1-5-7',       # Anonymous / AN
        'S-1-5-11',      # Authenticated Users / AU
        'S-1-5-18',      # SYSTEM
        'S-1-5-19',      # LocalService
        'S-1-5-20',      # NetworkService
        'S-1-5-32-544',  # Builtin Administrators / BA
        'S-1-5-32-545',  # Builtin Users / BU
        $serviceSid
    )
    Assert-True ($Value -cnotin $dangerous) 'AuthorizedClientSid identifies a broad or service principal.'
}

function Assert-InvocationNonce([string]$Value) {
    Assert-True ($Value.Length -eq 64) 'InvocationNonce must be exactly 32 bytes of lowercase hexadecimal correlation data.'
    Assert-True ([regex]::IsMatch($Value, '\A[0-9a-f]{64}\z', [Text.RegularExpressions.RegexOptions]::CultureInvariant)) 'InvocationNonce is noncanonical.'
}

function Assert-PolicySecurity([Microsoft.Win32.RegistryKey]$Key) {
    $security = $Key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
    Assert-True $security.AreAccessRulesProtected 'Authorization policy DACL inherits ambient access.'
    Assert-True ($security.GetOwner([Security.Principal.SecurityIdentifier]).Value -ceq $systemSid) 'Authorization policy owner is not SYSTEM.'
    $rules = @($security.GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]))
    Assert-True ($rules.Count -eq 3) 'Authorization policy has an unexpected ACE count.'
    $expectedRules = @{
        $systemSid = [uint32][Security.AccessControl.RegistryRights]::FullControl
        $administratorsSid = [uint32][Security.AccessControl.RegistryRights]::FullControl
        $serviceSid = [uint32][Security.AccessControl.RegistryRights]::ReadKey
    }
    foreach ($rule in $rules) {
        $ruleSid = $rule.IdentityReference.Value
        Assert-True ($expectedRules.ContainsKey($ruleSid)) 'Authorization policy contains an unexpected principal.'
        Assert-True ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) 'Authorization policy contains a deny ACE.'
        Assert-True (-not $rule.IsInherited) 'Authorization policy contains an inherited ACE.'
        Assert-True ($rule.InheritanceFlags -eq [Security.AccessControl.InheritanceFlags]::None) 'Authorization policy ACE is inheritable.'
        Assert-True ($rule.PropagationFlags -eq [Security.AccessControl.PropagationFlags]::None) 'Authorization policy ACE propagates.'
        Assert-True ([uint32]$rule.RegistryRights -eq $expectedRules[$ruleSid]) 'Authorization policy ACE rights are noncanonical.'
        [void]$expectedRules.Remove($ruleSid)
    }
    Assert-True ($expectedRules.Count -eq 0) 'Authorization policy is missing a required principal.'
}

function Test-StandardSoftwareCreatorOwnerInheritanceTemplate(
    [Security.AccessControl.GenericAce]$Ace,
    [string]$Path
) {
    return (
        $Path -ceq 'SOFTWARE' -and
        $Ace -is [Security.AccessControl.CommonAce] -and
        -not $Ace.IsCallback -and
        $Ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed -and
        $Ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessAllowed -and
        $Ace.AceFlags -eq [Security.AccessControl.AceFlags]::ContainerInherit -and
        [uint32]$Ace.AccessMask -eq [uint32]0x000f003f -and
        $Ace.SecurityIdentifier.Value -ceq 'S-1-3-0' -and
        $Ace.OpaqueLength -eq 0
    )
}

function Test-SafePolicyAncestorAcl(
    [Security.AccessControl.RawAcl]$Acl,
    [string]$Path,
    [string[]]$TrustedOwners,
    [uint32]$MutationMask
) {
    if ($null -eq $Acl) { return $false }
    $creatorOwnerTemplateCount = 0
    for ($index = 0; $index -lt $Acl.Count; $index++) {
        $ace = $Acl[$index]
        if (Test-StandardSoftwareCreatorOwnerInheritanceTemplate -Ace $ace -Path $Path) {
            $creatorOwnerTemplateCount++
            if ($creatorOwnerTemplateCount -gt 1) { return $false }
            continue
        }
        if ($ace -isnot [Security.AccessControl.QualifiedAce]) { return $false }
        if ($ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessDenied) { continue }
        if ($ace.AceQualifier -ne [Security.AccessControl.AceQualifier]::AccessAllowed) { return $false }
        if (([uint32]$ace.AccessMask -band $MutationMask) -eq 0) { continue }
        if ($TrustedOwners -cnotcontains $ace.SecurityIdentifier.Value) { return $false }
    }
    return $true
}

function Assert-SafePolicyAncestor([Microsoft.Win32.RegistryKey]$Key, [string]$Path) {
    $security = $Key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
    $owner = $security.GetOwner([Security.Principal.SecurityIdentifier]).Value
    $trustedOwners = @(
        $systemSid,
        $administratorsSid,
        'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464' # TrustedInstaller
    )
    Assert-True ($owner -cin $trustedOwners) "Authorization policy ancestor $Path has an untrusted owner."
    $raw = [Security.AccessControl.RawSecurityDescriptor]::new($security.GetSecurityDescriptorBinaryForm(), 0)
    Assert-True ($null -ne $raw.DiscretionaryAcl) "Authorization policy ancestor $Path has a null DACL."
    $mutationMask = [uint32]0x500d0026 # generic write/all plus set/create/link/delete/DACL/owner
    Assert-True (Test-SafePolicyAncestorAcl -Acl $raw.DiscretionaryAcl -Path $Path -TrustedOwners $trustedOwners -MutationMask $mutationMask) "Authorization policy ancestor $Path contains an unsafe raw DACL entry."
}

function Assert-Policy([Microsoft.Win32.RegistryKey]$Key, [bool]$ExpectProvisioningMarker) {
    Assert-True ($Key.SubKeyCount -eq 0) 'Authorization policy contains an unexpected subkey.'
    $expectedNames = if ($ExpectProvisioningMarker) {
        @($clientSidValue, $provisioningValue, $schemaValue)
    }
    else {
        @($clientSidValue, $schemaValue)
    }
    $actualNames = @($Key.GetValueNames() | Sort-Object -CaseSensitive)
    $expectedNames = @($expectedNames | Sort-Object -CaseSensitive)
    Assert-True ($actualNames.Count -eq $expectedNames.Count) 'Authorization policy contains an unexpected value.'
    for ($index = 0; $index -lt $expectedNames.Count; $index++) {
        Assert-True ($actualNames[$index] -ceq $expectedNames[$index]) 'Authorization policy value inventory is noncanonical.'
    }
    Assert-True ($Key.GetValueKind($schemaValue) -eq [Microsoft.Win32.RegistryValueKind]::DWord) 'Authorization schema value type is noncanonical.'
    Assert-True ([uint32]$Key.GetValue($schemaValue) -eq 1u) 'Authorization schema version is unsupported.'
    Assert-True ($Key.GetValueKind($clientSidValue) -eq [Microsoft.Win32.RegistryValueKind]::String) 'Authorized client SID value type is noncanonical.'
    Assert-True ([string]$Key.GetValue($clientSidValue) -ceq $AuthorizedClientSid) 'Authorized client SID value changed during provisioning.'
    if ($ExpectProvisioningMarker) {
        Assert-True ($Key.GetValueKind($provisioningValue) -eq [Microsoft.Win32.RegistryValueKind]::String) 'Provisioning marker type is noncanonical.'
        Assert-True ([string]$Key.GetValue($provisioningValue) -ceq 'incomplete') 'Provisioning marker is noncanonical.'
    }

    Assert-PolicySecurity -Key $Key
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([Runtime.InteropServices.OSPlatform]::Windows)) {
    throw 'Windows GPU broker client policy can be provisioned only on Windows.'
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Windows GPU broker client policy provisioning requires elevation.'
}
Assert-DedicatedAccountSid -Value $AuthorizedClientSid
Assert-InvocationNonce -Value $InvocationNonce

if (-not ('Scribe.GpuBroker.RegistryNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Scribe.GpuBroker {
    public static class RegistryNative {
        public const int REG_CREATED_NEW_KEY = 1;
        private const uint TOKEN_ADJUST_PRIVILEGES = 0x20;
        private const uint TOKEN_QUERY = 0x8;
        private const uint SE_PRIVILEGE_ENABLED = 0x2;
        private const int ERROR_FILE_NOT_FOUND = 2;
        private const int SDDL_REVISION_1 = 1;
        private const int REG_OPTION_OPEN_LINK = 8;
        private const int REG_LINK = 6;
        private const int KEY_QUERY_VALUE = 1;
        private const int KEY_READ = 0x00020019;
        private const int KEY_WOW64_64KEY = 0x00000100;
        private static readonly IntPtr HKEY_LOCAL_MACHINE = new IntPtr(-2147483646);

        [StructLayout(LayoutKind.Sequential)]
        internal struct Luid { internal uint LowPart; internal int HighPart; }

        [StructLayout(LayoutKind.Sequential)]
        internal struct TokenPrivileges {
            internal uint PrivilegeCount;
            internal Luid Luid;
            internal uint Attributes;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct SecurityAttributes {
            internal int Length;
            internal IntPtr SecurityDescriptor;
            internal int InheritHandle;
        }

        [DllImport("kernel32.dll")]
        private static extern IntPtr GetCurrentProcess();

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        [DllImport("kernel32.dll")]
        private static extern void SetLastError(uint error);

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool OpenProcessToken(
            IntPtr process,
            uint desiredAccess,
            out IntPtr token);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool LookupPrivilegeValueW(
            string systemName,
            string name,
            out Luid luid);

        [DllImport("advapi32.dll", EntryPoint = "AdjustTokenPrivileges", SetLastError = true)]
        private static extern bool AdjustTokenPrivilegesAndCapturePrevious(
            IntPtr token,
            bool disableAllPrivileges,
            ref TokenPrivileges newState,
            uint bufferLength,
            out TokenPrivileges previousState,
            out uint returnLength);

        [DllImport("advapi32.dll", EntryPoint = "AdjustTokenPrivileges", SetLastError = true)]
        private static extern bool RestoreTokenPrivileges(
            IntPtr token,
            bool disableAllPrivileges,
            ref TokenPrivileges newState,
            uint bufferLength,
            IntPtr previousState,
            IntPtr returnLength);

        [DllImport("kernel32.dll")]
        private static extern IntPtr LocalFree(IntPtr memory);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string stringSecurityDescriptor,
            int stringSDRevision,
            out IntPtr securityDescriptor,
            IntPtr securityDescriptorSize);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int RegCreateKeyExW(
            IntPtr hKey,
            string lpSubKey,
            int reserved,
            string lpClass,
            int options,
            int samDesired,
            ref SecurityAttributes securityAttributes,
            out SafeRegistryHandle result,
            out int disposition);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int RegOpenKeyExW(
            IntPtr hKey,
            string lpSubKey,
            int options,
            int samDesired,
            out SafeRegistryHandle result);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern int RegQueryValueExW(
            SafeRegistryHandle hKey,
            string lpValueName,
            IntPtr reserved,
            out int type,
            IntPtr data,
            ref int dataSize);

        public static int CreateProtectedKey(
            string path,
            string securityDescriptorDefinition,
            out SafeRegistryHandle result,
            out int disposition) {
            IntPtr descriptor;
            result = null;
            disposition = 0;
            if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
                securityDescriptorDefinition,
                SDDL_REVISION_1,
                out descriptor,
                IntPtr.Zero))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                SecurityAttributes securityAttributes = new SecurityAttributes {
                    Length = Marshal.SizeOf(typeof(SecurityAttributes)),
                    SecurityDescriptor = descriptor,
                    InheritHandle = 0
                };
                return RegCreateKeyExW(
                    HKEY_LOCAL_MACHINE,
                    path,
                    0,
                    null,
                    0,
                    0x000f003f | 0x00000100,
                    ref securityAttributes,
                    out result,
                    out disposition);
            }
            finally { LocalFree(descriptor); }
        }

        public static int OpenExistingKeyNoFollow(
            string path,
            out SafeRegistryHandle result,
            out bool isRegistryLink) {
            isRegistryLink = false;
            int status = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                path,
                REG_OPTION_OPEN_LINK,
                KEY_READ | KEY_WOW64_64KEY,
                out result);
            if (status != 0)
                return status;
            int type;
            int size = 0;
            int query = RegQueryValueExW(
                result,
                "SymbolicLinkValue",
                IntPtr.Zero,
                out type,
                IntPtr.Zero,
                ref size);
            isRegistryLink = query == 0 && type == REG_LINK;
            return 0;
        }

        public sealed class RestorePrivilegeScope : IDisposable {
            private IntPtr token;
            private TokenPrivileges previousState;
            private bool restorationComplete;

            internal RestorePrivilegeScope(IntPtr token, TokenPrivileges previousState) {
                this.token = token;
                this.previousState = previousState;
            }

            public void Dispose() {
                if (token == IntPtr.Zero)
                    return;

                if (!restorationComplete) {
                    if (previousState.PrivilegeCount != 0) {
                        SetLastError(0);
                        if (!RestoreTokenPrivileges(
                            token,
                            false,
                            ref previousState,
                            0,
                            IntPtr.Zero,
                            IntPtr.Zero))
                            throw new Win32Exception(
                                Marshal.GetLastWin32Error(),
                                "Could not restore the caller's SeRestorePrivilege state.");
                        int error = Marshal.GetLastWin32Error();
                        if (error != 0)
                            throw new Win32Exception(
                                error,
                                "Could not restore the caller's SeRestorePrivilege state.");
                    }
                    restorationComplete = true;
                }

                // Retain both the token and captured prior state if close
                // fails. A later outer-boundary attempt can retry the close
                // without repeating an already successful restoration.
                if (!CloseHandle(token))
                    throw new Win32Exception(
                        Marshal.GetLastWin32Error(),
                        "Could not close the caller's SeRestorePrivilege token.");

                // Ownership is relinquished only after exact restoration (or
                // a captured no-op) and successful handle closure.
                token = IntPtr.Zero;
                previousState = default(TokenPrivileges);
            }
        }

        public static void RestoreOrFailFast(RestorePrivilegeScope scope) {
            if (scope == null)
                throw new ArgumentNullException("scope");
            Exception firstFailure;
            try {
                scope.Dispose();
                return;
            }
            catch (Exception error) {
                firstFailure = error;
            }

            try {
                // Every outer-boundary failure receives at least one retry.
                // Returning to a long-lived host with the scope still live is
                // forbidden because SeRestorePrivilege may remain enabled.
                scope.Dispose();
            }
            catch (Exception retryFailure) {
                Environment.FailFast(
                    "Could not restore the caller's SeRestorePrivilege state after retry.",
                    new AggregateException(firstFailure, retryFailure));
            }
        }

        public static RestorePrivilegeScope EnableRestorePrivilege() {
            IntPtr token;
            if (!OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, out token))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                Luid luid;
                if (!LookupPrivilegeValueW(null, "SeRestorePrivilege", out luid))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                TokenPrivileges privileges = new TokenPrivileges {
                    PrivilegeCount = 1,
                    Luid = luid,
                    Attributes = SE_PRIVILEGE_ENABLED
                };
                TokenPrivileges previousState;
                uint returnLength;
                SetLastError(0);
                if (!AdjustTokenPrivilegesAndCapturePrevious(
                    token,
                    false,
                    ref privileges,
                    (uint)Marshal.SizeOf(typeof(TokenPrivileges)),
                    out previousState,
                    out returnLength))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                int error = Marshal.GetLastWin32Error();
                RestorePrivilegeScope scope = new RestorePrivilegeScope(token, previousState);
                token = IntPtr.Zero;
                if (error != 0 || returnLength > Marshal.SizeOf(typeof(TokenPrivileges))) {
                    Exception activationFailure = new Win32Exception(error != 0 ? error : 87);
                    RestoreOrFailFast(scope);
                    throw activationFailure;
                }
                return scope;
            }
            finally {
                if (token != IntPtr.Zero)
                    CloseHandle(token);
            }
        }
    }
}
'@
}

$restorePrivilegeScope = [Scribe.GpuBroker.RegistryNative]::EnableRestorePrivilege()
$successRecord = $null
try {
$policySddl = "O:SYD:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;$serviceSid)"
$rootHandle = $null
$rootIsLink = $false
$rootStatus = [Scribe.GpuBroker.RegistryNative]::OpenExistingKeyNoFollow(
    $policyRoot,
    [ref]$rootHandle,
    [ref]$rootIsLink
)
Assert-True ($rootStatus -eq 0) "Could not inspect fixed authorization policy root $policyRoot (Win32 $rootStatus)."
try {
    Assert-True (-not $rootIsLink) "Authorization policy root $policyRoot is a registry link."
    $rootKey = [Microsoft.Win32.RegistryKey]::FromHandle($rootHandle, [Microsoft.Win32.RegistryView]::Registry64)
    try { Assert-SafePolicyAncestor -Key $rootKey -Path $policyRoot }
    finally { $rootKey.Dispose() }
}
finally { $rootHandle.Dispose() }

foreach ($ancestorPath in $policyAncestors) {
    $ancestorHandle = $null
    $ancestorIsLink = $false
    $openStatus = [Scribe.GpuBroker.RegistryNative]::OpenExistingKeyNoFollow(
        $ancestorPath,
        [ref]$ancestorHandle,
        [ref]$ancestorIsLink
    )
    if ($openStatus -eq 0) {
        try {
            Assert-True (-not $ancestorIsLink) "Authorization policy ancestor $ancestorPath is a registry link."
            $ancestorKey = [Microsoft.Win32.RegistryKey]::FromHandle($ancestorHandle, [Microsoft.Win32.RegistryView]::Registry64)
            try { Assert-SafePolicyAncestor -Key $ancestorKey -Path $ancestorPath }
            finally { $ancestorKey.Dispose() }
        }
        finally { $ancestorHandle.Dispose() }
        continue
    }
    Assert-True ($openStatus -eq 2) "Could not inspect authorization policy ancestor $ancestorPath (Win32 $openStatus)."
    $ancestorDisposition = 0
    $createStatus = [Scribe.GpuBroker.RegistryNative]::CreateProtectedKey(
        $ancestorPath,
        $policySddl,
        [ref]$ancestorHandle,
        [ref]$ancestorDisposition
    )
    if ($createStatus -ne 0 -or $ancestorDisposition -ne [Scribe.GpuBroker.RegistryNative]::REG_CREATED_NEW_KEY) {
        if ($null -ne $ancestorHandle) { $ancestorHandle.Dispose() }
        throw "Could not create authorization policy ancestor $ancestorPath safely (Win32 $createStatus, disposition $ancestorDisposition)."
    }
    try {
        $ancestorKey = [Microsoft.Win32.RegistryKey]::FromHandle($ancestorHandle, [Microsoft.Win32.RegistryView]::Registry64)
        try { Assert-PolicySecurity -Key $ancestorKey }
        finally { $ancestorKey.Dispose() }
    }
    finally { $ancestorHandle.Dispose() }
    # Only this exact RegCreateKeyExW result is authoritative for cleanup
    # ownership. The success record is emitted only after the leaf commits.
    [void]$createdAncestorPaths.Add($ancestorPath)
}

$keyHandle = $null
$key = $null
$committed = $false
$disposition = 0
$existingHandle = $null
$existingIsLink = $false
$existingStatus = [Scribe.GpuBroker.RegistryNative]::OpenExistingKeyNoFollow(
    $policyPath,
    [ref]$existingHandle,
    [ref]$existingIsLink
)
if ($existingStatus -eq 0) {
    $existingHandle.Dispose()
    throw 'Refusing to modify a pre-existing Windows GPU broker client policy.'
}
Assert-True ($existingStatus -eq 2) "Could not inspect the fixed authorization policy (Win32 $existingStatus)."
$status = [Scribe.GpuBroker.RegistryNative]::CreateProtectedKey(
    $policyPath,
    $policySddl,
    [ref]$keyHandle,
    [ref]$disposition
)
if ($status -ne 0) {
    if ($null -ne $keyHandle) { $keyHandle.Dispose() }
    throw "Could not create the fixed authorization policy (Win32 $status)."
}
if ($disposition -ne [Scribe.GpuBroker.RegistryNative]::REG_CREATED_NEW_KEY) {
    $keyHandle.Dispose()
    throw 'Refusing to modify a pre-existing Windows GPU broker client policy.'
}

try {
    $key = [Microsoft.Win32.RegistryKey]::FromHandle($keyHandle, [Microsoft.Win32.RegistryView]::Registry64)
    # The creation call supplied the final descriptor. Verify the key was born
    # protected before the first value write, so no inherited-writer handle can
    # survive a later DACL change.
    Assert-PolicySecurity -Key $key
    $key.SetValue($provisioningValue, 'incomplete', [Microsoft.Win32.RegistryValueKind]::String)
    $key.SetValue($schemaValue, 1, [Microsoft.Win32.RegistryValueKind]::DWord)
    $key.SetValue($clientSidValue, $AuthorizedClientSid, [Microsoft.Win32.RegistryValueKind]::String)

    $key.Flush()
    Assert-Policy -Key $key -ExpectProvisioningMarker $true

    # Restore the caller token before the commit point. If restoration fails,
    # the incomplete marker remains and the service rejects this policy.
    $restorePrivilegeScope.Dispose()

    # Removing the marker is the only commit point. Before it, every service
    # startup rejects the extra value even if provisioning was interrupted.
    $key.DeleteValue($provisioningValue, $true)
    $key.Flush()
    Assert-Policy -Key $key -ExpectProvisioningMarker $false
    $committed = $true
    $successRecord = [ordered]@{
        schema_version = 1
        kind = $successRecordKind
        invocation_nonce = $InvocationNonce
        authorized_client_sid = $AuthorizedClientSid
        policy_path = $policyPath
        created_ancestors = @($createdAncestorPaths)
    }
}
catch {
    if ($null -ne $key -and -not $committed) {
        try {
            $key.SetValue($provisioningValue, 'incomplete', [Microsoft.Win32.RegistryValueKind]::String)
            $key.Flush()
        }
        catch { }
    }
    throw
}
finally {
    if ($null -ne $key) { $key.Dispose() }
    if ($null -ne $keyHandle) { $keyHandle.Dispose() }
}
}
finally {
    # This also runs when the script is dot-sourced into a long-lived elevated
    # host. Restore the exact TOKEN_PRIVILEGES state captured before enabling
    # SeRestorePrivilege; a restore failure makes the invocation fail.
    [Scribe.GpuBroker.RegistryNative]::RestoreOrFailFast($restorePrivilegeScope)
}

Assert-True ($null -ne $successRecord) 'Provisioning completed without a success record.'
# Stdout is emitted only after the privilege state has been restored, and is
# one nonce-bound record so automation can reject stale/interleaved output.
$successRecord | ConvertTo-Json -Compress -Depth 3 | Write-Output
