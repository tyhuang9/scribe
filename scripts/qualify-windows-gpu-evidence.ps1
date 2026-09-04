[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PlanPath,
    [Parameter(Mandatory = $true)][string]$EvidencePath,
    [string]$ArtifactRoot,
    [switch]$AllowFixture,
    [switch]$RequireEligible
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$MaxInputBytes = 16MB
$MaxLanes = 64
$MaxArtifacts = 4096
$MaxArtifactBytes = [UInt64](512MB)
$MaxControlBytes = 256KB
$ScifHeaderBytes = 26
$MinimumGpuMemoryBytes = [Int64](256MB)
$MinimumGpuPeakMemoryBytes = [Int64](16MB)
$ZeroSha256 = '0' * 64
$RequiredScenarios = @(
    'clean_installer',
    'device_loss',
    'disabled_device',
    'driver_change',
    'insufficient_vram',
    'mixed_gpu',
    'power_ac',
    'power_battery',
    'suspend_resume'
)
$ContractPaths = [ordered]@{
    auto_manifest_sha256 = 'runtime-manifests\gpu-auto-qualification-windows-x64.json'
    evaluator_sha256 = 'scripts\qualify-windows-gpu-evidence.ps1'
    toolchain_contract_sha256 = 'runtime-manifests\gpu-worker-toolchain-windows-x64.json'
}
$ProductionAuthorityPath = 'runtime-manifests\windows-gpu-qualification-production-authority.json'
$AttestationDomain = [Text.Encoding]::ASCII.GetBytes("SCRIBE-WINDOWS-GPU-QUALIFICATION-LANE-ATTESTATION-V1`0")

if ($null -eq ('ScribeWindowsQualification.NativeFile' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Encodings.Web;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace ScribeWindowsQualification
{
    public static class NativeFile
    {
        private const uint GenericRead = 0x80000000;
        private const uint FileReadAttributes = 0x00000080;
        private const uint FileShareRead = 0x00000001;
        private const uint OpenExisting = 3;
        private const uint FileAttributeDirectory = 0x00000010;
        private const uint FileAttributeReparsePoint = 0x00000400;
        private const uint FileFlagBackupSemantics = 0x02000000;
        private const uint FileFlagOpenReparsePoint = 0x00200000;
        private const uint FileFlagSequentialScan = 0x08000000;
        private const int FileStreamInfo = 7;
        private const int StreamBufferBytes = 64 * 1024;
        private const int StreamHeaderBytes = 24;
        private const int ReadBufferBytes = 64 * 1024;

        [StructLayout(LayoutKind.Sequential)]
        private struct NativeFileTime
        {
            internal uint Low;
            internal uint High;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ByHandleFileInformation
        {
            internal uint FileAttributes;
            internal NativeFileTime CreationTime;
            internal NativeFileTime LastAccessTime;
            internal NativeFileTime LastWriteTime;
            internal uint VolumeSerialNumber;
            internal uint FileSizeHigh;
            internal uint FileSizeLow;
            internal uint NumberOfLinks;
            internal uint FileIndexHigh;
            internal uint FileIndexLow;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetFileInformationByHandle(
            SafeFileHandle file,
            out ByHandleFileInformation information);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool GetFileInformationByHandleEx(
            SafeFileHandle file,
            int informationClass,
            IntPtr information,
            uint bufferSize);

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool ReadFile(
            SafeFileHandle file,
            byte[] buffer,
            uint bytesToRead,
            out uint bytesRead,
            IntPtr overlapped);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern uint GetFinalPathNameByHandleW(
            SafeFileHandle file,
            StringBuilder path,
            uint pathLength,
            uint flags);

        public static byte[] ReadBound(string suppliedPath, int maximumBytes)
        {
            if (maximumBytes <= 0 || maximumBytes > 16 * 1024 * 1024)
                throw new ArgumentOutOfRangeException(nameof(maximumBytes));
            string path = CanonicalLocalPath(suppliedPath);
            List<SafeFileHandle> ancestors = OpenAncestors(Path.GetDirectoryName(path));
            SafeFileHandle file = null;
            try
            {
                file = CreateFileW(
                    path,
                    GenericRead | FileReadAttributes,
                    FileShareRead,
                    IntPtr.Zero,
                    OpenExisting,
                    FileFlagOpenReparsePoint | FileFlagSequentialScan,
                    IntPtr.Zero);
                if (file.IsInvalid)
                    throw NativeError("open the qualification file without write or delete sharing", Marshal.GetLastWin32Error());
                ByHandleFileInformation before = Information(file, "inspect the qualification file");
                if ((before.FileAttributes & (FileAttributeDirectory | FileAttributeReparsePoint)) != 0)
                    throw new InvalidOperationException("Qualification input must be a regular non-reparse file.");
                if (before.NumberOfLinks != 1)
                    throw new InvalidOperationException("Qualification input must have exactly one hard link.");
                VerifyFinalPath(file, path, false);
                ulong length = ((ulong)before.FileSizeHigh << 32) | before.FileSizeLow;
                if (length == 0 || length > (ulong)maximumBytes)
                    throw new InvalidOperationException("Qualification input is empty or oversized.");
                ValidateOnlyUnnamedStream(file, length);

                byte[] result = new byte[checked((int)length)];
                byte[] buffer = new byte[Math.Min(ReadBufferBytes, Math.Max(result.Length, 1))];
                int offset = 0;
                while (offset < result.Length)
                {
                    uint read;
                    uint requested = checked((uint)Math.Min(buffer.Length, result.Length - offset));
                    if (!ReadFile(file, buffer, requested, out read, IntPtr.Zero))
                        throw NativeError("read the bound qualification file", Marshal.GetLastWin32Error());
                    if (read == 0)
                        throw new InvalidOperationException("Qualification input ended before its bound size.");
                    Buffer.BlockCopy(buffer, 0, result, offset, checked((int)read));
                    offset = checked(offset + (int)read);
                }
                uint trailing;
                if (!ReadFile(file, buffer, 1, out trailing, IntPtr.Zero))
                    throw NativeError("confirm the bound qualification length", Marshal.GetLastWin32Error());
                if (trailing != 0)
                    throw new InvalidOperationException("Qualification input grew during its bounded read.");
                ByHandleFileInformation after = Information(file, "reinspect the qualification file");
                if (!SameIdentity(before, after))
                    throw new InvalidOperationException("Qualification input changed during its bounded read.");
                return result;
            }
            finally
            {
                if (file != null) file.Dispose();
                for (int index = ancestors.Count - 1; index >= 0; index--) ancestors[index].Dispose();
            }
        }

        public static void ValidatePhysicalDirectory(string suppliedPath)
        {
            string path = CanonicalLocalPath(suppliedPath);
            List<SafeFileHandle> handles = OpenAncestors(path);
            for (int index = handles.Count - 1; index >= 0; index--) handles[index].Dispose();
        }

        private static List<SafeFileHandle> OpenAncestors(string finalDirectory)
        {
            string path = CanonicalLocalPath(finalDirectory);
            string root = Path.GetPathRoot(path);
            List<string> paths = new List<string>();
            string current = path.TrimEnd(Path.DirectorySeparatorChar);
            while (!string.IsNullOrEmpty(current))
            {
                paths.Add(current);
                if (string.Equals(current.TrimEnd(Path.DirectorySeparatorChar), root.TrimEnd(Path.DirectorySeparatorChar), StringComparison.OrdinalIgnoreCase))
                    break;
                current = Path.GetDirectoryName(current);
            }
            paths.Reverse();
            List<SafeFileHandle> handles = new List<SafeFileHandle>();
            try
            {
                foreach (string directory in paths)
                {
                    SafeFileHandle handle = CreateFileW(
                        directory,
                        FileReadAttributes,
                        FileShareRead,
                        IntPtr.Zero,
                        OpenExisting,
                        FileFlagBackupSemantics | FileFlagOpenReparsePoint,
                        IntPtr.Zero);
                    if (handle.IsInvalid)
                    {
                        int error = Marshal.GetLastWin32Error();
                        handle.Dispose();
                        throw NativeError("open a physical qualification ancestor", error);
                    }
                    ByHandleFileInformation information = Information(handle, "inspect a qualification ancestor");
                    if ((information.FileAttributes & FileAttributeDirectory) == 0 ||
                        (information.FileAttributes & FileAttributeReparsePoint) != 0)
                    {
                        handle.Dispose();
                        throw new InvalidOperationException("Qualification path crosses a link or reparse point.");
                    }
                    VerifyFinalPath(handle, directory, true);
                    handles.Add(handle);
                }
                return handles;
            }
            catch
            {
                for (int index = handles.Count - 1; index >= 0; index--) handles[index].Dispose();
                throw;
            }
        }

        private static string CanonicalLocalPath(string suppliedPath)
        {
            if (string.IsNullOrWhiteSpace(suppliedPath))
                throw new ArgumentException("Qualification path is empty.", nameof(suppliedPath));
            string path = Path.GetFullPath(suppliedPath);
            string root = Path.GetPathRoot(path);
            if (string.IsNullOrEmpty(root) || root.Length != 3 || root[1] != ':' || root[2] != Path.DirectorySeparatorChar)
                throw new InvalidOperationException("Qualification paths must use a local drive-rooted Windows path.");
            if (path.Substring(root.Length).IndexOf(':') >= 0)
                throw new InvalidOperationException("Qualification paths must not select an alternate data stream.");
            if (path.Length > 1024)
                throw new PathTooLongException("Qualification path exceeds its native bound.");
            return path;
        }

        private static void VerifyFinalPath(SafeFileHandle handle, string expected, bool directory)
        {
            StringBuilder value = new StringBuilder(1100);
            uint length = GetFinalPathNameByHandleW(handle, value, checked((uint)value.Capacity), 0);
            if (length == 0 || length >= value.Capacity)
                throw NativeError("resolve a qualification file identity", Marshal.GetLastWin32Error());
            string observed = value.ToString();
            if (observed.StartsWith(@"\\?\", StringComparison.Ordinal)) observed = observed.Substring(4);
            string left = Path.GetFullPath(observed);
            string right = Path.GetFullPath(expected);
            if (directory)
            {
                left = left.TrimEnd(Path.DirectorySeparatorChar);
                right = right.TrimEnd(Path.DirectorySeparatorChar);
            }
            if (!string.Equals(left, right, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("Qualification path does not name its bound physical identity.");
        }

        private static bool SameIdentity(ByHandleFileInformation left, ByHandleFileInformation right)
        {
            return left.FileAttributes == right.FileAttributes &&
                left.LastWriteTime.Low == right.LastWriteTime.Low &&
                left.LastWriteTime.High == right.LastWriteTime.High &&
                left.VolumeSerialNumber == right.VolumeSerialNumber &&
                left.FileSizeHigh == right.FileSizeHigh &&
                left.FileSizeLow == right.FileSizeLow &&
                left.NumberOfLinks == right.NumberOfLinks &&
                left.FileIndexHigh == right.FileIndexHigh &&
                left.FileIndexLow == right.FileIndexLow;
        }

        private static ByHandleFileInformation Information(SafeFileHandle handle, string operation)
        {
            ByHandleFileInformation value;
            if (!GetFileInformationByHandle(handle, out value))
                throw NativeError(operation, Marshal.GetLastWin32Error());
            return value;
        }

        private static void ValidateOnlyUnnamedStream(SafeFileHandle handle, ulong expectedLength)
        {
            IntPtr buffer = Marshal.AllocHGlobal(StreamBufferBytes);
            try
            {
                if (!GetFileInformationByHandleEx(handle, FileStreamInfo, buffer, StreamBufferBytes))
                    throw NativeError("enumerate qualification input streams", Marshal.GetLastWin32Error());
                int offset = 0;
                int count = 0;
                while (true)
                {
                    if (offset < 0 || offset > StreamBufferBytes - StreamHeaderBytes)
                        throw new InvalidOperationException("Qualification stream metadata is malformed.");
                    uint nextOffset = unchecked((uint)Marshal.ReadInt32(buffer, offset));
                    uint nameBytes = unchecked((uint)Marshal.ReadInt32(buffer, offset + 4));
                    long streamSize = Marshal.ReadInt64(buffer, offset + 8);
                    if ((nameBytes & 1) != 0 || nameBytes > StreamBufferBytes - offset - StreamHeaderBytes)
                        throw new InvalidOperationException("Qualification stream metadata exceeds its bound.");
                    string streamName = Marshal.PtrToStringUni(IntPtr.Add(buffer, offset + StreamHeaderBytes), checked((int)(nameBytes / 2)));
                    count = checked(count + 1);
                    if (count != 1 || !string.Equals(streamName, "::$DATA", StringComparison.Ordinal) ||
                        streamSize < 0 || (ulong)streamSize != expectedLength)
                    {
                        throw new InvalidOperationException("Qualification input must contain only its unnamed data stream.");
                    }
                    if (nextOffset == 0) break;
                    if (nextOffset < StreamHeaderBytes + nameBytes || nextOffset > StreamBufferBytes - offset)
                        throw new InvalidOperationException("Qualification stream metadata has an invalid next offset.");
                    offset = checked(offset + (int)nextOffset);
                }
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static Win32Exception NativeError(string operation, int error)
        {
            return new Win32Exception(error, "Could not " + operation + ".");
        }
    }

    public static class StrictJson
    {
        public static byte[] Canonicalize(byte[] bytes)
        {
            JsonDocumentOptions options = new JsonDocumentOptions
            {
                AllowTrailingCommas = false,
                CommentHandling = JsonCommentHandling.Disallow,
                MaxDepth = 64
            };
            using (JsonDocument document = JsonDocument.Parse(bytes, options))
            using (MemoryStream output = new MemoryStream())
            {
                JsonWriterOptions writerOptions = new JsonWriterOptions
                {
                    Encoder = JavaScriptEncoder.Default,
                    Indented = false,
                    SkipValidation = false
                };
                using (Utf8JsonWriter writer = new Utf8JsonWriter(output, writerOptions))
                {
                    WriteElement(writer, document.RootElement);
                }
                output.WriteByte((byte)'\n');
                return output.ToArray();
            }
        }

        public static byte[] CanonicalizeText(string text)
        {
            return Canonicalize(new UTF8Encoding(false, true).GetBytes(text));
        }

        public static bool Equal(byte[] left, byte[] right)
        {
            if (left == null || right == null || left.Length != right.Length) return false;
            return CryptographicOperations.FixedTimeEquals(left, right);
        }

        private static void WriteElement(Utf8JsonWriter writer, JsonElement element)
        {
            switch (element.ValueKind)
            {
                case JsonValueKind.Object:
                    writer.WriteStartObject();
                    List<JsonProperty> properties = new List<JsonProperty>();
                    HashSet<string> names = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
                    foreach (JsonProperty property in element.EnumerateObject())
                    {
                        if (!names.Add(property.Name))
                            throw new InvalidDataException("JSON object contains a duplicate or case-colliding field.");
                        ValidatePrintableAscii(property.Name, "JSON field name");
                        properties.Add(property);
                    }
                    properties.Sort((left, right) => StringComparer.Ordinal.Compare(left.Name, right.Name));
                    foreach (JsonProperty property in properties)
                    {
                        writer.WritePropertyName(property.Name);
                        WriteElement(writer, property.Value);
                    }
                    writer.WriteEndObject();
                    break;
                case JsonValueKind.Array:
                    writer.WriteStartArray();
                    foreach (JsonElement item in element.EnumerateArray()) WriteElement(writer, item);
                    writer.WriteEndArray();
                    break;
                case JsonValueKind.String:
                    string value = element.GetString();
                    ValidatePrintableAscii(value, "JSON string");
                    writer.WriteStringValue(value);
                    break;
                case JsonValueKind.Number:
                    long integer;
                    if (!element.TryGetInt64(out integer))
                        throw new InvalidDataException("Qualification JSON accepts bounded integers only.");
                    writer.WriteNumberValue(integer);
                    break;
                case JsonValueKind.True:
                    writer.WriteBooleanValue(true);
                    break;
                case JsonValueKind.False:
                    writer.WriteBooleanValue(false);
                    break;
                case JsonValueKind.Null:
                    writer.WriteNullValue();
                    break;
                default:
                    throw new InvalidDataException("Qualification JSON contains an unsupported value.");
            }
        }

        private static void ValidatePrintableAscii(string value, string label)
        {
            if (value == null) throw new InvalidDataException(label + " is null.");
            foreach (char character in value)
            {
                if (character < 0x20 || character > 0x7e)
                    throw new InvalidDataException(label + " must contain printable ASCII only.");
            }
        }
    }
}
'@
}

function Fail([string]$Message) {
    throw [IO.InvalidDataException]::new($Message)
}

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) { Fail $Message }
}

function Assert-Object($Value, [string]$Label) {
    Assert-Condition ($Value -is [Collections.IDictionary]) "$Label must be an object."
}

function Assert-Array($Value, [string]$Label) {
    Assert-Condition ($Value -is [Collections.IList] -and $Value -isnot [string]) "$Label must be an array."
}

function Assert-ExactKeys($Value, [string[]]$Expected, [string]$Label) {
    Assert-Object $Value $Label
    $actual = @($Value.Keys | Sort-Object -CaseSensitive)
    $wanted = @($Expected | Sort-Object -CaseSensitive)
    Assert-Condition ($actual.Count -eq $wanted.Count) "$Label has unexpected or missing fields."
    for ($index = 0; $index -lt $wanted.Count; $index++) {
        Assert-Condition ($actual[$index] -ceq $wanted[$index]) "$Label has unexpected or missing fields."
    }
}

function Get-JsonString($Value, [string]$Label, [int]$Maximum = 256) {
    Assert-Condition ($Value -is [string] -and $Value.Length -gt 0 -and $Value.Length -le $Maximum) "$Label must be a nonempty bounded JSON string."
    Assert-Condition ($Value -cmatch '^[\x20-\x7e]+$') "$Label must contain printable ASCII only."
    return [string]$Value
}

function Get-JsonInteger($Value, [string]$Label, [Int64]$Minimum = 0, [Int64]$Maximum = [Int64]::MaxValue) {
    $integerTypes = @([byte], [sbyte], [int16], [uint16], [int32], [uint32], [int64])
    Assert-Condition ($null -ne $Value -and $Value -isnot [bool] -and $integerTypes -contains $Value.GetType()) "$Label must be a JSON integer."
    $result = [Int64]$Value
    Assert-Condition ($result -ge $Minimum -and $result -le $Maximum) "$Label must be a bounded JSON integer."
    return $result
}

function Get-JsonBoolean($Value, [string]$Label) {
    Assert-Condition ($Value -is [bool]) "$Label must be a JSON boolean."
    return [bool]$Value
}

function Get-Sha256Value($Value, [string]$Label, [bool]$AllowZero = $false) {
    $digest = Get-JsonString $Value $Label 64
    Assert-Condition ($digest -cmatch '^[0-9a-f]{64}$' -and ($AllowZero -or $digest -cne $ZeroSha256)) "$Label must be a lowercase nonzero SHA-256 digest."
    return $digest
}

function Get-Identifier($Value, [string]$Label, [int]$Maximum = 160) {
    $result = Get-JsonString $Value $Label $Maximum
    Assert-Condition ($result -cmatch '^[a-z0-9][a-z0-9._:-]*$') "$Label is not a canonical identifier."
    return $result
}

function Get-PackComponent($Value, [string]$Label) {
    $result = Get-JsonString $Value $Label 96
    Assert-Condition ($result -cmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$') "$Label is not a canonical pack component."
    return $result
}

function Get-BoundBytes([string]$Path, [string]$Label, [int]$MaximumBytes = $MaxInputBytes) {
    try {
        return ,([ScribeWindowsQualification.NativeFile]::ReadBound([IO.Path]::GetFullPath($Path), $MaximumBytes))
    }
    catch {
        Fail "Could not read $Label through the Windows qualification boundary: $($_.Exception.Message)"
    }
}

function Get-Sha256Bytes([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Get-CanonicalBytesFromObject($Value) {
    $json = $Value | ConvertTo-Json -Compress -Depth 64
    return ,([ScribeWindowsQualification.StrictJson]::CanonicalizeText($json))
}

function Get-CanonicalDigest($Value) {
    [byte[]]$bytes = Get-CanonicalBytesFromObject $Value
    return Get-Sha256Bytes $bytes
}

function Get-CanonicalBase64($Value, [string]$Label, [int]$MaximumBytes) {
    $encoded = Get-JsonString $Value $Label ([Math]::Max(4, (($MaximumBytes + 2) / 3) * 4))
    try { [byte[]]$bytes = [Convert]::FromBase64String($encoded) }
    catch { Fail "$Label must be canonical base64." }
    Assert-Condition ($bytes.Length -le $MaximumBytes -and [Convert]::ToBase64String($bytes) -ceq $encoded) "$Label must be canonical base64."
    return ,$bytes
}

function Get-CaptureContractProjection($Plan, $Identity) {
    return [ordered]@{
        campaign_nonce = $Plan.capture_authority.campaign_nonce
        capture_contract = $Plan.capture_contract
        capture_key_id = $Plan.capture_authority.capture_key_id
        contract_bindings = $Plan.contract_bindings
        lane_identity = $Identity
        required_lane_identities = @($Plan.required_lanes | ForEach-Object { $_.identity })
        policy = [ordered]@{
            cold_runs = $Plan.cold_runs
            fixture_only = $Plan.fixture_only
            kind = $Plan.kind
            maximum_gpu_p95_cpu_percent = $Plan.maximum_gpu_p95_cpu_percent
            required_scenarios = $Plan.required_scenarios
            runtime_bucket_complete = $Plan.runtime_bucket_complete
            schema_version = $Plan.schema_version
            target_arch = $Plan.target_arch
            target_os = $Plan.target_os
            warm_runs = $Plan.warm_runs
        }
        schema_version = 1
    }
}

function Get-AttestationPreimage([byte[]]$CanonicalRecord) {
    [byte[]]$length = [BitConverter]::GetBytes([UInt64]$CanonicalRecord.Length)
    if (-not [BitConverter]::IsLittleEndian) { [Array]::Reverse($length) }
    [byte[]]$preimage = [byte[]]::new($AttestationDomain.Length + 8 + $CanonicalRecord.Length)
    [Array]::Copy($AttestationDomain, 0, $preimage, 0, $AttestationDomain.Length)
    [Array]::Copy($length, 0, $preimage, $AttestationDomain.Length, 8)
    [Array]::Copy($CanonicalRecord, 0, $preimage, $AttestationDomain.Length + 8, $CanonicalRecord.Length)
    return ,$preimage
}

function Import-P256PublicKey($Value, [string]$ExpectedKeyId, [string]$Label) {
    [byte[]]$spki = Get-CanonicalBase64 $Value "$Label SPKI" 256
    $keyId = "p256:$(Get-Sha256Bytes $spki)"
    Assert-Condition ($keyId -ceq $ExpectedKeyId) "$Label SPKI does not match capture_key_id."
    $ecdsa = [Security.Cryptography.ECDsa]::Create()
    try {
        [int]$read = 0
        $ecdsa.ImportSubjectPublicKeyInfo($spki, [ref]$read)
        Assert-Condition ($read -eq $spki.Length) "$Label SPKI has trailing bytes."
        $parameters = $ecdsa.ExportParameters($false)
        Assert-Condition ($ecdsa.KeySize -eq 256 -and $parameters.Curve.Oid.Value -ceq '1.2.840.10045.3.1.7' -and $parameters.Q.X.Length -eq 32 -and $parameters.Q.Y.Length -eq 32) "$Label key must be NIST P-256."
        [byte[]]$roundTrip = $ecdsa.ExportSubjectPublicKeyInfo()
        Assert-Condition ([Security.Cryptography.CryptographicOperations]::FixedTimeEquals($spki, $roundTrip)) "$Label SPKI is not the canonical P-256 encoding."
        return $ecdsa
    }
    catch {
        $ecdsa.Dispose()
        if ($_.Exception -is [IO.InvalidDataException]) { throw }
        Fail "$Label SPKI is invalid: $($_.Exception.Message)"
    }
}

function Import-CanonicalJson([string]$Path, [string]$Label) {
    [byte[]]$raw = Get-BoundBytes $Path $Label
    try {
        [byte[]]$canonical = [ScribeWindowsQualification.StrictJson]::Canonicalize($raw)
    }
    catch {
        Fail "$Label is invalid strict JSON: $($_.Exception.Message)"
    }
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($raw, $canonical)) "$Label is not canonical JSON."
    try {
        $document = [Text.Encoding]::UTF8.GetString($raw) | ConvertFrom-Json -AsHashtable -Depth 64
    }
    catch {
        Fail "$Label could not be parsed: $($_.Exception.Message)"
    }
    Assert-Object $document $Label
    return [pscustomobject]@{ Document = $document; Raw = $raw }
}

function Test-StableDeviceId([string]$Value) {
    return $Value -cmatch '^native:[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$' -or
        $Value -cmatch '^native:luid:[0-9a-f]{16}$' -or
        $Value -cmatch '^native:uuid:[0-9a-f]{32}$'
}

function Test-WindowsDisplayDriver([string]$Value) {
    if ($Value -cnotmatch '^windows-display:[0-9]{1,5}(?:\.[0-9]{1,5}){1,7}$') { return $false }
    foreach ($component in $Value.Substring('windows-display:'.Length).Split('.')) {
        [UInt32]$parsed = 0
        if (-not [UInt32]::TryParse($component, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$parsed) -or $parsed -gt [UInt16]::MaxValue) { return $false }
    }
    return $true
}

function Test-DriverIdentity([string]$Backend, [string]$Value) {
    if ($Backend -ceq 'cuda') { return Test-WindowsDisplayDriver $Value }
    if ($Backend -ceq 'vulkan') {
        return (Test-WindowsDisplayDriver $Value) -or
            $Value -cmatch '^vulkan:[0-9a-f]{4}:[0-9a-f]{8}:[0-9a-f]{8}:[0-9a-f]{32}$'
    }
    return $false
}

function Test-DriverVendorBinding([string]$Backend, [string]$Value, [string]$Vendor) {
    if (-not (Test-DriverIdentity $Backend $Value)) { return $false }
    if ($Backend -ceq 'cuda') { return $Vendor -ceq 'nvidia' }
    if ($Value.StartsWith('windows-display:')) { return @('nvidia', 'amd', 'intel') -ccontains $Vendor }
    $vendorId = $Value.Split(':')[1]
    $expectedVendor = switch ($vendorId) { '10de' { 'nvidia' } '1002' { 'amd' } '1022' { 'amd' } '8086' { 'intel' } default { '' } }
    return $expectedVendor -ceq $Vendor
}

function Assert-Pack($Pack, [string]$Label) {
    Assert-ExactKeys $Pack @('pack_id', 'pack_version', 'pack_digest', 'security_epoch', 'runtime_abi') $Label
    $null = Get-PackComponent $Pack.pack_id "$Label.pack_id"
    $null = Get-PackComponent $Pack.pack_version "$Label.pack_version"
    $null = Get-Sha256Value $Pack.pack_digest "$Label.pack_digest"
    $null = Get-JsonInteger $Pack.security_epoch "$Label.security_epoch" 1 ([uint32]::MaxValue)
    $null = Get-JsonInteger $Pack.runtime_abi "$Label.runtime_abi" 1 ([uint16]::MaxValue)
}

function Assert-Worker($Worker, [string]$Label, [bool]$Cpu) {
    Assert-ExactKeys $Worker @('backend', 'provider_id', 'worker_build_id', 'worker_sha256', 'protocol_version', 'runtime_abi') $Label
    $backend = Get-JsonString $Worker.backend "$Label.backend" 16
    $provider = Get-Identifier $Worker.provider_id "$Label.provider_id"
    if ($Cpu) {
        Assert-Condition ($backend -ceq 'cpu' -and $provider -ceq 'cpu') "$Label must identify the reviewed CPU baseline."
    }
    else {
        Assert-Condition ($backend -ceq $provider -and @('cuda', 'vulkan') -ccontains $backend) "$Label provider must match its compiled GPU backend."
    }
    $null = Get-JsonString $Worker.worker_build_id "$Label.worker_build_id" 160
    $null = Get-Sha256Value $Worker.worker_sha256 "$Label.worker_sha256"
    Assert-Condition ((Get-JsonInteger $Worker.protocol_version "$Label.protocol_version" 1 255) -eq 5) "$Label.protocol_version must match protocol 5."
    $null = Get-JsonInteger $Worker.runtime_abi "$Label.runtime_abi" 1 ([uint16]::MaxValue)
}

function Assert-DeviceSet($DeviceSet, [string]$Label, [string]$SelectedStableId, [string]$SelectedVendor, [string]$SelectedClass, [string]$SelectedDriver, [Int64]$SelectedTotalMemory, [string]$Backend) {
    Assert-ExactKeys $DeviceSet @('snapshot_sha256', 'device_count', 'mixed_gpu', 'devices') $Label
    $snapshotDigest = Get-Sha256Value $DeviceSet.snapshot_sha256 "$Label.snapshot_sha256"
    $count = Get-JsonInteger $DeviceSet.device_count "$Label.device_count" 1 16
    $mixed = Get-JsonBoolean $DeviceSet.mixed_gpu "$Label.mixed_gpu"
    Assert-Array $DeviceSet.devices "$Label.devices"
    [object[]]$devices = @($DeviceSet.devices)
    Assert-Condition ($devices.Count -eq $count) "$Label.device_count does not match devices."
    Assert-Condition ($snapshotDigest -ceq (Get-CanonicalDigest $devices)) "$Label.snapshot_sha256 does not match the canonical complete device inventory."
    $previous = ''
    $selectedFound = $false
    $diversity = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($device in $devices) {
        Assert-ExactKeys $device @('stable_device_id', 'vendor', 'device_class', 'provider_eligible', 'driver', 'total_memory_bytes') "$Label device"
        $stable = Get-JsonString $device.stable_device_id "$Label device stable_device_id" 64
        Assert-Condition (Test-StableDeviceId $stable) "$Label contains a noncanonical stable device identity."
        Assert-Condition ([StringComparer]::Ordinal.Compare($stable, $previous) -gt 0) "$Label devices must be strictly sorted and unique."
        $previous = $stable
        $vendor = Get-JsonString $device.vendor "$Label device vendor" 16
        Assert-Condition (@('nvidia', 'amd', 'intel') -ccontains $vendor) "$Label contains an unsupported vendor."
        $class = Get-JsonString $device.device_class "$Label device device_class" 32
        Assert-Condition (@('discrete_gpu', 'integrated_gpu', 'unified_gpu') -ccontains $class) "$Label contains an unsupported device class."
        $providerEligible = Get-JsonBoolean $device.provider_eligible "$Label device provider_eligible"
        $deviceDriver = Get-JsonString $device.driver "$Label device driver" 128
        $null = Get-JsonInteger $device.total_memory_bytes "$Label device total_memory_bytes" $MinimumGpuMemoryBytes
        $null = $diversity.Add("$vendor/$class")
        if ($stable -ceq $SelectedStableId) {
            $selectedFound = $true
            Assert-Condition ($vendor -ceq $SelectedVendor) "$Label selected-device vendor does not match identity.device.vendor."
            Assert-Condition ($class -ceq $SelectedClass) "$Label selected-device class does not match identity.device.device_class."
            Assert-Condition $providerEligible "$Label selected device must be eligible for the candidate provider."
            Assert-Condition ($deviceDriver -ceq $SelectedDriver) "$Label selected-device driver does not match identity.driver.value."
            Assert-Condition ($device.total_memory_bytes -eq $SelectedTotalMemory) "$Label selected-device total memory does not match identity.device.total_memory_bytes."
        }
        if ($providerEligible) {
            Assert-Condition (Test-DriverVendorBinding $Backend $deviceDriver $vendor) "$Label provider-eligible device driver does not match its backend and vendor."
        }
        else {
            Assert-Condition ($deviceDriver -ceq 'none') "$Label provider-ineligible device must use the canonical no-driver form."
        }
    }
    Assert-Condition $selectedFound "$Label omits the selected stable device."
    Assert-Condition ($mixed -eq ($diversity.Count -gt 1)) "$Label.mixed_gpu does not match actual vendor or device-class diversity."
}

function Assert-Acquisition($Acquisition, [string]$Label, [string]$StableId, [string]$Vendor, [string]$DeviceClass, [string]$Driver, [Int64]$TotalMemory, [string]$Backend) {
    Assert-ExactKeys $Acquisition @('protocol', 'batch_id', 'machine_id_sha256', 'options_sha256', 'host', 'threading', 'controls', 'ordering', 'device_set', 'telemetry') $Label
    Assert-ExactKeys $Acquisition.protocol @('protocol_id', 'protocol_version', 'harness_sha256') "$Label.protocol"
    Assert-Condition ((Get-Identifier $Acquisition.protocol.protocol_id "$Label.protocol.protocol_id") -ceq 'scribe-windows-gpu-qualification') "$Label protocol ID is unsupported."
    Assert-Condition ((Get-JsonInteger $Acquisition.protocol.protocol_version "$Label.protocol.protocol_version" 1 1) -eq 1) "$Label protocol version is unsupported."
    $null = Get-Sha256Value $Acquisition.protocol.harness_sha256 "$Label.protocol.harness_sha256"
    $null = Get-Identifier $Acquisition.batch_id "$Label.batch_id"
    $null = Get-Sha256Value $Acquisition.machine_id_sha256 "$Label.machine_id_sha256"
    $null = Get-Sha256Value $Acquisition.options_sha256 "$Label.options_sha256"
    Assert-ExactKeys $Acquisition.host @('cpu_arch', 'cpu_model_sha256', 'physical_cores', 'logical_cpus', 'total_memory_bytes') "$Label.host"
    Assert-Condition ((Get-JsonString $Acquisition.host.cpu_arch "$Label.host.cpu_arch" 16) -ceq 'x86_64') "$Label host architecture must be x86_64."
    $null = Get-Sha256Value $Acquisition.host.cpu_model_sha256 "$Label.host.cpu_model_sha256"
    $physical = Get-JsonInteger $Acquisition.host.physical_cores "$Label.host.physical_cores" 1 4096
    $logical = Get-JsonInteger $Acquisition.host.logical_cpus "$Label.host.logical_cpus" 1 8192
    Assert-Condition ($physical -le $logical) "$Label host topology is inconsistent."
    $null = Get-JsonInteger $Acquisition.host.total_memory_bytes "$Label.host.total_memory_bytes" 1GB
    Assert-ExactKeys $Acquisition.threading @('cpu_worker_threads', 'gpu_worker_threads', 'cpu_affinity_sha256', 'gpu_affinity_sha256') "$Label.threading"
    $null = Get-JsonInteger $Acquisition.threading.cpu_worker_threads "$Label.threading.cpu_worker_threads" 1 $logical
    $null = Get-JsonInteger $Acquisition.threading.gpu_worker_threads "$Label.threading.gpu_worker_threads" 1 $logical
    $null = Get-Sha256Value $Acquisition.threading.cpu_affinity_sha256 "$Label.threading.cpu_affinity_sha256"
    $null = Get-Sha256Value $Acquisition.threading.gpu_affinity_sha256 "$Label.threading.gpu_affinity_sha256"
    Assert-ExactKeys $Acquisition.controls @('power_source', 'power_plan_sha256', 'gpu_power_profile', 'thermal_policy', 'background_load_policy') "$Label.controls"
    Assert-Condition ((Get-JsonString $Acquisition.controls.power_source "$Label.controls.power_source" 16) -ceq 'ac') "$Label benchmarks must be acquired on AC power."
    $null = Get-Sha256Value $Acquisition.controls.power_plan_sha256 "$Label.controls.power_plan_sha256"
    Assert-Condition ((Get-JsonString $Acquisition.controls.gpu_power_profile "$Label.controls.gpu_power_profile" 64) -ceq 'fixed_maximum_performance') "$Label GPU power profile violates protocol v1."
    Assert-Condition ((Get-JsonString $Acquisition.controls.thermal_policy "$Label.controls.thermal_policy" 64) -ceq 'no_throttling_observed') "$Label thermal policy violates protocol v1."
    Assert-Condition ((Get-JsonString $Acquisition.controls.background_load_policy "$Label.controls.background_load_policy" 64) -ceq 'isolated') "$Label background-load policy violates protocol v1."
    Assert-ExactKeys $Acquisition.ordering @('scheme', 'warm_priming_runs') "$Label.ordering"
    Assert-Condition ((Get-JsonString $Acquisition.ordering.scheme "$Label.ordering.scheme" 64) -ceq 'paired_alternating_cpu_first_v1') "$Label ordering violates protocol v1."
    Assert-Condition ((Get-JsonInteger $Acquisition.ordering.warm_priming_runs "$Label.ordering.warm_priming_runs" 0 16) -eq 1) "$Label warm priming violates protocol v1."
    Assert-DeviceSet $Acquisition.device_set "$Label.device_set" $StableId $Vendor $DeviceClass $Driver $TotalMemory $Backend
    Assert-ExactKeys $Acquisition.telemetry @('source', 'scope', 'sample_interval_ms') "$Label.telemetry"
    Assert-Condition ((Get-JsonString $Acquisition.telemetry.source "$Label.telemetry.source" 64) -ceq 'windows_counters_and_provider') "$Label telemetry source is unsupported."
    Assert-Condition ((Get-JsonString $Acquisition.telemetry.scope "$Label.telemetry.scope" 64) -ceq 'worker_process_and_selected_device') "$Label telemetry scope is unsupported."
    $null = Get-JsonInteger $Acquisition.telemetry.sample_interval_ms "$Label.telemetry.sample_interval_ms" 1 1000
}

function Assert-Identity($Identity, [string]$Label, [string]$ExpectedAppVersion = '') {
    Assert-ExactKeys $Identity @(
        'lane_id', 'app_build_id', 'windows_version', 'target_arch', 'backend', 'provider_id',
        'cpu_baseline', 'gpu_worker', 'acquisition', 'pack', 'model', 'workload',
        'device', 'driver', 'installation'
    ) $Label
    $null = Get-Identifier $Identity.lane_id "$Label.lane_id"
    $appBuild = Get-JsonString $Identity.app_build_id "$Label.app_build_id" 160
    Assert-Condition ($appBuild -cmatch '^local-transcriber@((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)#([0-9a-f]{40})$') "$Label.app_build_id is not a concrete SemVer desktop build identity."
    $buildVersion = $Matches[1]
    $buildRevision = $Matches[2]
    if (-not [string]::IsNullOrEmpty($ExpectedAppVersion)) {
        Assert-Condition ($buildVersion -ceq $ExpectedAppVersion) "$Label app build version does not match the bound Windows toolchain."
    }
    $windowsVersion = Get-JsonString $Identity.windows_version "$Label.windows_version" 32
    Assert-Condition ($windowsVersion -cmatch '^10\.0\.[0-9]{4,6}$') "$Label.windows_version is not a reviewed Windows build identity."
    Assert-Condition ((Get-JsonString $Identity.target_arch "$Label.target_arch" 16) -ceq 'x86_64') "$Label.target_arch must be x86_64."
    $backend = Get-JsonString $Identity.backend "$Label.backend" 16
    $provider = Get-Identifier $Identity.provider_id "$Label.provider_id"
    Assert-Worker $Identity.cpu_baseline "$Label.cpu_baseline" $true
    Assert-Worker $Identity.gpu_worker "$Label.gpu_worker" $false
    foreach ($worker in @($Identity.cpu_baseline, $Identity.gpu_worker)) {
        Assert-Condition ($worker.worker_build_id -ceq "scribe-inference-worker@$buildVersion#$buildRevision") "$Label worker build must use the desktop version and revision."
    }
    Assert-Pack $Identity.pack "$Label.pack"
    Assert-ExactKeys $Identity.model @('model_id', 'model_digest') "$Label.model"
    $null = Get-Identifier $Identity.model.model_id "$Label.model.model_id"
    $null = Get-Sha256Value $Identity.model.model_digest "$Label.model.model_digest"
    Assert-ExactKeys $Identity.workload @('workload_id', 'audio_sha256', 'expected_transcript_sha256') "$Label.workload"
    $null = Get-Identifier $Identity.workload.workload_id "$Label.workload.workload_id"
    $null = Get-Sha256Value $Identity.workload.audio_sha256 "$Label.workload.audio_sha256"
    $null = Get-Sha256Value $Identity.workload.expected_transcript_sha256 "$Label.workload.expected_transcript_sha256"
    Assert-ExactKeys $Identity.device @('stable_device_id', 'vendor', 'device_class', 'memory_model', 'total_memory_bytes', 'qualified_minimum_total_memory_bytes', 'qualified_minimum_available_memory_bytes') "$Label.device"
    $stable = Get-JsonString $Identity.device.stable_device_id "$Label.device.stable_device_id" 64
    Assert-Condition (Test-StableDeviceId $stable) "$Label.device.stable_device_id is not canonical."
    $vendor = Get-JsonString $Identity.device.vendor "$Label.device.vendor" 16
    Assert-Condition (@('nvidia', 'amd', 'intel') -ccontains $vendor) "$Label.device.vendor is unsupported."
    $class = Get-JsonString $Identity.device.device_class "$Label.device.device_class" 32
    Assert-Condition (@('discrete_gpu', 'integrated_gpu', 'unified_gpu') -ccontains $class) "$Label.device.device_class is unsupported."
    $memoryModel = Get-JsonString $Identity.device.memory_model "$Label.device.memory_model" 32
    $expectedMemoryModel = if ($class -ceq 'discrete_gpu') { 'dedicated_vram' } else { 'shared_host_memory' }
    Assert-Condition ($memoryModel -ceq $expectedMemoryModel) "$Label.device.memory_model does not match its class."
    $total = Get-JsonInteger $Identity.device.total_memory_bytes "$Label.device.total_memory_bytes" $MinimumGpuMemoryBytes
    $minimumTotal = Get-JsonInteger $Identity.device.qualified_minimum_total_memory_bytes "$Label.device.qualified_minimum_total_memory_bytes" $MinimumGpuMemoryBytes $total
    $null = Get-JsonInteger $Identity.device.qualified_minimum_available_memory_bytes "$Label.device.qualified_minimum_available_memory_bytes" $MinimumGpuMemoryBytes $minimumTotal
    Assert-Condition ($minimumTotal -eq $total) "$Label projected minimum total memory must equal the observed lane total memory."
    Assert-ExactKeys $Identity.driver @('kind', 'value') "$Label.driver"
    Assert-Condition ((Get-JsonString $Identity.driver.kind "$Label.driver.kind" 16) -ceq 'exact') "$Label.driver.kind must be exact."
    $driver = Get-JsonString $Identity.driver.value "$Label.driver.value" 128
    Assert-Condition (Test-DriverVendorBinding $backend $driver $vendor) "$Label.driver.value is not canonical for the selected Windows backend and vendor."
    Assert-ExactKeys $Identity.installation @('package_kind', 'package_sha256', 'catalog_sha256', 'clean_machine_image_sha256') "$Label.installation"
    Assert-Condition ((Get-JsonString $Identity.installation.package_kind "$Label.installation.package_kind" 16) -ceq 'installer') "$Label installation must use the Windows installer."
    $null = Get-Sha256Value $Identity.installation.package_sha256 "$Label.installation.package_sha256"
    $null = Get-Sha256Value $Identity.installation.catalog_sha256 "$Label.installation.catalog_sha256"
    $null = Get-Sha256Value $Identity.installation.clean_machine_image_sha256 "$Label.installation.clean_machine_image_sha256"
    Assert-Acquisition $Identity.acquisition "$Label.acquisition" $stable $vendor $class $driver $total $backend

    $validBinding = ($backend -ceq 'cuda' -and $provider -ceq 'transcribe-cpp-ggml-cuda' -and $vendor -ceq 'nvidia') -or
        ($backend -ceq 'vulkan' -and $provider -ceq 'transcribe-cpp-ggml-vulkan' -and @('nvidia', 'amd', 'intel') -ccontains $vendor)
    Assert-Condition $validBinding "$Label has an invalid backend, provider, and vendor binding."
    Assert-Condition ($Identity.gpu_worker.backend -ceq $backend -and $Identity.gpu_worker.provider_id -ceq $backend) "$Label GPU worker does not match the compiled candidate."
    Assert-Condition ($Identity.gpu_worker.runtime_abi -eq $Identity.pack.runtime_abi) "$Label GPU worker ABI does not match the pack."
    Assert-Condition ($Identity.pack.pack_id -ceq "scribe-$backend-windows-x64") "$Label pack ID does not match the backend."
    Assert-Condition ($Identity.cpu_baseline.worker_sha256 -cne $Identity.gpu_worker.worker_sha256) "$Label CPU and GPU workers must be distinct."
}

function Import-ProductionAuthority([string]$RepositoryRoot) {
    $loaded = Import-CanonicalJson (Join-Path $RepositoryRoot $ProductionAuthorityPath) 'Windows GPU qualification production authority'
    $authority = $loaded.Document
    Assert-ExactKeys $authority @('schema_version', 'kind', 'approved_plans') 'Windows GPU qualification production authority'
    Assert-Condition ((Get-JsonInteger $authority.schema_version 'production authority.schema_version' 2 2) -eq 2) 'Production authority schema is unsupported.'
    Assert-Condition ((Get-JsonString $authority.kind 'production authority.kind') -ceq 'windows_gpu_qualification_production_authority') 'Production authority kind is unsupported.'
    Assert-Array $authority.approved_plans 'production authority.approved_plans'
    $approved = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    $previous = ''
    foreach ($entry in @($authority.approved_plans)) {
        Assert-ExactKeys $entry @('plan_sha256', 'capture_key_id', 'capture_public_key_spki_base64') 'production authority approved plan'
        $digest = Get-Sha256Value $entry.plan_sha256 'production authority approved plan.plan_sha256'
        Assert-Condition ([StringComparer]::Ordinal.Compare($digest, $previous) -gt 0) 'Production authority approvals must be strictly sorted and unique.'
        $previous = $digest
        $keyId = Get-JsonString $entry.capture_key_id 'production authority approved plan.capture_key_id' 69
        Assert-Condition ($keyId -cmatch '^p256:[0-9a-f]{64}$') 'Production authority capture key ID is invalid.'
        $key = Import-P256PublicKey $entry.capture_public_key_spki_base64 $keyId 'production authority approved plan capture key'
        $key.Dispose()
        $approved.Add($digest, $entry)
    }
    return [pscustomobject]@{ Approved = $approved; Digest = Get-Sha256Bytes $loaded.Raw }
}

function Assert-AutoEntry($Entry, [string]$Label) {
    Assert-ExactKeys $Entry @('pack', 'model_digest', 'backend', 'provider_id', 'vendor', 'device_class', 'minimum_total_memory_bytes', 'minimum_available_memory_bytes', 'driver', 'evidence') $Label
    Assert-Pack $Entry.pack "$Label.pack"
    $null = Get-Sha256Value $Entry.model_digest "$Label.model_digest"
    $backend = Get-JsonString $Entry.backend "$Label.backend" 16
    $provider = Get-Identifier $Entry.provider_id "$Label.provider_id"
    $vendor = Get-JsonString $Entry.vendor "$Label.vendor" 16
    $class = Get-JsonString $Entry.device_class "$Label.device_class" 32
    Assert-Condition (@('discrete_gpu', 'integrated_gpu', 'unified_gpu') -ccontains $class) "$Label device class is unsupported."
    Assert-Condition (($backend -ceq 'cuda' -and $provider -ceq 'transcribe-cpp-ggml-cuda' -and $vendor -ceq 'nvidia') -or ($backend -ceq 'vulkan' -and $provider -ceq 'transcribe-cpp-ggml-vulkan' -and @('nvidia', 'amd', 'intel') -ccontains $vendor)) "$Label backend binding is invalid."
    Assert-Condition ($Entry.pack.pack_id -ceq "scribe-$backend-windows-x64") "$Label pack ID does not match its backend."
    $total = Get-JsonInteger $Entry.minimum_total_memory_bytes "$Label.minimum_total_memory_bytes" 1
    $null = Get-JsonInteger $Entry.minimum_available_memory_bytes "$Label.minimum_available_memory_bytes" 1 $total
    Assert-ExactKeys $Entry.driver @('kind', 'value') "$Label.driver"
    Assert-Condition ((Get-JsonString $Entry.driver.kind "$Label.driver.kind" 16) -ceq 'exact') "$Label driver must be exact."
    $entryDriver = Get-JsonString $Entry.driver.value "$Label.driver.value" 128
    Assert-Condition (Test-DriverVendorBinding $backend $entryDriver $vendor) "$Label driver value is not canonical for its backend and vendor."
    Assert-ExactKeys $Entry.evidence @('id', 'cold_runs', 'warm_runs', 'gpu_p95_ms', 'cpu_p95_ms', 'correctness_verified', 'reliability_verified', 'cold_evidence_sha256', 'warm_evidence_sha256', 'transcript_parity_evidence_sha256') "$Label.evidence"
    $null = Get-Identifier $Entry.evidence.id "$Label.evidence.id"
    $null = Get-JsonInteger $Entry.evidence.cold_runs "$Label.evidence.cold_runs" 5 ([uint16]::MaxValue)
    $null = Get-JsonInteger $Entry.evidence.warm_runs "$Label.evidence.warm_runs" 20 ([uint16]::MaxValue)
    $gpuP95 = Get-JsonInteger $Entry.evidence.gpu_p95_ms "$Label.evidence.gpu_p95_ms" 1
    $cpuP95 = Get-JsonInteger $Entry.evidence.cpu_p95_ms "$Label.evidence.cpu_p95_ms" 1
    Assert-Condition (([Numerics.BigInteger]$gpuP95 * 100) -le ([Numerics.BigInteger]$cpuP95 * 110)) "$Label GPU p95 exceeds the Auto threshold."
    Assert-Condition (Get-JsonBoolean $Entry.evidence.correctness_verified "$Label.evidence.correctness_verified") "$Label correctness is not verified."
    Assert-Condition (Get-JsonBoolean $Entry.evidence.reliability_verified "$Label.evidence.reliability_verified") "$Label reliability is not verified."
    foreach ($field in @('cold_evidence_sha256', 'warm_evidence_sha256', 'transcript_parity_evidence_sha256')) { $null = Get-Sha256Value $Entry.evidence[$field] "$Label.evidence.$field" }
}

function Import-AutoManifest([byte[]]$Raw) {
    try { [byte[]]$canonical = [ScribeWindowsQualification.StrictJson]::Canonicalize($Raw) }
    catch { Fail "Windows Auto manifest is invalid JSON: $($_.Exception.Message)" }
    $manifest = [Text.Encoding]::UTF8.GetString($Raw) | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-ExactKeys $manifest @('schema_version', 'mode', 'target_os', 'target_arch', 'entries') 'Windows Auto manifest'
    Assert-Condition ((Get-JsonInteger $manifest.schema_version 'Windows Auto manifest.schema_version' 2 2) -eq 2) 'Windows Auto manifest schema is unsupported.'
    Assert-Condition ((Get-JsonString $manifest.mode 'Windows Auto manifest.mode') -ceq 'default_deny') 'Windows Auto manifest mode is unsupported.'
    Assert-Condition ((Get-JsonString $manifest.target_os 'Windows Auto manifest.target_os') -ceq 'windows' -and (Get-JsonString $manifest.target_arch 'Windows Auto manifest.target_arch') -ceq 'x86_64') 'Windows Auto manifest platform is unsupported.'
    Assert-Array $manifest.entries 'Windows Auto manifest.entries'
    foreach ($entry in @($manifest.entries)) { Assert-AutoEntry $entry 'Windows Auto manifest entry' }
    $canonicalEntries = @(
        foreach ($entry in @($manifest.entries)) {
            [ordered]@{
                pack = [ordered]@{ pack_id = $entry.pack.pack_id; pack_version = $entry.pack.pack_version; pack_digest = $entry.pack.pack_digest; security_epoch = $entry.pack.security_epoch; runtime_abi = $entry.pack.runtime_abi }
                model_digest = $entry.model_digest
                backend = $entry.backend
                provider_id = $entry.provider_id
                vendor = $entry.vendor
                device_class = $entry.device_class
                minimum_total_memory_bytes = $entry.minimum_total_memory_bytes
                minimum_available_memory_bytes = $entry.minimum_available_memory_bytes
                driver = [ordered]@{ kind = $entry.driver.kind; value = $entry.driver.value }
                evidence = [ordered]@{
                    id = $entry.evidence.id; cold_runs = $entry.evidence.cold_runs; warm_runs = $entry.evidence.warm_runs
                    gpu_p95_ms = $entry.evidence.gpu_p95_ms; cpu_p95_ms = $entry.evidence.cpu_p95_ms
                    correctness_verified = $entry.evidence.correctness_verified; reliability_verified = $entry.evidence.reliability_verified
                    cold_evidence_sha256 = $entry.evidence.cold_evidence_sha256; warm_evidence_sha256 = $entry.evidence.warm_evidence_sha256
                    transcript_parity_evidence_sha256 = $entry.evidence.transcript_parity_evidence_sha256
                }
            }
        }
    )
    $canonicalManifest = [ordered]@{ schema_version = $manifest.schema_version; mode = $manifest.mode; target_os = $manifest.target_os; target_arch = $manifest.target_arch; entries = $canonicalEntries }
    [byte[]]$expectedRaw = [Text.UTF8Encoding]::new($false).GetBytes(($canonicalManifest | ConvertTo-Json -Compress -Depth 64) + "`n")
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($Raw, $expectedRaw)) 'Windows Auto manifest is not canonical JSON.'
    return ,([object[]]@($manifest.entries))
}

function Assert-Plan($Plan, [string]$RepositoryRoot) {
    Assert-ExactKeys $Plan @('schema_version', 'kind', 'fixture_only', 'target_os', 'target_arch', 'cold_runs', 'warm_runs', 'maximum_gpu_p95_cpu_percent', 'runtime_bucket_complete', 'required_scenarios', 'contract_bindings', 'capture_contract', 'capture_authority', 'required_lanes') 'qualification plan'
    Assert-Condition ((Get-JsonInteger $Plan.schema_version 'qualification plan.schema_version' 2 2) -eq 2) 'Qualification plan schema is unsupported.'
    Assert-Condition ((Get-JsonString $Plan.kind 'qualification plan.kind') -ceq 'windows_gpu_release_qualification_plan') 'Qualification plan kind is unsupported.'
    $fixture = Get-JsonBoolean $Plan.fixture_only 'qualification plan.fixture_only'
    Assert-Condition ((Get-JsonString $Plan.target_os 'qualification plan.target_os') -ceq 'windows') 'Qualification plan must target Windows.'
    Assert-Condition ((Get-JsonString $Plan.target_arch 'qualification plan.target_arch') -ceq 'x86_64') 'Qualification plan must target x86_64.'
    Assert-Condition ((Get-JsonInteger $Plan.cold_runs 'qualification plan.cold_runs') -eq 5) 'Qualification plan must require exactly five cold runs.'
    Assert-Condition ((Get-JsonInteger $Plan.warm_runs 'qualification plan.warm_runs') -eq 20) 'Qualification plan must require exactly twenty warm runs.'
    Assert-Condition ((Get-JsonInteger $Plan.maximum_gpu_p95_cpu_percent 'qualification plan.maximum_gpu_p95_cpu_percent') -eq 110) 'Qualification plan must use the 110 percent p95 boundary.'
    $null = Get-JsonBoolean $Plan.runtime_bucket_complete 'qualification plan.runtime_bucket_complete'
    Assert-Array $Plan.required_scenarios 'qualification plan.required_scenarios'
    [object[]]$scenarios = @($Plan.required_scenarios)
    Assert-Condition ($scenarios.Count -eq $RequiredScenarios.Count) 'Qualification plan scenarios are incomplete.'
    for ($index = 0; $index -lt $RequiredScenarios.Count; $index++) { Assert-Condition ($scenarios[$index] -ceq $RequiredScenarios[$index]) 'Qualification plan scenarios are not canonical.' }
    Assert-ExactKeys $Plan.capture_contract @('artifact_targets', 'cold_captures_per_target', 'control_kind', 'header_bytes', 'launch_scopes', 'max_control_body_bytes', 'protocol_magic', 'protocol_version', 'request_id', 'session_id', 'warm_captures_per_target') 'qualification plan.capture_contract'
    Assert-Array $Plan.capture_contract.artifact_targets 'qualification plan.capture_contract.artifact_targets'
    [byte[]]$actualTargets = Get-CanonicalBytesFromObject $Plan.capture_contract.artifact_targets
    [byte[]]$expectedTargets = Get-CanonicalBytesFromObject @([ordered]@{ artifact = 'gguf'; target = 'windows-x86_64' }, [ordered]@{ artifact = 'onnx_asr'; target = 'windows-x86_64' })
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($actualTargets, $expectedTargets)) 'Qualification capture contract artifact targets must be exact and ordered.'
    Assert-Array $Plan.capture_contract.launch_scopes 'qualification plan.capture_contract.launch_scopes'
    Assert-Condition ((@($Plan.capture_contract.launch_scopes) -join ',') -ceq 'cpu,provider_discovery,selected_device') 'Qualification capture contract launch scopes are not canonical.'
    Assert-Condition ((Get-JsonInteger $Plan.capture_contract.cold_captures_per_target 'qualification plan.capture_contract.cold_captures_per_target') -eq 5 -and (Get-JsonInteger $Plan.capture_contract.warm_captures_per_target 'qualification plan.capture_contract.warm_captures_per_target') -eq 1) 'Qualification capture generation counts are unsupported.'
    Assert-Condition ((Get-JsonInteger $Plan.capture_contract.header_bytes 'qualification plan.capture_contract.header_bytes') -eq 26 -and (Get-JsonInteger $Plan.capture_contract.max_control_body_bytes 'qualification plan.capture_contract.max_control_body_bytes') -eq 262144 -and (Get-JsonInteger $Plan.capture_contract.protocol_version 'qualification plan.capture_contract.protocol_version') -eq 5 -and (Get-JsonInteger $Plan.capture_contract.control_kind 'qualification plan.capture_contract.control_kind') -eq 1 -and (Get-JsonInteger $Plan.capture_contract.session_id 'qualification plan.capture_contract.session_id') -eq 0 -and (Get-JsonInteger $Plan.capture_contract.request_id 'qualification plan.capture_contract.request_id') -eq 0 -and (Get-JsonString $Plan.capture_contract.protocol_magic 'qualification plan.capture_contract.protocol_magic' 4) -ceq 'SCIF') 'Qualification capture wire contract is unsupported.'
    $captureAuthorityKeys = if ($fixture) { @('campaign_nonce', 'capture_key_id', 'fixture_capture_public_key_spki_base64') } else { @('campaign_nonce', 'capture_key_id') }
    Assert-ExactKeys $Plan.capture_authority $captureAuthorityKeys 'qualification plan.capture_authority'
    $nonce = Get-Sha256Value $Plan.capture_authority.campaign_nonce 'qualification plan.capture_authority.campaign_nonce' $true
    Assert-Condition ($nonce -cne $ZeroSha256) 'Qualification campaign nonce must be nonzero.'
    $captureKeyId = Get-JsonString $Plan.capture_authority.capture_key_id 'qualification plan.capture_authority.capture_key_id' 69
    Assert-Condition ($captureKeyId -cmatch '^p256:[0-9a-f]{64}$') 'Qualification capture key ID is invalid.'
    if ($fixture) {
        $fixtureKey = Import-P256PublicKey $Plan.capture_authority.fixture_capture_public_key_spki_base64 $captureKeyId 'fixture capture key'
        $fixtureKey.Dispose()
    }
    Assert-ExactKeys $Plan.contract_bindings @($ContractPaths.Keys) 'qualification plan.contract_bindings'
    $contracts = [ordered]@{}
    foreach ($field in $ContractPaths.Keys) {
        $expected = Get-Sha256Value $Plan.contract_bindings[$field] "qualification plan.contract_bindings.$field"
        [byte[]]$raw = Get-BoundBytes (Join-Path $RepositoryRoot $ContractPaths[$field]) "checked-in $field"
        Assert-Condition ((Get-Sha256Bytes $raw) -ceq $expected) "Qualification plan $field does not bind the checked-in contract."
        $contracts[$field] = $raw
    }
    try {
        $null = [ScribeWindowsQualification.StrictJson]::Canonicalize($contracts.toolchain_contract_sha256)
        $toolchain = [Text.UTF8Encoding]::new($false, $true).GetString($contracts.toolchain_contract_sha256) | ConvertFrom-Json -AsHashtable -Depth 64
    }
    catch { Fail "The bound Windows toolchain contract is invalid strict UTF-8/JSON: $($_.Exception.Message)" }
    Assert-Object $toolchain 'bound Windows toolchain contract'
    $toolchainAppVersion = Get-JsonString $toolchain.app_version 'bound Windows toolchain contract.app_version' 64
    Assert-Condition ($toolchainAppVersion -cmatch '^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$') 'The bound Windows toolchain app_version is not canonical SemVer.'
    Assert-Array $Plan.required_lanes 'qualification plan.required_lanes'
    [object[]]$required = @($Plan.required_lanes)
    Assert-Condition ($required.Count -le $MaxLanes) 'Qualification plan exceeds the representative-lane bound.'
    $previous = ''
    $digests = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $required) {
        Assert-ExactKeys $entry @('identity', 'evidence_sha256') 'required lane'
        Assert-Identity $entry.identity 'required lane identity' $toolchainAppVersion
        Assert-Condition ([StringComparer]::Ordinal.Compare([string]$entry.identity.lane_id, $previous) -gt 0) 'Required lanes must be strictly sorted and unique.'
        $previous = $entry.identity.lane_id
        $digest = Get-Sha256Value $entry.evidence_sha256 'required lane evidence_sha256'
        Assert-Condition ($digests.Add($digest)) 'Qualification plan reuses an evidence digest.'
    }
    return [pscustomobject]@{ Required = $required; Contracts = $contracts; CaptureKeyId = $captureKeyId }
}

function Get-ArtifactRelativePath($Value, [string]$Label) {
    $raw = Get-JsonString $Value $Label 240
    Assert-Condition (-not $raw.StartsWith('/') -and -not $raw.Contains('\') -and -not $raw.Contains(':')) "$Label must be a relative canonical artifact path."
    $parts = $raw.Split('/')
    Assert-Condition ($parts.Count -gt 0) "$Label must be a relative canonical artifact path."
    foreach ($part in $parts) { Assert-Condition ($part -cmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$') "$Label contains an unsafe artifact component." }
    return $raw
}

function Get-UnsignedLane($Lane) {
    $unsigned = [ordered]@{}
    foreach ($key in $Lane.Keys) { if ($key -cne 'attestation') { $unsigned[$key] = $Lane[$key] } }
    return $unsigned
}

function Resolve-CapturePublicKey($Plan, [string]$PlanDigest, $Authority) {
    if ($Plan.fixture_only) {
        return Import-P256PublicKey $Plan.capture_authority.fixture_capture_public_key_spki_base64 $Plan.capture_authority.capture_key_id 'fixture capture key'
    }
    Assert-Condition ($Authority.Approved.ContainsKey($PlanDigest)) 'Qualification plan is not approved by the protected production authority.'
    $approval = $Authority.Approved[$PlanDigest]
    Assert-Condition ($approval.capture_key_id -ceq $Plan.capture_authority.capture_key_id) 'Production authority capture key differs from the approved plan.'
    return Import-P256PublicKey $approval.capture_public_key_spki_base64 $approval.capture_key_id 'production capture key'
}

function Assert-LaneAttestation($Lane, $Plan, $ExpectedLane, [string]$PlanDigest, $Authority, [string]$Label) {
    Assert-ExactKeys $Lane @('identity', 'acquisition_artifact_path', 'acquisition_artifact_sha256', 'run_sets', 'scenarios', 'captures', 'artifact_inventory', 'attestation') $Label
    Assert-ExactKeys $Lane.attestation @('key_id', 'record', 'signature_base64', 'signature_scheme') "$Label.attestation"
    $keyId = Get-JsonString $Lane.attestation.key_id "$Label.attestation.key_id" 69
    Assert-Condition ($keyId -ceq $Plan.capture_authority.capture_key_id) "$Label attestation key does not match the plan capture authority."
    Assert-Condition ((Get-JsonString $Lane.attestation.signature_scheme "$Label.attestation.signature_scheme" 64) -ceq 'ecdsa-p256-sha256-ieee-p1363') "$Label attestation signature scheme is unsupported."
    [byte[]]$signature = Get-CanonicalBase64 $Lane.attestation.signature_base64 "$Label.attestation.signature_base64" 64
    Assert-Condition ($signature.Length -eq 64) "$Label attestation must contain a 64-byte IEEE-P1363 signature."

    Assert-Array $Lane.artifact_inventory "$Label.artifact_inventory"
    [object[]]$inventory = @($Lane.artifact_inventory)
    Assert-Condition ($inventory.Count -gt 0 -and $inventory.Count -le $MaxArtifacts) "$Label artifact inventory is empty or oversized."
    $priorPath = ''
    $inventoryDigests = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $inventory) {
        Assert-ExactKeys $entry @('artifact_path', 'artifact_sha256') "$Label artifact inventory entry"
        $path = Get-ArtifactRelativePath $entry.artifact_path "$Label artifact inventory entry.artifact_path"
        Assert-Condition ([StringComparer]::Ordinal.Compare($path, $priorPath) -gt 0) "$Label artifact inventory must be strictly stable-path-sorted and unique ($priorPath then $path)."
        $priorPath = $path
        $digest = Get-Sha256Value $entry.artifact_sha256 "$Label artifact inventory entry.artifact_sha256"
        Assert-Condition ($inventoryDigests.Add($digest)) "$Label artifact inventory reuses a digest."
    }

    $unsignedLane = Get-UnsignedLane $Lane
    $lanePayloadDigest = Get-CanonicalDigest $unsignedLane
    $inventoryDigest = Get-CanonicalDigest $inventory
    $captureContractDigest = Get-CanonicalDigest (Get-CaptureContractProjection $Plan $ExpectedLane.identity)
    $record = $Lane.attestation.record
    Assert-ExactKeys $record @('schema_version', 'kind', 'capture_contract_sha256', 'campaign_nonce', 'lane_id', 'acquisition_batch_id', 'lane_payload_sha256', 'artifact_inventory_sha256') "$Label.attestation.record"
    Assert-Condition ((Get-JsonInteger $record.schema_version "$Label.attestation.record.schema_version" 1 1) -eq 1 -and (Get-JsonString $record.kind "$Label.attestation.record.kind") -ceq 'windows_gpu_qualification_lane_attestation') "$Label attestation record contract is unsupported."
    $expectedRecord = [ordered]@{
        acquisition_batch_id = $ExpectedLane.identity.acquisition.batch_id
        artifact_inventory_sha256 = $inventoryDigest
        campaign_nonce = $Plan.capture_authority.campaign_nonce
        capture_contract_sha256 = $captureContractDigest
        kind = 'windows_gpu_qualification_lane_attestation'
        lane_id = $ExpectedLane.identity.lane_id
        lane_payload_sha256 = $lanePayloadDigest
        schema_version = 1
    }
    [byte[]]$actualRecordBytes = Get-CanonicalBytesFromObject $record
    [byte[]]$expectedRecordBytes = Get-CanonicalBytesFromObject $expectedRecord
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($actualRecordBytes, $expectedRecordBytes)) "$Label attestation record does not bind the exact lane, inventory, campaign, and capture contract."
    [byte[]]$preimage = Get-AttestationPreimage $actualRecordBytes
    $key = Resolve-CapturePublicKey $Plan $PlanDigest $Authority
    try {
        $verified = $key.VerifyData($preimage, $signature, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.DSASignatureFormat]::IeeeP1363FixedFieldConcatenation)
    }
    finally { $key.Dispose() }
    Assert-Condition $verified "$Label attestation signature is invalid."
    return ,$inventory
}

function Read-LaneInventory($Context, [object[]]$Inventory, [string]$Label) {
    $Context.Inventory = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    $Context.Used = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Inventory) {
        $relative = [string]$entry.artifact_path
        $digest = [string]$entry.artifact_sha256
        Assert-Condition ($Context.Paths.Add($relative)) 'Qualification evidence reuses or case-collides an artifact path.'
        Assert-Condition ($Context.Digests.Add($digest)) 'Qualification evidence reuses an artifact digest.'
        $full = Join-Path $Context.Root ($relative.Replace('/', '\'))
        [byte[]]$raw = Get-BoundBytes $full "$Label inventory artifact"
        $Context.Count++
        $Context.Bytes = [UInt64]$Context.Bytes + [UInt64]$raw.Length
        Assert-Condition ($Context.Count -le $MaxArtifacts) 'Qualification evidence exceeds the artifact-count bound.'
        Assert-Condition ($Context.Bytes -le $MaxArtifactBytes) 'Qualification evidence exceeds the cumulative artifact-byte bound.'
        Assert-Condition ((Get-Sha256Bytes $raw) -ceq $digest) "$Label inventory artifact digest does not match the supplied file."
        $Context.Inventory.Add($relative, [pscustomobject]@{ Digest = $digest; Raw = $raw })
    }
}

function Read-Artifact($Context, $RelativeValue, [string]$ExpectedDigest, [string]$Label) {
    $relative = Get-ArtifactRelativePath $RelativeValue "$Label.artifact_path"
    Assert-Condition ($null -ne $Context.Inventory -and $Context.Inventory.ContainsKey($relative)) "$Label artifact is absent from the signed inventory."
    $bound = $Context.Inventory[$relative]
    Assert-Condition ($bound.Digest -ceq $ExpectedDigest) "$Label artifact digest differs from the signed inventory."
    Assert-Condition ($Context.Used.Add($relative)) "$Label artifact is referenced more than once."
    return ,([byte[]]$bound.Raw)
}

function Assert-ArtifactEnvelope($Context, $RelativeValue, [string]$ExpectedDigest, [string]$ExpectedKind, $ExpectedRecord, [string]$Label) {
    [byte[]]$raw = Read-Artifact $Context $RelativeValue $ExpectedDigest $Label
    try { [byte[]]$canonical = [ScribeWindowsQualification.StrictJson]::Canonicalize($raw) }
    catch { Fail "$Label artifact is invalid JSON: $($_.Exception.Message)" }
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($raw, $canonical)) "$Label artifact is not canonical JSON."
    $envelope = [Text.Encoding]::UTF8.GetString($raw) | ConvertFrom-Json -AsHashtable -Depth 64
    Assert-ExactKeys $envelope @('schema_version', 'kind', 'record') "$Label artifact envelope"
    Assert-Condition ((Get-JsonInteger $envelope.schema_version "$Label artifact schema_version" 1 1) -eq 1) "$Label artifact schema is unsupported."
    Assert-Condition ((Get-JsonString $envelope.kind "$Label artifact kind") -ceq $ExpectedKind) "$Label artifact kind is unsupported."
    [byte[]]$actualRecord = Get-CanonicalBytesFromObject $envelope.record
    [byte[]]$expectedRecord = Get-CanonicalBytesFromObject $ExpectedRecord
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($actualRecord, $expectedRecord)) "$Label artifact record does not match the reviewed evidence."
}

function Import-ScifControlFrame($Value, [string]$Label) {
    [byte[]]$frame = Get-CanonicalBase64 $Value $Label ($ScifHeaderBytes + $MaxControlBytes)
    Assert-Condition ($frame.Length -ge $ScifHeaderBytes) "$Label is shorter than the SCIF v5 header."
    Assert-Condition ([Text.Encoding]::ASCII.GetString($frame, 0, 4) -ceq 'SCIF') "$Label has invalid SCIF magic."
    Assert-Condition ($frame[4] -eq 5 -and $frame[5] -eq 1) "$Label must be a SCIF v5 control frame."
    [UInt32]$bodyLength = [BitConverter]::ToUInt32($frame, 6)
    [UInt64]$sessionId = [BitConverter]::ToUInt64($frame, 10)
    [UInt64]$requestId = [BitConverter]::ToUInt64($frame, 18)
    Assert-Condition ($bodyLength -gt 0 -and $bodyLength -le $MaxControlBytes -and $frame.Length -eq $ScifHeaderBytes + $bodyLength) "$Label body length is invalid or has trailing bytes."
    Assert-Condition ($sessionId -eq 0 -and $requestId -eq 0) "$Label must use handshake session/request 0/0."
    [byte[]]$body = $frame[$ScifHeaderBytes..($frame.Length - 1)]
    try { $null = [ScribeWindowsQualification.StrictJson]::Canonicalize($body) }
    catch { Fail "$Label control body is invalid strict UTF-8/JSON: $($_.Exception.Message)" }
    try { $control = [Text.UTF8Encoding]::new($false, $true).GetString($body) | ConvertFrom-Json -AsHashtable -Depth 64 }
    catch { Fail "$Label control body could not be parsed: $($_.Exception.Message)" }
    Assert-Object $control "$Label control body"
    return $control
}

function Assert-WirePackExpectation($Pack, [string]$Label, $Identity) {
    Assert-ExactKeys $Pack @('pack_id', 'pack_version', 'pack_digest', 'security_epoch', 'runtime_abi', 'backend', 'provider') $Label
    Assert-Condition ((Get-PackComponent $Pack.pack_id "$Label.pack_id") -ceq $Identity.pack.pack_id -and (Get-PackComponent $Pack.pack_version "$Label.pack_version") -ceq $Identity.pack.pack_version -and (Get-Sha256Value $Pack.pack_digest "$Label.pack_digest") -ceq $Identity.pack.pack_digest) "$Label does not match the reviewed pack."
    Assert-Condition ((Get-JsonInteger $Pack.security_epoch "$Label.security_epoch" 1 ([uint32]::MaxValue)) -eq $Identity.pack.security_epoch -and (Get-JsonInteger $Pack.runtime_abi "$Label.runtime_abi" 1 ([uint16]::MaxValue)) -eq $Identity.pack.runtime_abi) "$Label ABI/security epoch does not match the reviewed pack."
    Assert-Condition ((Get-JsonString $Pack.backend "$Label.backend" 16) -ceq $Identity.backend -and (Get-Identifier $Pack.provider "$Label.provider") -ceq $Identity.provider_id) "$Label backend/provider does not match the reviewed pack."
}

function Assert-WireWorkerIdentity($Value, [string]$Label, $Identity, [bool]$Cpu, [bool]$Capability) {
    $keys = @('app_build', 'worker_build', 'bundled_worker_sha256', 'abi', 'role', 'provider')
    if ($Capability) { $keys += @('challenge', 'artifacts') }
    if (-not $Cpu) { $keys += 'pack' }
    Assert-ExactKeys $Value $keys $Label
    $worker = if ($Cpu) { $Identity.cpu_baseline } else { $Identity.gpu_worker }
    Assert-Condition ((Get-JsonString $Value.app_build "$Label.app_build" 160) -ceq $Identity.app_build_id) "$Label app_build differs from the reviewed desktop."
    Assert-Condition ((Get-JsonString $Value.worker_build "$Label.worker_build" 160) -ceq $worker.worker_build_id) "$Label worker_build differs from the reviewed worker."
    Assert-Condition ((Get-Sha256Value $Value.bundled_worker_sha256 "$Label.bundled_worker_sha256") -ceq $worker.worker_sha256) "$Label bundled worker digest differs from the reviewed executable."
    Assert-Condition ((Get-JsonInteger $Value.abi "$Label.abi" 1 ([uint16]::MaxValue)) -eq $worker.runtime_abi -and (Get-JsonString $Value.role "$Label.role" 16) -ceq 'inference' -and (Get-JsonString $Value.provider "$Label.provider" 16) -ceq $(if ($Cpu) { 'cpu' } else { $Identity.backend })) "$Label ABI, role, or provider is incompatible."
    if ($Capability) {
        [byte[]]$actualTargets = Get-CanonicalBytesFromObject $Value.artifacts
        [byte[]]$expectedTargets = Get-CanonicalBytesFromObject @([ordered]@{ artifact = 'gguf'; target = 'windows-x86_64' }, [ordered]@{ artifact = 'onnx_asr'; target = 'windows-x86_64' })
        Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($actualTargets, $expectedTargets)) "$Label artifact targets are not the exact ordered Windows inference targets."
    }
    if (-not $Cpu) {
        $wirePack = if ($Capability) { $Value.pack.expectation } else { $Value.pack }
        if ($Capability) { Assert-ExactKeys $Value.pack @('expectation', 'devices') "$Label.pack" }
        Assert-WirePackExpectation $wirePack "$Label pack expectation" $Identity
    }
}

function Assert-Capture($Capture, [string]$Label, $Identity, $Context, $RunGenerationCaptures, $AllowedExtraGenerations, $ChallengeSet, $ValidatedCaptures) {
    Assert-ExactKeys $Capture @('artifact_path', 'artifact_sha256', 'generation', 'launch_scope', 'request_frame_base64', 'response_frame_base64') $Label
    $artifactDigest = Get-Sha256Value $Capture.artifact_sha256 "$Label.artifact_sha256"
    $generation = Get-Identifier $Capture.generation "$Label.generation" 200
    Assert-Condition ($RunGenerationCaptures.ContainsKey($generation) -or $AllowedExtraGenerations.Contains($generation)) "$Label does not describe a required worker generation."
    $scope = Get-JsonString $Capture.launch_scope "$Label.launch_scope" 32
    Assert-Condition (@('cpu', 'provider_discovery', 'selected_device') -ccontains $scope) "$Label launch scope is unsupported."
    $cpu = $scope -ceq 'cpu'
    $request = Import-ScifControlFrame $Capture.request_frame_base64 "$Label request frame"
    Assert-ExactKeys $request @('command', 'challenge', 'expected') "$Label request"
    Assert-Condition ((Get-JsonString $request.command "$Label request.command" 16) -ceq 'hello') "$Label request is not Hello."
    $challenge = Get-JsonString $request.challenge "$Label request.challenge" 64
    Assert-Condition ($challenge -cmatch '^[0-9a-f]{64}$') "$Label request challenge must be 32 raw random bytes in lowercase hexadecimal."
    Assert-Condition ($ChallengeSet.Add($challenge)) 'Qualification raw Hello captures must use a unique challenge per worker generation.'
    Assert-WireWorkerIdentity $request.expected "$Label request.expected" $Identity $cpu $false

    $response = Import-ScifControlFrame $Capture.response_frame_base64 "$Label response frame"
    Assert-ExactKeys $response @('command', 'capability') "$Label response"
    Assert-Condition ((Get-JsonString $response.command "$Label response.command" 16) -ceq 'ready') "$Label response is not Ready."
    Assert-WireWorkerIdentity $response.capability "$Label response.capability" $Identity $cpu $true
    $echo = Get-JsonString $response.capability.challenge "$Label response.capability.challenge" 64
    [byte[]]$challengeBytes = [Text.Encoding]::ASCII.GetBytes($challenge)
    [byte[]]$echoBytes = [Text.Encoding]::ASCII.GetBytes($echo)
    Assert-Condition ([Security.Cryptography.CryptographicOperations]::FixedTimeEquals($echoBytes, $challengeBytes)) "$Label Ready capability did not echo the exact raw challenge."

    $stable = 'cpu:host'
    [Int64]$currentIndex = -1
    if (-not $cpu) {
        Assert-Array $response.capability.pack.devices "$Label response.capability.pack.devices"
        [object[]]$devices = @($response.capability.pack.devices)
        [object[]]$expectedDevices = if ($scope -ceq 'provider_discovery') { @($Identity.acquisition.device_set.devices | Where-Object { $_.provider_eligible }) } else { @($Identity.acquisition.device_set.devices | Where-Object { $_.stable_device_id -ceq $Identity.device.stable_device_id }) }
        Assert-Condition ($devices.Count -eq $expectedDevices.Count -and $devices.Count -gt 0) "$Label device list does not match its discovery/selected launch scope."
        $indexes = [Collections.Generic.HashSet[Int64]]::new()
        $prior = ''
        for ($index = 0; $index -lt $devices.Count; $index++) {
            $device = $devices[$index]
            $expectedDevice = $expectedDevices[$index]
            Assert-ExactKeys $device @('stable_device_identity', 'process_index', 'display_name', 'driver_version', 'device_class', 'vendor', 'memory_total_bytes', 'memory_available_bytes') "$Label device[$index]"
            $deviceStable = Get-JsonString $device.stable_device_identity "$Label device[$index].stable_device_identity" 64
            Assert-Condition ((Test-StableDeviceId $deviceStable) -and [StringComparer]::Ordinal.Compare($deviceStable, $prior) -gt 0) "$Label devices must use strictly sorted stable OS identities."
            $prior = $deviceStable
            $processIndex = Get-JsonInteger $device.process_index "$Label device[$index].process_index" 0 ([int32]::MaxValue)
            Assert-Condition ($indexes.Add($processIndex)) "$Label devices reuse a transient process index."
            $null = Get-JsonString $device.display_name "$Label device[$index].display_name" 256
            $driver = Get-JsonString $device.driver_version "$Label device[$index].driver_version" 128
            $class = Get-JsonString $device.device_class "$Label device[$index].device_class" 32
            $vendor = Get-JsonString $device.vendor "$Label device[$index].vendor" 16
            $total = Get-JsonInteger $device.memory_total_bytes "$Label device[$index].memory_total_bytes" $MinimumGpuMemoryBytes
            $null = Get-JsonInteger $device.memory_available_bytes "$Label device[$index].memory_available_bytes" 0 $total
            Assert-Condition ($deviceStable -ceq $expectedDevice.stable_device_id -and $class -ceq $expectedDevice.device_class -and $vendor -ceq $expectedDevice.vendor -and $driver -ceq $expectedDevice.driver -and $total -eq $expectedDevice.total_memory_bytes) "$Label device does not match the reviewed provider inventory."
            Assert-Condition (Test-DriverVendorBinding $Identity.backend $driver $vendor) "$Label device driver does not match its backend/vendor."
            if ($deviceStable -ceq $Identity.device.stable_device_id) { $stable = $deviceStable; $currentIndex = $processIndex }
        }
        if ($scope -ceq 'selected_device') { Assert-Condition ($stable -ceq $Identity.device.stable_device_id -and $devices.Count -eq 1) "$Label selected-device scope is not narrowed to the selected stable identity." }
    }
    $record = [ordered]@{}
    foreach ($key in $Capture.Keys) { if (@('artifact_path', 'artifact_sha256') -cnotcontains $key) { $record[$key] = $Capture[$key] } }
    $captureDigest = Get-CanonicalDigest $record
    if ($RunGenerationCaptures.ContainsKey($generation)) { Assert-Condition ($RunGenerationCaptures[$generation] -ceq $captureDigest) "$Label content does not match the run's signed raw capture digest." }
    Assert-Condition (-not $ValidatedCaptures.ContainsKey($captureDigest)) 'Qualification evidence reuses one raw capture for multiple generations.'
    Assert-ArtifactEnvelope $Context $Capture.artifact_path $artifactDigest 'windows_gpu_qualification_raw_scif_capture' $record $Label
    $summary = [pscustomobject]@{ Digest = $captureDigest; Generation = $generation; Scope = $scope; StableId = $stable; CurrentIndex = $currentIndex; Challenge = $challenge }
    $ValidatedCaptures[$captureDigest] = $summary
    return $summary
}

function Assert-Execution($Execution, [string]$Label, [string]$Target, $Identity, [string]$Mode, [int]$Sequence, $GenerationCaptures, $CaptureGenerations) {
    Assert-ExactKeys $Execution @('backend', 'provider_id', 'worker_build_id', 'worker_sha256', 'protocol_version', 'runtime_abi', 'worker_generation', 'capture_sha256', 'stable_device_id', 'device_memory_kind', 'pack_digest', 'model_digest', 'driver', 'windows_version', 'options_sha256') $Label
    $worker = if ($Target -ceq 'cpu') { $Identity.cpu_baseline } else { $Identity.gpu_worker }
    $expectedGeneration = if ($Mode -ceq 'cold') { "$($Identity.acquisition.batch_id):$($Identity.lane_id):$Mode`:$Target`:$('{0:d2}' -f $Sequence)" } else { "$($Identity.acquisition.batch_id):$($Identity.lane_id):warm:$Target" }
    $expected = [ordered]@{
        backend = if ($Target -ceq 'cpu') { 'cpu' } else { $Identity.backend }
        provider_id = $worker.provider_id
        worker_build_id = $worker.worker_build_id
        worker_sha256 = $worker.worker_sha256
        protocol_version = $worker.protocol_version
        runtime_abi = $worker.runtime_abi
        worker_generation = $expectedGeneration
        stable_device_id = if ($Target -ceq 'cpu') { 'cpu:host' } else { $Identity.device.stable_device_id }
        device_memory_kind = if ($Target -ceq 'cpu') { 'none' } else { $Identity.device.memory_model }
        pack_digest = if ($Target -ceq 'cpu') { $ZeroSha256 } else { $Identity.pack.pack_digest }
        model_digest = $Identity.model.model_digest
        driver = if ($Target -ceq 'cpu') { 'cpu:none' } else { $Identity.driver.value }
        windows_version = $Identity.windows_version
        options_sha256 = $Identity.acquisition.options_sha256
    }
    foreach ($field in $expected.Keys) {
        if ($expected[$field] -is [ValueType]) { $null = Get-JsonInteger $Execution[$field] "$Label.$field" 1 }
        else { $null = Get-JsonString $Execution[$field] "$Label.$field" 160 }
        Assert-Condition ($Execution[$field] -ceq $expected[$field]) "$Label.$field does not match the admitted execution target."
    }
    $capture = Get-Sha256Value $Execution.capture_sha256 "$Label.capture_sha256"
    $generation = $Execution.worker_generation
    if ($GenerationCaptures.ContainsKey($generation)) {
        Assert-Condition ($GenerationCaptures[$generation] -ceq $capture) 'One retained worker generation reported multiple raw captures.'
    }
    else {
        Assert-Condition (-not $CaptureGenerations.ContainsKey($capture)) 'Distinct worker generations reused one raw capture.'
        $GenerationCaptures[$generation] = $capture
        $CaptureGenerations[$capture] = $generation
    }
}

function Assert-Run($Run, [string]$Label, [int]$Sequence, [string]$Target, [string]$Mode, $Identity, $Context, $GenerationCaptures, $CaptureGenerations) {
    Assert-ExactKeys $Run @('sequence', 'artifact_path', 'artifact_sha256', 'acquisition_batch_id', 'machine_id_sha256', 'session_id', 'pair_id', 'pair_order', 'reset_state', 'priming_runs', 'device_set_sha256', 'execution', 'outcome', 'failure_category', 'end_to_end_ms', 'backend_ms', 'peak_process_memory_bytes', 'peak_vram_bytes', 'peak_shared_device_memory_bytes', 'available_device_memory_bytes_before', 'available_device_memory_bytes_after', 'transcript_sha256') $Label
    Assert-Condition ((Get-JsonInteger $Run.sequence "$Label.sequence" 1 20) -eq $Sequence) "$Label sequence is not contiguous."
    $artifactDigest = Get-Sha256Value $Run.artifact_sha256 "$Label.artifact_sha256"
    Assert-Condition ((Get-Identifier $Run.acquisition_batch_id "$Label.acquisition_batch_id") -ceq $Identity.acquisition.batch_id) "$Label is from a different acquisition batch."
    Assert-Condition ((Get-Sha256Value $Run.machine_id_sha256 "$Label.machine_id_sha256") -ceq $Identity.acquisition.machine_id_sha256) "$Label is from a different machine."
    $expectedSession = if ($Mode -ceq 'cold') { "$($Identity.acquisition.batch_id):$Mode`:$('{0:d2}' -f $Sequence):session" } else { "$($Identity.acquisition.batch_id):warm:session" }
    $expectedPair = "$($Identity.acquisition.batch_id):$Mode`:$('{0:d2}' -f $Sequence)"
    $expectedOrder = if ($Sequence % 2) { 'cpu_then_gpu' } else { 'gpu_then_cpu' }
    $expectedReset = if ($Mode -ceq 'cold') { 'fresh_process_fresh_model' } else { 'same_process_primed_model' }
    Assert-Condition ((Get-Identifier $Run.session_id "$Label.session_id") -ceq $expectedSession) "$Label session violates acquisition protocol v1."
    Assert-Condition ((Get-Identifier $Run.pair_id "$Label.pair_id") -ceq $expectedPair) "$Label pair violates acquisition protocol v1."
    Assert-Condition ((Get-JsonString $Run.pair_order "$Label.pair_order" 32) -ceq $expectedOrder) "$Label order violates acquisition protocol v1."
    Assert-Condition ((Get-JsonString $Run.reset_state "$Label.reset_state" 40) -ceq $expectedReset) "$Label reset state violates acquisition protocol v1."
    $expectedPriming = if ($Mode -ceq 'cold') { 0 } else { $Identity.acquisition.ordering.warm_priming_runs }
    Assert-Condition ((Get-JsonInteger $Run.priming_runs "$Label.priming_runs" 0 16) -eq $expectedPriming) "$Label priming violates acquisition protocol v1."
    Assert-Condition ((Get-Sha256Value $Run.device_set_sha256 "$Label.device_set_sha256") -ceq $Identity.acquisition.device_set.snapshot_sha256) "$Label device-set binding differs."
    Assert-Execution $Run.execution "$Label.execution" $Target $Identity $Mode $Sequence $GenerationCaptures $CaptureGenerations
    $outcome = Get-JsonString $Run.outcome "$Label.outcome" 16
    Assert-Condition (@('success', 'failure') -ccontains $outcome) "$Label outcome is unsupported."
    $failure = Get-JsonString $Run.failure_category "$Label.failure_category" 32
    Assert-Condition (@('none', 'unavailable', 'startup', 'handshake', 'timeout', 'oom', 'device_loss', 'provider_error', 'worker_crash', 'correctness_mismatch', 'invalid_input', 'model_corruption', 'cancelled', 'partial_output') -ccontains $failure) "$Label failure category is unsupported."
    $endToEnd = Get-JsonInteger $Run.end_to_end_ms "$Label.end_to_end_ms"
    $backendMs = Get-JsonInteger $Run.backend_ms "$Label.backend_ms"
    $processMemory = Get-JsonInteger $Run.peak_process_memory_bytes "$Label.peak_process_memory_bytes"
    $vram = Get-JsonInteger $Run.peak_vram_bytes "$Label.peak_vram_bytes"
    $shared = Get-JsonInteger $Run.peak_shared_device_memory_bytes "$Label.peak_shared_device_memory_bytes"
    $availableBefore = Get-JsonInteger $Run.available_device_memory_bytes_before "$Label.available_device_memory_bytes_before"
    $availableAfter = Get-JsonInteger $Run.available_device_memory_bytes_after "$Label.available_device_memory_bytes_after"
    $transcript = Get-Sha256Value $Run.transcript_sha256 "$Label.transcript_sha256" $true
    if ($outcome -ceq 'success') {
        Assert-Condition ($failure -ceq 'none' -and $endToEnd -gt 0 -and $backendMs -gt 0 -and $processMemory -gt 0 -and $backendMs -le $endToEnd -and $transcript -cne $ZeroSha256) "$Label has inconsistent successful-run metrics."
        if ($Target -ceq 'cpu') {
            Assert-Condition ($vram -eq 0 -and $shared -eq 0 -and $availableBefore -eq 0 -and $availableAfter -eq 0) "$Label CPU run reports GPU memory."
        }
        elseif ($Identity.device.memory_model -ceq 'dedicated_vram') {
            Assert-Condition ($vram -ge $MinimumGpuPeakMemoryBytes -and $vram -le $Identity.device.total_memory_bytes -and $shared -eq 0 -and $availableBefore -gt 0 -and $availableBefore -le $Identity.device.total_memory_bytes -and $availableAfter -le $Identity.device.total_memory_bytes) "$Label discrete-GPU telemetry is implausible."
        }
        else {
            Assert-Condition ($shared -ge $MinimumGpuPeakMemoryBytes -and $shared -le $Identity.device.total_memory_bytes -and $vram -eq 0 -and $availableBefore -gt 0 -and $availableBefore -le $Identity.device.total_memory_bytes -and $availableAfter -le $Identity.device.total_memory_bytes) "$Label shared-GPU telemetry is implausible."
        }
    }
    else {
        Assert-Condition ($failure -cne 'none' -and $transcript -ceq $ZeroSha256) "$Label has inconsistent failure metadata."
    }
    $record = [ordered]@{}
    foreach ($key in $Run.Keys) { if (@('artifact_path', 'artifact_sha256') -cnotcontains $key) { $record[$key] = $Run[$key] } }
    Assert-ArtifactEnvelope $Context $Run.artifact_path $artifactDigest 'windows_gpu_qualification_run_artifact' $record $Label
    return $Run
}

function Assert-Scenario($Scenario, [string]$ExpectedScenario, [string]$Label, $Identity, $Context) {
    Assert-ExactKeys $Scenario @('scenario', 'artifact_path', 'artifact_sha256', 'result', 'power_source', 'requested_mode', 'selected_backend', 'selected_stable_device_id', 'observed_failure_category', 'selection_reevaluated', 'active_request_migrated', 'partial_output_replayed', 'recovered_next_request', 'driver_before', 'driver_after', 'device_set_sha256', 'available_device_memory_bytes', 'package_sha256', 'clean_machine', 'capture_before_sha256', 'capture_after_sha256', 'process_index_before', 'process_index_after') $Label
    Assert-Condition ((Get-JsonString $Scenario.scenario "$Label.scenario") -ceq $ExpectedScenario) "$Label scenario is not canonical."
    $artifactDigest = Get-Sha256Value $Scenario.artifact_sha256 "$Label.artifact_sha256"
    $result = Get-JsonString $Scenario.result "$Label.result" 16
    Assert-Condition (@('pass', 'fail') -ccontains $result) "$Label result is unsupported."
    $power = Get-JsonString $Scenario.power_source "$Label.power_source" 16
    Assert-Condition (@('ac', 'battery') -ccontains $power) "$Label power source is unsupported."
    Assert-Condition ((Get-JsonString $Scenario.requested_mode "$Label.requested_mode" 16) -ceq 'auto') "$Label must exercise Auto."
    $selectedBackend = Get-JsonString $Scenario.selected_backend "$Label.selected_backend" 16
    Assert-Condition (@('cpu', $Identity.backend) -ccontains $selectedBackend) "$Label selected backend is not admitted."
    $selectedStable = Get-JsonString $Scenario.selected_stable_device_id "$Label.selected_stable_device_id" 64
    $expectedSelectedStable = if ($selectedBackend -ceq 'cpu') { 'cpu:host' } else { $Identity.device.stable_device_id }
    Assert-Condition ($selectedStable -ceq $expectedSelectedStable) "$Label selected stable device does not match its backend."
    $failure = Get-JsonString $Scenario.observed_failure_category "$Label.observed_failure_category" 32
    Assert-Condition (@('none', 'unavailable', 'oom', 'device_loss') -ccontains $failure) "$Label failure category is unsupported."
    foreach ($field in @('selection_reevaluated', 'active_request_migrated', 'partial_output_replayed', 'recovered_next_request', 'clean_machine')) { $null = Get-JsonBoolean $Scenario[$field] "$Label.$field" }
    $driverBefore = Get-JsonString $Scenario.driver_before "$Label.driver_before" 128
    $driverAfter = Get-JsonString $Scenario.driver_after "$Label.driver_after" 128
    Assert-Condition ((Test-DriverVendorBinding $Identity.backend $driverBefore $Identity.device.vendor) -and (Test-DriverVendorBinding $Identity.backend $driverAfter $Identity.device.vendor)) "$Label driver facts are not canonical for the selected backend and vendor."
    Assert-Condition ($driverAfter -ceq $Identity.driver.value) "$Label does not end at the admitted driver."
    Assert-Condition ((Get-Sha256Value $Scenario.device_set_sha256 "$Label.device_set_sha256") -ceq $Identity.acquisition.device_set.snapshot_sha256) "$Label device-set binding differs."
    $available = Get-JsonInteger $Scenario.available_device_memory_bytes "$Label.available_device_memory_bytes" 0 $Identity.device.total_memory_bytes
    Assert-Condition ((Get-Sha256Value $Scenario.package_sha256 "$Label.package_sha256") -ceq $Identity.installation.package_sha256) "$Label installer binding differs."
    $captureBefore = Get-Sha256Value $Scenario.capture_before_sha256 "$Label.capture_before_sha256" ($ExpectedScenario -cne 'mixed_gpu')
    $captureAfter = Get-Sha256Value $Scenario.capture_after_sha256 "$Label.capture_after_sha256" ($ExpectedScenario -cne 'mixed_gpu')
    $indexBefore = Get-JsonInteger $Scenario.process_index_before "$Label.process_index_before" -1 15
    $indexAfter = Get-JsonInteger $Scenario.process_index_after "$Label.process_index_after" -1 15
    if ($ExpectedScenario -cne 'mixed_gpu') {
        Assert-Condition ($captureBefore -ceq $ZeroSha256 -and $captureAfter -ceq $ZeroSha256 -and $indexBefore -eq -1 -and $indexAfter -eq -1) "$Label non-remapping scenario must use the canonical no-capture-remap form."
    }
    $record = [ordered]@{}
    foreach ($key in $Scenario.Keys) { if (@('artifact_path', 'artifact_sha256') -cnotcontains $key) { $record[$key] = $Scenario[$key] } }
    Assert-ArtifactEnvelope $Context $Scenario.artifact_path $artifactDigest 'windows_gpu_qualification_scenario_artifact' $record $Label

    $common = $result -ceq 'pass' -and $Scenario.selection_reevaluated -and -not $Scenario.active_request_migrated -and -not $Scenario.partial_output_replayed -and $Scenario.recovered_next_request
    $specific = switch ($ExpectedScenario) {
        'clean_installer' { $power -ceq 'ac' -and $selectedBackend -ceq $Identity.backend -and $failure -ceq 'none' -and $Scenario.clean_machine }
        'device_loss' { $selectedBackend -ceq 'cpu' -and $failure -ceq 'device_loss' -and $driverBefore -ceq $Identity.driver.value }
        'disabled_device' { $selectedBackend -ceq 'cpu' -and $failure -ceq 'unavailable' -and $driverBefore -ceq $Identity.driver.value }
        'driver_change' { $selectedBackend -ceq $Identity.backend -and $failure -ceq 'none' -and $driverBefore -cne $driverAfter }
        'insufficient_vram' { $selectedBackend -ceq 'cpu' -and $failure -ceq 'unavailable' -and $available -lt $Identity.device.qualified_minimum_available_memory_bytes }
        'mixed_gpu' { $selectedBackend -ceq $Identity.backend -and $failure -ceq 'none' -and $Identity.acquisition.device_set.mixed_gpu -and $captureBefore -cne $captureAfter -and $indexBefore -ge 0 -and $indexAfter -ge 0 -and $indexBefore -ne $indexAfter }
        'power_ac' { $power -ceq 'ac' -and $selectedBackend -ceq $Identity.backend -and $failure -ceq 'none' }
        'power_battery' {
            $power -ceq 'battery' -and $failure -ceq 'none' -and (
                ($Identity.device.device_class -ceq 'discrete_gpu' -and $selectedBackend -ceq 'cpu') -or
                ($Identity.device.device_class -cne 'discrete_gpu' -and $selectedBackend -ceq $Identity.backend)
            )
        }
        'suspend_resume' { $selectedBackend -ceq $Identity.backend -and $failure -ceq 'none' -and $driverBefore -ceq $driverAfter }
        default { $false }
    }
    return [pscustomobject]@{ Value = $Scenario; Passed = [bool]($common -and $specific) }
}

function Get-NearestRank([Int64[]]$Values, [int]$Percentile) {
    $ordered = @($Values | Sort-Object)
    $rank = [int][Math]::Floor(($ordered.Count * $Percentile + 99) / 100)
    return [Int64]$ordered[$rank - 1]
}

function Get-MetricSummary([object[]]$Runs) {
    [object[]]$successful = @($Runs | Where-Object { $_.outcome -ceq 'success' })
    $failures = [ordered]@{}
    foreach ($name in @($Runs | Where-Object { $_.outcome -ceq 'failure' } | ForEach-Object { $_.failure_category } | Sort-Object -Unique)) { $failures[$name] = @($Runs | Where-Object { $_.outcome -ceq 'failure' -and $_.failure_category -ceq $name }).Count }
    $result = [ordered]@{ failure_categories = $failures; run_count = $Runs.Count; successful_runs = $successful.Count }
    foreach ($field in @('end_to_end_ms', 'backend_ms', 'peak_process_memory_bytes', 'peak_vram_bytes', 'peak_shared_device_memory_bytes', 'available_device_memory_bytes_before', 'available_device_memory_bytes_after')) {
        if ($successful.Count -eq 0) { $result[$field] = $null }
        else {
            [Int64[]]$values = @($successful | ForEach-Object { [Int64]$_[$field] })
            $result[$field] = [ordered]@{ p50 = Get-NearestRank $values 50; p95 = Get-NearestRank $values 95 }
        }
    }
    return $result
}

function Get-AutoProjection($Identity, $RunSets, $Parsed, [Int64]$MinimumAvailableMemoryBytes) {
    $coldParity = [ordered]@{ cpu = @($Parsed.cold.cpu | ForEach-Object { $_.transcript_sha256 }); gpu = @($Parsed.cold.gpu | ForEach-Object { $_.transcript_sha256 }) }
    $warmParity = [ordered]@{ cpu = @($Parsed.warm.cpu | ForEach-Object { $_.transcript_sha256 }); gpu = @($Parsed.warm.gpu | ForEach-Object { $_.transcript_sha256 }) }
    return [ordered]@{
        pack = $Identity.pack
        model_digest = $Identity.model.model_digest
        backend = $Identity.backend
        provider_id = $Identity.provider_id
        vendor = $Identity.device.vendor
        device_class = $Identity.device.device_class
        minimum_total_memory_bytes = $Identity.device.total_memory_bytes
        minimum_available_memory_bytes = $MinimumAvailableMemoryBytes
        driver = $Identity.driver
        evidence = [ordered]@{
            id = $Identity.lane_id
            cold_runs = @($Parsed.cold.cpu).Count
            warm_runs = @($Parsed.warm.cpu).Count
            gpu_p95_ms = Get-NearestRank @($Parsed.warm.gpu | ForEach-Object { [Int64]$_.end_to_end_ms }) 95
            cpu_p95_ms = Get-NearestRank @($Parsed.warm.cpu | ForEach-Object { [Int64]$_.end_to_end_ms }) 95
            correctness_verified = $true
            reliability_verified = $true
            cold_evidence_sha256 = Get-CanonicalDigest $RunSets.cold
            warm_evidence_sha256 = Get-CanonicalDigest $RunSets.warm
            transcript_parity_evidence_sha256 = Get-CanonicalDigest ([ordered]@{ expected = $Identity.workload.expected_transcript_sha256; cold = $coldParity; warm = $warmParity })
        }
    }
}

function Assert-LaneEvidence($Lane, $Expected, $Plan, [int]$Index, $Context, $GenerationCaptures, $CaptureGenerations, $Challenges, $ValidatedCaptures) {
    $label = "evidence lane $Index"
    Assert-Identity $Lane.identity "$label.identity"
    [byte[]]$identityActual = Get-CanonicalBytesFromObject $Lane.identity
    [byte[]]$identityExpected = Get-CanonicalBytesFromObject $Expected.identity
    Assert-Condition ([ScribeWindowsQualification.StrictJson]::Equal($identityActual, $identityExpected)) "$label identity does not match its reviewed plan."
    Assert-Condition ((Get-CanonicalDigest $Lane) -ceq $Expected.evidence_sha256) "$label does not match its reviewed evidence digest."
    $acquisitionDigest = Get-Sha256Value $Lane.acquisition_artifact_sha256 "$label.acquisition_artifact_sha256"
    Assert-ArtifactEnvelope $Context $Lane.acquisition_artifact_path $acquisitionDigest 'windows_gpu_qualification_acquisition_artifact' $Lane.identity.acquisition "$label acquisition"
    Assert-ExactKeys $Lane.run_sets @('cold', 'warm') "$label.run_sets"
    $parsed = [ordered]@{}
    foreach ($mode in @('cold', 'warm')) {
        $expectedCount = if ($mode -ceq 'cold') { [int]$Plan.cold_runs } else { [int]$Plan.warm_runs }
        Assert-ExactKeys $Lane.run_sets[$mode] @('cpu', 'gpu') "$label.$mode"
        $parsed[$mode] = [ordered]@{}
        foreach ($target in @('cpu', 'gpu')) {
            Assert-Array $Lane.run_sets[$mode][$target] "$label.$mode.$target"
            [object[]]$runs = @($Lane.run_sets[$mode][$target])
            Assert-Condition ($runs.Count -eq $expectedCount) "$label.$mode.$target has the wrong run count."
            $parsedRuns = [Collections.Generic.List[object]]::new()
            for ($offset = 0; $offset -lt $runs.Count; $offset++) { $parsedRuns.Add((Assert-Run $runs[$offset] "$label.$mode.$target[$offset]" ($offset + 1) $target $mode $Lane.identity $Context $GenerationCaptures $CaptureGenerations)) }
            $parsed[$mode][$target] = $parsedRuns.ToArray()
        }
    }
    Assert-Array $Lane.scenarios "$label.scenarios"
    [object[]]$scenarios = @($Lane.scenarios)
    Assert-Condition ($scenarios.Count -eq $RequiredScenarios.Count) "$label scenarios are incomplete."
    $scenarioResults = [Collections.Generic.List[object]]::new()
    for ($offset = 0; $offset -lt $RequiredScenarios.Count; $offset++) { $scenarioResults.Add((Assert-Scenario $scenarios[$offset] $RequiredScenarios[$offset] "$label.scenarios[$offset]" $Lane.identity $Context)) }
    $beforeGeneration = "$($Lane.identity.acquisition.batch_id):$($Lane.identity.lane_id):scenario:mixed_gpu:before"
    $afterGeneration = "$($Lane.identity.acquisition.batch_id):$($Lane.identity.lane_id):scenario:mixed_gpu:after"
    $discoveryGeneration = "$($Lane.identity.acquisition.batch_id):$($Lane.identity.lane_id):provider_discovery"
    $extraGenerations = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $null = $extraGenerations.Add($beforeGeneration)
    $null = $extraGenerations.Add($afterGeneration)
    $null = $extraGenerations.Add($discoveryGeneration)
    $requiredGenerations = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $expectedScopes = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
    foreach ($mode in @('cold', 'warm')) { foreach ($target in @('cpu', 'gpu')) { foreach ($run in @($parsed[$mode][$target])) { $generation = [string]$run.execution.worker_generation; $null = $requiredGenerations.Add($generation); $expectedScopes[$generation] = if ($target -ceq 'cpu') { 'cpu' } else { 'selected_device' } } } }
    $null = $requiredGenerations.Add($beforeGeneration)
    $null = $requiredGenerations.Add($afterGeneration)
    $null = $requiredGenerations.Add($discoveryGeneration)
    $expectedScopes[$beforeGeneration] = 'selected_device'
    $expectedScopes[$afterGeneration] = 'selected_device'
    $expectedScopes[$discoveryGeneration] = 'provider_discovery'
    Assert-Array $Lane.captures "$label.captures"
    [object[]]$captures = @($Lane.captures)
    Assert-Condition ($captures.Count -eq $requiredGenerations.Count) "$label must contain exactly one raw SCIF capture per required worker generation and discovery launch."
    $laneCaptureGenerations = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($capture in $captures) {
        $summary = Assert-Capture $capture "$label capture" $Lane.identity $Context $GenerationCaptures $extraGenerations $Challenges $ValidatedCaptures
        Assert-Condition ($requiredGenerations.Contains($summary.Generation) -and $laneCaptureGenerations.Add($summary.Generation)) "$label contains an unexpected or duplicate capture generation."
        Assert-Condition ($summary.Scope -ceq $expectedScopes[$summary.Generation]) "$label capture launch scope does not match its measured/discovery purpose."
    }
    Assert-Condition ($laneCaptureGenerations.Count -eq $requiredGenerations.Count) "$label omits one or more required raw SCIF captures."
    $mixedScenario = @($scenarioResults | Where-Object { $_.Value.scenario -ceq 'mixed_gpu' })[0].Value
    Assert-Condition ($ValidatedCaptures.ContainsKey($mixedScenario.capture_before_sha256) -and $ValidatedCaptures.ContainsKey($mixedScenario.capture_after_sha256)) "$label mixed-GPU scenario references an unknown raw capture."
    $beforeCapture = $ValidatedCaptures[$mixedScenario.capture_before_sha256]
    $afterCapture = $ValidatedCaptures[$mixedScenario.capture_after_sha256]
    Assert-Condition ($beforeCapture.Generation -ceq $beforeGeneration -and $afterCapture.Generation -ceq $afterGeneration -and $beforeCapture.Scope -ceq 'selected_device' -and $afterCapture.Scope -ceq 'selected_device') "$label mixed-GPU scenario references the wrong worker generations or launch scope."
    Assert-Condition ($beforeCapture.StableId -ceq $Lane.identity.device.stable_device_id -and $afterCapture.StableId -ceq $Lane.identity.device.stable_device_id -and $beforeCapture.Challenge -cne $afterCapture.Challenge -and $beforeCapture.CurrentIndex -eq $mixedScenario.process_index_before -and $afterCapture.CurrentIndex -eq $mixedScenario.process_index_after -and $beforeCapture.CurrentIndex -ne $afterCapture.CurrentIndex) "$label mixed-GPU raw captures do not prove stable-device remapping across fresh challenges and different process indexes."
    [object[]]$allRuns = @($parsed.cold.cpu) + @($parsed.cold.gpu) + @($parsed.warm.cpu) + @($parsed.warm.gpu)
    [object[]]$successfulGpuRuns = @(@($parsed.cold.gpu) + @($parsed.warm.gpu) | Where-Object { $_.outcome -ceq 'success' })
    [object[]]$successfulGpuScenarios = @($scenarioResults | Where-Object { $_.Value.result -ceq 'pass' -and $_.Value.selected_backend -ceq $Lane.identity.backend -and $_.Value.observed_failure_category -ceq 'none' } | ForEach-Object { $_.Value })
    [Int64[]]$exercisedAvailableMemory = @($successfulGpuRuns | ForEach-Object { [Int64]$_.available_device_memory_bytes_before }) + @($successfulGpuScenarios | ForEach-Object { [Int64]$_.available_device_memory_bytes })
    Assert-Condition ($exercisedAvailableMemory.Count -gt 0) "$label has no successful GPU-start memory evidence."
    $derivedMinimumAvailableMemory = [Int64]($exercisedAvailableMemory | Measure-Object -Minimum).Minimum
    Assert-Condition ($Lane.identity.device.qualified_minimum_available_memory_bytes -eq $derivedMinimumAvailableMemory) "$label projected minimum available memory must equal the minimum exercised successful GPU-start availability."
    Assert-Condition (@($successfulGpuRuns | Where-Object { $_.available_device_memory_bytes_before -lt $derivedMinimumAvailableMemory }).Count -eq 0 -and @($successfulGpuScenarios | Where-Object { $_.available_device_memory_bytes -lt $derivedMinimumAvailableMemory }).Count -eq 0) "$label contains a successful GPU start below its projected available-memory floor."
    $allSuccessful = @($allRuns | Where-Object { $_.outcome -cne 'success' }).Count -eq 0
    $correctness = $allSuccessful -and @($allRuns | Where-Object { $_.transcript_sha256 -cne $Lane.identity.workload.expected_transcript_sha256 }).Count -eq 0
    $reliability = $allSuccessful
    $scenarioPassed = @($scenarioResults | Where-Object { -not $_.Passed }).Count -eq 0
    $performance = $allSuccessful
    if ($performance) {
        foreach ($mode in @('cold', 'warm')) {
            $gpu = Get-NearestRank @($parsed[$mode].gpu | ForEach-Object { [Int64]$_.end_to_end_ms }) 95
            $cpu = Get-NearestRank @($parsed[$mode].cpu | ForEach-Object { [Int64]$_.end_to_end_ms }) 95
            if (([Numerics.BigInteger]$gpu * 100) -gt ([Numerics.BigInteger]$cpu * [Int64]$Plan.maximum_gpu_p95_cpu_percent)) { $performance = $false }
        }
    }
    $reasons = [Collections.Generic.List[string]]::new()
    if (-not $correctness) { $reasons.Add('correctness_not_equivalent') }
    if (-not $reliability) { $reasons.Add('reliability_not_equivalent') }
    if (-not $scenarioPassed) { $reasons.Add('scenario_evidence_failed') }
    if (-not $performance) { $reasons.Add('gpu_p95_exceeds_cpu_boundary') }
    $passed = $reasons.Count -eq 0
    $summary = [ordered]@{
        backend = $Lane.identity.backend
        device_stable_id = $Lane.identity.device.stable_device_id
        driver = $Lane.identity.driver.value
        lane_id = $Lane.identity.lane_id
        metrics = [ordered]@{
            cold = [ordered]@{ cpu = Get-MetricSummary @($parsed.cold.cpu); gpu = Get-MetricSummary @($parsed.cold.gpu) }
            warm = [ordered]@{ cpu = Get-MetricSummary @($parsed.warm.cpu); gpu = Get-MetricSummary @($parsed.warm.gpu) }
        }
        checks = [ordered]@{ correctness_equivalent = $correctness; performance_passed = $performance; reliability_equivalent = $reliability; scenarios_passed = $scenarioPassed }
        evidence_memory_floor = [ordered]@{ minimum_available_memory_bytes = $derivedMinimumAvailableMemory; minimum_total_memory_bytes = [Int64]$Lane.identity.device.total_memory_bytes }
        qualification_passed = $passed
        reasons = $reasons.ToArray()
        auto_entry_projection = if ($passed) { Get-AutoProjection $Lane.identity $Lane.run_sets $parsed $derivedMinimumAvailableMemory } else { $null }
    }
    Assert-Condition ($Context.Used.Count -eq $Context.Inventory.Count) "$label signed artifact inventory contains unreferenced files."
    return $summary
}

function Get-Decision($Plan, [byte[]]$PlanRaw, $Evidence, [string]$RepositoryRoot, [bool]$FixtureAllowed, [string]$ArtifactDirectory) {
    $validatedPlan = Assert-Plan $Plan $RepositoryRoot
    $authority = Import-ProductionAuthority $RepositoryRoot
    [object[]]$manifestEntries = Import-AutoManifest $validatedPlan.Contracts.auto_manifest_sha256
    Assert-ExactKeys $Evidence @('schema_version', 'kind', 'fixture_only', 'plan_sha256', 'lanes') 'qualification evidence'
    Assert-Condition ((Get-JsonInteger $Evidence.schema_version 'qualification evidence.schema_version' 2 2) -eq 2) 'Qualification evidence schema is unsupported.'
    Assert-Condition ((Get-JsonString $Evidence.kind 'qualification evidence.kind') -ceq 'windows_gpu_release_qualification_evidence') 'Qualification evidence kind is unsupported.'
    $fixture = Get-JsonBoolean $Evidence.fixture_only 'qualification evidence.fixture_only'
    Assert-Condition ($fixture -eq $Plan.fixture_only) 'Qualification plan and evidence fixture modes differ.'
    Assert-Condition (-not $fixture -or $FixtureAllowed) 'Fixture-only qualification evidence requires -AllowFixture.'
    $planDigest = Get-Sha256Bytes $PlanRaw
    Assert-Condition ((Get-Sha256Value $Evidence.plan_sha256 'qualification evidence.plan_sha256') -ceq $planDigest) 'Qualification evidence does not bind the exact reviewed plan.'
    Assert-Condition ($fixture -or $authority.Approved.ContainsKey($planDigest)) 'Qualification plan is not approved by the protected production authority.'
    Assert-Array $Evidence.lanes 'qualification evidence.lanes'
    [object[]]$lanes = @($Evidence.lanes)
    Assert-Condition ($lanes.Count -le $MaxLanes) 'Qualification evidence exceeds the representative-lane bound.'
    Assert-Condition ($lanes.Count -eq $validatedPlan.Required.Count) 'Qualification evidence does not cover every representative lane.'
    if ($lanes.Count -gt 0) {
        Assert-Condition (-not [string]::IsNullOrWhiteSpace($ArtifactDirectory)) 'Nonempty qualification evidence requires -ArtifactRoot.'
        try { [ScribeWindowsQualification.NativeFile]::ValidatePhysicalDirectory([IO.Path]::GetFullPath($ArtifactDirectory)) }
        catch { Fail "Artifact root is not a physical Windows directory: $($_.Exception.Message)" }
    }
    $context = [pscustomobject]@{
        Root = if ([string]::IsNullOrWhiteSpace($ArtifactDirectory)) { '' } else { [IO.Path]::GetFullPath($ArtifactDirectory) }
        Paths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        Digests = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        Count = 0
        Bytes = [UInt64]0
        Inventory = $null
        Used = $null
    }
    $generationCaptures = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
    $captureGenerations = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::Ordinal)
    $challenges = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $validatedCaptures = [Collections.Generic.Dictionary[string, object]]::new([StringComparer]::Ordinal)
    $summaries = [Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $lanes.Count; $index++) {
        # Authenticate the complete lane and signed inventory before opening or
        # interpreting any referenced artifact bytes.
        [object[]]$inventory = Assert-LaneAttestation $lanes[$index] $Plan $validatedPlan.Required[$index] $planDigest $authority "evidence lane $index"
        Read-LaneInventory $context $inventory "evidence lane $index"
        $summaries.Add((Assert-LaneEvidence $lanes[$index] $validatedPlan.Required[$index] $Plan $index $context $generationCaptures $captureGenerations $challenges $validatedCaptures))
    }
    $summaryArray = $summaries.ToArray()
    $complete = $validatedPlan.Required.Count -gt 0 -and $summaryArray.Count -eq $validatedPlan.Required.Count
    $mixedCoverage = $complete -and @($lanes | Where-Object { $_.identity.acquisition.device_set.mixed_gpu }).Count -gt 0
    $containsSharedMemoryLane = @($lanes | Where-Object { $_.identity.device.device_class -cne 'discrete_gpu' }).Count -gt 0
    Assert-Condition (-not $Plan.runtime_bucket_complete -or -not $containsSharedMemoryLane) 'Qualification schema v2 cannot mark an integrated or unified GPU runtime bucket complete without paired battery performance runs.'
    $passed = $complete -and $mixedCoverage -and @($summaryArray | Where-Object { -not $_.qualification_passed }).Count -eq 0
    [object[]]$projections = @($summaryArray | Where-Object { $null -ne $_.auto_entry_projection } | ForEach-Object { $_.auto_entry_projection })
    $projectionDigests = @($projections | ForEach-Object { Get-CanonicalDigest $_ } | Sort-Object)
    $manifestDigests = @($manifestEntries | ForEach-Object { Get-CanonicalDigest $_ } | Sort-Object)
    $coverageDigests = @($projections | ForEach-Object { $copy = [ordered]@{}; foreach ($key in $_.Keys) { if ($key -cne 'evidence') { $copy[$key] = $_[$key] } }; Get-CanonicalDigest $copy })
    $activationComplete = $passed -and $Plan.runtime_bucket_complete -and $projections.Count -gt 0 -and @($projectionDigests | Sort-Object -Unique).Count -eq $projectionDigests.Count -and @($coverageDigests | Sort-Object -Unique).Count -eq $coverageDigests.Count -and ($projectionDigests -join ',') -ceq ($manifestDigests -join ',')
    $eligible = $passed -and $activationComplete -and -not $fixture
    $reason = if ($fixture) { 'fixture_only_never_auto_eligible' } elseif (-not $complete) { 'no_complete_representative_evidence' } elseif (-not $mixedCoverage) { 'mixed_gpu_coverage_missing' } elseif (-not $passed) { 'one_or_more_representative_lanes_failed' } elseif (-not $Plan.runtime_bucket_complete) { 'runtime_bucket_coverage_not_reviewed' } elseif (-not $activationComplete) { 'exact_one_to_one_auto_projection_missing' } else { 'complete_release_evidence_passed' }
    return [ordered]@{
        schema_version = 2
        kind = 'windows_gpu_release_qualification_decision'
        activation_manifest_complete = $activationComplete
        activation_projection_count = $projections.Count
        artifact_bytes = [Int64]$context.Bytes
        artifact_count = $context.Count
        auto_eligible = $eligible
        decision_reason = $reason
        evidence_complete = $complete
        evidence_sha256 = Get-CanonicalDigest $Evidence
        fixture_only = $fixture
        lanes = $summaryArray
        mixed_gpu_coverage = $mixedCoverage
        plan_sha256 = $planDigest
        production_authority_sha256 = $authority.Digest
        qualification_passed = $passed
        runtime_bucket_complete = [bool]$Plan.runtime_bucket_complete
    }
}

$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
try {
    $loadedPlan = Import-CanonicalJson $PlanPath 'qualification plan'
    $loadedEvidence = Import-CanonicalJson $EvidencePath 'qualification evidence'
    $decision = Get-Decision $loadedPlan.Document $loadedPlan.Raw $loadedEvidence.Document $repositoryRoot $AllowFixture.IsPresent $ArtifactRoot
    [byte[]]$payload = Get-CanonicalBytesFromObject $decision
    [Console]::OpenStandardOutput().Write($payload, 0, $payload.Length)
    if ($RequireEligible -and -not $decision.auto_eligible) { exit 2 }
    exit 0
}
catch {
    [Console]::Error.WriteLine("Windows GPU qualification rejected: $($_.Exception.Message)")
    exit 1
}
