[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidateNotNullOrEmpty()]
    [string]$AuthorizedClientSid
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$policyPath = 'SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization'
$serviceSid = 'S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137'
$systemSid = 'S-1-5-18'
$administratorsSid = 'S-1-5-32-544'
$schemaValue = 'SchemaVersion'
$clientSidValue = 'AuthorizedClientSid'
$provisioningValue = 'ProvisioningState'

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

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([Runtime.InteropServices.OSPlatform]::Windows)) {
    throw 'Windows GPU broker client policy can be provisioned only on Windows.'
}
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Windows GPU broker client policy provisioning requires elevation.'
}
Assert-DedicatedAccountSid -Value $AuthorizedClientSid

if (-not ('Scribe.GpuBroker.RegistryNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Scribe.GpuBroker {
    public static class RegistryNative {
        private const uint TOKEN_ADJUST_PRIVILEGES = 0x20;
        private const uint TOKEN_QUERY = 0x8;
        private const uint SE_PRIVILEGE_ENABLED = 0x2;
        private const int ERROR_NOT_ALL_ASSIGNED = 1300;

        [StructLayout(LayoutKind.Sequential)]
        private struct Luid { internal uint LowPart; internal int HighPart; }

        [StructLayout(LayoutKind.Sequential)]
        private struct TokenPrivileges {
            internal uint PrivilegeCount;
            internal Luid Luid;
            internal uint Attributes;
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

        [DllImport("advapi32.dll", SetLastError = true)]
        private static extern bool AdjustTokenPrivileges(
            IntPtr token,
            bool disableAllPrivileges,
            ref TokenPrivileges newState,
            uint bufferLength,
            IntPtr previousState,
            IntPtr returnLength);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern int RegCreateKeyExW(
            IntPtr hKey,
            string lpSubKey,
            int reserved,
            string lpClass,
            int options,
            int samDesired,
            IntPtr securityAttributes,
            out SafeRegistryHandle result,
            out int disposition);

        public static void EnableRestorePrivilege() {
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
                SetLastError(0);
                if (!AdjustTokenPrivileges(token, false, ref privileges, 0, IntPtr.Zero, IntPtr.Zero))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                int error = Marshal.GetLastWin32Error();
                if (error == ERROR_NOT_ALL_ASSIGNED)
                    throw new Win32Exception(error);
            }
            finally { CloseHandle(token); }
        }
    }
}
'@
}

[Scribe.GpuBroker.RegistryNative]::EnableRestorePrivilege()
$hklm = [IntPtr]::new(-2147483646)
$keyHandle = $null
$key = $null
$committed = $false
$disposition = 0
$status = [Scribe.GpuBroker.RegistryNative]::RegCreateKeyExW(
    $hklm,
    $policyPath,
    0,
    $null,
    0,
    0x000f003f -bor 0x00000100,
    [IntPtr]::Zero,
    [ref]$keyHandle,
    [ref]$disposition
)
if ($status -ne 0) { throw "Could not create the fixed authorization policy (Win32 $status)." }
if ($disposition -ne 1) {
    $keyHandle.Dispose()
    throw 'Refusing to modify a pre-existing Windows GPU broker client policy.'
}

try {
    $key = [Microsoft.Win32.RegistryKey]::FromHandle($keyHandle, [Microsoft.Win32.RegistryView]::Registry64)
    $key.SetValue($provisioningValue, 'incomplete', [Microsoft.Win32.RegistryValueKind]::String)
    $key.SetValue($schemaValue, 1, [Microsoft.Win32.RegistryValueKind]::DWord)
    $key.SetValue($clientSidValue, $AuthorizedClientSid, [Microsoft.Win32.RegistryValueKind]::String)

    $security = [Security.AccessControl.RegistrySecurity]::new()
    $security.SetAccessRuleProtection($true, $false)
    $security.SetOwner([Security.Principal.SecurityIdentifier]::new($systemSid))
    $allow = [Security.AccessControl.AccessControlType]::Allow
    foreach ($entry in @(
        @($systemSid, [Security.AccessControl.RegistryRights]::FullControl),
        @($administratorsSid, [Security.AccessControl.RegistryRights]::FullControl),
        @($serviceSid, [Security.AccessControl.RegistryRights]::ReadKey)
    )) {
        $rule = [Security.AccessControl.RegistryAccessRule]::new(
            [Security.Principal.SecurityIdentifier]::new([string]$entry[0]),
            $entry[1],
            [Security.AccessControl.InheritanceFlags]::None,
            [Security.AccessControl.PropagationFlags]::None,
            $allow
        )
        $security.AddAccessRule($rule)
    }
    $key.SetAccessControl($security)
    $key.Flush()
    Assert-Policy -Key $key -ExpectProvisioningMarker $true

    # Removing the marker is the only commit point. Before it, every service
    # startup rejects the extra value even if provisioning was interrupted.
    $key.DeleteValue($provisioningValue, $true)
    $key.Flush()
    Assert-Policy -Key $key -ExpectProvisioningMarker $false
    $committed = $true
    Write-Output "Provisioned protected Windows GPU broker client policy for $AuthorizedClientSid. Restart the broker service to load it."
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
