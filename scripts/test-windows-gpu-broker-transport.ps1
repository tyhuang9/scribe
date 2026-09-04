param(
    [switch]$RequireScmIntegration,

    [Parameter(DontShow)]
    [switch]$RunPersistentPrivilegeRestoreFailFastFixture,

    [Parameter(DontShow)]
    [switch]$RunEphemeralIdentityProbe,

    [Parameter(DontShow)]
    [switch]$RunEphemeralFullControlProbe,

    [Parameter(DontShow)]
    [switch]$RunEphemeralStalledProbe,

    [Parameter(DontShow)]
    [switch]$RunEphemeralServerAccessProbe,

    [Parameter(DontShow)]
    [string]$ExpectedEphemeralSid,

    [Parameter(DontShow)]
    [uint32]$ExpectedBrokerProcessId
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$serviceName = 'ScribeGpuPromotionBroker'
$pipeName = 'ScribeGpuPromotionBroker.v1'
$serviceSid = 'S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137'
$policyPath = 'SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization'
$policyRegistryPath = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifest = Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\Cargo.toml'
$provisioner = Join-Path $repositoryRoot 'scripts\provision-windows-gpu-broker-client-policy.ps1'
$targetRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-gpu-broker-transport-$([guid]::NewGuid().ToString('N'))"
$previousCargoTarget = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process')
$createdService = $false
$machineTarget = $null
$handoff = $null
$output = $null
$primaryFailure = $null
$cleanupFailures = [Collections.Generic.List[object]]::new()
$safeToRemoveMachineTarget = $false
$ownedMachineTarget = $null
$ownedPolicyState = $null
$ownedPolicyAncestors = @()
$ownedEphemeralAccount = $null
$ephemeralPassword = $null
$activeCredentialProcess = $null
$policyAncestorPaths = @(
    'SOFTWARE\Scribe',
    'SOFTWARE\Scribe\GpuPromotionBroker',
    'SOFTWARE\Scribe\GpuPromotionBroker\v1'
)

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-EphemeralProbeIdentity([string]$ExpectedSid) {
    Assert-True (-not [string]::IsNullOrWhiteSpace($ExpectedSid)) 'Credential probe requires its exact expected SID.'
    Assert-True ($ExpectedSid -cmatch '^S-1-5-21-(?:[0-9]{1,10}-){3}[0-9]{1,10}$') 'Credential probe expected SID is not a machine-account SID.'
    $canonicalExpected = [Security.Principal.SecurityIdentifier]::new($ExpectedSid)
    Assert-True ($canonicalExpected.Value -ceq $ExpectedSid) 'Credential probe expected SID is noncanonical.'
    $expectedRid = [uint64]($ExpectedSid.Substring($ExpectedSid.LastIndexOf('-') + 1))
    Assert-True ($expectedRid -ge 1000) 'Credential probe expected SID has a reserved RID.'
    $current = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        Assert-True ($null -ne $current.User -and $current.User.Value -ceq $ExpectedSid) 'Credential probe did not run with the exact ephemeral TokenUser SID.'
        $currentPrincipal = [Security.Principal.WindowsPrincipal]::new($current)
        Assert-True (-not $currentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) 'Credential probe unexpectedly has administrator membership.'
    }
    finally { $current.Dispose() }
}

$ephemeralProbeCount = @(
    $RunEphemeralIdentityProbe,
    $RunEphemeralFullControlProbe,
    $RunEphemeralStalledProbe,
    $RunEphemeralServerAccessProbe
).Where({ $_ }).Count
if ($ephemeralProbeCount -gt 1) { throw 'Only one fixed ephemeral credential probe may run.' }

function Test-ServerAccessProbeRecord([AllowNull()][string]$Record) {
    if ($null -eq $Record -or $Record.Length -gt 192) { return $false }
    $match = [regex]::Match(
        $Record,
        '\Aephemeral-server-access;session=(?<sessionState>zero|nonzero|error):(?<sessionStatus>[0-9]{1,10});process=(?<processState>ok|error):(?<processStatus>[0-9]{1,10});token=(?<tokenState>ok|error|not_attempted):(?<tokenStatus>[0-9]{1,10})\z',
        [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if (-not $match.Success) { return $false }

    foreach ($field in @('session', 'process', 'token')) {
        $statusText = $match.Groups["${field}Status"].Value
        $status = [uint32]0
        if (-not [uint32]::TryParse($statusText, [ref]$status) -or $status.ToString() -cne $statusText) { return $false }
        $state = $match.Groups["${field}State"].Value
        if (($state -ceq 'error') -ne ($status -ne 0)) { return $false }
        if ($state -ceq 'not_attempted' -and $status -ne 0) { return $false }
    }

    $processFailed = $match.Groups['processState'].Value -ceq 'error'
    $tokenNotAttempted = $match.Groups['tokenState'].Value -ceq 'not_attempted'
    return $processFailed -eq $tokenNotAttempted
}

if (($ephemeralProbeCount -eq 0 -or $RunEphemeralServerAccessProbe) -and -not ('Scribe.GpuBroker.ServerAccessProbeNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

namespace Scribe.GpuBroker {
    // Read-only test probe for the exact service process identified and
    // ownership-checked by the elevated parent harness.
    public static class ServerAccessProbeNative {
        private const uint PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
        private const uint TOKEN_QUERY = 0x0008;
        private const uint TOKEN_SESSION_ID = 12;
        private const int ERROR_ACCESS_DENIED = 5;
        private static readonly uint[] ForbiddenProcessRights = new uint[] {
            0x00000001, // PROCESS_TERMINATE
            0x00000010, // PROCESS_VM_READ
            0x00000040, // PROCESS_DUP_HANDLE
            0x00000400, // PROCESS_QUERY_INFORMATION
            0x00010000, // DELETE
            0x00020000, // READ_CONTROL
            0x00040000, // WRITE_DAC
            0x00080000, // WRITE_OWNER
            0x00100000  // SYNCHRONIZE
        };
        private static readonly uint[] ForbiddenTokenRights = new uint[] {
            0x00000001, // TOKEN_ASSIGN_PRIMARY
            0x00000002, // TOKEN_DUPLICATE
            0x00000004, // TOKEN_IMPERSONATE
            0x00000010, // TOKEN_QUERY_SOURCE
            0x00000020, // TOKEN_ADJUST_PRIVILEGES
            0x00000040, // TOKEN_ADJUST_GROUPS
            0x00000080, // TOKEN_ADJUST_DEFAULT
            0x00000100, // TOKEN_ADJUST_SESSIONID
            0x00010000, // DELETE
            0x00020000, // READ_CONTROL
            0x00040000, // WRITE_DAC
            0x00080000  // WRITE_OWNER
        };

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool ProcessIdToSessionId(uint processId, out uint sessionId);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern IntPtr OpenProcess(
            uint desiredAccess,
            [MarshalAs(UnmanagedType.Bool)] bool inheritHandle,
            uint processId);

        [DllImport("advapi32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool OpenProcessToken(IntPtr process, uint desiredAccess, out IntPtr token);

        [DllImport("advapi32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetTokenInformation(
            IntPtr token,
            uint informationClass,
            out uint information,
            uint informationLength,
            out uint returnLength);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool CloseHandle(IntPtr handle);

        public static string Probe(uint processId) {
            uint sessionId;
            string session;
            if (!ProcessIdToSessionId(processId, out sessionId))
                session = "error:" + ((uint)Marshal.GetLastWin32Error()).ToString();
            else
                session = (sessionId == 0 ? "zero:0" : "nonzero:0");

            IntPtr process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, processId);
            if (process == IntPtr.Zero) {
                uint processError = (uint)Marshal.GetLastWin32Error();
                return "ephemeral-server-access;session=" + session +
                    ";process=error:" + processError.ToString() +
                    ";token=not_attempted:0";
            }

            IntPtr token = IntPtr.Zero;
            string tokenResult;
            try {
                if (!OpenProcessToken(process, TOKEN_QUERY, out token))
                    tokenResult = "error:" + ((uint)Marshal.GetLastWin32Error()).ToString();
                else
                    tokenResult = "ok:0";
            }
            finally {
                if (token != IntPtr.Zero)
                    CloseHandle(token);
                CloseHandle(process);
            }
            return "ephemeral-server-access;session=" + session +
                ";process=ok:0;token=" + tokenResult;
        }

        public static void VerifyMinimalRights(uint processId) {
            IntPtr process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, processId);
            if (process == IntPtr.Zero)
                throw new InvalidOperationException("Exact broker process query access was denied.");
            IntPtr token = IntPtr.Zero;
            try {
                if (!OpenProcessToken(process, TOKEN_QUERY, out token))
                    throw new InvalidOperationException("Exact broker token query access was denied.");
                uint sessionId;
                uint returnLength;
                if (!GetTokenInformation(token, TOKEN_SESSION_ID, out sessionId, 4, out returnLength) ||
                    returnLength != 4 || sessionId != 0)
                    throw new InvalidOperationException("Broker token session identity was rejected.");

                foreach (uint forbidden in ForbiddenProcessRights) {
                    IntPtr excessive = OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | forbidden,
                        false,
                        processId);
                    if (excessive != IntPtr.Zero) {
                        CloseHandle(excessive);
                        throw new InvalidOperationException("Broker process granted excessive client rights.");
                    }
                    if (Marshal.GetLastWin32Error() != ERROR_ACCESS_DENIED)
                        throw new InvalidOperationException("Broker process excess-right denial was noncanonical.");
                }
                foreach (uint forbidden in ForbiddenTokenRights) {
                    IntPtr excessive;
                    if (OpenProcessToken(process, TOKEN_QUERY | forbidden, out excessive)) {
                        CloseHandle(excessive);
                        throw new InvalidOperationException("Broker token granted excessive client rights.");
                    }
                    if (Marshal.GetLastWin32Error() != ERROR_ACCESS_DENIED)
                        throw new InvalidOperationException("Broker token excess-right denial was noncanonical.");
                }
            }
            finally {
                if (token != IntPtr.Zero)
                    CloseHandle(token);
                CloseHandle(process);
            }
        }
    }
}
'@
}

if ($ephemeralProbeCount -eq 1) {
    Assert-EphemeralProbeIdentity -ExpectedSid $ExpectedEphemeralSid
    if ($RunEphemeralIdentityProbe) {
        [Console]::Out.WriteLine('ephemeral-identity-ok')
        [Console]::Out.Flush()
        return
    }

    if ($RunEphemeralServerAccessProbe) {
        Assert-True ($ExpectedBrokerProcessId -gt 0) 'Server-access probe requires an exact nonzero broker process ID.'
        $serverAccessRecord = [Scribe.GpuBroker.ServerAccessProbeNative]::Probe($ExpectedBrokerProcessId)
        Assert-True (Test-ServerAccessProbeRecord -Record $serverAccessRecord) 'Server-access probe returned a noncanonical record.'
        [Console]::Out.WriteLine($serverAccessRecord)
        [Console]::Out.Flush()
        [Scribe.GpuBroker.ServerAccessProbeNative]::VerifyMinimalRights($ExpectedBrokerProcessId)
        return
    }

    if ($RunEphemeralFullControlProbe) {
        $probe = [IO.Pipes.NamedPipeClientStream]::new(
            '.',
            $pipeName,
            [IO.Pipes.PipeAccessRights]::FullControl,
            [IO.Pipes.PipeOptions]::Asynchronous,
            [Security.Principal.TokenImpersonationLevel]::Identification,
            [IO.HandleInheritability]::None
        )
        try {
            $denied = $false
            try { $probe.Connect(2000) }
            catch [UnauthorizedAccessException] { $denied = $true }
            Assert-True $denied 'Ephemeral client received authority beyond the fixed production access mask.'
        }
        finally { $probe.Dispose() }
        [Console]::Out.WriteLine('ephemeral-full-control-denied')
        [Console]::Out.Flush()
        return
    }

    $exactRights = [IO.Pipes.PipeAccessRights](
        [uint32][IO.Pipes.PipeAccessRights]::ReadData -bor
        [uint32][IO.Pipes.PipeAccessRights]::WriteData -bor
        [uint32][IO.Pipes.PipeAccessRights]::ReadAttributes -bor
        [uint32][IO.Pipes.PipeAccessRights]::WriteAttributes -bor
        [uint32][IO.Pipes.PipeAccessRights]::Synchronize
    )
    Assert-True ([uint32]$exactRights -eq 0x00100183) 'Ephemeral stalled probe no longer requests the production client access mask.'
    $probe = [IO.Pipes.NamedPipeClientStream]::new(
        '.',
        $pipeName,
        $exactRights,
        [IO.Pipes.PipeOptions]::Asynchronous,
        [Security.Principal.TokenImpersonationLevel]::Identification,
        [IO.HandleInheritability]::None
    )
    try {
        $probe.Connect(5000)
        [Console]::Out.WriteLine('ephemeral-stalled-ready')
        [Console]::Out.Flush()
        try {
            $read = $probe.ReadByte()
            Assert-True ($read -eq -1) 'Stalled credential probe received unexpected broker bytes.'
        }
        catch [IO.IOException] {
            # SCM stop may close the pipe with either EOF or a broken-pipe error.
        }
    }
    finally { $probe.Dispose() }
    [Console]::Out.WriteLine('ephemeral-stalled-disconnected')
    [Console]::Out.Flush()
    return
}

if (-not ('Scribe.GpuBroker.RegistryCleanupNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace Scribe.GpuBroker {
    // Test-only cleanup opens the exact registry object without following a
    // registry link, validates through that handle, and passes the same live
    // SafeRegistryHandle to NtDeleteKey. NTSTATUS zero is the sole success
    // value. SafeHandle marshalling pins the handle for the native call.
    public static class RegistryCleanupNative {
        private const int DELETE = 0x00010000;
        private const int READ_CONTROL = 0x00020000;
        private const int KEY_QUERY_VALUE = 0x00000001;
        private const int KEY_ENUMERATE_SUB_KEYS = 0x00000008;
        private const int KEY_WRITE = 0x00020006;
        private const int KEY_WOW64_64KEY = 0x00000100;
        private const int REG_OPTION_OPEN_LINK = 0x00000008;
        private const int REG_LINK = 6;
        private const int ERROR_INVALID_DATA = 13;
        private const string POLICY_PARENT = @"SOFTWARE\Scribe\GpuPromotionBroker\v1";
        private const string POLICY_LEAF = "Authorization";
        private const string BOUNDARY_PREFIX = "Authorization.boundary-";
        private static readonly IntPtr HKEY_LOCAL_MACHINE = new IntPtr(-2147483646);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
        private static extern int RegOpenKeyExW(
            IntPtr hKey,
            string lpSubKey,
            int ulOptions,
            int samDesired,
            out SafeRegistryHandle phkResult);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
        private static extern int RegQueryValueExW(
            SafeRegistryHandle hKey,
            string lpValueName,
            IntPtr lpReserved,
            out int lpType,
            IntPtr lpData,
            ref int lpcbData);

        [DllImport("ntdll.dll")]
        private static extern int NtDeleteKey(SafeRegistryHandle keyHandle);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode)]
        private static extern int RegRenameKey(
            SafeRegistryHandle hKey,
            string lpSubKeyName,
            string lpNewKeyName);

        public static int OpenKeyForBoundDelete(
            string path,
            out SafeRegistryHandle result,
            out bool isRegistryLink) {
            isRegistryLink = false;
            int status = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                path,
                REG_OPTION_OPEN_LINK,
                DELETE | READ_CONTROL | KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_WOW64_64KEY,
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

        public static int DeleteExactKey(SafeRegistryHandle keyHandle) {
            if (keyHandle == null || keyHandle.IsInvalid || keyHandle.IsClosed)
                throw new ArgumentException("A live exact registry handle is required.", "keyHandle");
            return NtDeleteKey(keyHandle);
        }

        public static int RenameAuthorizationForBoundaryTest(string newLeafName) {
            if (newLeafName == null || !newLeafName.StartsWith(BOUNDARY_PREFIX, StringComparison.Ordinal) ||
                newLeafName.Length != BOUNDARY_PREFIX.Length + 32)
                throw new ArgumentException("Boundary leaf name is noncanonical.", "newLeafName");
            for (int index = BOUNDARY_PREFIX.Length; index < newLeafName.Length; index++) {
                char value = newLeafName[index];
                if (!((value >= '0' && value <= '9') || (value >= 'a' && value <= 'f')))
                    throw new ArgumentException("Boundary leaf name is noncanonical.", "newLeafName");
            }

            SafeRegistryHandle parent = null;
            SafeRegistryHandle leaf = null;
            try {
                bool parentIsLink;
                int status = OpenKeyForBoundaryRename(
                    POLICY_PARENT,
                    KEY_WRITE | DELETE | KEY_QUERY_VALUE | KEY_WOW64_64KEY,
                    out parent,
                    out parentIsLink);
                if (status != 0 || parentIsLink)
                    return status != 0 ? status : ERROR_INVALID_DATA;

                bool leafIsLink;
                status = OpenKeyForBoundaryRename(
                    POLICY_PARENT + @"\" + POLICY_LEAF,
                    KEY_WRITE | DELETE | KEY_QUERY_VALUE | KEY_WOW64_64KEY,
                    out leaf,
                    out leafIsLink);
                if (status != 0 || leafIsLink)
                    return status != 0 ? status : ERROR_INVALID_DATA;

                return RegRenameKey(parent, POLICY_LEAF, newLeafName);
            }
            finally {
                if (leaf != null)
                    leaf.Dispose();
                if (parent != null)
                    parent.Dispose();
            }
        }

        private static int OpenKeyForBoundaryRename(
            string path,
            int desiredAccess,
            out SafeRegistryHandle result,
            out bool isRegistryLink) {
            isRegistryLink = false;
            int status = RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                path,
                REG_OPTION_OPEN_LINK,
                desiredAccess,
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

    }

    // The privilege-state assertions run the real provisioner in this process
    // so they can observe the exact caller token before and after its scope.
    // This helper owns only the outer test fixture state and restores it on
    // Dispose; the provisioner remains responsible for its nested restoration.
    public static class TokenPrivilegeNative {
        private const uint TOKEN_ADJUST_PRIVILEGES = 0x20;
        private const uint TOKEN_QUERY = 0x8;
        private const uint SE_PRIVILEGE_ENABLED = 0x2;
        private const int ERROR_INSUFFICIENT_BUFFER = 122;
        private const int TokenPrivileges = 3;

        [StructLayout(LayoutKind.Sequential)]
        internal struct Luid {
            internal uint LowPart;
            internal int HighPart;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct LuidAndAttributes {
            internal Luid Luid;
            internal uint Attributes;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct TokenPrivilegesOne {
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
        private static extern bool GetTokenInformation(
            IntPtr token,
            int informationClass,
            IntPtr information,
            int informationLength,
            out int returnLength);

        [DllImport("advapi32.dll", EntryPoint = "AdjustTokenPrivileges", SetLastError = true)]
        private static extern bool AdjustTokenPrivilegesAndCapturePrevious(
            IntPtr token,
            bool disableAllPrivileges,
            ref TokenPrivilegesOne newState,
            uint bufferLength,
            out TokenPrivilegesOne previousState,
            out uint returnLength);

        [DllImport("advapi32.dll", EntryPoint = "AdjustTokenPrivileges", SetLastError = true)]
        private static extern bool RestoreTokenPrivileges(
            IntPtr token,
            bool disableAllPrivileges,
            ref TokenPrivilegesOne newState,
            uint bufferLength,
            IntPtr previousState,
            IntPtr returnLength);

        public sealed class PrivilegeStateScope : IDisposable {
            private IntPtr token;
            private TokenPrivilegesOne previousState;

            internal PrivilegeStateScope(IntPtr token, TokenPrivilegesOne previousState) {
                this.token = token;
                this.previousState = previousState;
            }

            public void Dispose() {
                if (token == IntPtr.Zero)
                    return;
                int failure = 0;
                try {
                    if (previousState.PrivilegeCount != 0) {
                        SetLastError(0);
                        if (!RestoreTokenPrivileges(
                            token,
                            false,
                            ref previousState,
                            0,
                            IntPtr.Zero,
                            IntPtr.Zero))
                            failure = Marshal.GetLastWin32Error();
                        else {
                            int error = Marshal.GetLastWin32Error();
                            if (error != 0)
                                failure = error;
                        }
                    }
                }
                finally {
                    IntPtr ownedToken = token;
                    token = IntPtr.Zero;
                    if (!CloseHandle(ownedToken) && failure == 0)
                        failure = Marshal.GetLastWin32Error();
                }
                if (failure != 0)
                    throw new Win32Exception(failure, "Could not restore the test fixture privilege state.");
            }
        }

        public static uint GetRestorePrivilegeAttributes() {
            IntPtr token;
            if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, out token))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                Luid target;
                if (!LookupPrivilegeValueW(null, "SeRestorePrivilege", out target))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                int required;
                SetLastError(0);
                if (GetTokenInformation(token, TokenPrivileges, IntPtr.Zero, 0, out required) ||
                    Marshal.GetLastWin32Error() != ERROR_INSUFFICIENT_BUFFER)
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                IntPtr buffer = Marshal.AllocHGlobal(required);
                try {
                    if (!GetTokenInformation(token, TokenPrivileges, buffer, required, out required))
                        throw new Win32Exception(Marshal.GetLastWin32Error());
                    uint count = unchecked((uint)Marshal.ReadInt32(buffer));
                    int entrySize = Marshal.SizeOf(typeof(LuidAndAttributes));
                    for (uint index = 0; index < count; index++) {
                        IntPtr entryAddress = IntPtr.Add(buffer, 4 + checked((int)index) * entrySize);
                        LuidAndAttributes entry = (LuidAndAttributes)Marshal.PtrToStructure(
                            entryAddress,
                            typeof(LuidAndAttributes));
                        if (entry.Luid.LowPart == target.LowPart && entry.Luid.HighPart == target.HighPart)
                            return entry.Attributes;
                    }
                }
                finally { Marshal.FreeHGlobal(buffer); }
                throw new InvalidOperationException("SeRestorePrivilege is absent from the caller token.");
            }
            finally { CloseHandle(token); }
        }

        public static PrivilegeStateScope SetRestorePrivilegeEnabled(bool enabled) {
            IntPtr token;
            if (!OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, out token))
                throw new Win32Exception(Marshal.GetLastWin32Error());
            try {
                Luid luid;
                if (!LookupPrivilegeValueW(null, "SeRestorePrivilege", out luid))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                TokenPrivilegesOne requested = new TokenPrivilegesOne {
                    PrivilegeCount = 1,
                    Luid = luid,
                    Attributes = enabled ? SE_PRIVILEGE_ENABLED : 0
                };
                TokenPrivilegesOne previous;
                uint returnLength;
                SetLastError(0);
                if (!AdjustTokenPrivilegesAndCapturePrevious(
                    token,
                    false,
                    ref requested,
                    (uint)Marshal.SizeOf(typeof(TokenPrivilegesOne)),
                    out previous,
                    out returnLength))
                    throw new Win32Exception(Marshal.GetLastWin32Error());
                int error = Marshal.GetLastWin32Error();
                PrivilegeStateScope scope = new PrivilegeStateScope(token, previous);
                token = IntPtr.Zero;
                if (error != 0 || returnLength > Marshal.SizeOf(typeof(TokenPrivilegesOne))) {
                    scope.Dispose();
                    throw new Win32Exception(error != 0 ? error : 87);
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

if (-not ('Scribe.GpuBroker.PrivilegeRestoreRetryModel' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;
using System.Threading.Tasks;

namespace Scribe.GpuBroker {
    // This deterministic model exists only in the transport test process. It
    // mirrors the production scope's retain/retry/FailFast state machine
    // without exposing a production environment, CLI, or mutable failure hook.
    public static class PrivilegeRestoreRetryModel {
        private sealed class Scope : IDisposable {
            private bool tokenOwned = true;
            private int previousState = 37;
            private bool restorationComplete;
            private int restoreFailuresRemaining;
            private int closeFailuresRemaining;

            internal Scope(int restoreFailures, int closeFailures) {
                restoreFailuresRemaining = restoreFailures;
                closeFailuresRemaining = closeFailures;
                CurrentState = 91;
            }

            internal bool MarkerIncomplete { get; private set; } = true;
            internal bool TokenOwned { get { return tokenOwned; } }
            internal int PreviousState { get { return previousState; } }
            internal int CurrentState { get; private set; }
            internal int RestoreAttempts { get; private set; }
            internal int CloseAttempts { get; private set; }

            public void Dispose() {
                if (!tokenOwned)
                    return;

                if (!restorationComplete) {
                    RestoreAttempts++;
                    if (restoreFailuresRemaining > 0) {
                        restoreFailuresRemaining--;
                        throw new InvalidOperationException("injected restore failure");
                    }
                    CurrentState = previousState;
                    restorationComplete = true;
                }

                CloseAttempts++;
                if (closeFailuresRemaining > 0) {
                    closeFailuresRemaining--;
                    throw new InvalidOperationException("injected close failure");
                }

                tokenOwned = false;
                previousState = 0;
            }
        }

        private static void Require(bool condition, string message) {
            if (!condition)
                throw new InvalidOperationException(message);
        }

        private static void RestoreOrFailFast(Scope scope, bool emitTerminalEvidence) {
            Exception firstFailure;
            try {
                scope.Dispose();
                return;
            }
            catch (Exception error) {
                firstFailure = error;
            }

            try {
                scope.Dispose();
            }
            catch (Exception retryFailure) {
                Exception terminalFailure = new AggregateException(firstFailure, retryFailure);
                if (emitTerminalEvidence) {
                    Console.Out.Write(
                        "marker=incomplete;restore_attempts=" + scope.RestoreAttempts +
                        ";token_owned=" + scope.TokenOwned.ToString().ToLowerInvariant() +
                        ";previous_state=" + scope.PreviousState);
                    Console.Out.Flush();
                }
                Environment.FailFast(
                    "Injected persistent SeRestorePrivilege restoration failure.",
                    terminalFailure);
            }
        }

        public static Task<string> ReadToEndBoundedAsync(StreamReader reader, int maximumCharacters) {
            if (reader == null)
                throw new ArgumentNullException("reader");
            if (maximumCharacters < 1)
                throw new ArgumentOutOfRangeException("maximumCharacters");
            return Task.Run(() => {
                StringBuilder captured = new StringBuilder(Math.Min(maximumCharacters, 4096));
                char[] buffer = new char[4096];
                int count;
                while ((count = reader.Read(buffer, 0, buffer.Length)) != 0) {
                    int remaining = maximumCharacters - captured.Length;
                    if (remaining > 0)
                        captured.Append(buffer, 0, Math.Min(remaining, count));
                }
                return captured.ToString();
            });
        }

        public static Task<string> ReadLineBoundedAsync(StreamReader reader, int maximumCharacters) {
            if (reader == null)
                throw new ArgumentNullException("reader");
            if (maximumCharacters < 1)
                throw new ArgumentOutOfRangeException("maximumCharacters");
            return Task.Run(() => {
                StringBuilder captured = new StringBuilder(Math.Min(maximumCharacters, 256));
                while (true) {
                    int value = reader.Read();
                    if (value == -1 || value == '\n')
                        return captured.ToString().TrimEnd('\r');
                    if (captured.Length == maximumCharacters)
                        throw new InvalidDataException("Child-process readiness line exceeded its fixed bound.");
                    captured.Append((char)value);
                }
            });
        }

        public static void AssertTransientRecovery() {
            Scope innerFailure = new Scope(1, 0);
            Exception innerOriginal = null;
            Exception innerPropagated = null;
            try {
                try {
                    innerFailure.Dispose();
                }
                catch (Exception error) {
                    innerOriginal = error;
                    throw;
                }
                finally {
                    RestoreOrFailFast(innerFailure, false);
                }
            }
            catch (Exception error) {
                innerPropagated = error;
            }
            Require(Object.ReferenceEquals(innerOriginal, innerPropagated),
                "A successful outer retry replaced the inner restoration exception.");
            Require(innerFailure.MarkerIncomplete,
                "The incomplete marker was removed after a transient inner restoration failure.");
            Require(innerFailure.RestoreAttempts == 2 && innerFailure.CloseAttempts == 1,
                "The transient inner restoration failure did not receive one successful retry.");
            Require(!innerFailure.TokenOwned && innerFailure.PreviousState == 0 && innerFailure.CurrentState == 37,
                "The transient inner restoration retry did not restore exact state and release ownership.");

            Scope preInnerFailure = new Scope(1, 0);
            Exception provisioningOriginal = new InvalidOperationException("original provisioning failure");
            Exception provisioningPropagated = null;
            try {
                try {
                    throw provisioningOriginal;
                }
                finally {
                    RestoreOrFailFast(preInnerFailure, false);
                }
            }
            catch (Exception error) {
                provisioningPropagated = error;
            }
            Require(Object.ReferenceEquals(provisioningOriginal, provisioningPropagated),
                "A successful outer retry replaced the original provisioning exception.");
            Require(preInnerFailure.MarkerIncomplete && preInnerFailure.RestoreAttempts == 2,
                "A failure before inner disposal did not receive a restoration retry.");
            Require(!preInnerFailure.TokenOwned && preInnerFailure.CurrentState == 37,
                "The pre-inner restoration retry did not restore exact state.");

            Scope closeFailure = new Scope(0, 1);
            try {
                closeFailure.Dispose();
                throw new InvalidOperationException("Injected close failure unexpectedly succeeded.");
            }
            catch (InvalidOperationException error) {
                Require(error.Message == "injected close failure", "The close-failure fixture raised the wrong error.");
            }
            Require(closeFailure.TokenOwned && closeFailure.PreviousState == 37,
                "A close failure discarded the retained token or prior state.");
            Require(closeFailure.CurrentState == 37 && closeFailure.RestoreAttempts == 1,
                "The close-failure fixture did not restore exact state before close.");
            RestoreOrFailFast(closeFailure, false);
            Require(!closeFailure.TokenOwned && closeFailure.RestoreAttempts == 1 && closeFailure.CloseAttempts == 2,
                "The close retry repeated restoration or failed to release ownership.");
        }

        public static void RunPersistentFailure() {
            Scope scope = new Scope(Int32.MaxValue, 0);
            try {
                scope.Dispose();
            }
            catch (InvalidOperationException) {
                // This is the inner pre-commit failure. The marker remains and
                // the outer boundary now owns the mandatory retry behavior.
            }
            RestoreOrFailFast(scope, true);
            throw new InvalidOperationException("Environment.FailFast unexpectedly returned.");
        }
    }
}
'@
}

if (-not ('Scribe.GpuBroker.CredentialCommandLine' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Text;

namespace Scribe.GpuBroker {
    // ProcessStartInfo uses this Windows quoting algorithm when ArgumentList is
    // converted for CreateProcessWithLogonW. This test-only renderer is used
    // solely to validate the native UTF-16 command-line bound; its output is
    // never executed.
    public static class CredentialCommandLine {
        public const int CreateProcessWithLogonMaximumUtf16Units = 1024;
        public const int ReservedUtf16Units = 64;
        public const int MaximumUtf16UnitsIncludingNull = 960;

        public static string Render(string fileName, string[] arguments) {
            if (fileName == null)
                throw new ArgumentNullException("fileName");
            if (arguments == null)
                throw new ArgumentNullException("arguments");
            string trimmedFileName = fileName.Trim();
            if (trimmedFileName.Length == 0 ||
                !String.Equals(fileName, trimmedFileName, StringComparison.Ordinal) ||
                fileName.IndexOf('\0') >= 0 ||
                fileName.IndexOf('"') >= 0)
                throw new ArgumentException("Executable path is not canonical for credentialed launch.", "fileName");

            StringBuilder commandLine = new StringBuilder();
            commandLine.Append('"');
            commandLine.Append(fileName);
            commandLine.Append('"');
            foreach (string argument in arguments) {
                if (argument == null)
                    throw new ArgumentException("Credentialed launch arguments cannot contain null.", "arguments");
                if (argument.IndexOf('\0') >= 0)
                    throw new ArgumentException("Credentialed launch arguments cannot contain NUL.", "arguments");
                commandLine.Append(' ');
                AppendArgument(commandLine, argument);
            }
            return commandLine.ToString();
        }

        public static int GetUtf16LengthIncludingNull(string fileName, string[] arguments) {
            return checked(Render(fileName, arguments).Length + 1);
        }

        public static int ValidateLength(string fileName, string[] arguments) {
            int length = GetUtf16LengthIncludingNull(fileName, arguments);
            if (length > MaximumUtf16UnitsIncludingNull)
                throw new InvalidOperationException(
                    "Credentialed command line requires " + length +
                    " UTF-16 units including NUL; limit is " +
                    MaximumUtf16UnitsIncludingNull + ".");
            return length;
        }

        private static void AppendArgument(StringBuilder commandLine, string argument) {
            bool needsQuotes = argument.Length == 0;
            if (!needsQuotes) {
                foreach (char value in argument) {
                    if (Char.IsWhiteSpace(value) || value == '"') {
                        needsQuotes = true;
                        break;
                    }
                }
            }
            if (!needsQuotes) {
                commandLine.Append(argument);
                return;
            }

            commandLine.Append('"');
            int index = 0;
            while (index < argument.Length) {
                int backslashes = 0;
                while (index < argument.Length && argument[index] == '\\') {
                    index++;
                    backslashes++;
                }
                if (index == argument.Length) {
                    commandLine.Append('\\', checked(backslashes * 2));
                    break;
                }
                if (argument[index] == '"') {
                    commandLine.Append('\\', checked(backslashes * 2 + 1));
                    commandLine.Append('"');
                }
                else {
                    commandLine.Append('\\', backslashes);
                    commandLine.Append(argument[index]);
                }
                index++;
            }
            commandLine.Append('"');
        }
    }
}
'@
}

function Invoke-Process {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @(),
        [ValidateRange(1, 3600)][int]$TimeoutSeconds = 300,
        [ValidateRange(1, 1048576)][int]$MaximumCapturedOutputCharacters = 1048576,
        [switch]$AllowFailure
    )
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $repositoryRoot
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    if ($null -eq $process) { throw "Failed to start $FilePath." }
    try {
        # Drain both streams completely to avoid child-process pipe deadlock,
        # while bounding retained output from crash/FailFast diagnostics.
        $stdoutTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadToEndBoundedAsync(
            $process.StandardOutput,
            $MaximumCapturedOutputCharacters
        )
        $stderrTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadToEndBoundedAsync(
            $process.StandardError,
            $MaximumCapturedOutputCharacters
        )
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "$FilePath did not exit within $TimeoutSeconds seconds."
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $result = [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout
            Stderr = $stderr
        }
    }
    finally { $process.Dispose() }
    if (-not $AllowFailure -and $result.ExitCode -ne 0) {
        throw "$FilePath failed with exit $($result.ExitCode): $($result.Stderr)"
    }
    return $result
}

function Invoke-Sc([string[]]$Arguments, [switch]$AllowFailure) {
    return Invoke-Process -FilePath (Join-Path $env:SystemRoot 'System32\sc.exe') -Arguments $Arguments -TimeoutSeconds 30 -AllowFailure:$AllowFailure
}

function Get-BrokerService {
    return Get-Service -Name $serviceName -ErrorAction SilentlyContinue
}

function Test-RestrictedServiceSidType([string]$ScOutput) {
    $sidTypeMatches = [regex]::Matches(
        $ScOutput,
        '(?m)^\s*SERVICE_SID_TYPE\s*:\s*(?<value>\S+)\s*$',
        [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    return $sidTypeMatches.Count -eq 1 -and $sidTypeMatches[0].Groups['value'].Value -ceq 'RESTRICTED'
}

function Assert-OwnedBrokerService([string]$ExpectedPath) {
    $config = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'"
    Assert-True ($null -ne $config) 'Temporary service configuration is unavailable.'
    Assert-True ($config.StartName -ceq 'NT AUTHORITY\LocalService') 'Service account is not LocalService.'
    Assert-True ($config.ServiceType -ceq 'Own Process') 'Service is not configured as an own-process service.'
    Assert-True ([IO.Path]::GetFullPath($config.PathName.Trim('"')) -ceq [IO.Path]::GetFullPath($ExpectedPath)) 'SCM service path no longer matches the protected freshly built binary; refusing destructive cleanup.'
    $queriedSidType = Invoke-Sc -Arguments @('qsidtype', $serviceName)
    Assert-True (Test-RestrictedServiceSidType -ScOutput $queriedSidType.Stdout) 'SCM no longer reports the restricted service SID type; refusing destructive cleanup.'
    return $config
}

function Wait-ServiceAbsent([int]$TimeoutSeconds) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        if ($null -eq (Get-BrokerService)) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Service $serviceName was not deleted within $TimeoutSeconds seconds."
}

if ($RunPersistentPrivilegeRestoreFailFastFixture) {
    [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::RunPersistentFailure()
    throw 'Persistent privilege restoration fixture returned after FailFast.'
}

function Test-PrivilegeRestoreRetryLifecycle {
    [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::AssertTransientRecovery()

    $powerShell = (Get-Process -Id $PID).Path
    $persistent = Invoke-Process -FilePath $powerShell -Arguments @(
        '-NoProfile',
        '-File', $PSCommandPath,
        '-RunPersistentPrivilegeRestoreFailFastFixture'
    ) -TimeoutSeconds 30 -MaximumCapturedOutputCharacters 16384 -AllowFailure
    Assert-True ($persistent.ExitCode -ne 0) 'Persistent privilege restoration failure did not terminate its disposable process.'
    Assert-True ($persistent.Stdout -ceq 'marker=incomplete;restore_attempts=3;token_owned=true;previous_state=37') 'Persistent privilege restoration failure lost the incomplete marker, retry count, token, or captured state before FailFast.'
}

function Wait-ServiceNotRunning([int]$TimeoutSeconds) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        $service = Get-BrokerService
        if ($null -eq $service -or $service.Status -eq [System.ServiceProcess.ServiceControllerStatus]::Stopped) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Service $serviceName remained active after a rejected startup."
}

function Get-PolicySecurityFingerprint([Microsoft.Win32.RegistryKey]$Key) {
    $security = $Key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
    return [Convert]::ToBase64String($security.GetSecurityDescriptorBinaryForm())
}

function New-ExpectedPolicyValues([string]$Sid) {
    return [ordered]@{
        'AuthorizedClientSid' = [pscustomobject]@{ Kind = [Microsoft.Win32.RegistryValueKind]::String; Value = $Sid }
        'SchemaVersion' = [pscustomobject]@{ Kind = [Microsoft.Win32.RegistryValueKind]::DWord; Value = [uint32]1 }
    }
}

function Assert-OwnedPolicyKeyState([Microsoft.Win32.RegistryKey]$Key, [object]$State) {
    Assert-True ($null -ne $State) 'Policy ownership state is unavailable.'
    Assert-True ($Key.SubKeyCount -eq 0) 'Refusing to remove a policy containing subkeys.'
    $actualNames = @($Key.GetValueNames() | Sort-Object -CaseSensitive)
    $expectedNames = @($State.Values.Keys | Sort-Object -CaseSensitive)
    Assert-True ($actualNames.Count -eq $expectedNames.Count) 'Policy value inventory changed; refusing cleanup.'
    for ($index = 0; $index -lt $expectedNames.Count; $index++) {
        Assert-True ($actualNames[$index] -ceq $expectedNames[$index]) 'Policy value inventory changed; refusing cleanup.'
        $name = $expectedNames[$index]
        $expected = $State.Values[$name]
        Assert-True ($Key.GetValueKind($name) -eq $expected.Kind) "Policy value type changed for $name; refusing cleanup."
        $actual = $Key.GetValue($name)
        if ($expected.Kind -eq [Microsoft.Win32.RegistryValueKind]::DWord) {
            Assert-True ([uint32]$actual -eq [uint32]$expected.Value) "Policy value changed for $name; refusing cleanup."
        }
        else {
            Assert-True ([string]$actual -ceq [string]$expected.Value) "Policy value changed for $name; refusing cleanup."
        }
    }
    Assert-True ((Get-PolicySecurityFingerprint -Key $Key) -ceq $State.SecurityFingerprint) 'Policy security descriptor changed; refusing cleanup.'
    if ($State.RequireCanonicalAcl) { Assert-ExactPolicyAclForKey -Key $Key }
}

function Assert-OwnedPolicyState([object]$State) {
    Assert-True ($null -ne $State) 'Policy ownership state is unavailable.'
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $false)
        Assert-True ($null -ne $key) 'Owned authorization policy disappeared; refusing cleanup.'
        try { Assert-OwnedPolicyKeyState -Key $key -State $State }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Remove-RegistryKeyByValidatedHandle([string]$Path, [scriptblock]$Validate, [scriptblock]$BeforeDelete) {
    $handle = $null
    $isRegistryLink = $false
    $openStatus = [Scribe.GpuBroker.RegistryCleanupNative]::OpenKeyForBoundDelete(
        $Path,
        [ref]$handle,
        [ref]$isRegistryLink
    )
    if ($openStatus -ne 0) {
        if ($null -ne $handle) { $handle.Dispose() }
        throw "Could not open exact cleanup target $Path without following registry links (Win32 $openStatus)."
    }
    try {
        Assert-True (-not $isRegistryLink) "Cleanup target $Path is a registry link."
        $key = [Microsoft.Win32.RegistryKey]::FromHandle($handle, [Microsoft.Win32.RegistryView]::Registry64)
        try {
            & $Validate $key
            if ($null -ne $BeforeDelete) { & $BeforeDelete }
            # Validation and deletion are intentionally ordered on the same
            # still-live handle. A same-name replacement cannot redirect this
            # NtDeleteKey call to a different registry object.
            $ntStatus = [Scribe.GpuBroker.RegistryCleanupNative]::DeleteExactKey($handle)
            $ntStatusBits = [int64]$ntStatus -band [uint32]::MaxValue
            Assert-True ($ntStatus -eq 0) ("NtDeleteKey rejected exact cleanup target {0} (NTSTATUS 0x{1:x8})." -f $Path, $ntStatusBits)
        }
        finally { $key.Dispose() }
    }
    finally { $handle.Dispose() }
}

function Remove-OwnedPolicy(
    [scriptblock]$BeforeDelete,
    [scriptblock]$AfterDelete,
    [Exception]$PathReappearedException
) {
    if ($null -eq $ownedPolicyState) { return }
    $state = $ownedPolicyState
    Assert-True ($policyRegistryPath -ceq 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization') 'Policy cleanup target changed.'
    Remove-RegistryKeyByValidatedHandle -Path $policyPath -Validate {
        param([Microsoft.Win32.RegistryKey]$key)
        Assert-OwnedPolicyKeyState -Key $key -State $state
    } -BeforeDelete $BeforeDelete
    # NtDeleteKey succeeded for the validated object. Relinquish ownership
    # before any path observation or test hook can see a same-name replacement.
    $script:ownedPolicyState = $null
    if ($null -ne $AfterDelete) { & $AfterDelete }
    if (Test-Path -LiteralPath $policyRegistryPath) {
        if ($null -ne $PathReappearedException) { throw $PathReappearedException }
        throw 'Owned authorization policy remained after cleanup.'
    }
}

function Remove-OwnedPolicyAncestors([scriptblock]$AfterDelete) {
    if ($ownedPolicyAncestors.Count -eq 0) { return }
    Assert-True (@($ownedPolicyAncestors | Where-Object { $_ -cnotin $policyAncestorPaths }).Count -eq 0) 'Policy ancestor cleanup target changed.'
    $paths = @($ownedPolicyAncestors)
    [array]::Reverse($paths)
    foreach ($path in $paths) {
        Remove-RegistryKeyByValidatedHandle -Path $path -Validate {
            param([Microsoft.Win32.RegistryKey]$key)
            Assert-True ($key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0) "Owned policy ancestor $path is no longer empty; refusing cleanup."
            Assert-ExactPolicyAclForKey -Key $key
        }
        # A later cleanup failure must never retry a key already deleted by
        # its exact handle; the same path may now identify a foreign object.
        $script:ownedPolicyAncestors = @($ownedPolicyAncestors | Where-Object { $_ -cne $path })
        Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_LOCAL_MACHINE\$path")) "Owned policy ancestor $path was replaced during cleanup."
        if ($null -ne $AfterDelete) { & $AfterDelete $path }
    }
}

function New-AncestorReplacementFixture([string]$Path) {
    Assert-True ($Path -cin $policyAncestorPaths) 'Ancestor replacement fixture target is outside the fixed policy chain.'
    Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_LOCAL_MACHINE\$Path")) 'Ancestor replacement fixture target already exists.'
    $token = [guid]::NewGuid().ToString('N')
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.CreateSubKey($Path, $true)
        Assert-True ($null -ne $key) 'Could not create the same-name ancestor replacement fixture.'
        try {
            $key.SetValue('CleanupBoundaryToken', $token, [Microsoft.Win32.RegistryValueKind]::String)
            $key.Flush()
            $fingerprint = Get-PolicySecurityFingerprint -Key $key
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    return [pscustomobject]@{
        Path = $Path
        Token = $token
        SecurityFingerprint = $fingerprint
    }
}

function Assert-AncestorReplacementFixture([Microsoft.Win32.RegistryKey]$Key, [object]$State) {
    Assert-True ($Key.SubKeyCount -eq 0) 'Ancestor replacement fixture unexpectedly contains subkeys.'
    $names = @($Key.GetValueNames())
    Assert-True ($names.Count -eq 1 -and $names[0] -ceq 'CleanupBoundaryToken') 'Ancestor replacement fixture value inventory changed.'
    Assert-True ($Key.GetValueKind('CleanupBoundaryToken') -eq [Microsoft.Win32.RegistryValueKind]::String) 'Ancestor replacement fixture value type changed.'
    Assert-True ([string]$Key.GetValue('CleanupBoundaryToken') -ceq $State.Token) 'Ancestor replacement fixture token changed.'
    Assert-True ((Get-PolicySecurityFingerprint -Key $Key) -ceq $State.SecurityFingerprint) 'Ancestor replacement fixture security descriptor changed.'
}

function Test-PartialAncestorCleanupOwnershipBoundary {
    Assert-True ($null -eq $ownedPolicyState) 'Ancestor cleanup boundary test requires the policy leaf to be absent.'
    Assert-True ($ownedPolicyAncestors.Count -ge 2) 'Ancestor cleanup boundary test requires at least two exact provisioner-owned ancestors.'
    $initialCount = $ownedPolicyAncestors.Count
    $boundaryState = [pscustomobject]@{
        DeletedPath = $null
        Replacement = $null
    }
    $expectedInterruption = [InvalidOperationException]::new("expected-ancestor-cleanup-interruption-$([guid]::NewGuid().ToString('N'))")
    $observedExpectedInterruption = $false
    try {
        try {
            Remove-OwnedPolicyAncestors -AfterDelete {
                param([string]$path)
                if ($null -eq $boundaryState.DeletedPath) {
                    $boundaryState.DeletedPath = $path
                    Assert-True ($path -cnotin $ownedPolicyAncestors) 'Successfully deleted ancestor remained in the ownership inventory during the cleanup hook.'
                    Assert-True ($ownedPolicyAncestors.Count -eq ($initialCount - 1)) 'Ancestor ownership was not reduced immediately after exact deletion.'
                    $boundaryState.Replacement = New-AncestorReplacementFixture -Path $path
                    throw $expectedInterruption
                }
            }
        }
        catch {
            if ([object]::ReferenceEquals($_.Exception, $expectedInterruption)) {
                $observedExpectedInterruption = $true
            }
            else { throw }
        }
        Assert-True $observedExpectedInterruption 'Ancestor cleanup did not stop at the deterministic post-delete test boundary.'
        Assert-True ($null -ne $boundaryState.Replacement -and $null -ne $boundaryState.DeletedPath) 'Ancestor cleanup did not establish its same-name replacement fixture.'
        Assert-True ($boundaryState.DeletedPath -cnotin $ownedPolicyAncestors) 'Deleted ancestor was reacquired after a same-name replacement appeared.'

        $retryRejected = $false
        try { Remove-OwnedPolicyAncestors }
        catch { $retryRejected = $true }
        Assert-True $retryRejected 'Ancestor cleanup crossed a same-name replacement to delete a remaining parent.'
        Assert-True ($boundaryState.DeletedPath -cnotin $ownedPolicyAncestors) 'Cleanup retry targeted a same-name replacement through stale ownership.'
        Assert-True (Test-Path -LiteralPath "Registry::HKEY_LOCAL_MACHINE\$($boundaryState.DeletedPath)") 'Cleanup retry deleted the same-name replacement.'
        $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
        try {
            $key = $base.OpenSubKey($boundaryState.DeletedPath, $false)
            Assert-True ($null -ne $key) 'Same-name ancestor replacement disappeared before validation.'
            try { Assert-AncestorReplacementFixture -Key $key -State $boundaryState.Replacement }
            finally { $key.Dispose() }
        }
        finally { $base.Dispose() }
    }
    finally {
        try {
            if ($null -ne $boundaryState.Replacement -and (Test-Path -LiteralPath "Registry::HKEY_LOCAL_MACHINE\$($boundaryState.Replacement.Path)")) {
                Remove-RegistryKeyByValidatedHandle -Path $boundaryState.Replacement.Path -Validate {
                    param([Microsoft.Win32.RegistryKey]$key)
                    Assert-AncestorReplacementFixture -Key $key -State $boundaryState.Replacement
                }
            }
        }
        finally {
            if ($ownedPolicyAncestors.Count -gt 0) { Remove-OwnedPolicyAncestors }
        }
    }
    Assert-True ($ownedPolicyAncestors.Count -eq 0) 'Ancestor cleanup boundary test retained ownership after recovery.'
    Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_LOCAL_MACHINE\$($boundaryState.DeletedPath)")) 'Ancestor cleanup boundary recovery left its replacement behind.'
}

function New-ProvisioningInvocationNonce {
    $bytes = [byte[]]::new(32)
    $generator = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $generator.GetBytes($bytes) }
    finally { $generator.Dispose() }
    return -join @($bytes | ForEach-Object { $_.ToString('x2', [Globalization.CultureInfo]::InvariantCulture) })
}

function Read-ProvisioningSuccessRecord([object]$Result, [string]$Sid, [string]$InvocationNonce) {
    Assert-True ($Result.ExitCode -eq 0) 'A failed provisioning process cannot produce an owned success record.'
    Assert-True ([string]::IsNullOrWhiteSpace($Result.Stderr)) 'Successful provisioning emitted unexpected stderr.'
    $serialized = $Result.Stdout.Trim()
    Assert-True (-not [string]::IsNullOrWhiteSpace($serialized)) 'Successful provisioning omitted its machine-readable record.'
    Assert-True (-not $serialized.Contains("`n", [StringComparison]::Ordinal) -and -not $serialized.Contains("`r", [StringComparison]::Ordinal)) 'Successful provisioning emitted more than one output record.'
    try { $record = $serialized | ConvertFrom-Json }
    catch { throw 'Successful provisioning emitted malformed JSON.' }
    $expectedProperties = @(
        'schema_version',
        'kind',
        'invocation_nonce',
        'authorized_client_sid',
        'policy_path',
        'created_ancestors'
    )
    $actualProperties = @($record.PSObject.Properties.Name)
    Assert-True ($actualProperties.Count -eq $expectedProperties.Count) 'Provisioning success record has an unexpected property inventory.'
    for ($index = 0; $index -lt $expectedProperties.Count; $index++) {
        Assert-True ($actualProperties[$index] -ceq $expectedProperties[$index]) 'Provisioning success record property spelling or order is noncanonical.'
    }
    Assert-True ([uint32]$record.schema_version -eq 1u) 'Provisioning success record schema is unsupported.'
    Assert-True ([string]$record.kind -ceq 'scribe-windows-gpu-broker-client-policy-provisioning-success-v1') 'Provisioning success record kind is noncanonical.'
    Assert-True ([string]$record.invocation_nonce -ceq $InvocationNonce) 'Provisioning success record does not match this invocation nonce.'
    Assert-True ([string]$record.authorized_client_sid -ceq $Sid) 'Provisioning success record does not match the requested SID.'
    Assert-True ([string]$record.policy_path -ceq $policyPath) 'Provisioning success record names an unexpected policy path.'

    Assert-True ($record.created_ancestors -is [array]) 'Provisioning success record ancestor inventory is not an array.'
    $createdAncestors = @($record.created_ancestors)
    $previousIndex = -1
    foreach ($path in $createdAncestors) {
        Assert-True ($path -is [string]) 'Provisioning success record contains a non-string ancestor path.'
        $pathIndex = [array]::IndexOf([string[]]$policyAncestorPaths, [string]$path)
        Assert-True ($pathIndex -gt $previousIndex) 'Provisioning success record ancestors are duplicated, reordered, or outside the fixed chain.'
        Assert-True ([string]$path -cnotin $ownedPolicyAncestors) 'Provisioning success record claimed an ancestor already owned by an earlier invocation.'
        $previousIndex = $pathIndex
    }
    return [string[]]$createdAncestors
}

function New-ProvisioningSuccessRecordFixture([string]$Sid, [string]$InvocationNonce) {
    return [ordered]@{
        schema_version = 1
        kind = 'scribe-windows-gpu-broker-client-policy-provisioning-success-v1'
        invocation_nonce = $InvocationNonce
        authorized_client_sid = $Sid
        policy_path = $policyPath
        created_ancestors = @($policyAncestorPaths)
    }
}

function ConvertTo-ProvisioningSuccessResult([System.Collections.IDictionary]$Record) {
    return [pscustomobject]@{
        ExitCode = 0
        Stdout = ($Record | ConvertTo-Json -Compress -Depth 3)
        Stderr = ''
    }
}

function Assert-ProvisioningSuccessRecordRejected(
    [string]$Label,
    [object]$Result,
    [string]$Sid,
    [string]$InvocationNonce
) {
    $rejected = $false
    try { [void](Read-ProvisioningSuccessRecord -Result $Result -Sid $Sid -InvocationNonce $InvocationNonce) }
    catch { $rejected = $true }
    Assert-True $rejected "Provisioning success record validation accepted $Label."
}

function Test-ProvisioningSuccessRecordValidation {
    $fixtureSid = 'S-1-5-21-1-2-3-1000'
    $expectedNonce = 'a' * 64
    $valid = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    $acceptedAncestors = @(Read-ProvisioningSuccessRecord `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $valid) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce)
    Assert-True ($acceptedAncestors.Count -eq $policyAncestorPaths.Count) 'Canonical provisioning success fixture was not accepted.'

    $wrongNonce = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce ('b' * 64)
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'a stale or wrong invocation nonce' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $wrongNonce) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $forgedAncestor = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    $forgedAncestor.created_ancestors = @('SOFTWARE\Scribe\GpuPromotionBroker\v1\Foreign')
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'a forged ancestor path' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $forgedAncestor) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $reorderedAncestors = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    $reorderedAncestors.created_ancestors = @($policyAncestorPaths[2], $policyAncestorPaths[1])
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'reordered ancestors' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $reorderedAncestors) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $duplicateAncestors = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    $duplicateAncestors.created_ancestors = @($policyAncestorPaths[0], $policyAncestorPaths[0])
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'duplicate ancestors' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $duplicateAncestors) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $scalarAncestor = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    $scalarAncestor.created_ancestors = $policyAncestorPaths[0]
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'a non-array ancestor inventory' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $scalarAncestor) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $missingProperty = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    [void]$missingProperty.Remove('created_ancestors')
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'an incomplete property inventory' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $missingProperty) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $extraProperty = New-ProvisioningSuccessRecordFixture -Sid $fixtureSid -InvocationNonce $expectedNonce
    $extraProperty.Add('forged_property', 'forbidden')
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'a forged extra property' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $extraProperty) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce

    $reorderedProperties = [ordered]@{
        kind = $valid.kind
        schema_version = $valid.schema_version
        invocation_nonce = $valid.invocation_nonce
        authorized_client_sid = $valid.authorized_client_sid
        policy_path = $valid.policy_path
        created_ancestors = $valid.created_ancestors
    }
    Assert-ProvisioningSuccessRecordRejected `
        -Label 'a reordered property inventory' `
        -Result (ConvertTo-ProvisioningSuccessResult -Record $reorderedProperties) `
        -Sid $fixtureSid `
        -InvocationNonce $expectedNonce
}

function Invoke-ProtectedPolicyProvisioner([string]$Sid, [string]$InvocationNonce) {
    $powerShell = (Get-Process -Id $PID).Path
    return Invoke-Process -FilePath $powerShell -Arguments @(
        '-NoProfile',
        '-File', $provisioner,
        '-AuthorizedClientSid', $Sid,
        '-InvocationNonce', $InvocationNonce
    ) -TimeoutSeconds 30 -AllowFailure
}

function Invoke-ProtectedPolicyProvisionerInCurrentProcess([string]$Sid, [string]$InvocationNonce) {
    try {
        $output = @(& $provisioner -AuthorizedClientSid $Sid -InvocationNonce $InvocationNonce)
        return [pscustomobject]@{
            ExitCode = 0
            Stdout = (($output | ForEach-Object { [string]$_ }) -join [Environment]::NewLine)
            Stderr = ''
        }
    }
    catch {
        return [pscustomobject]@{
            ExitCode = 1
            Stdout = ''
            Stderr = $_.Exception.Message
        }
    }
}

function Assert-RestorePrivilegeState([uint32]$ExpectedAttributes, [string]$Label) {
    $actualAttributes = [Scribe.GpuBroker.TokenPrivilegeNative]::GetRestorePrivilegeAttributes()
    # SE_PRIVILEGE_USED_FOR_ACCESS is an observation bit Windows may set when
    # the enabled privilege is exercised. Every caller-controlled state bit,
    # including SE_PRIVILEGE_ENABLED, must match exactly.
    $callerControlledMask = [uint32]0x7fffffff
    Assert-True (($actualAttributes -band $callerControlledMask) -eq ($ExpectedAttributes -band $callerControlledMask)) $Label
}

function Test-RestorePrivilegeRestoration([bool]$InitiallyEnabled, [string]$Sid) {
    $enabledMask = [uint32]0x2
    $originalAttributes = [Scribe.GpuBroker.TokenPrivilegeNative]::GetRestorePrivilegeAttributes()
    $fixtureScope = $null
    try {
        $fixtureScope = [Scribe.GpuBroker.TokenPrivilegeNative]::SetRestorePrivilegeEnabled($InitiallyEnabled)
        $expectedAttributes = [Scribe.GpuBroker.TokenPrivilegeNative]::GetRestorePrivilegeAttributes()
        Assert-True ((($expectedAttributes -band $enabledMask) -ne 0) -eq $InitiallyEnabled) "Could not establish the requested initial SeRestorePrivilege state ($InitiallyEnabled)."

        $successNonce = New-ProvisioningInvocationNonce
        $success = Invoke-ProtectedPolicyProvisionerInCurrentProcess -Sid $Sid -InvocationNonce $successNonce
        Assert-True ($success.ExitCode -eq 0) "In-process provisioner success fixture failed: $($success.Stderr)"
        Assert-RestorePrivilegeState -ExpectedAttributes $expectedAttributes -Label "Provisioner did not restore SeRestorePrivilege after success from initial enabled state $InitiallyEnabled."
        $createdByInvocation = @(Read-ProvisioningSuccessRecord -Result $success -Sid $Sid -InvocationNonce $successNonce)
        Set-OwnedPolicyState -Values (New-ExpectedPolicyValues -Sid $Sid) -RequireCanonicalAcl $true
        $script:ownedPolicyAncestors = @($ownedPolicyAncestors) + @($createdByInvocation)

        # A pre-existing exact leaf fails only after the provisioner has entered
        # its SeRestorePrivilege scope, exercising the exceptional unwind path.
        $failureNonce = New-ProvisioningInvocationNonce
        $failure = Invoke-ProtectedPolicyProvisionerInCurrentProcess -Sid $Sid -InvocationNonce $failureNonce
        Assert-True ($failure.ExitCode -ne 0) 'In-process provisioner failure fixture unexpectedly succeeded.'
        Assert-True ($failure.Stderr.Contains('pre-existing Windows GPU broker client policy', [StringComparison]::Ordinal)) 'In-process provisioner did not reach the deliberately induced post-privilege failure.'
        Assert-RestorePrivilegeState -ExpectedAttributes $expectedAttributes -Label "Provisioner did not restore SeRestorePrivilege after failure from initial enabled state $InitiallyEnabled."
    }
    finally {
        try {
            if ($null -ne $ownedPolicyState) { Remove-OwnedPolicy }
        }
        finally {
            if ($null -ne $fixtureScope) { $fixtureScope.Dispose() }
            Assert-RestorePrivilegeState -ExpectedAttributes $originalAttributes -Label 'Privilege test fixture did not restore the original SeRestorePrivilege attributes.'
        }
    }
}

function Set-OwnedPolicyState([System.Collections.IDictionary]$Values, [bool]$RequireCanonicalAcl) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $false)
        Assert-True ($null -ne $key) 'Successfully provisioned authorization policy is unavailable.'
        try {
            $state = [pscustomobject]@{
                Values = $Values
                RequireCanonicalAcl = $RequireCanonicalAcl
                SecurityFingerprint = Get-PolicySecurityFingerprint -Key $key
            }
            if ($RequireCanonicalAcl) { Assert-ExactPolicyAclForKey -Key $key }
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    Assert-OwnedPolicyState -State $state
    $script:ownedPolicyState = $state
}

function New-ProtectedPolicy([string]$Sid) {
    $invocationNonce = New-ProvisioningInvocationNonce
    $result = Invoke-ProtectedPolicyProvisioner -Sid $Sid -InvocationNonce $invocationNonce
    if ($result.ExitCode -eq 0) {
        Assert-True ($null -eq $ownedPolicyState) 'Provisioner succeeded while another policy was owned by the harness.'
        $createdByInvocation = @(Read-ProvisioningSuccessRecord -Result $result -Sid $Sid -InvocationNonce $invocationNonce)
        Set-OwnedPolicyState -Values (New-ExpectedPolicyValues -Sid $Sid) -RequireCanonicalAcl $true
        $script:ownedPolicyAncestors = @($ownedPolicyAncestors) + @($createdByInvocation)
    }
    return $result
}

function New-EphemeralPassword {
    $password = [Security.SecureString]::new()
    $random = [byte[]]::new(48)
    try {
        [Security.Cryptography.RandomNumberGenerator]::Fill($random)
        foreach ($character in @([char]'A', [char]'a', [char]'7', [char]'!')) {
            $password.AppendChar($character)
        }
        $alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789!#$%&*+-=?@^_'
        foreach ($value in $random) {
            $password.AppendChar($alphabet[[int]$value % $alphabet.Length])
        }
        $password.MakeReadOnly()
        return $password
    }
    catch {
        $password.Dispose()
        throw
    }
    finally { [Array]::Clear($random, 0, $random.Length) }
}

function New-CryptographicHex([int]$ByteCount) {
    $bytes = [byte[]]::new($ByteCount)
    try {
        [Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
        return [Convert]::ToHexString($bytes).ToLowerInvariant()
    }
    finally { [Array]::Clear($bytes, 0, $bytes.Length) }
}

function Get-ValidatedNoTouchPath([string]$DriveRoot, [string]$Prefix, [string]$Token) {
    Assert-True ($DriveRoot -cmatch '^[A-Z]:\\$') 'No-touch path root is not an exact canonical local drive root.'
    Assert-True ([IO.Path]::GetFullPath($DriveRoot) -ceq $DriveRoot) 'No-touch path root did not round-trip canonically.'
    Assert-True ($Prefix -ceq 'h' -or $Prefix -ceq 'o') 'No-touch path prefix is not fixed.'
    Assert-True ($Token -cmatch '^[0-9a-f]{32}$') 'No-touch path token is not canonical lowercase 128-bit hexadecimal.'
    $leaf = $Prefix + $Token
    Assert-True ($leaf -cmatch "^$Prefix[0-9a-f]{32}$") 'No-touch path leaf is noncanonical.'
    $candidate = $DriveRoot + $leaf
    Assert-True ([IO.Path]::IsPathFullyQualified($candidate)) 'No-touch path is not fully qualified.'
    Assert-True ([IO.Path]::GetFullPath($candidate) -ceq $candidate) 'No-touch path did not round-trip canonically.'
    Assert-True ([IO.Path]::GetPathRoot($candidate) -ceq $DriveRoot) 'No-touch path escaped its exact system-volume root.'
    Assert-True ([IO.Path]::GetDirectoryName($candidate) -ceq $DriveRoot) 'No-touch path is not a direct drive-root child.'
    Assert-True ([IO.Path]::GetFileName($candidate) -ceq $leaf) 'No-touch path leaf changed after canonicalization.'
    return $candidate
}

function Get-ValidatedMachineTargetPath([string]$CommonAppData, [string]$Token) {
    Assert-True (-not [string]::IsNullOrWhiteSpace($CommonAppData)) 'Windows did not provide the machine-wide application-data root.'
    Assert-True ([IO.Path]::IsPathFullyQualified($CommonAppData)) 'Machine-wide application-data root is not fully qualified.'
    $resolvedCommonAppData = [IO.Path]::GetFullPath($CommonAppData).TrimEnd('\')
    Assert-True ($resolvedCommonAppData.Equals($CommonAppData.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) 'Machine-wide application-data root is noncanonical.'
    Assert-True ($Token -cmatch '^[0-9a-f]{32}$') 'SCM test staging token is not canonical lowercase 128-bit hexadecimal.'
    $leaf = 's' + $Token
    Assert-True ($leaf -cmatch '^s[0-9a-f]{32}$') 'SCM test staging leaf is noncanonical.'
    $candidate = [IO.Path]::GetFullPath((Join-Path $resolvedCommonAppData $leaf))
    Assert-True ([IO.Path]::GetDirectoryName($candidate).Equals($resolvedCommonAppData, [StringComparison]::OrdinalIgnoreCase)) 'SCM test staging is not a direct CommonApplicationData child.'
    Assert-True ([IO.Path]::GetFileName($candidate) -ceq $leaf) 'SCM test staging basename changed during canonicalization.'
    return $candidate
}

function Test-FixturePathSetAvailable([bool]$StagingExists, [bool]$HandoffExists, [bool]$OutputExists) {
    return -not ($StagingExists -or $HandoffExists -or $OutputExists)
}

function Select-AvailableFixturePathSet([string]$CommonAppData, [string]$DriveRoot) {
    $maximumAttempts = 8
    for ($attempt = 0; $attempt -lt $maximumAttempts; $attempt++) {
        $candidateToken = New-CryptographicHex -ByteCount 16
        Assert-True ($candidateToken -cmatch '^[0-9a-f]{32}$') 'Credentialed fixture path token is not canonical lowercase 128-bit hexadecimal.'
        $candidateMachineTarget = Get-ValidatedMachineTargetPath -CommonAppData $CommonAppData -Token $candidateToken
        $candidateHandoff = Get-ValidatedNoTouchPath -DriveRoot $DriveRoot -Prefix 'h' -Token $candidateToken
        $candidateOutput = Get-ValidatedNoTouchPath -DriveRoot $DriveRoot -Prefix 'o' -Token $candidateToken
        $stagingExists = Test-Path -LiteralPath $candidateMachineTarget
        $handoffExists = Test-Path -LiteralPath $candidateHandoff
        $outputExists = Test-Path -LiteralPath $candidateOutput
        $candidateSetAvailable = Test-FixturePathSetAvailable -StagingExists $stagingExists -HandoffExists $handoffExists -OutputExists $outputExists
        if (-not $candidateSetAvailable) { continue }
        return [pscustomobject]@{
            Token = $candidateToken
            MachineTarget = $candidateMachineTarget
            Handoff = $candidateHandoff
            Output = $candidateOutput
        }
    }
    throw "Could not select an absent three-path credentialed fixture set after $maximumAttempts attempts."
}

function Test-FixturePathSetAvailabilityContract {
    Assert-True (Test-FixturePathSetAvailable -StagingExists $false -HandoffExists $false -OutputExists $false) 'All-absent fixture path set was rejected.'
    for ($mask = 1; $mask -lt 8; $mask++) {
        $stagingExists = ($mask -band 1) -ne 0
        $handoffExists = ($mask -band 2) -ne 0
        $outputExists = ($mask -band 4) -ne 0
        Assert-True (-not (Test-FixturePathSetAvailable -StagingExists $stagingExists -HandoffExists $handoffExists -OutputExists $outputExists)) 'Fixture path collision classifier accepted an occupied candidate set.'
    }
}

function Assert-NoTouchPathsRemainAbsent([string]$HandoffRoot, [string]$OutputRoot) {
    Assert-True ($HandoffRoot -cne $OutputRoot) 'No-touch handoff and output paths are not distinct.'
    Assert-True (-not (Test-Path -LiteralPath $HandoffRoot)) 'The no-touch handoff path appeared and will be left untouched.'
    Assert-True (-not (Test-Path -LiteralPath $OutputRoot)) 'The no-touch output path appeared and will be left untouched.'
}

function Assert-CanonicalEphemeralSid([string]$Sid, [string]$Name) {
    Assert-True ($Sid -cmatch '^S-1-5-21-(?:[0-9]{1,10}-){3}[0-9]{1,10}$') 'Ephemeral account SID is not a canonical machine-account SID.'
    $sidObject = [Security.Principal.SecurityIdentifier]::new($Sid)
    Assert-True ($sidObject.Value -ceq $Sid) 'Ephemeral account SID does not round-trip canonically.'
    $rid = [uint64]($Sid.Substring($Sid.LastIndexOf('-') + 1))
    Assert-True ($rid -ge 1000) 'Ephemeral account RID is reserved.'
    $translated = $sidObject.Translate([Security.Principal.NTAccount]).Value
    Assert-True ($translated.Equals("$env:COMPUTERNAME\$Name", [StringComparison]::OrdinalIgnoreCase)) 'Ephemeral SID does not resolve to the exact new local machine account.'
    return $sidObject
}

function Get-ExactLocalUserBySid([Security.Principal.SecurityIdentifier]$Sid) {
    return @(Get-LocalUser -SID $Sid -ErrorAction SilentlyContinue)
}

function Assert-OwnedEphemeralAccount([object]$State, [object]$ExpectedEnabled = $null) {
    Assert-True ($null -ne $State) 'Ephemeral account ownership is unavailable.'
    $bySid = @(Get-ExactLocalUserBySid -Sid $State.Sid)
    Assert-True ($bySid.Count -eq 1) 'Owned ephemeral SID no longer resolves to exactly one local account.'
    $account = $bySid[0]
    Assert-True ($account.SID.Value -ceq $State.Sid.Value) 'Owned ephemeral account SID changed.'
    Assert-True ($account.Name -ceq $State.Name) 'Owned ephemeral account name changed.'
    Assert-True ($account.Description -ceq $State.Marker) 'Owned ephemeral account marker changed.'
    Assert-True ($null -ne $account.AccountExpires -and $account.AccountExpires.ToUniversalTime().Ticks -eq $State.AccountExpiresUtcTicks) 'Owned ephemeral account expiry changed.'
    if ($null -ne $ExpectedEnabled) {
        Assert-True ($account.Enabled -eq [bool]$ExpectedEnabled) 'Owned ephemeral account enabled state changed unexpectedly.'
    }

    $byName = @(Get-LocalUser -Name $State.Name -ErrorAction SilentlyContinue)
    Assert-True ($byName.Count -eq 1 -and $byName[0].SID.Value -ceq $State.Sid.Value) 'Owned ephemeral name no longer resolves to the exact SID.'
    return $account
}

function Assert-NoEphemeralProfileRegistration([Security.Principal.SecurityIdentifier]$Sid) {
    $value = $Sid.Value
    Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$value")) 'Credentialed fixture registered a persistent user profile.'
    Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_USERS\$value")) 'Credentialed fixture loaded a user registry hive.'
    Assert-True (-not (Test-Path -LiteralPath "Registry::HKEY_USERS\${value}_Classes")) 'Credentialed fixture loaded a user classes hive.'
}

function New-EphemeralStandardAccount {
    Assert-True ($null -eq $script:ownedEphemeralAccount) 'Ephemeral account ownership already exists.'
    Assert-True ($null -eq $script:ephemeralPassword) 'Ephemeral credential material already exists.'
    $name = $null
    do {
        $name = 'scbgpu' + (New-CryptographicHex -ByteCount 6)
    } while ($null -ne (Get-LocalUser -Name $name -ErrorAction SilentlyContinue))
    $marker = 'ScribeGpu:' + (New-CryptographicHex -ByteCount 16)
    Assert-True ($marker.Length -le 48) 'Ephemeral account ownership marker exceeds the local-account description limit.'
    $expires = [DateTime]::Now.AddHours(1)
    $script:ephemeralPassword = New-EphemeralPassword
    $created = New-LocalUser `
        -Name $name `
        -Password $script:ephemeralPassword `
        -Description $marker `
        -AccountExpires $expires `
        -UserMayNotChangePassword `
        -Disabled
    $script:ownedEphemeralAccount = [pscustomobject]@{
        Name = $name
        Sid = $(if ($null -eq $created) { $null } else { $created.SID })
        Marker = $marker
        AccountExpiresUtcTicks = $(if ($null -eq $created -or $null -eq $created.AccountExpires) { 0 } else { $created.AccountExpires.ToUniversalTime().Ticks })
    }
    Assert-True ($null -ne $script:ownedEphemeralAccount.Sid) 'Windows did not return the created ephemeral account SID.'
    $sid = Assert-CanonicalEphemeralSid -Sid $script:ownedEphemeralAccount.Sid.Value -Name $script:ownedEphemeralAccount.Name
    $verified = Assert-OwnedEphemeralAccount -State $script:ownedEphemeralAccount -ExpectedEnabled $false
    Assert-True ($verified.AccountExpires -gt [DateTime]::Now -and $verified.AccountExpires -le [DateTime]::Now.AddMinutes(65)) 'Ephemeral account expiry is not bounded to the test job.'
    $administrators = @(Get-LocalGroupMember -SID ([Security.Principal.SecurityIdentifier]::new('S-1-5-32-544')))
    Assert-True (-not ($administrators | Where-Object { $_.SID.Value -ceq $sid.Value })) 'Ephemeral account unexpectedly belongs to Administrators.'
    $standardUsersSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-32-545')
    $users = @(Get-LocalGroupMember -SID $standardUsersSid)
    if (@($users | Where-Object { $_.SID.Value -ceq $sid.Value }).Count -eq 0) {
        Add-LocalGroupMember -SID $standardUsersSid -Member $verified
        $users = @(Get-LocalGroupMember -SID $standardUsersSid)
    }
    Assert-True (@($users | Where-Object { $_.SID.Value -ceq $sid.Value }).Count -eq 1) 'Ephemeral account is not an exact member of the standard Users group.'
    Assert-NoEphemeralProfileRegistration -Sid $sid
    return $script:ownedEphemeralAccount
}

function Remove-OwnedEphemeralAccount {
    $state = $script:ownedEphemeralAccount
    if ($null -eq $state) { return }
    Assert-True ($null -eq $script:activeCredentialProcess) 'Refusing account cleanup while its exact credentialed process may still be active.'
    $account = Assert-OwnedEphemeralAccount -State $state
    if ($account.Enabled) { Disable-LocalUser -SID $state.Sid }
    [void](Assert-OwnedEphemeralAccount -State $state -ExpectedEnabled $false)
    Remove-LocalUser -SID $state.Sid
    $deletedSid = $state.Sid
    $deletedName = $state.Name
    $script:ownedEphemeralAccount = $null
    Assert-True (@(Get-ExactLocalUserBySid -Sid $deletedSid).Count -eq 0) 'Deleted ephemeral SID still resolves to a local account.'
    Assert-True (@(Get-LocalUser -Name $deletedName -ErrorAction SilentlyContinue).Count -eq 0) 'Deleted ephemeral account name was replaced during cleanup.'
}

function New-EphemeralProcessStartInfo([string]$FilePath, [string[]]$Arguments) {
    Assert-True ($null -ne $script:ownedEphemeralAccount -and $null -ne $script:ephemeralPassword) 'Ephemeral process launch lacks exact account ownership or credential material.'
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.CreateNoWindow = $true
    $start.WorkingDirectory = [IO.Path]::GetFullPath($machineTarget)
    $start.UserName = $script:ownedEphemeralAccount.Name
    $start.Domain = $env:COMPUTERNAME
    $start.Password = $script:ephemeralPassword
    $start.LoadUserProfile = $false
    $start.Environment.Clear()
    $start.Environment['SystemRoot'] = $env:SystemRoot
    $start.Environment['WINDIR'] = $env:WINDIR
    $start.Environment['COMSPEC'] = Join-Path $env:SystemRoot 'System32\cmd.exe'
    $start.Environment['TEMP'] = $machineTarget
    $start.Environment['TMP'] = $machineTarget
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    return $start
}

function Start-EphemeralProcess([string]$FilePath, [string[]]$Arguments) {
    Assert-True ($null -eq $script:activeCredentialProcess) 'A prior credentialed process is still active.'
    $immutableArguments = if ($null -eq $Arguments) { [string[]]@() } else { [string[]]$Arguments.Clone() }
    $commandLineLength = [Scribe.GpuBroker.CredentialCommandLine]::ValidateLength($FilePath, $immutableArguments)
    Write-Verbose "Credentialed command-line UTF-16 length is $commandLineLength/960 including NUL."
    $start = New-EphemeralProcessStartInfo -FilePath $FilePath -Arguments $immutableArguments
    $process = [Diagnostics.Process]::Start($start)
    if ($null -eq $process) { throw "Failed to start credentialed $FilePath." }
    $script:activeCredentialProcess = $process
    return $process
}

function Test-CredentialCommandLineContract {
    Assert-True ([Scribe.GpuBroker.CredentialCommandLine]::MaximumUtf16UnitsIncludingNull -eq [Scribe.GpuBroker.CredentialCommandLine]::CreateProcessWithLogonMaximumUtf16Units - [Scribe.GpuBroker.CredentialCommandLine]::ReservedUtf16Units) 'Credentialed command-line bound lost its fixed 64-unit reserve below the native 1024-unit ceiling.'
    $fileName = 'C:\x.exe'
    $quote = [string][char]34
    $slash = [string][char]92
    $quotedFileName = $quote + $fileName + $quote
    $surrogatePair = [char]::ConvertFromUtf32(0x1f600)
    Assert-True ($surrogatePair.Length -eq 2) 'Credentialed command-line surrogate-pair fixture is not two UTF-16 units.'
    $nonAsciiWhitespace = [string][char]0x00a0
    $backslashesBeforeQuote = 'a' + ($slash * 2) + $quote + 'b'
    $trailingBackslash = 'a b' + $slash
    $cases = @(
        [pscustomobject]@{ Name = 'empty'; Argument = ''; Expected = $quotedFileName + ' ' + $quote + $quote },
        [pscustomobject]@{ Name = 'simple'; Argument = 'alpha'; Expected = $quotedFileName + ' alpha' },
        [pscustomobject]@{ Name = 'surrogate pair'; Argument = $surrogatePair; Expected = $quotedFileName + ' ' + $surrogatePair },
        [pscustomobject]@{ Name = 'ASCII whitespace'; Argument = 'two words'; Expected = $quotedFileName + ' ' + $quote + 'two words' + $quote },
        [pscustomobject]@{ Name = 'non-ASCII whitespace'; Argument = 'a' + $nonAsciiWhitespace + 'b'; Expected = $quotedFileName + ' ' + $quote + 'a' + $nonAsciiWhitespace + 'b' + $quote },
        [pscustomobject]@{ Name = 'backslashes before quote'; Argument = $backslashesBeforeQuote; Expected = $quotedFileName + ' ' + $quote + 'a' + ($slash * 5) + $quote + 'b' + $quote },
        [pscustomobject]@{ Name = 'quoted trailing backslash'; Argument = $trailingBackslash; Expected = $quotedFileName + ' ' + $quote + 'a b' + ($slash * 2) + $quote }
    )
    foreach ($case in $cases) {
        $rendered = [Scribe.GpuBroker.CredentialCommandLine]::Render($fileName, [string[]]@($case.Argument))
        Assert-True ($rendered -ceq $case.Expected) "Credentialed command-line renderer changed for $($case.Name)."
        Assert-True ([Scribe.GpuBroker.CredentialCommandLine]::GetUtf16LengthIncludingNull($fileName, [string[]]@($case.Argument)) -eq $rendered.Length + 1) "Credentialed command-line UTF-16 count changed for $($case.Name)."
    }
    $multipleRendered = [Scribe.GpuBroker.CredentialCommandLine]::Render($fileName, [string[]]@('alpha', 'two words', ''))
    Assert-True ($multipleRendered -ceq ($quotedFileName + ' alpha ' + $quote + 'two words' + $quote + ' ' + $quote + $quote)) 'Credentialed command-line renderer did not preserve one separator per argument.'

    $nullArgumentArray = [string[]]::new(1)
    foreach ($rejected in @(
        [pscustomobject]@{ Name = 'empty filename'; FileName = ''; Arguments = [string[]]@() },
        [pscustomobject]@{ Name = 'whitespace-only filename'; FileName = ' '; Arguments = [string[]]@() },
        [pscustomobject]@{ Name = 'leading filename whitespace'; FileName = ' C:\x.exe'; Arguments = [string[]]@() },
        [pscustomobject]@{ Name = 'trailing filename whitespace'; FileName = 'C:\x.exe '; Arguments = [string[]]@() },
        [pscustomobject]@{ Name = 'prequoted filename'; FileName = '"C:\x.exe"'; Arguments = [string[]]@() },
        [pscustomobject]@{ Name = 'filename NUL'; FileName = "C:\x`0.exe"; Arguments = [string[]]@() },
        [pscustomobject]@{ Name = 'null argument array'; FileName = $fileName; Arguments = $null },
        [pscustomobject]@{ Name = 'null argument'; FileName = $fileName; Arguments = $nullArgumentArray },
        [pscustomobject]@{ Name = 'argument NUL'; FileName = $fileName; Arguments = [string[]]@("a`0b") }
    )) {
        $failure = $null
        try { [void][Scribe.GpuBroker.CredentialCommandLine]::Render($rejected.FileName, $rejected.Arguments) }
        catch { $failure = $_.Exception }
        Assert-True ($null -ne $failure) "Credentialed command-line renderer accepted invalid $($rejected.Name) input."
    }

    $oneCharacterLength = [Scribe.GpuBroker.CredentialCommandLine]::GetUtf16LengthIncludingNull($fileName, [string[]]@('a'))
    $acceptedWidth = 1 + 960 - $oneCharacterLength
    Assert-True ($acceptedWidth -gt 0) 'Credentialed command-line exact-bound fixture is invalid.'
    $acceptedArgument = [string]::new([char]'a', $acceptedWidth)
    $acceptedLength = [Scribe.GpuBroker.CredentialCommandLine]::ValidateLength($fileName, [string[]]@($acceptedArgument))
    Assert-True ($acceptedLength -eq 960) 'Credentialed command-line preflight rejected or miscounted the exact 960-unit boundary.'
    $rejectedLengthFailure = $null
    try { [void][Scribe.GpuBroker.CredentialCommandLine]::ValidateLength($fileName, [string[]]@([string]::new([char]'a', $acceptedWidth + 1))) }
    catch { $rejectedLengthFailure = $_.Exception }
    Assert-True ($null -ne $rejectedLengthFailure -and $rejectedLengthFailure.Message.Contains('requires 961 UTF-16 units including NUL; limit is 960.', [StringComparison]::Ordinal)) 'Credentialed command-line preflight did not reject the exact 961-unit boundary.'

    $maximumToken = 'f' * 32
    $maximumClient = "C:\ProgramData\s$maximumToken\c.exe"
    $maximumArguments = New-ValidClientArguments -HandoffRoot "C:\h$maximumToken" -OutputRoot "C:\o$maximumToken"
    $maximumShapeLength = [Scribe.GpuBroker.CredentialCommandLine]::ValidateLength($maximumClient, [string[]]$maximumArguments)
    Assert-True ($maximumShapeLength -le 960) 'Fixed maximum credentialed real-client path shapes exceed the reserved command-line budget.'

    Assert-True ($null -eq $script:activeCredentialProcess) 'Credentialed preflight failure fixture started with active process ownership.'
    $currentExecutable = (Get-Process -Id $PID).Path
    $currentOneCharacterLength = [Scribe.GpuBroker.CredentialCommandLine]::GetUtf16LengthIncludingNull($currentExecutable, [string[]]@('a'))
    $preflightRejectedWidth = 1 + 961 - $currentOneCharacterLength
    Assert-True ($preflightRejectedWidth -gt 0) 'Credentialed launch preflight fixture cannot reach its exact failure boundary.'
    $launchFailure = $null
    try { [void](Start-EphemeralProcess -FilePath $currentExecutable -Arguments ([string[]]@([string]::new([char]'a', $preflightRejectedWidth)))) }
    catch { $launchFailure = $_.Exception }
    Assert-True ($null -ne $launchFailure -and $launchFailure.Message.Contains('requires 961 UTF-16 units including NUL; limit is 960.', [StringComparison]::Ordinal)) 'Credentialed launch did not fail at the command-line preflight boundary.'
    Assert-True ($null -eq $script:activeCredentialProcess) 'Credentialed command-line preflight failure started or adopted a process.'
}

function Release-ExitedEphemeralProcess([Diagnostics.Process]$Process) {
    Assert-True ([object]::ReferenceEquals($script:activeCredentialProcess, $Process)) 'Credentialed process ownership changed before release.'
    Assert-True $Process.HasExited 'Refusing to release credentialed process ownership before exit is positively confirmed.'
    $Process.Dispose()
    $script:activeCredentialProcess = $null
}

function Complete-EphemeralProcess(
    [Diagnostics.Process]$Process,
    [ValidateRange(1, 3600)][int]$TimeoutSeconds,
    [ValidateRange(1, 1048576)][int]$MaximumCapturedOutputCharacters,
    [switch]$AllowFailure
) {
    Assert-True ([object]::ReferenceEquals($script:activeCredentialProcess, $Process)) 'Credentialed process ownership changed before completion.'
    try {
        $stdoutTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadToEndBoundedAsync($Process.StandardOutput, $MaximumCapturedOutputCharacters)
        $stderrTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadToEndBoundedAsync($Process.StandardError, $MaximumCapturedOutputCharacters)
        if (-not $Process.WaitForExit($TimeoutSeconds * 1000)) {
            $Process.Kill($true)
            Assert-True ($Process.WaitForExit(10000)) 'Credentialed process termination remained uncertain after kill.'
            throw 'Credentialed process did not exit within its fixed timeout.'
        }
        $result = [pscustomobject]@{
            ExitCode = $Process.ExitCode
            Stdout = $stdoutTask.GetAwaiter().GetResult()
            Stderr = $stderrTask.GetAwaiter().GetResult()
        }
    }
    finally {
        if ([object]::ReferenceEquals($script:activeCredentialProcess, $Process) -and $Process.HasExited) {
            Release-ExitedEphemeralProcess -Process $Process
        }
    }
    if (-not $AllowFailure -and $result.ExitCode -ne 0) {
        throw "Credentialed process failed with exit $($result.ExitCode): $($result.Stderr)"
    }
    return $result
}

function Invoke-EphemeralProcess {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @(),
        [ValidateRange(1, 3600)][int]$TimeoutSeconds = 30,
        [ValidateRange(1, 1048576)][int]$MaximumCapturedOutputCharacters = 65536,
        [switch]$AllowFailure
    )
    $process = Start-EphemeralProcess -FilePath $FilePath -Arguments $Arguments
    return Complete-EphemeralProcess `
        -Process $process `
        -TimeoutSeconds $TimeoutSeconds `
        -MaximumCapturedOutputCharacters $MaximumCapturedOutputCharacters `
        -AllowFailure:$AllowFailure
}

function Test-EphemeralProcessOwnershipBoundary {
    Assert-True ($null -eq $script:activeCredentialProcess) 'Credentialed process ownership fixture started with an active process.'
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Get-Process -Id $PID).Path
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.ArgumentList.Add('-NoProfile')
    $start.ArgumentList.Add('-NonInteractive')
    $start.ArgumentList.Add('-Command')
    $start.ArgumentList.Add('[Threading.Thread]::Sleep(30000)')
    $process = [Diagnostics.Process]::Start($start)
    if ($null -eq $process) { throw 'Could not start the credential-process ownership fixture.' }
    $script:activeCredentialProcess = $process
    try {
        $releaseFailure = $null
        try { Release-ExitedEphemeralProcess -Process $process }
        catch { $releaseFailure = $_.Exception }
        Assert-True ($null -ne $releaseFailure -and $releaseFailure.Message -ceq 'Refusing to release credentialed process ownership before exit is positively confirmed.') 'Live credential-process ownership release did not fail closed.'
        Assert-True ([object]::ReferenceEquals($script:activeCredentialProcess, $process)) 'Failed credential-process release discarded exact ownership.'
        $process.Kill($true)
        Assert-True ($process.WaitForExit(5000)) 'Credential-process ownership fixture did not exit after kill.'
        Release-ExitedEphemeralProcess -Process $process
        Assert-True ($null -eq $script:activeCredentialProcess) 'Confirmed credential-process exit retained stale ownership.'
    }
    finally {
        if ([object]::ReferenceEquals($script:activeCredentialProcess, $process)) {
            if (-not $process.HasExited) {
                $process.Kill($true)
                Assert-True ($process.WaitForExit(5000)) 'Credential-process ownership fixture cleanup could not confirm exit.'
            }
            Release-ExitedEphemeralProcess -Process $process
        }
    }
}

function Test-StandardSoftwareCreatorOwnerInheritanceTemplateContract {
    $parseTokens = $null
    $parseErrors = $null
    $provisionerAst = [Management.Automation.Language.Parser]::ParseFile(
        $provisioner,
        [ref]$parseTokens,
        [ref]$parseErrors
    )
    Assert-True ($parseErrors.Count -eq 0) 'Client policy provisioner could not be parsed for its exact CREATOR OWNER predicate test.'
    $rawClassifierNames = @(
        'Test-StandardSoftwareCreatorOwnerInheritanceTemplate',
        'Test-SafePolicyAncestorAcl'
    )
    $rawClassifierDefinitions = @($provisionerAst.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -cin $rawClassifierNames
    }, $true))
    Assert-True ($rawClassifierDefinitions.Count -eq 2) 'Client policy provisioner must contain exactly one definition of each raw ancestor-DACL classifier function.'
    foreach ($classifierName in $rawClassifierNames) {
        Assert-True (@($rawClassifierDefinitions | Where-Object { $_.Name -ceq $classifierName }).Count -eq 1) "Client policy provisioner does not contain exactly one $classifierName definition."
    }
    $predicateSource = ($rawClassifierDefinitions | Where-Object { $_.Name -ceq $rawClassifierNames[0] }).Extent.Text
    $aclClassifierSource = ($rawClassifierDefinitions | Where-Object { $_.Name -ceq $rawClassifierNames[1] }).Extent.Text

    & {
        param([string]$ExactPredicateSource, [string]$ExactAclClassifierSource)
        . ([scriptblock]::Create($ExactPredicateSource))
        . ([scriptblock]::Create($ExactAclClassifierSource))

        $creatorOwner = [Security.Principal.SecurityIdentifier]::new('S-1-3-0')
        $authenticatedUsers = [Security.Principal.SecurityIdentifier]::new('S-1-5-11')
        $accountSid = [Security.Principal.SecurityIdentifier]::new('S-1-5-21-1-2-3-1000')
        $system = [Security.Principal.SecurityIdentifier]::new('S-1-5-18')
        $fullControl = [int][Security.AccessControl.RegistryRights]::FullControl
        $readKey = [int][Security.AccessControl.RegistryRights]::ReadKey
        $containerInherit = [Security.AccessControl.AceFlags]::ContainerInherit
        $containerAndObjectInherit = [Security.AccessControl.AceFlags](
            [uint32][Security.AccessControl.AceFlags]::ContainerInherit -bor
            [uint32][Security.AccessControl.AceFlags]::ObjectInherit
        )
        $containerAndInherited = [Security.AccessControl.AceFlags](
            [uint32][Security.AccessControl.AceFlags]::ContainerInherit -bor
            [uint32][Security.AccessControl.AceFlags]::Inherited
        )
        $allow = [Security.AccessControl.AceQualifier]::AccessAllowed
        $deny = [Security.AccessControl.AceQualifier]::AccessDenied
        Assert-True ([uint32]$fullControl -eq 0x000f003f) 'RegistryRights.FullControl no longer maps to the pinned registry access mask.'

        function New-CommonAce(
            [Security.Principal.SecurityIdentifier]$Sid = $creatorOwner,
            [int]$AccessMask = $fullControl,
            [Security.AccessControl.AceFlags]$Flags = $containerInherit,
            [Security.AccessControl.AceQualifier]$Qualifier = $allow,
            [bool]$IsCallback = $false,
            [byte[]]$Opaque = $null
        ) {
            return [Security.AccessControl.CommonAce]::new($Flags, $Qualifier, $AccessMask, $Sid, $IsCallback, $Opaque)
        }

        function New-ObjectAce(
            [Security.Principal.SecurityIdentifier]$Sid,
            [int]$AccessMask,
            [Security.AccessControl.AceFlags]$Flags,
            [Security.AccessControl.AceQualifier]$Qualifier = $allow,
            [bool]$IsCallback = $false,
            [byte[]]$Opaque = $null
        ) {
            return [Security.AccessControl.ObjectAce]::new(
                $Flags,
                $Qualifier,
                $AccessMask,
                $Sid,
                [Security.AccessControl.ObjectAceFlags]::None,
                [Guid]::Empty,
                [Guid]::Empty,
                $IsCallback,
                $Opaque
            )
        }

        function New-RawAcl([Security.AccessControl.GenericAce[]]$Aces) {
            $acl = [Security.AccessControl.RawAcl]::new(2, $Aces.Count)
            for ($index = 0; $index -lt $Aces.Count; $index++) { $acl.InsertAce($index, $Aces[$index]) }
            Write-Output -NoEnumerate $acl
        }

        function Test-RawAcl([Security.AccessControl.GenericAce[]]$Aces, [string]$Path = 'SOFTWARE') {
            return Test-SafePolicyAncestorAcl `
                -Acl (New-RawAcl -Aces $Aces) `
                -Path $Path `
                -TrustedOwners @('S-1-5-18', 'S-1-5-32-544', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464') `
                -MutationMask ([uint32]0x500d0026)
        }

        $canonical = New-CommonAce
        Assert-True ($canonical.AceType -eq [Security.AccessControl.AceType]::AccessAllowed -and -not $canonical.IsCallback) 'Canonical CREATOR OWNER fixture is not an explicit non-callback AccessAllowed CommonAce.'
        Assert-True (Test-StandardSoftwareCreatorOwnerInheritanceTemplate -Ace $canonical -Path 'SOFTWARE') 'Exact raw HKLM\SOFTWARE CREATOR OWNER inheritance template was rejected.'
        Assert-True (Test-RawAcl -Aces @($canonical)) 'Exact raw HKLM\SOFTWARE CREATOR OWNER DACL fixture was rejected.'

        $predicateRejections = @(
            [pscustomobject]@{ Label = 'descendant path'; Path = 'SOFTWARE\Scribe'; Ace = $canonical },
            [pscustomobject]@{ Label = 'case-variant path'; Path = 'software'; Ace = $canonical },
            [pscustomobject]@{ Label = 'trailing-separator path'; Path = 'SOFTWARE\'; Ace = $canonical },
            [pscustomobject]@{ Label = 'Authenticated Users SID'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Sid $authenticatedUsers) },
            [pscustomobject]@{ Label = 'account SID'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Sid $accountSid) },
            [pscustomobject]@{ Label = 'deny ACE'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Qualifier $deny) },
            [pscustomobject]@{ Label = 'callback ACE'; Path = 'SOFTWARE'; Ace = (New-CommonAce -IsCallback $true -Opaque ([byte[]](1, 2, 3, 4))) },
            [pscustomobject]@{ Label = 'inherited ACE'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Flags $containerAndInherited) },
            [pscustomobject]@{ Label = 'reduced rights'; Path = 'SOFTWARE'; Ace = (New-CommonAce -AccessMask $readKey) },
            [pscustomobject]@{ Label = 'altered rights'; Path = 'SOFTWARE'; Ace = (New-CommonAce -AccessMask ([int]([uint32]$fullControl -bxor 1u))) },
            [pscustomobject]@{ Label = 'no inheritance'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Flags ([Security.AccessControl.AceFlags]::None)) },
            [pscustomobject]@{ Label = 'extra inheritance'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Flags $containerAndObjectInherit) },
            [pscustomobject]@{ Label = 'inherit-only propagation'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Flags ([Security.AccessControl.AceFlags]([uint32]$containerInherit -bor [uint32][Security.AccessControl.AceFlags]::InheritOnly))) },
            [pscustomobject]@{ Label = 'no-propagate inheritance'; Path = 'SOFTWARE'; Ace = (New-CommonAce -Flags ([Security.AccessControl.AceFlags]([uint32]$containerInherit -bor [uint32][Security.AccessControl.AceFlags]::NoPropagateInherit))) },
            [pscustomobject]@{ Label = 'object ACE'; Path = 'SOFTWARE'; Ace = (New-ObjectAce -Sid $creatorOwner -AccessMask $fullControl -Flags $containerInherit) }
        )
        foreach ($rejection in $predicateRejections) {
            Assert-True (-not (Test-StandardSoftwareCreatorOwnerInheritanceTemplate -Ace $rejection.Ace -Path $rejection.Path)) "Raw CREATOR OWNER predicate accepted its $($rejection.Label) rejection fixture."
        }

        $callbackFull = New-CommonAce -IsCallback $true -Opaque ([byte[]](1, 2, 3, 4))
        $inheritedCallbackFull = New-CommonAce -Flags $containerAndInherited -IsCallback $true -Opaque ([byte[]](1, 2, 3, 4))
        $objectFull = New-ObjectAce -Sid $authenticatedUsers -AccessMask $fullControl -Flags ([Security.AccessControl.AceFlags]::None)
        $inheritedObjectFull = New-ObjectAce -Sid $authenticatedUsers -AccessMask $fullControl -Flags ([Security.AccessControl.AceFlags]::Inherited)
        $unknownBytes = [byte[]](0x14, 0x00, 0x08, 0x00, 1, 2, 3, 4)
        $unknownAce = [Security.AccessControl.GenericAce]::CreateFromBinaryForm($unknownBytes, 0)
        Assert-True ($unknownAce -is [Security.AccessControl.CustomAce]) 'Unknown raw-ACE fixture did not deserialize as CustomAce.'
        $alteredMaskAce = New-CommonAce -AccessMask ([int]([uint32]$fullControl -bxor 1u))
        $unsafeAcls = @(
            [pscustomobject]@{ Label = 'duplicate canonical template'; Aces = @($canonical, (New-CommonAce)); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'callback template with opaque bytes'; Aces = @($callbackFull); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'untrusted full-control object ACE'; Aces = @($objectFull); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'inherited full-control object ACE'; Aces = @($inheritedObjectFull); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'inherited full-control callback ACE'; Aces = @($inheritedCallbackFull); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'untrusted full-control CommonAce'; Aces = @((New-CommonAce -Sid $authenticatedUsers -Flags ([Security.AccessControl.AceFlags]::None))); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'inherited CREATOR OWNER CommonAce'; Aces = @((New-CommonAce -Flags $containerAndInherited)); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'altered CREATOR OWNER mask'; Aces = @($alteredMaskAce); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'altered CREATOR OWNER flags'; Aces = @((New-CommonAce -Flags $containerAndObjectInherit)); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'CREATOR OWNER descendant'; Aces = @($canonical); Path = 'SOFTWARE\Scribe' },
            [pscustomobject]@{ Label = 'mutating audit ACE'; Aces = @((New-CommonAce -Sid $authenticatedUsers -Flags ([Security.AccessControl.AceFlags]::SuccessfulAccess) -Qualifier ([Security.AccessControl.AceQualifier]::SystemAudit))); Path = 'SOFTWARE' },
            [pscustomobject]@{ Label = 'unknown custom ACE'; Aces = @($unknownAce); Path = 'SOFTWARE' }
        )
        foreach ($unsafeAcl in $unsafeAcls) {
            Assert-True (-not (Test-RawAcl -Aces $unsafeAcl.Aces -Path $unsafeAcl.Path)) "Raw ancestor-DACL classifier accepted its $($unsafeAcl.Label) rejection fixture."
        }

        $safeAcls = @(
            [pscustomobject]@{ Label = 'trusted non-callback CommonAce mutation'; Aces = @((New-CommonAce -Sid $system -Flags ([Security.AccessControl.AceFlags]::None))) },
            [pscustomobject]@{ Label = 'trusted callback mutation'; Aces = @((New-CommonAce -Sid $system -Flags ([Security.AccessControl.AceFlags]::None) -IsCallback $true -Opaque ([byte[]](1, 2, 3, 4)))) },
            [pscustomobject]@{ Label = 'trusted object mutation'; Aces = @((New-ObjectAce -Sid $system -AccessMask $fullControl -Flags ([Security.AccessControl.AceFlags]::None))) },
            [pscustomobject]@{ Label = 'untrusted CommonAce read-only access'; Aces = @((New-CommonAce -Sid $authenticatedUsers -AccessMask $readKey -Flags ([Security.AccessControl.AceFlags]::None))) },
            [pscustomobject]@{ Label = 'untrusted callback read-only access'; Aces = @((New-CommonAce -Sid $authenticatedUsers -AccessMask $readKey -Flags ([Security.AccessControl.AceFlags]::None) -IsCallback $true -Opaque ([byte[]](1, 2, 3, 4)))) },
            [pscustomobject]@{ Label = 'untrusted object read-only access'; Aces = @((New-ObjectAce -Sid $authenticatedUsers -AccessMask $readKey -Flags ([Security.AccessControl.AceFlags]::None))) },
            [pscustomobject]@{ Label = 'untrusted CommonAce deny'; Aces = @((New-CommonAce -Sid $authenticatedUsers -Flags ([Security.AccessControl.AceFlags]::None) -Qualifier $deny)) },
            [pscustomobject]@{ Label = 'untrusted object deny'; Aces = @((New-ObjectAce -Sid $authenticatedUsers -AccessMask $fullControl -Flags ([Security.AccessControl.AceFlags]::None) -Qualifier $deny)) }
        )
        foreach ($safeAcl in $safeAcls) {
            Assert-True (Test-RawAcl -Aces $safeAcl.Aces) "Raw ancestor-DACL classifier rejected its safe $($safeAcl.Label) fixture."
        }
    } $predicateSource $aclClassifierSource
}

function New-WeakPolicy([string]$Sid) {
    $result = New-ProtectedPolicy -Sid $Sid
    Assert-True ($result.ExitCode -eq 0) "Could not provision the weak-DACL fixture: $($result.Stderr)"
    Assert-OwnedPolicyState -State $ownedPolicyState
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Owned authorization policy disappeared before weak-DACL setup.'
        try {
            $security = $key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
            $broadRead = [Security.AccessControl.RegistryAccessRule]::new(
                [Security.Principal.SecurityIdentifier]::new('S-1-5-11'),
                [Security.AccessControl.RegistryRights]::ReadKey,
                [Security.AccessControl.InheritanceFlags]::None,
                [Security.AccessControl.PropagationFlags]::None,
                [Security.AccessControl.AccessControlType]::Allow
            )
            [void]$security.AddAccessRule($broadRead)
            $key.SetAccessControl($security)
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    Set-OwnedPolicyState -Values (New-ExpectedPolicyValues -Sid $Sid) -RequireCanonicalAcl $false
}

function Set-PolicyValue([string]$Name, [object]$Value, [Microsoft.Win32.RegistryValueKind]$Kind) {
    Assert-True ($null -ne $ownedPolicyState) 'Refusing to mutate a policy not created by this harness.'
    Assert-OwnedPolicyState -State $ownedPolicyState
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        if ($null -eq $key) { throw 'Owned authorization policy disappeared.' }
        try { $key.SetValue($Name, $Value, $Kind); $key.Flush() }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    $values = [ordered]@{}
    foreach ($existingName in $ownedPolicyState.Values.Keys) { $values[$existingName] = $ownedPolicyState.Values[$existingName] }
    $values[$Name] = [pscustomobject]@{ Kind = $Kind; Value = $Value }
    Set-OwnedPolicyState -Values $values -RequireCanonicalAcl $ownedPolicyState.RequireCanonicalAcl
}

function Replace-PolicyValueSpelling(
    [string]$ExistingName,
    [string]$ReplacementName,
    [object]$Value,
    [Microsoft.Win32.RegistryValueKind]$Kind
) {
    Assert-True ($ExistingName -cne $ReplacementName -and $ExistingName.Equals($ReplacementName, [StringComparison]::OrdinalIgnoreCase)) 'Registry spelling fixture must change case only.'
    Assert-True ($null -ne $ownedPolicyState) 'Refusing to mutate a policy not created by this harness.'
    Assert-OwnedPolicyState -State $ownedPolicyState
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Owned authorization policy disappeared before spelling mutation.'
        try {
            $key.DeleteValue($ExistingName, $true)
            $key.SetValue($ReplacementName, $Value, $Kind)
            $key.Flush()
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    $values = [ordered]@{}
    foreach ($existing in $ownedPolicyState.Values.Keys) {
        if ([string]$existing -cne $ExistingName) { $values[[string]$existing] = $ownedPolicyState.Values[$existing] }
    }
    $values[$ReplacementName] = [pscustomobject]@{ Kind = $Kind; Value = $Value }
    Set-OwnedPolicyState -Values $values -RequireCanonicalAcl $ownedPolicyState.RequireCanonicalAcl
}

function Assert-ExactPolicyAclForKey([Microsoft.Win32.RegistryKey]$Key) {
    $acl = $Key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
    Assert-True $acl.AreAccessRulesProtected 'Authorization policy DACL is not protected.'
    Assert-True ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ceq 'S-1-5-18') 'Authorization policy owner is not SYSTEM.'
    $rules = @($acl.GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]))
    Assert-True ($rules.Count -eq 3) 'Authorization policy does not have exactly three ACEs.'
    $expected = @{
        'S-1-5-18' = [uint32][Security.AccessControl.RegistryRights]::FullControl
        'S-1-5-32-544' = [uint32][Security.AccessControl.RegistryRights]::FullControl
        $serviceSid = [uint32][Security.AccessControl.RegistryRights]::ReadKey
    }
    foreach ($rule in $rules) {
        Assert-True ($expected.ContainsKey($rule.IdentityReference.Value)) 'Authorization policy contains an unexpected SID.'
        Assert-True (-not $rule.IsInherited -and $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) 'Authorization policy contains inherited or deny access.'
        Assert-True ([uint32]$rule.RegistryRights -eq $expected[$rule.IdentityReference.Value]) 'Authorization policy ACE mask is noncanonical.'
        [void]$expected.Remove($rule.IdentityReference.Value)
    }
    Assert-True ($expected.Count -eq 0) 'Authorization policy is missing a required SID.'
}

function Assert-ExactPolicyAcl {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $false)
        Assert-True ($null -ne $key) 'Authorization policy is unavailable.'
        try { Assert-ExactPolicyAclForKey -Key $key }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Assert-RejectedServiceStartup([string]$Label, [string]$Client, [string[]]$ClientArguments) {
    [void](Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure)
    Wait-ServiceNotRunning -TimeoutSeconds 10
    $probe = Invoke-Process -FilePath $Client -Arguments $ClientArguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($probe.ExitCode -eq 78) "$Label did not leave the fixed pipe unavailable."
    Assert-True ($probe.Stderr.Contains('broker is unavailable', [StringComparison]::Ordinal)) "$Label exposed a pipe after rejected startup."
}

function Get-SanitizedClientDiagnosticCategory([AllowNull()][string]$StandardError) {
    $normalized = if ($null -eq $StandardError) { '' } else { $StandardError.Trim() }
    if ($normalized.Equals('Protected Windows GPU promotion broker authenticated; production authority is not provisioned, and no filesystem, ledger, or signing authority was accessed.', [StringComparison]::Ordinal)) {
        return 'authenticated_not_provisioned'
    }
    if ($normalized.Equals('Protected Windows GPU promotion broker is unavailable and production authority is not provisioned; no filesystem, ledger, or signing authority was accessed.', [StringComparison]::Ordinal)) {
        return 'unavailable'
    }
    if ($normalized.Equals('Protected Windows GPU promotion intent was rejected.', [StringComparison]::Ordinal)) {
        return 'intent_rejected'
    }
    if ($normalized.Equals('Protected Windows GPU promotion transport was rejected.', [StringComparison]::Ordinal)) {
        return 'transport_rejected'
    }
    if ($normalized.Equals('Protected Windows GPU promotion client initialization failed.', [StringComparison]::Ordinal)) {
        return 'client_initialization_failed'
    }
    return 'unexpected'
}

function Get-SanitizedClientFailureDiagnostic(
    [int]$ExitCode,
    [AllowNull()][string]$StandardOutput,
    [AllowNull()][string]$StandardError,
    [AllowNull()][string]$ServiceStatus,
    [AllowNull()][string]$ServerAccessProbe
) {
    $allowedServiceStatuses = @(
        'Stopped',
        'StartPending',
        'StopPending',
        'Running',
        'ContinuePending',
        'PausePending',
        'Paused',
        'absent',
        'query_failed'
    )
    $sanitizedServiceStatus = if ($allowedServiceStatuses -ccontains $ServiceStatus) { $ServiceStatus } else { 'unknown' }
    $stderrCategory = Get-SanitizedClientDiagnosticCategory -StandardError $StandardError
    $stdoutLength = if ($null -eq $StandardOutput) { 0 } else { $StandardOutput.Length }
    $stderrLength = if ($null -eq $StandardError) { 0 } else { $StandardError.Length }
    $sanitizedServerAccess = if (Test-ServerAccessProbeRecord -Record $ServerAccessProbe) { $ServerAccessProbe } else { 'invalid' }
    return "Authenticated service response did not map to NotProvisioned. exit_code=$ExitCode; stderr_category=$stderrCategory; stdout_utf16_length=$stdoutLength; stderr_utf16_length=$stderrLength; broker_service_status=$sanitizedServiceStatus; server_access_probe=$sanitizedServerAccess"
}

function Test-ServerAccessProbeContract {
    $valid = @(
        'ephemeral-server-access;session=zero:0;process=ok:0;token=ok:0',
        'ephemeral-server-access;session=nonzero:0;process=error:5;token=not_attempted:0',
        'ephemeral-server-access;session=error:5;process=ok:0;token=error:5'
    )
    foreach ($record in $valid) {
        Assert-True (Test-ServerAccessProbeRecord -Record $record) 'Server-access probe parser rejected a canonical record.'
    }
    foreach ($record in @(
        $null,
        '',
        'ephemeral-server-access;session=zero:00;process=ok:0;token=ok:0',
        'ephemeral-server-access;session=error:0;process=ok:0;token=ok:0',
        'ephemeral-server-access;session=zero:0;process=error:5;token=ok:0',
        'ephemeral-server-access;session=zero:0;process=ok:0;token=not_attempted:0',
        'ephemeral-server-access;session=zero:0;process=ok:0;token=error:4294967296',
        "ephemeral-server-access;session=zero:0;process=ok:0;token=ok:0`n",
        "ephemeral-server-access;session=zero:0;process=ok:0;token=ok:0`r`nraw-sensitive-diagnostic-sentinel"
    )) {
        Assert-True (-not (Test-ServerAccessProbeRecord -Record $record)) 'Server-access probe parser accepted a noncanonical record.'
    }

    $currentProcessRecord = [Scribe.GpuBroker.ServerAccessProbeNative]::Probe([uint32][Environment]::ProcessId)
    Assert-True (Test-ServerAccessProbeRecord -Record $currentProcessRecord) 'Native server-access self-probe returned a noncanonical record.'
    Assert-True ($currentProcessRecord -cmatch ';process=ok:0;token=ok:0$') 'Native server-access self-probe could not query its own process and token.'
}

function Test-SanitizedClientDiagnosticCategoryContract {
    $fixedDiagnostics = [ordered]@{
        'Protected Windows GPU promotion broker authenticated; production authority is not provisioned, and no filesystem, ledger, or signing authority was accessed.' = 'authenticated_not_provisioned'
        'Protected Windows GPU promotion broker is unavailable and production authority is not provisioned; no filesystem, ledger, or signing authority was accessed.' = 'unavailable'
        'Protected Windows GPU promotion intent was rejected.' = 'intent_rejected'
        'Protected Windows GPU promotion transport was rejected.' = 'transport_rejected'
        'Protected Windows GPU promotion client initialization failed.' = 'client_initialization_failed'
    }
    $clientMainSource = Get-Content -LiteralPath (Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\src\main.rs') -Raw
    foreach ($entry in $fixedDiagnostics.GetEnumerator()) {
        Assert-True ([regex]::Matches($clientMainSource, [regex]::Escape($entry.Key)).Count -eq 1) 'Sanitized diagnostic classifier is not bound to exactly one production client diagnostic.'
        $exactCategory = Get-SanitizedClientDiagnosticCategory -StandardError $entry.Key
        Assert-True ($exactCategory -ceq $entry.Value) 'Sanitized diagnostic classifier rejected an exact fixed production diagnostic.'
        $trimmedCategory = Get-SanitizedClientDiagnosticCategory -StandardError (" `r`n" + $entry.Key + "`t ")
        Assert-True ($trimmedCategory -ceq $entry.Value) 'Sanitized diagnostic classifier changed its edge-whitespace trimming policy.'
        Assert-True (-not $exactCategory.Contains('Protected Windows GPU promotion', [StringComparison]::Ordinal)) 'Sanitized diagnostic category retained raw production diagnostic text.'
    }

    $authenticatedDiagnostic = [string]$fixedDiagnostics.Keys[0]
    $unavailableDiagnostic = [string]$fixedDiagnostics.Keys[1]
    $unexpectedCases = @(
        [pscustomobject]@{ Name = 'null'; Value = $null },
        [pscustomobject]@{ Name = 'empty'; Value = '' },
        [pscustomobject]@{ Name = 'whitespace'; Value = " `r`n`t" },
        [pscustomobject]@{ Name = 'near match'; Value = $authenticatedDiagnostic + 'x' },
        [pscustomobject]@{ Name = 'case change'; Value = $authenticatedDiagnostic.ToUpperInvariant() },
        [pscustomobject]@{ Name = 'concatenated diagnostics'; Value = $authenticatedDiagnostic + $unavailableDiagnostic },
        [pscustomobject]@{ Name = 'multiline content'; Value = $authenticatedDiagnostic + "`r`nuntrusted-extra-line" },
        [pscustomobject]@{ Name = 'unknown raw sentinel'; Value = 'raw-sensitive-diagnostic-sentinel' }
    )
    foreach ($case in $unexpectedCases) {
        $category = Get-SanitizedClientDiagnosticCategory -StandardError $case.Value
        Assert-True ($category -ceq 'unexpected') "Sanitized diagnostic classifier accepted $($case.Name)."
        Assert-True (-not $category.Contains('raw-sensitive-diagnostic-sentinel', [StringComparison]::Ordinal)) 'Unexpected diagnostic category retained raw text.'
    }

    $rawStdout = 'stdout-sensitive-sentinel'
    $rawStderr = 'raw-sensitive-diagnostic-sentinel'
    $failureDiagnostic = Get-SanitizedClientFailureDiagnostic `
        -ExitCode 74 `
        -StandardOutput $rawStdout `
        -StandardError $rawStderr `
        -ServiceStatus 'service-status-sensitive-sentinel' `
        -ServerAccessProbe 'server-access-sensitive-sentinel'
    $expectedFailureDiagnostic = "Authenticated service response did not map to NotProvisioned. exit_code=74; stderr_category=unexpected; stdout_utf16_length=$($rawStdout.Length); stderr_utf16_length=$($rawStderr.Length); broker_service_status=unknown; server_access_probe=invalid"
    Assert-True ($failureDiagnostic -ceq $expectedFailureDiagnostic) 'Unexpected client output did not produce the exact sanitized failure record.'
    foreach ($rawValue in @($rawStdout, $rawStderr, 'service-status-sensitive-sentinel', 'server-access-sensitive-sentinel')) {
        Assert-True (-not $failureDiagnostic.Contains($rawValue, [StringComparison]::Ordinal)) 'Sanitized failure record retained untrusted diagnostic content.'
    }
    $validProbeRecord = 'ephemeral-server-access;session=error:5;process=error:5;token=not_attempted:0'
    $validatedDiagnostic = Get-SanitizedClientFailureDiagnostic -ExitCode 74 -StandardOutput '' -StandardError 'Protected Windows GPU promotion transport was rejected.' -ServiceStatus 'Running' -ServerAccessProbe $validProbeRecord
    Assert-True ($validatedDiagnostic.EndsWith("server_access_probe=$validProbeRecord", [StringComparison]::Ordinal)) 'Validated server-access probe record was not included exactly.'
    $nullDiagnostic = Get-SanitizedClientFailureDiagnostic -ExitCode 1 -StandardOutput $null -StandardError $null -ServiceStatus $null -ServerAccessProbe $null
    Assert-True ($nullDiagnostic -ceq 'Authenticated service response did not map to NotProvisioned. exit_code=1; stderr_category=unexpected; stdout_utf16_length=0; stderr_utf16_length=0; broker_service_status=unknown; server_access_probe=invalid') 'Null client output did not produce the exact sanitized failure record.'
}

function New-ValidClientArguments([string]$HandoffRoot, [string]$OutputRoot) {
    return @(
        'promote-windows-pack-set',
        '--handoff-root', $HandoffRoot,
        '--output-root', $OutputRoot,
        '--source-repository', 'tyhuang9/scribe',
        '--source-ref', 'refs/heads/main',
        '--source-revision', ('a' * 40),
        '--workflow-ref', 'tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main',
        '--workflow-source-sha', ('a' * 40),
        '--run-id', '1001',
        '--run-attempt', '1',
        '--artifact-id', '2002',
        '--artifact-digest', ('b' * 64),
        '--handoff-sha256', ('c' * 64),
        '--release-set-digest', ('d' * 64),
        '--toolchain-manifest-sha256', ('e' * 64),
        '--pack-version', '0.1.0-transport-fixture',
        '--minimum-security-epoch', '1',
        '--require-unused-release-set'
    )
}

try {
    Test-ServerAccessProbeContract
    Test-StandardSoftwareCreatorOwnerInheritanceTemplateContract
    Test-EphemeralProcessOwnershipBoundary
    Test-CredentialCommandLineContract
    Test-FixturePathSetAvailabilityContract
    Test-SanitizedClientDiagnosticCategoryContract
    $goldenRequest = '{"schema_version":1,"command":"promote-windows-pack-set","client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","promotion_intent_sha256":"bf7b4002065dcc87c6d7abd70899c76a23c880c82e1869c4ba2bdbf39dcebe3c","intent":{"schema_version":1,"policy_namespace":"scribe-windows-gpu-production-v1","source_repository":"owner/repo","source_ref":"refs/heads/main","source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","workflow_ref":"owner/repo/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main","workflow_source_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","run_id":"123","run_attempt":"1","artifact_id":"456","artifact_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","handoff_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","release_set_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","toolchain_manifest_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","pack_version":"0.1.0","minimum_security_epoch":1,"require_unused_release_set":true}}'
    $goldenMaterial = [Text.Encoding]::UTF8.GetBytes("scribe-windows-gpu-promotion-request-v1`0$goldenRequest")
    $goldenDigest = ([BitConverter]::ToString([Security.Cryptography.SHA256]::HashData($goldenMaterial))).Replace('-', '').ToLowerInvariant()
    Assert-True ($goldenDigest -ceq '3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083') 'PowerShell and Rust disagree on the canonical broker request digest.'
    $goldenResponse = '{"schema_version":1,"client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","promotion_intent_sha256":"bf7b4002065dcc87c6d7abd70899c76a23c880c82e1869c4ba2bdbf39dcebe3c","request_sha256":"3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083","outcome":{"status":"not_provisioned","code":"production_authority_not_provisioned"}}'
    $goldenResponseMaterial = [Text.Encoding]::UTF8.GetBytes("scribe-windows-gpu-promotion-response-v1`0$goldenResponse")
    $goldenResponseDigest = ([BitConverter]::ToString([Security.Cryptography.SHA256]::HashData($goldenResponseMaterial))).Replace('-', '').ToLowerInvariant()
    Assert-True ($goldenResponseDigest -ceq '7d4774c4ad2c0f59d57079e33d3729863a2a679739845f21b4a023207b580143') 'PowerShell and Rust disagree on the canonical broker response digest.'
    Assert-True (Test-RestrictedServiceSidType -ScOutput "[SC] QueryServiceConfig2 SUCCESS`r`n`r`nSERVICE_NAME: $serviceName`r`n        SERVICE_SID_TYPE :  RESTRICTED`r`n") 'The SCM SID parser rejected representative aligned qsidtype output.'
    Assert-True (-not (Test-RestrictedServiceSidType -ScOutput 'SERVICE_SID_TYPE: UNRESTRICTED')) 'The SCM SID parser accepted a non-restricted service.'
    Assert-True (-not (Test-RestrictedServiceSidType -ScOutput "SERVICE_SID_TYPE: RESTRICTED`nSERVICE_SID_TYPE: RESTRICTED")) 'The SCM SID parser accepted ambiguous duplicate fields.'
    Test-ProvisioningSuccessRecordValidation
    Test-PrivilegeRestoreRetryLifecycle

    if (Get-BrokerService) {
        throw "Refusing to modify the pre-existing fixed-name service $serviceName."
    }

    Assert-True ([IO.Path]::IsPathFullyQualified($env:SystemRoot)) 'Windows system directory is not fully qualified.'
    $systemDirectory = [IO.Path]::GetFullPath($env:SystemRoot).TrimEnd('\')
    Assert-True ($systemDirectory.Equals($env:SystemRoot.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)) 'Windows system directory is noncanonical.'
    $systemRootCandidate = [IO.Path]::GetPathRoot($systemDirectory)
    Assert-True ($systemRootCandidate -match '^[A-Za-z]:\\$') 'Windows system directory is not on a local drive root.'
    $systemVolumeRoot = ([char]::ToUpperInvariant($systemRootCandidate[0])).ToString() + ':\'
    Assert-True ([IO.Path]::GetFullPath($systemVolumeRoot) -ceq $systemVolumeRoot) 'Windows system-volume root is noncanonical.'
    $commonAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)
    $fixturePaths = Select-AvailableFixturePathSet -CommonAppData $commonAppData -DriveRoot $systemVolumeRoot
    $pathToken = $fixturePaths.Token
    $machineTarget = $fixturePaths.MachineTarget
    $handoff = $fixturePaths.Handoff
    $output = $fixturePaths.Output
    Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output

    New-Item -ItemType Directory -Path $targetRoot | Out-Null
    $env:CARGO_TARGET_DIR = Join-Path $targetRoot 'cargo-target'
    Invoke-Process -FilePath 'cargo' -Arguments @('build', '--release', '--locked', '--offline', '--manifest-path', $manifest, '--bins') | Out-Null
    $client = Join-Path $env:CARGO_TARGET_DIR 'release\scribe-windows-gpu-promotion-client.exe'
    $builtService = Join-Path $env:CARGO_TARGET_DIR 'release\scribe-windows-gpu-promotion-service.exe'
    Assert-True (Test-Path -LiteralPath $client -PathType Leaf) 'Release broker client was not built.'
    Assert-True (Test-Path -LiteralPath $builtService -PathType Leaf) 'Release broker service was not built.'

    $arguments = New-ValidClientArguments -HandoffRoot $handoff -OutputRoot $output

    $missing = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($missing.ExitCode -eq 78) 'An absent broker did not map to the fail-closed unprovisioned exit.'
    Assert-True ($missing.Stderr.Trim() -ceq 'Protected Windows GPU promotion broker is unavailable and production authority is not provisioned; no filesystem, ledger, or signing authority was accessed.') 'An absent broker did not emit its fixed unavailable diagnostic.'
    Assert-True (-not (Test-Path -LiteralPath $handoff)) 'Absent-service handling touched the handoff path.'
    Assert-True (-not (Test-Path -LiteralPath $output)) 'Absent-service handling touched the output path.'

    $console = Invoke-Process -FilePath $builtService -TimeoutSeconds 20 -AllowFailure
    Assert-True ($console.ExitCode -eq 78) 'The SCM-only service did not reject an interactive console launch with its fixed exit.'

    $squatter = [IO.Pipes.NamedPipeServerStream]::new(
        $pipeName,
        [IO.Pipes.PipeDirection]::InOut,
        1,
        [IO.Pipes.PipeTransmissionMode]::Message,
        [IO.Pipes.PipeOptions]::Asynchronous
    )
    try {
        $connected = $squatter.WaitForConnectionAsync()
        $spoofed = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
        Assert-True ($spoofed.ExitCode -eq 74) 'The client did not classify a same-name user-process pipe server as rejected authentication.'
        Assert-True ($spoofed.Stderr.Contains('transport was rejected', [StringComparison]::Ordinal)) 'The client did not emit its fixed rejected-transport diagnostic.'
        Assert-True ($connected.Wait(5000)) 'The client did not reach the fixed-name squatter.'
        $buffer = [byte[]]::new(1)
        $read = $squatter.ReadAsync($buffer, 0, 1)
        Assert-True ($read.Wait(5000)) 'The squatter did not observe the client closing its authenticated connection.'
        Assert-True ($read.Result -eq 0) 'The client sent request bytes before authenticating the service.'
    }
    finally { $squatter.Dispose() }

    function Test-FileSystemAccessRuleConstructorNormalization {
        $inputRights = [Security.AccessControl.FileSystemRights]::ReadAndExecute
        Assert-True ([uint32]$inputRights -eq 0x000200a9) 'The input ReadAndExecute access mask changed before FileSystemAccessRule normalization.'
        $persistedRights = [Security.AccessControl.FileSystemRights](
            [uint32]$inputRights -bor
            [uint32][Security.AccessControl.FileSystemRights]::Synchronize
        )
        Assert-True ([uint32]$persistedRights -eq 0x001200a9) 'The expected ReadAndExecute|Synchronize access mask changed.'
        $rule = [Security.AccessControl.FileSystemAccessRule]::new(
            [Security.Principal.SecurityIdentifier]::new('S-1-5-18'),
            $inputRights,
            [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit',
            [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow
        )
        Assert-True ([uint32]$rule.FileSystemRights -eq [uint32]$persistedRights) 'FileSystemAccessRule constructor did not normalize ReadAndExecute to its persisted ReadAndExecute|Synchronize mask.'
        foreach ($forbidden in @(
            [Security.AccessControl.FileSystemRights]::Write,
            [Security.AccessControl.FileSystemRights]::Delete,
            [Security.AccessControl.FileSystemRights]::ChangePermissions,
            [Security.AccessControl.FileSystemRights]::TakeOwnership
        )) {
            Assert-True (([uint32]$rule.FileSystemRights -band [uint32]$forbidden) -eq 0) "FileSystemAccessRule constructor granted forbidden authority $forbidden."
        }
    }

    Test-FileSystemAccessRuleConstructorNormalization
    $runnerIdentity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        $runnerSid = $runnerIdentity.User.Value
        $principal = [Security.Principal.WindowsPrincipal]::new($runnerIdentity)
        $isElevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    }
    finally { $runnerIdentity.Dispose() }
    if (-not $isElevated) {
        if ($RequireScmIntegration) { throw 'Restricted-service integration requires an elevated disposable Windows host.' }
        Write-Output 'Restricted-service integration skipped: current process is not elevated.'
        Write-Output 'Windows GPU broker transport contract tests passed.'
        return
    }

    if (Test-Path -LiteralPath $policyRegistryPath) {
        throw 'Refusing to modify a pre-existing fixed Windows GPU broker client policy.'
    }
    $ephemeralAccount = New-EphemeralStandardAccount
    $ephemeralSid = $ephemeralAccount.Sid.Value
    Assert-True ($runnerSid -cne $ephemeralSid) 'Elevated runner SID cannot be the valid broker client SID.'
    $postCreateFailure = [InvalidOperationException]::new('expected-post-create-pre-enable-failure')
    $observedPostCreateFailure = $null
    try { throw $postCreateFailure }
    catch { $observedPostCreateFailure = $_.Exception }
    Assert-True ([object]::ReferenceEquals($postCreateFailure, $observedPostCreateFailure)) 'Post-create/pre-enable failure boundary replaced the original failure.'
    [void](Assert-OwnedEphemeralAccount -State $ephemeralAccount -ExpectedEnabled $false)
    Test-RestorePrivilegeRestoration -InitiallyEnabled $false -Sid $ephemeralSid
    Test-RestorePrivilegeRestoration -InitiallyEnabled $true -Sid $ephemeralSid
    Assert-True (-not (Test-Path -LiteralPath $policyRegistryPath)) 'Privilege restoration fixtures left an authorization policy behind.'
    foreach ($rejectedSid in @('BUILTIN\Users', 'S-1-5-11', 'S-1-5-20', $serviceSid, 'S-1-5-21-1-2-3-500')) {
        $rejectedProvision = New-ProtectedPolicy -Sid $rejectedSid
        Assert-True ($rejectedProvision.ExitCode -ne 0) "Provisioner accepted dangerous client identity $rejectedSid."
        Assert-True (-not (Test-Path -LiteralPath $policyRegistryPath)) 'Rejected provisioning created a policy key.'
        Assert-True ($null -eq $ownedPolicyState) 'Rejected provisioning established destructive policy ownership.'
    }

    $validatedMachineTarget = Get-ValidatedMachineTargetPath -CommonAppData $commonAppData -Token $pathToken
    Assert-True ($validatedMachineTarget.Equals($machineTarget, [StringComparison]::OrdinalIgnoreCase)) 'Selected SCM test staging path changed before creation.'
    Assert-True (-not (Test-Path -LiteralPath $machineTarget)) 'Refusing to adopt a pre-existing SCM test staging path.'
    $createdMachineTarget = New-Item -ItemType Directory -Path $machineTarget
    Assert-True ($createdMachineTarget.FullName.Equals($machineTarget, [StringComparison]::OrdinalIgnoreCase)) 'Windows created an unexpected SCM test staging path.'
    Assert-True (($createdMachineTarget.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'SCM test staging directory is a reparse point.'
    $ownedMachineTarget = $machineTarget
    $machineAcl = Get-Acl -LiteralPath $machineTarget
    $machineAcl.SetAccessRuleProtection($true, $false)
    foreach ($rule in @($machineAcl.Access)) { [void]$machineAcl.RemoveAccessRuleSpecific($rule) }
    $inheritance = [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit'
    $propagation = [Security.AccessControl.PropagationFlags]::None
    $allow = [Security.AccessControl.AccessControlType]::Allow
    $persistedReadAndExecuteRights = [Security.AccessControl.FileSystemRights](
        [uint32][Security.AccessControl.FileSystemRights]::ReadAndExecute -bor
        [uint32][Security.AccessControl.FileSystemRights]::Synchronize
    )
    foreach ($entry in @(
        @('S-1-5-18', [Security.AccessControl.FileSystemRights]::FullControl),
        @('S-1-5-32-544', [Security.AccessControl.FileSystemRights]::FullControl),
        @($serviceSid, [Security.AccessControl.FileSystemRights]::ReadAndExecute),
        @($ephemeralSid, [Security.AccessControl.FileSystemRights]::ReadAndExecute)
    )) {
        $identitySid = [Security.Principal.SecurityIdentifier]::new([string]$entry[0])
        $accessRule = [Security.AccessControl.FileSystemAccessRule]::new($identitySid, $entry[1], $inheritance, $propagation, $allow)
        $machineAcl.AddAccessRule($accessRule)
    }
    Set-Acl -LiteralPath $machineTarget -AclObject $machineAcl
    $verifiedAcl = Get-Acl -LiteralPath $machineTarget
    Assert-True $verifiedAcl.AreAccessRulesProtected 'SCM test staging inherited an ambient writable DACL.'
    $verifiedRules = @($verifiedAcl.GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]))
    $allowedSids = @('S-1-5-18', 'S-1-5-32-544', $serviceSid, $ephemeralSid)
    Assert-True ($verifiedRules.Count -eq 4) 'SCM test staging contains an unexpected access rule.'
    Assert-True (-not ($verifiedRules | Where-Object { $_.AccessControlType -ne $allow -or $_.IdentityReference.Value -notin $allowedSids })) 'SCM test staging contains unexpected identity or deny rules.'
    foreach ($expectedSid in $allowedSids) {
        $matchingRules = @($verifiedRules | Where-Object { $_.IdentityReference.Value -ceq $expectedSid })
        Assert-True ($matchingRules.Count -eq 1) "SCM test staging does not contain one exact rule for $expectedSid."
        $expectedRights = if ($expectedSid -ceq 'S-1-5-18' -or $expectedSid -ceq 'S-1-5-32-544') {
            [Security.AccessControl.FileSystemRights]::FullControl
        }
        else { $persistedReadAndExecuteRights }
        Assert-True ([uint32]$matchingRules[0].FileSystemRights -eq [uint32]$expectedRights) "SCM test staging rights changed for $expectedSid."
        Assert-True ($matchingRules[0].InheritanceFlags -eq $inheritance -and $matchingRules[0].PropagationFlags -eq $propagation) "SCM test staging inheritance changed for $expectedSid."
    }
    $serviceRules = @($verifiedRules | Where-Object { $_.IdentityReference.Value -ceq $serviceSid })
    Assert-True ($serviceRules.Count -eq 1) 'SCM test staging does not have one exact service-SID access rule.'
    Assert-True (($serviceRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) -eq 0) 'The test service SID can modify its staged binary.'
    Assert-True (($serviceRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadAndExecute) -eq [Security.AccessControl.FileSystemRights]::ReadAndExecute) 'The test service SID cannot read and execute its staged binary.'
    $clientRules = @($verifiedRules | Where-Object { $_.IdentityReference.Value -ceq $ephemeralSid })
    Assert-True ($clientRules.Count -eq 1) 'SCM test staging does not have one exact ephemeral-client access rule.'
    Assert-True (($clientRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) -eq 0) 'The ephemeral client can modify protected staging.'
    Assert-True (($clientRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadAndExecute) -eq [Security.AccessControl.FileSystemRights]::ReadAndExecute) 'The ephemeral client cannot read and execute protected staging.'
    $serviceForScm = Join-Path $machineTarget 's.exe'
    $clientForCredential = Join-Path $machineTarget 'c.exe'
    $harnessForCredential = Join-Path $machineTarget 'p.ps1'
    Copy-Item -LiteralPath $builtService -Destination $serviceForScm
    Copy-Item -LiteralPath $client -Destination $clientForCredential
    Copy-Item -LiteralPath $PSCommandPath -Destination $harnessForCredential
    Assert-True (Test-Path -LiteralPath $serviceForScm -PathType Leaf) 'Protected SCM service staging failed.'
    foreach ($stagedPath in @($serviceForScm, $clientForCredential, $harnessForCredential)) {
        $stagedItem = Get-Item -LiteralPath $stagedPath -Force
        Assert-True (($stagedItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Protected staging produced a reparse point.'
    }
    Assert-True ((Get-FileHash -Algorithm SHA256 -LiteralPath $serviceForScm).Hash -ceq (Get-FileHash -Algorithm SHA256 -LiteralPath $builtService).Hash) 'Protected SCM service staging changed the built service bytes.'
    Assert-True ((Get-FileHash -Algorithm SHA256 -LiteralPath $clientForCredential).Hash -ceq (Get-FileHash -Algorithm SHA256 -LiteralPath $client).Hash) 'Protected client staging changed the real client bytes.'
    Assert-True ((Get-FileHash -Algorithm SHA256 -LiteralPath $harnessForCredential).Hash -ceq (Get-FileHash -Algorithm SHA256 -LiteralPath $PSCommandPath).Hash) 'Protected harness staging changed the fixed probe bytes.'
    $realClientCommandLineLength = [Scribe.GpuBroker.CredentialCommandLine]::ValidateLength($clientForCredential, [string[]]$arguments)
    Write-Verbose "Credentialed real-client command-line UTF-16 length is $realClientCommandLineLength/960 including NUL."
    Enable-LocalUser -SID $ephemeralAccount.Sid
    $enabledAccount = Assert-OwnedEphemeralAccount -State $ephemeralAccount -ExpectedEnabled $true
    Assert-True $enabledAccount.Enabled 'Ephemeral account did not become enabled after protected staging completed.'
    Assert-NoEphemeralProfileRegistration -Sid $ephemeralAccount.Sid
    $probePowerShell = (Get-Process -Id $PID).Path
    $identityProbe = Invoke-EphemeralProcess -FilePath $probePowerShell -Arguments @(
        '-NoProfile',
        '-NonInteractive',
        '-File', $harnessForCredential,
        '-ExpectedEphemeralSid', $ephemeralSid,
        '-RunEphemeralIdentityProbe'
    )
    Assert-True ($identityProbe.Stdout.Trim() -ceq 'ephemeral-identity-ok') 'Primary-token identity probe did not return its exact success marker.'
    Assert-True ($identityProbe.Stderr.Length -eq 0) 'Primary-token identity probe emitted unexpected diagnostics.'
    Assert-NoEphemeralProfileRegistration -Sid $ephemeralAccount.Sid

    $shownSid = Invoke-Sc -Arguments @('showsid', $serviceName)
    Assert-True ($shownSid.Stdout.Contains($serviceSid, [StringComparison]::Ordinal)) 'Windows derived an unexpected fixed service SID.'

    $quotedService = '"' + $serviceForScm + '"'
    $create = Invoke-Sc -Arguments @(
        'create', $serviceName,
        'type=', 'own',
        'start=', 'demand',
        'obj=', 'NT AUTHORITY\LocalService',
        'binPath=', $quotedService,
        'DisplayName=', 'Scribe GPU Promotion Broker Transport Test'
    ) -AllowFailure
    Assert-True ($create.ExitCode -eq 0) "Failed to create the temporary broker service: $($create.Stderr)"
    $createdService = $true

    $sidType = Invoke-Sc -Arguments @('sidtype', $serviceName, 'restricted') -AllowFailure
    Assert-True ($sidType.ExitCode -eq 0) "Failed to configure the restricted service SID: $($sidType.Stderr)"
    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)

    Assert-RejectedServiceStartup -Label 'Missing policy' -Client $client -ClientArguments $arguments
    Assert-True (-not (Test-Path -LiteralPath $handoff)) 'Missing-policy startup touched the handoff path.'
    Assert-True (-not (Test-Path -LiteralPath $output)) 'Missing-policy startup touched the output path.'

    New-WeakPolicy -Sid $ephemeralSid
    Assert-RejectedServiceStartup -Label 'Weak broad-read policy DACL' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $ephemeralSid
    Assert-True ($provisioned.ExitCode -eq 0) "Protected policy provisioning failed: $($provisioned.Stderr)"
    Assert-ExactPolicyAcl
    $duplicateProvision = New-ProtectedPolicy -Sid $ephemeralSid
    Assert-True ($duplicateProvision.ExitCode -ne 0) 'Provisioner modified a pre-existing policy.'

    # A failed provision never establishes ownership, and cleanup must also
    # refuse an owned key whose exact fixture state changed unexpectedly.
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Owned policy disappeared before adversarial cleanup proof.'
        try { $key.SetValue('CleanupTamper', 'foreign', [Microsoft.Win32.RegistryValueKind]::String); $key.Flush() }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    $cleanupRejected = $false
    try { Remove-OwnedPolicy }
    catch { $cleanupRejected = $true }
    Assert-True $cleanupRejected 'Policy cleanup accepted a changed same-name key.'
    Assert-True (Test-Path -LiteralPath $policyRegistryPath) 'Policy cleanup deleted a changed same-name key.'
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Changed policy disappeared during adversarial cleanup proof.'
        try { $key.DeleteValue('CleanupTamper', $true); $key.Flush() }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    Assert-OwnedPolicyState -State $ownedPolicyState

    # Rename the validated object while its exact cleanup handle remains live,
    # then capture only the replacement invocation result. Record parsing and
    # ownership adoption wait until NtDeleteKey has removed the renamed object
    # and cleanup has relinquished the old state.
    $boundaryLeafName = "Authorization.boundary-$([guid]::NewGuid().ToString('N'))"
    $boundaryRenamedPath = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Scribe\GpuPromotionBroker\v1\$boundaryLeafName"
    $boundaryState = [pscustomobject]@{
        RenameSucceeded = $false
        InvocationNonce = New-ProvisioningInvocationNonce
        InvocationResult = $null
        InvocationError = $null
    }
    $expectedBoundaryDetection = [InvalidOperationException]::new("expected-policy-path-reappeared-$([guid]::NewGuid().ToString('N'))")
    $boundarySwapDetected = $false
    try {
        Remove-OwnedPolicy -BeforeDelete {
            $renameStatus = [Scribe.GpuBroker.RegistryCleanupNative]::RenameAuthorizationForBoundaryTest($boundaryLeafName)
            Assert-True ($renameStatus -eq 0) "Could not rename the validated boundary object (Win32 $renameStatus)."
            $boundaryState.RenameSucceeded = $true
            try {
                $boundaryState.InvocationResult = Invoke-ProtectedPolicyProvisioner `
                    -Sid $ephemeralSid `
                    -InvocationNonce $boundaryState.InvocationNonce
            }
            catch { $boundaryState.InvocationError = $_ }
        } -PathReappearedException $expectedBoundaryDetection
    }
    catch {
        if ([object]::ReferenceEquals($_.Exception, $expectedBoundaryDetection)) {
            $boundarySwapDetected = $true
        }
        else { throw }
    }
    Assert-True $boundaryState.RenameSucceeded 'Policy cleanup did not reach the exact-handle rename boundary.'
    Assert-True ($null -eq $ownedPolicyState) 'Policy cleanup retained authority after its exact NtDeleteKey succeeded.'
    if ($null -ne $boundaryState.InvocationError) { throw $boundaryState.InvocationError }
    Assert-True ($null -ne $boundaryState.InvocationResult) 'Boundary replacement invocation did not return a result.'
    Assert-True ($boundaryState.InvocationResult.ExitCode -eq 0) "Could not create the boundary-swap policy: $($boundaryState.InvocationResult.Stderr)"
    $replacementAncestors = @(Read-ProvisioningSuccessRecord `
        -Result $boundaryState.InvocationResult `
        -Sid $ephemeralSid `
        -InvocationNonce $boundaryState.InvocationNonce)
    Assert-True $boundarySwapDetected 'Policy cleanup did not detect a same-path object created before exact handle-bound deletion.'
    Assert-True (Test-Path -LiteralPath $policyRegistryPath) 'Handle-bound cleanup targeted the boundary-swap replacement.'
    Assert-True (-not (Test-Path -LiteralPath $boundaryRenamedPath)) 'Handle-bound cleanup did not delete the original renamed registry object.'
    Set-OwnedPolicyState -Values (New-ExpectedPolicyValues -Sid $ephemeralSid) -RequireCanonicalAcl $true
    $script:ownedPolicyAncestors = @($ownedPolicyAncestors) + @($replacementAncestors)
    Assert-OwnedPolicyState -State $ownedPolicyState

    Set-PolicyValue -Name 'UnexpectedValue' -Value 'forbidden' -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Extra policy value' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $ephemeralSid
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not reprovision the noncanonical-value-name fixture.'
    Replace-PolicyValueSpelling -ExistingName 'AuthorizedClientSid' -ReplacementName 'authorizedclientsid' -Value $ephemeralSid -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Noncanonical policy value name' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $ephemeralSid
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not reprovision the malformed-schema fixture.'
    Set-PolicyValue -Name 'SchemaVersion' -Value '1' -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Malformed policy schema' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $ephemeralSid
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not reprovision the malformed-policy fixture.'
    Set-PolicyValue -Name 'AuthorizedClientSid' -Value 'S-1-5-11' -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Broad malformed policy SID' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $orphanSid = 'S-1-5-21-4294967290-4294967291-4294967292-4294967293'
    $provisioned = New-ProtectedPolicy -Sid $orphanSid
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not provision the orphan-SID denial fixture.'
    Assert-ExactPolicyAcl
    Assert-RejectedServiceStartup -Label 'Unmapped policy SID' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $ephemeralSid
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not provision the ephemeral TokenUser SID.'
    Assert-ExactPolicyAcl
    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) "SCM rejected the ephemeral-client policy: $($start.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))

    $fullControlProbe = Invoke-EphemeralProcess -FilePath $probePowerShell -Arguments @(
        '-NoProfile',
        '-NonInteractive',
        '-File', $harnessForCredential,
        '-ExpectedEphemeralSid', $ephemeralSid,
        '-RunEphemeralFullControlProbe'
    )
    Assert-True ($fullControlProbe.Stdout.Trim() -ceq 'ephemeral-full-control-denied') 'FullControl denial was not established under the exact ephemeral identity.'
    Assert-True ($fullControlProbe.Stderr.Length -eq 0) 'FullControl credential probe emitted unexpected diagnostics.'

    $stalledProcess = Start-EphemeralProcess -FilePath $probePowerShell -Arguments @(
        '-NoProfile',
        '-NonInteractive',
        '-File', $harnessForCredential,
        '-ExpectedEphemeralSid', $ephemeralSid,
        '-RunEphemeralStalledProbe'
    )
    try {
        $stalledErrorTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadToEndBoundedAsync($stalledProcess.StandardError, 16384)
        $readinessTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadLineBoundedAsync($stalledProcess.StandardOutput, 128)
        Assert-True ($readinessTask.Wait(8000)) 'Ephemeral stalled probe did not report readiness within its fixed timeout.'
        Assert-True ($readinessTask.GetAwaiter().GetResult() -ceq 'ephemeral-stalled-ready') 'Ephemeral stalled probe did not establish exact-mask readiness.'
        Assert-True (-not $stalledProcess.HasExited) 'Ephemeral stalled probe exited before SCM cancellation began.'
        [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
        $stopProof = [Diagnostics.Stopwatch]::StartNew()
        $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
        Assert-True ($stop.ExitCode -eq 0) "SCM rejected the bounded-stop request: $($stop.Stderr)"
        (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(4))
        $stopProof.Stop()
        Assert-True ($stopProof.Elapsed.TotalMilliseconds -lt 4500) 'SCM stop did not cancel the stalled broker read materially before its five-second natural timeout.'
        $stalledRemainderTask = [Scribe.GpuBroker.PrivilegeRestoreRetryModel]::ReadToEndBoundedAsync($stalledProcess.StandardOutput, 16384)
        Assert-True ($stalledProcess.WaitForExit(10000)) 'Ephemeral stalled probe did not exit after SCM closed its pipe.'
        Assert-True ($stalledProcess.ExitCode -eq 0) "Ephemeral stalled probe failed: $($stalledErrorTask.GetAwaiter().GetResult())"
        Assert-True ($stalledRemainderTask.GetAwaiter().GetResult().Trim() -ceq 'ephemeral-stalled-disconnected') 'Ephemeral stalled probe did not confirm pipe disconnection.'
        Assert-True ($stalledErrorTask.GetAwaiter().GetResult().Length -eq 0) 'Ephemeral stalled probe emitted unexpected diagnostics.'
    }
    finally {
        if ([object]::ReferenceEquals($script:activeCredentialProcess, $stalledProcess)) {
            if (-not $stalledProcess.HasExited) {
                $stalledProcess.Kill($true)
                Assert-True ($stalledProcess.WaitForExit(10000)) 'Stalled credential probe termination remained uncertain after kill.'
            }
            Release-ExitedEphemeralProcess -Process $stalledProcess
        }
    }

    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) "SCM rejected the second service start: $($start.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))
    $serverAccessService = Assert-OwnedBrokerService -ExpectedPath $serviceForScm
    $serverAccessController = Get-BrokerService
    Assert-True ($null -ne $serverAccessController -and $serverAccessController.Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Server-access proof requires the exact owned broker service to remain running.'
    $serverAccessProcessId = [uint32]$serverAccessService.ProcessId
    Assert-True ($serverAccessProcessId -gt 0) 'Server-access proof requires the exact owned broker service process ID.'
    $serverAccessProbe = Invoke-EphemeralProcess -FilePath $probePowerShell -Arguments @(
        '-NoProfile',
        '-NonInteractive',
        '-File', $harnessForCredential,
        '-ExpectedEphemeralSid', $ephemeralSid,
        '-ExpectedBrokerProcessId', $serverAccessProcessId.ToString([Globalization.CultureInfo]::InvariantCulture),
        '-RunEphemeralServerAccessProbe'
    ) -TimeoutSeconds 20 -AllowFailure
    Assert-True ($serverAccessProbe.Stdout.Length -le 192) 'Server-access proof exceeded its canonical output bound.'
    $serverAccessProbeRecord = $serverAccessProbe.Stdout.Trim()
    Assert-True (Test-ServerAccessProbeRecord -Record $serverAccessProbeRecord) 'Server-access proof emitted a noncanonical diagnostic record.'
    Assert-True ($serverAccessProbe.ExitCode -eq 0) "Exact query or excess-right server-access proof failed after validated record $serverAccessProbeRecord."
    Assert-True ($serverAccessProbe.Stderr.Length -eq 0) 'Successful server-access proof emitted unexpected diagnostics.'
    Assert-True ($serverAccessProbeRecord -ceq 'ephemeral-server-access;session=error:5;process=ok:0;token=ok:0') 'Server-access proof did not establish exact query access while retaining ProcessIdToSessionId denial.'
    $serverAccessServiceAfterProbe = Assert-OwnedBrokerService -ExpectedPath $serviceForScm
    $serverAccessControllerAfterProbe = Get-BrokerService
    Assert-True ([uint32]$serverAccessServiceAfterProbe.ProcessId -eq $serverAccessProcessId -and $null -ne $serverAccessControllerAfterProbe -and $serverAccessControllerAfterProbe.Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Exact owned broker service identity changed during server-access proof.'
    Set-PolicyValue -Name 'AuthorizedClientSid' -Value $orphanSid -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    $roundTrip = Invoke-EphemeralProcess -FilePath $clientForCredential -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    if ($roundTrip.ExitCode -ne 78) {
        $diagnosticService = Assert-OwnedBrokerService -ExpectedPath $serviceForScm
        $diagnosticController = Get-BrokerService
        Assert-True ($null -ne $diagnosticController -and $diagnosticController.Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Client failure diagnostic requires the exact owned broker service to remain running.'
        $diagnosticProcessId = [uint32]$diagnosticService.ProcessId
        Assert-True ($diagnosticProcessId -eq $serverAccessProcessId) 'Exact owned broker service identity changed between server-access proof and client failure.'
        throw (Get-SanitizedClientFailureDiagnostic `
            -ExitCode ([int]$roundTrip.ExitCode) `
            -StandardOutput $roundTrip.Stdout `
            -StandardError $roundTrip.Stderr `
            -ServiceStatus $diagnosticController.Status.ToString() `
            -ServerAccessProbe $serverAccessProbeRecord)
    }
    Assert-True ($roundTrip.Stdout.Length -eq 0) 'Broker client wrote protocol data to stdout.'
    Assert-True ($roundTrip.Stderr.Trim() -ceq 'Protected Windows GPU promotion broker authenticated; production authority is not provisioned, and no filesystem, ledger, or signing authority was accessed.') 'Broker client did not emit its fixed authenticated NotProvisioned diagnostic.'
    Assert-True ((Get-Service -Name $serviceName).Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Broker service did not remain running after the authenticated round trip.'
    Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output

    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
    $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
    Assert-True ($stop.ExitCode -eq 0) "SCM rejected the snapshot-policy stop request: $($stop.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))

    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -ne 0) 'Service restart accepted the mutated unmapped authorization SID.'
    Wait-ServiceNotRunning -TimeoutSeconds 10
    $afterRestart = Invoke-EphemeralProcess -FilePath $clientForCredential -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($afterRestart.ExitCode -eq 78) 'Rejected unmapped-policy restart did not keep the broker unavailable.'
    Assert-True ($afterRestart.Stderr.Contains('broker is unavailable', [StringComparison]::Ordinal)) 'Rejected unmapped-policy restart exposed a broker pipe.'
    Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output
    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
    Remove-OwnedPolicy
    Test-PartialAncestorCleanupOwnershipBoundary
    Set-LocalUser -SID $ephemeralAccount.Sid -Description 'foreign-cleanup-marker'
    $accountCleanupFailure = $null
    try { Remove-OwnedEphemeralAccount }
    catch { $accountCleanupFailure = $_.Exception }
    Assert-True ($null -ne $accountCleanupFailure -and $accountCleanupFailure.Message -ceq 'Owned ephemeral account marker changed.') 'Ephemeral account cleanup did not fail specifically on the changed ownership marker.'
    Assert-True (@(Get-ExactLocalUserBySid -Sid $ephemeralAccount.Sid).Count -eq 1) 'Rejected account cleanup removed the exact owned SID.'
    Assert-True ($null -ne $ownedEphemeralAccount) 'Rejected account cleanup discarded SID-bound ownership.'
    Set-LocalUser -SID $ephemeralAccount.Sid -Description $ephemeralAccount.Marker
    [void](Assert-OwnedEphemeralAccount -State $ephemeralAccount -ExpectedEnabled $true)
    Assert-NoEphemeralProfileRegistration -Sid $ephemeralAccount.Sid
    Write-Output 'Windows GPU broker transport contract tests passed.'
}
catch { $primaryFailure = $_ }
finally {
    try {
        if ($null -ne $activeCredentialProcess) {
            if (-not $activeCredentialProcess.HasExited) {
                $activeCredentialProcess.Kill($true)
                Assert-True ($activeCredentialProcess.WaitForExit(10000)) 'Credentialed process cleanup could not confirm exit after kill.'
            }
            Release-ExitedEphemeralProcess -Process $activeCredentialProcess
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($null -ne $ownedEphemeralAccount) {
            Assert-NoEphemeralProfileRegistration -Sid $ownedEphemeralAccount.Sid
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try { Remove-OwnedEphemeralAccount }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($null -ne $ephemeralPassword) {
            $ephemeralPassword.Dispose()
            $script:ephemeralPassword = $null
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($null -ne $ownedPolicyState) { Remove-OwnedPolicy }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try { Remove-OwnedPolicyAncestors }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($createdService) {
            $existing = Get-BrokerService
            if ($null -ne $existing) {
                [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
                if ($existing.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Stopped) {
                    $cleanupStop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
                    Assert-True ($cleanupStop.ExitCode -eq 0) "SCM rejected cleanup stop: $($cleanupStop.Stderr)"
                    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))
                }
                [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
                $cleanupDelete = Invoke-Sc -Arguments @('delete', $serviceName) -AllowFailure
                Assert-True ($cleanupDelete.ExitCode -eq 0) "SCM rejected cleanup delete: $($cleanupDelete.Stderr)"
            }
            Wait-ServiceAbsent -TimeoutSeconds 10
        }
        Assert-True ($null -eq $activeCredentialProcess) 'Refusing protected staging cleanup while a credentialed process may still be active.'
        Assert-True ($null -eq (Get-BrokerService)) 'Refusing protected staging cleanup while the exact broker service still exists.'
        $safeToRemoveMachineTarget = $null -ne $ownedMachineTarget
    }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($safeToRemoveMachineTarget -and $null -ne $machineTarget -and $null -ne $ownedMachineTarget) {
            $resolvedCommonAppData = [IO.Path]::GetFullPath([Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)).TrimEnd('\')
            $resolvedMachineTarget = [IO.Path]::GetFullPath($machineTarget)
            Assert-True ($resolvedMachineTarget.Equals($ownedMachineTarget, [StringComparison]::OrdinalIgnoreCase)) 'Refusing protected staging cleanup after ownership path changed.'
            Assert-True ([IO.Path]::GetDirectoryName($resolvedMachineTarget).Equals($resolvedCommonAppData, [StringComparison]::OrdinalIgnoreCase)) 'Refusing protected staging cleanup outside its exact CommonApplicationData parent.'
            Assert-True ([IO.Path]::GetFileName($resolvedMachineTarget) -cmatch '^s[0-9a-f]{32}$') 'Refusing protected staging cleanup for a noncanonical basename.'
            $machineItem = Get-Item -LiteralPath $resolvedMachineTarget -Force -ErrorAction SilentlyContinue
            if ($null -ne $machineItem) {
                Assert-True (($machineItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refusing protected staging cleanup through a reparse point.'
                $expectedStagingSources = @{
                    's.exe' = $builtService
                    'c.exe' = $client
                    'p.ps1' = $PSCommandPath
                }
                foreach ($entry in @(Get-ChildItem -LiteralPath $resolvedMachineTarget -Force)) {
                    Assert-True (@('s.exe', 'c.exe', 'p.ps1') -ccontains $entry.Name) 'Refusing protected staging cleanup containing an unexpected entry.'
                    Assert-True (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0 -and -not $entry.PSIsContainer) 'Refusing protected staging cleanup containing a link or directory.'
                    Assert-True ((Get-FileHash -Algorithm SHA256 -LiteralPath $entry.FullName).Hash -ceq (Get-FileHash -Algorithm SHA256 -LiteralPath $expectedStagingSources[$entry.Name]).Hash) 'Refusing protected staging cleanup after a staged file changed.'
                }
                Remove-Item -LiteralPath $resolvedMachineTarget -Recurse -Force
                Assert-True (-not (Test-Path -LiteralPath $resolvedMachineTarget)) 'Protected staging remained after exact cleanup.'
                $ownedMachineTarget = $null
            }
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($null -ne $handoff -and $null -ne $output) {
            Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try { [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $previousCargoTarget, 'Process') }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        $resolvedTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
        $resolvedTarget = [IO.Path]::GetFullPath($targetRoot)
        if ($resolvedTarget.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedTarget).StartsWith('scribe-gpu-broker-transport-', [StringComparison]::Ordinal)) {
            Remove-Item -LiteralPath $resolvedTarget -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    if ($null -eq $primaryFailure -and $cleanupFailures.Count -gt 0) {
        throw $cleanupFailures[0]
    }
}

if ($null -ne $primaryFailure) {
    foreach ($cleanupFailure in $cleanupFailures) {
        Write-Warning "Non-destructive broker test cleanup was incomplete: $($cleanupFailure.Exception.Message)"
    }
    throw $primaryFailure
}
