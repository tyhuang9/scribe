Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1')

if (-not ('ScribeEvidenceNative.BoundPendingFile' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Microsoft.Win32.SafeHandles;

namespace ScribeEvidenceNative
{
    public sealed class BoundEvidenceRead
    {
        public byte[] Bytes { get; }
        public string Sha256 { get; }

        internal BoundEvidenceRead(byte[] bytes, string sha256)
        {
            Bytes = bytes;
            Sha256 = sha256;
        }
    }

    public sealed class BoundVerifiedEvidence
    {
        public string Utf8Json { get; }
        public string Sha256 { get; }
        public string Identity { get; }

        internal BoundVerifiedEvidence(string utf8Json, string sha256, string identity)
        {
            Utf8Json = utf8Json;
            Sha256 = sha256;
            Identity = identity;
        }
    }

    public static class StrictEvidenceJson
    {
        private static readonly string[] RootProperties = new string[]
        {
            "schema_version", "fixture_only", "untrusted", "auto_eligible",
            "source_revision", "pack", "model_sha256", "wav_sha256", "gpu",
            "nvidia_baseline", "cold_runs_per_backend", "warm_runs_per_backend",
            "cpu", "vulkan", "expected_phrase_present_every_run",
            "normalized_transcript_parity", "same_device_internally_verified"
        };
        private static readonly string[] PackProperties = new string[]
        {
            "id", "version", "digest", "security_epoch", "runtime_abi"
        };
        private static readonly string[] GpuProperties = new string[]
        {
            "backend", "provider", "vendor", "device_class", "driver", "memory_total_bytes"
        };
        private static readonly string[] BaselineProperties = new string[]
        {
            "product", "driver", "memory_total_bytes", "memory_used_bytes", "gpu_utilization_percent"
        };
        private static readonly string[] BackendProperties = new string[] { "cold", "warm" };
        private static readonly string[] RunSetProperties = new string[]
        {
            "end_to_end", "end_to_end_ms", "backend_processing",
            "backend_processing_ms", "model_load", "model_load_ms"
        };
        private static readonly string[] StatisticsProperties = new string[] { "p50_ms", "p95_ms" };

        public static void Validate(string utf8Json)
        {
            if (utf8Json == null) throw SchemaError();
            JsonDocumentOptions options = new JsonDocumentOptions
            {
                AllowTrailingCommas = false,
                CommentHandling = JsonCommentHandling.Disallow,
                MaxDepth = 64
            };
            using (JsonDocument document = JsonDocument.Parse(utf8Json, options))
            {
                JsonElement root = document.RootElement;
                ValidateNoDuplicateProperties(root);
                ValidateRoot(root);
            }
        }

        private static void ValidateRoot(JsonElement root)
        {
            RequireExactObject(root, RootProperties);
            RequireUnsignedInteger(root.GetProperty("schema_version"));
            RequireBoolean(root.GetProperty("fixture_only"));
            RequireBoolean(root.GetProperty("untrusted"));
            RequireBoolean(root.GetProperty("auto_eligible"));
            RequireString(root.GetProperty("source_revision"));
            ValidatePack(root.GetProperty("pack"));
            RequireString(root.GetProperty("model_sha256"));
            RequireString(root.GetProperty("wav_sha256"));
            ValidateGpu(root.GetProperty("gpu"));
            ValidateBaseline(root.GetProperty("nvidia_baseline"));
            RequireUnsignedInteger(root.GetProperty("cold_runs_per_backend"));
            RequireUnsignedInteger(root.GetProperty("warm_runs_per_backend"));
            ValidateBackend(root.GetProperty("cpu"));
            ValidateBackend(root.GetProperty("vulkan"));
            RequireBoolean(root.GetProperty("expected_phrase_present_every_run"));
            RequireBoolean(root.GetProperty("normalized_transcript_parity"));
            RequireBoolean(root.GetProperty("same_device_internally_verified"));
        }

        private static void ValidatePack(JsonElement value)
        {
            RequireExactObject(value, PackProperties);
            RequireString(value.GetProperty("id"));
            RequireString(value.GetProperty("version"));
            RequireString(value.GetProperty("digest"));
            RequireUnsignedInteger(value.GetProperty("security_epoch"));
            RequireUnsignedInteger(value.GetProperty("runtime_abi"));
        }

        private static void ValidateGpu(JsonElement value)
        {
            RequireExactObject(value, GpuProperties);
            RequireString(value.GetProperty("backend"));
            RequireString(value.GetProperty("provider"));
            RequireString(value.GetProperty("vendor"));
            RequireString(value.GetProperty("device_class"));
            RequireString(value.GetProperty("driver"));
            RequireUnsignedInteger(value.GetProperty("memory_total_bytes"));
        }

        private static void ValidateBaseline(JsonElement value)
        {
            RequireExactObject(value, BaselineProperties);
            RequireString(value.GetProperty("product"));
            RequireString(value.GetProperty("driver"));
            RequireUnsignedInteger(value.GetProperty("memory_total_bytes"));
            RequireUnsignedInteger(value.GetProperty("memory_used_bytes"));
            RequireUnsignedInteger(value.GetProperty("gpu_utilization_percent"));
        }

        private static void ValidateBackend(JsonElement value)
        {
            RequireExactObject(value, BackendProperties);
            ValidateRunSet(value.GetProperty("cold"), true);
            ValidateRunSet(value.GetProperty("warm"), false);
        }

        private static void ValidateRunSet(JsonElement value, bool cold)
        {
            RequireExactObject(value, RunSetProperties);
            ValidateStatistics(value.GetProperty("end_to_end"));
            ValidateUnsignedIntegerArray(value.GetProperty("end_to_end_ms"));
            ValidateStatistics(value.GetProperty("backend_processing"));
            ValidateUnsignedIntegerArray(value.GetProperty("backend_processing_ms"));
            if (cold)
            {
                ValidateStatistics(value.GetProperty("model_load"));
                ValidateUnsignedIntegerArray(value.GetProperty("model_load_ms"));
            }
            else
            {
                RequireKind(value.GetProperty("model_load"), JsonValueKind.Null);
                RequireKind(value.GetProperty("model_load_ms"), JsonValueKind.Null);
            }
        }

        private static void ValidateStatistics(JsonElement value)
        {
            RequireExactObject(value, StatisticsProperties);
            RequireUnsignedInteger(value.GetProperty("p50_ms"));
            RequireUnsignedInteger(value.GetProperty("p95_ms"));
        }

        private static void ValidateUnsignedIntegerArray(JsonElement value)
        {
            RequireKind(value, JsonValueKind.Array);
            foreach (JsonElement item in value.EnumerateArray()) RequireUnsignedInteger(item);
        }

        private static void ValidateNoDuplicateProperties(JsonElement value)
        {
            if (value.ValueKind == JsonValueKind.Object)
            {
                HashSet<string> names = new HashSet<string>(StringComparer.Ordinal);
                foreach (JsonProperty property in value.EnumerateObject())
                {
                    if (!names.Add(property.Name)) throw SchemaError();
                    ValidateNoDuplicateProperties(property.Value);
                }
            }
            else if (value.ValueKind == JsonValueKind.Array)
            {
                foreach (JsonElement item in value.EnumerateArray()) ValidateNoDuplicateProperties(item);
            }
        }

        private static void RequireExactObject(JsonElement value, string[] propertyNames)
        {
            RequireKind(value, JsonValueKind.Object);
            HashSet<string> remaining = new HashSet<string>(propertyNames, StringComparer.Ordinal);
            foreach (JsonProperty property in value.EnumerateObject())
            {
                if (!remaining.Remove(property.Name)) throw SchemaError();
            }
            if (remaining.Count != 0) throw SchemaError();
        }

        private static void RequireString(JsonElement value)
        {
            RequireKind(value, JsonValueKind.String);
        }

        private static void RequireBoolean(JsonElement value)
        {
            if (value.ValueKind != JsonValueKind.True && value.ValueKind != JsonValueKind.False)
                throw SchemaError();
        }

        private static void RequireUnsignedInteger(JsonElement value)
        {
            RequireKind(value, JsonValueKind.Number);
            string raw = value.GetRawText();
            if (raw.Length == 0) throw SchemaError();
            if (raw.Length > 1 && (raw[0] < '1' || raw[0] > '9')) throw SchemaError();
            for (int index = raw.Length == 1 ? 0 : 1; index < raw.Length; index++)
            {
                if (raw[index] < '0' || raw[index] > '9') throw SchemaError();
            }
            ulong parsed;
            if (!UInt64.TryParse(raw, NumberStyles.None, CultureInfo.InvariantCulture, out parsed))
                throw SchemaError();
        }

        private static void RequireKind(JsonElement value, JsonValueKind kind)
        {
            if (value.ValueKind != kind) throw SchemaError();
        }

        private static InvalidOperationException SchemaError()
        {
            return new InvalidOperationException("Evidence report violates the strict JSON schema.");
        }
    }

    public sealed class BoundPendingFile : IDisposable
    {
        public const int ConsumerApiVersion = 2;
        private const uint GenericRead = 0x80000000;
        private const uint DeleteAccess = 0x00010000;
        private const uint FileListDirectory = 0x00000001;
        private const uint FileReadAttributes = 0x00000080;
        private const uint FileShareRead = 0x00000001;
        private const uint FileShareAll = 0x00000007;
        private const uint OpenExisting = 3;
        private const uint FileAttributeDirectory = 0x00000010;
        private const uint FileAttributeReparsePoint = 0x00000400;
        private const uint FileFlagBackupSemantics = 0x02000000;
        private const uint FileFlagOpenReparsePoint = 0x00200000;
        private const uint FileFlagSequentialScan = 0x08000000;
        private const int ErrorFileNotFound = 2;
        private const int ErrorPathNotFound = 3;
        private const int ErrorNoMoreFiles = 18;
        private const int FileRenameInfo = 3;
        private const int FileDispositionInfo = 4;
        private const int FileStreamInfo = 7;
        private const int FileIdInfo = 18;
        private const int FileIdExtdDirectoryInfo = 19;
        private const int StreamInfoBufferBytes = 64 * 1024;
        private const int StreamInfoHeaderBytes = 24;
        private const int DirectoryInfoBufferBytes = 64 * 1024;
        private const int DirectoryInfoHeaderBytes = 88;
        private const int MaximumDirectoryEntries = 4096;
        private const int ReadBufferBytes = 64 * 1024;
        private const int MaximumNativePathCharacters = 1024;

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

        [DllImport("kernel32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SetFileInformationByHandle(
            SafeFileHandle file,
            int informationClass,
            IntPtr information,
            uint bufferSize);

        private readonly SafeFileHandle parentHandle;
        private readonly SafeFileHandle directoryHandle;
        private readonly SafeFileHandle fileHandle;
        private readonly string directoryPath;
        private readonly int length;
        private readonly bool mutationAllowed;
        private bool readComplete;
        private bool mutationComplete;
        private bool disposed;

        public string Identity { get; }
        public long Length { get { return length; } }

        private BoundPendingFile(
            SafeFileHandle parentHandle,
            SafeFileHandle directoryHandle,
            SafeFileHandle fileHandle,
            string directoryPath,
            int length,
            string identity,
            bool mutationAllowed)
        {
            this.parentHandle = parentHandle;
            this.directoryHandle = directoryHandle;
            this.fileHandle = fileHandle;
            this.directoryPath = directoryPath;
            this.length = length;
            this.mutationAllowed = mutationAllowed;
            Identity = identity;
        }

        public static BoundPendingFile Open(
            string evidenceRoot,
            string pendingPath,
            string expectedLeaf,
            int maximumBytes,
            bool allowMissing,
            bool allowEmpty)
        {
            return OpenCore(
                evidenceRoot,
                pendingPath,
                expectedLeaf,
                maximumBytes,
                allowMissing,
                allowEmpty,
                GenericRead | DeleteAccess,
                true,
                true);
        }

        public static BoundPendingFile OpenPublished(
            string evidenceRoot,
            string publishedPath,
            string expectedLeaf,
            int maximumBytes)
        {
            return OpenCore(
                evidenceRoot,
                publishedPath,
                expectedLeaf,
                maximumBytes,
                false,
                false,
                GenericRead,
                false,
                false);
        }

        private static BoundPendingFile OpenCore(
            string evidenceRoot,
            string evidencePath,
            string expectedLeaf,
            int maximumBytes,
            bool allowMissing,
            bool allowEmpty,
            uint desiredAccess,
            bool requireSingleLink,
            bool mutationAllowed)
        {
            if (maximumBytes <= 0 || maximumBytes > 1024 * 1024)
                throw new ArgumentOutOfRangeException(nameof(maximumBytes));
            ValidateLeaf(expectedLeaf, nameof(expectedLeaf));

            string root = NormalizeDirectory(evidenceRoot);
            string pending = Path.GetFullPath(evidencePath);
            string rootParent = NormalizeDirectory(Path.GetDirectoryName(root));
            string rootLeaf = Path.GetFileName(root);
            ValidateDirectoryLeaf(rootLeaf);
            if (root.Length > MaximumNativePathCharacters || pending.Length > MaximumNativePathCharacters)
                throw new PathTooLongException("Evidence publication paths exceed their native bound.");
            if (!string.Equals(Path.GetFileName(pending), expectedLeaf, StringComparison.Ordinal) ||
                !string.Equals(NormalizeDirectory(Path.GetDirectoryName(pending)), root, StringComparison.OrdinalIgnoreCase))
            {
                throw new InvalidOperationException("Pending evidence must be the exact direct child of its evidence root.");
            }

            SafeFileHandle parent = CreateFileW(
                rootParent,
                FileListDirectory | FileReadAttributes,
                FileShareAll,
                IntPtr.Zero,
                OpenExisting,
                FileFlagBackupSemantics | FileFlagOpenReparsePoint,
                IntPtr.Zero);
            if (parent.IsInvalid)
            {
                int error = Marshal.GetLastWin32Error();
                parent.Dispose();
                throw NativeError("lock the physical evidence-directory parent", error);
            }

            SafeFileHandle directory = null;
            SafeFileHandle file = null;
            try
            {
                ByHandleFileInformation parentInfo = GetInformation(parent, "inspect the evidence-directory parent");
                if ((parentInfo.FileAttributes & FileAttributeDirectory) == 0 ||
                    (parentInfo.FileAttributes & FileAttributeReparsePoint) != 0)
                {
                    throw new InvalidOperationException("Evidence-directory parent must be a physical non-reparse directory.");
                }

                directory = CreateFileW(
                    root,
                    FileListDirectory | FileReadAttributes,
                    FileShareAll,
                    IntPtr.Zero,
                    OpenExisting,
                    FileFlagBackupSemantics | FileFlagOpenReparsePoint,
                    IntPtr.Zero);
                if (directory.IsInvalid)
                {
                    int error = Marshal.GetLastWin32Error();
                    directory.Dispose();
                    directory = null;
                    throw NativeError("open the physical evidence directory", error);
                }

                ByHandleFileInformation directoryInfo = GetInformation(directory, "inspect the evidence directory");
                if ((directoryInfo.FileAttributes & FileAttributeDirectory) == 0 ||
                    (directoryInfo.FileAttributes & FileAttributeReparsePoint) != 0)
                {
                    throw new InvalidOperationException("Evidence root must be a physical non-reparse directory.");
                }
                ValidateDirectoryMembership(parent, directory, rootLeaf);

                file = CreateFileW(
                    pending,
                    desiredAccess,
                    FileShareRead,
                    IntPtr.Zero,
                    OpenExisting,
                    FileFlagOpenReparsePoint | FileFlagSequentialScan,
                    IntPtr.Zero);
                if (file.IsInvalid)
                {
                    int error = Marshal.GetLastWin32Error();
                    file.Dispose();
                    file = null;
                    if (allowMissing && (error == ErrorFileNotFound || error == ErrorPathNotFound))
                        return null;
                    throw NativeError("open the pending evidence without write/delete sharing", error);
                }

                ByHandleFileInformation fileInfo = GetInformation(file, "inspect the pending evidence identity");
                if ((fileInfo.FileAttributes & (FileAttributeDirectory | FileAttributeReparsePoint)) != 0)
                    throw new InvalidOperationException("Pending evidence must be a regular non-reparse file.");
                if (requireSingleLink && fileInfo.NumberOfLinks != 1)
                    throw new InvalidOperationException("Pending evidence must have exactly one hard link.");

                ulong fileLength = ((ulong)fileInfo.FileSizeHigh << 32) | fileInfo.FileSizeLow;
                if (fileLength > (ulong)maximumBytes || (!allowEmpty && fileLength == 0))
                    throw new InvalidOperationException("Pending evidence is outside the bounded file-size contract.");
                ValidateDirectoryMembership(directory, file, expectedLeaf);
                ValidateOnlyUnnamedDataStream(file, fileLength);

                string identity = string.Format(
                    System.Globalization.CultureInfo.InvariantCulture,
                    "{0:x8}:{1:x8}{2:x8}",
                    fileInfo.VolumeSerialNumber,
                    fileInfo.FileIndexHigh,
                    fileInfo.FileIndexLow);
                BoundPendingFile result = new BoundPendingFile(
                    parent,
                    directory,
                    file,
                    root,
                    checked((int)fileLength),
                    identity,
                    mutationAllowed);
                parent = null;
                directory = null;
                file = null;
                return result;
            }
            finally
            {
                if (file != null) file.Dispose();
                if (directory != null) directory.Dispose();
                if (parent != null) parent.Dispose();
            }
        }

        public BoundEvidenceRead ReadAllAndHash()
        {
            ThrowIfDisposedOrMutated();
            if (!mutationAllowed)
                throw new InvalidOperationException("Published evidence must use the digest-bound verifier.");
            byte[] bytes = ReadAllBoundBytes();
            byte[] digest = ComputeSha256(bytes);
            return new BoundEvidenceRead(bytes, FormatSha256(digest));
        }

        public BoundVerifiedEvidence ReadAllAndVerify(string expectedSha256)
        {
            ThrowIfDisposedOrMutated();
            if (mutationAllowed)
                throw new InvalidOperationException("Pending evidence cannot be consumed as published evidence.");

            byte[] expectedDigest = ParseCanonicalSha256(expectedSha256);
            byte[] bytes = ReadAllBoundBytes();
            byte[] actualDigest = ComputeSha256(bytes);
            if (!CryptographicOperations.FixedTimeEquals(actualDigest, expectedDigest))
                throw new InvalidOperationException("Published evidence SHA-256 does not match the independently supplied digest.");

            string utf8Json;
            try
            {
                utf8Json = new UTF8Encoding(false, true).GetString(bytes);
            }
            catch (DecoderFallbackException exception)
            {
                throw new InvalidOperationException("Published evidence is not strict UTF-8.", exception);
            }
            return new BoundVerifiedEvidence(utf8Json, FormatSha256(actualDigest), Identity);
        }

        public string GetFinalPath(string finalLeaf)
        {
            ThrowIfDisposedOrMutated();
            ThrowIfMutationNotAllowed();
            ValidateLeaf(finalLeaf, nameof(finalLeaf));
            return Path.Combine(directoryPath, finalLeaf);
        }

        public void RenameNoReplace(string finalLeaf)
        {
            ThrowIfDisposedOrMutated();
            ThrowIfMutationNotAllowed();
            if (!readComplete)
                throw new InvalidOperationException("Pending evidence must be read and hashed before publication.");
            ValidateLeaf(finalLeaf, nameof(finalLeaf));

            string finalPath = Path.Combine(directoryPath, finalLeaf);
            if (finalPath.Length > MaximumNativePathCharacters)
                throw new PathTooLongException("Final evidence path exceeds its native bound.");
            byte[] name = Encoding.Unicode.GetBytes(finalPath + "\0");
            int rootOffset = IntPtr.Size == 8 ? 8 : 4;
            int nameLengthOffset = checked(rootOffset + IntPtr.Size);
            int nameOffset = checked(nameLengthOffset + sizeof(uint));
            int minimumStructureBytes = IntPtr.Size == 8 ? 24 : 16;
            int bufferBytes = checked(minimumStructureBytes + name.Length);
            IntPtr information = Marshal.AllocHGlobal(bufferBytes);
            try
            {
                for (int index = 0; index < bufferBytes; index++) Marshal.WriteByte(information, index, 0);
                Marshal.WriteIntPtr(information, rootOffset, IntPtr.Zero);
                Marshal.WriteInt32(information, nameLengthOffset, name.Length - sizeof(char));
                Marshal.Copy(name, 0, IntPtr.Add(information, nameOffset), name.Length);
                if (!SetFileInformationByHandle(
                    fileHandle,
                    FileRenameInfo,
                    information,
                    checked((uint)bufferBytes)))
                {
                    throw NativeError("atomically publish the pending evidence without replacement", Marshal.GetLastWin32Error());
                }
                mutationComplete = true;
            }
            finally
            {
                Marshal.FreeHGlobal(information);
            }
        }

        public void Delete()
        {
            ThrowIfDisposedOrMutated();
            ThrowIfMutationNotAllowed();
            IntPtr information = Marshal.AllocHGlobal(1);
            try
            {
                Marshal.WriteByte(information, 1);
                if (!SetFileInformationByHandle(
                    fileHandle,
                    FileDispositionInfo,
                    information,
                    1))
                {
                    throw NativeError("delete the validated pending evidence identity", Marshal.GetLastWin32Error());
                }
                mutationComplete = true;
            }
            finally
            {
                Marshal.FreeHGlobal(information);
            }
        }

        public void Dispose()
        {
            if (disposed) return;
            disposed = true;
            fileHandle.Dispose();
            directoryHandle.Dispose();
            parentHandle.Dispose();
        }

        private static string NormalizeDirectory(string path)
        {
            string full = Path.GetFullPath(path);
            string root = Path.GetPathRoot(full);
            string trimmed = full.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            string trimmedRoot = root.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
            return string.Equals(trimmed, trimmedRoot, StringComparison.OrdinalIgnoreCase) ? root : trimmed;
        }

        private byte[] ReadAllBoundBytes()
        {
            if (readComplete) throw new InvalidOperationException("Bound evidence was already read.");

            byte[] bytes = new byte[length];
            byte[] buffer = new byte[Math.Min(ReadBufferBytes, Math.Max(length, 1))];
            int total = 0;
            while (total < length)
            {
                uint requested = checked((uint)Math.Min(buffer.Length, length - total));
                uint read;
                if (!ReadFile(fileHandle, buffer, requested, out read, IntPtr.Zero))
                    throw NativeError("read the bound evidence", Marshal.GetLastWin32Error());
                if (read == 0)
                    throw new InvalidOperationException("Bound evidence ended before its bound size.");
                Buffer.BlockCopy(buffer, 0, bytes, total, checked((int)read));
                total = checked(total + (int)read);
            }

            uint trailing;
            if (!ReadFile(fileHandle, buffer, 1, out trailing, IntPtr.Zero))
                throw NativeError("confirm the bound evidence length", Marshal.GetLastWin32Error());
            if (trailing != 0)
                throw new InvalidOperationException("Bound evidence grew after its identity was opened.");

            readComplete = true;
            return bytes;
        }

        private static byte[] ComputeSha256(byte[] bytes)
        {
            using (SHA256 sha256 = SHA256.Create())
            {
                return sha256.ComputeHash(bytes);
            }
        }

        private static string FormatSha256(byte[] digest)
        {
            return BitConverter.ToString(digest).Replace("-", string.Empty).ToLowerInvariant();
        }

        private static byte[] ParseCanonicalSha256(string digest)
        {
            if (digest == null || digest.Length != 64)
                throw new ArgumentException("Expected evidence SHA-256 must be exactly 64 lowercase hexadecimal characters.", nameof(digest));
            byte[] parsed = new byte[32];
            for (int index = 0; index < parsed.Length; index++)
            {
                int high = ParseLowerHex(digest[index * 2]);
                int low = ParseLowerHex(digest[index * 2 + 1]);
                if (high < 0 || low < 0)
                    throw new ArgumentException("Expected evidence SHA-256 must be exactly 64 lowercase hexadecimal characters.", nameof(digest));
                parsed[index] = checked((byte)((high << 4) | low));
            }
            return parsed;
        }

        private static int ParseLowerHex(char value)
        {
            if (value >= '0' && value <= '9') return value - '0';
            if (value >= 'a' && value <= 'f') return value - 'a' + 10;
            return -1;
        }

        private static void ValidateLeaf(string leaf, string parameter)
        {
            if (string.IsNullOrEmpty(leaf) || leaf.Length > 128 || !leaf.EndsWith(".json", StringComparison.Ordinal) ||
                !IsAsciiAlphaNumeric(leaf[0]))
                throw new ArgumentException("Evidence leaf is not a bounded canonical JSON name.", parameter);
            foreach (char character in leaf)
            {
                if (!IsAsciiAlphaNumeric(character) && character != '.' && character != '_' && character != '-')
                    throw new ArgumentException("Evidence leaf is not a bounded canonical JSON name.", parameter);
            }
        }

        private static void ValidateDirectoryLeaf(string leaf)
        {
            if (string.IsNullOrEmpty(leaf) || leaf.Length > 96 || !IsAsciiAlphaNumericIgnoreCase(leaf[0]))
                throw new InvalidOperationException("Evidence root leaf is not bounded and canonical.");
            foreach (char character in leaf)
            {
                if (!IsAsciiAlphaNumericIgnoreCase(character) && character != '.' && character != '_' && character != '-')
                    throw new InvalidOperationException("Evidence root leaf is not bounded and canonical.");
            }
        }

        private static bool IsAsciiAlphaNumeric(char character)
        {
            return (character >= 'a' && character <= 'z') || (character >= '0' && character <= '9');
        }

        private static bool IsAsciiAlphaNumericIgnoreCase(char character)
        {
            return IsAsciiAlphaNumeric(character) || (character >= 'A' && character <= 'Z');
        }

        private static ByHandleFileInformation GetInformation(SafeFileHandle handle, string operation)
        {
            ByHandleFileInformation information;
            if (!GetFileInformationByHandle(handle, out information))
                throw NativeError(operation, Marshal.GetLastWin32Error());
            return information;
        }

        private static void ValidateOnlyUnnamedDataStream(SafeFileHandle handle, ulong expectedLength)
        {
            IntPtr buffer = Marshal.AllocHGlobal(StreamInfoBufferBytes);
            try
            {
                if (!GetFileInformationByHandleEx(
                    handle,
                    FileStreamInfo,
                    buffer,
                    StreamInfoBufferBytes))
                {
                    throw NativeError("enumerate bounded pending evidence streams", Marshal.GetLastWin32Error());
                }

                int offset = 0;
                int count = 0;
                while (true)
                {
                    if (offset < 0 || offset > StreamInfoBufferBytes - StreamInfoHeaderBytes)
                        throw new InvalidOperationException("Pending evidence stream metadata is malformed.");
                    uint nextOffset = unchecked((uint)Marshal.ReadInt32(buffer, offset));
                    uint nameBytes = unchecked((uint)Marshal.ReadInt32(buffer, offset + 4));
                    long streamSize = Marshal.ReadInt64(buffer, offset + 8);
                    if ((nameBytes & 1) != 0 || nameBytes > StreamInfoBufferBytes - offset - StreamInfoHeaderBytes)
                        throw new InvalidOperationException("Pending evidence stream metadata exceeds its bounded buffer.");
                    string streamName = Marshal.PtrToStringUni(
                        IntPtr.Add(buffer, offset + StreamInfoHeaderBytes),
                        checked((int)(nameBytes / 2)));
                    count = checked(count + 1);
                    if (count != 1 || !string.Equals(streamName, "::$DATA", StringComparison.Ordinal) ||
                        streamSize < 0 || (ulong)streamSize != expectedLength)
                    {
                        throw new InvalidOperationException("Pending evidence must contain only its unnamed data stream.");
                    }
                    if (nextOffset == 0) break;
                    if (nextOffset < StreamInfoHeaderBytes + nameBytes || nextOffset > StreamInfoBufferBytes - offset)
                        throw new InvalidOperationException("Pending evidence stream metadata has an invalid next offset.");
                    offset = checked(offset + (int)nextOffset);
                }
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static void ValidateDirectoryMembership(
            SafeFileHandle directory,
            SafeFileHandle file,
            string expectedLeaf)
        {
            byte[] expectedIdentity = GetFileId(file);
            IntPtr buffer = Marshal.AllocHGlobal(DirectoryInfoBufferBytes);
            try
            {
                int entryCount = 0;
                while (true)
                {
                    if (!GetFileInformationByHandleEx(
                        directory,
                        FileIdExtdDirectoryInfo,
                        buffer,
                        DirectoryInfoBufferBytes))
                    {
                        int error = Marshal.GetLastWin32Error();
                        if (error == ErrorNoMoreFiles) break;
                        throw NativeError("enumerate the bound evidence directory", error);
                    }

                    int offset = 0;
                    while (true)
                    {
                        if (offset < 0 || offset > DirectoryInfoBufferBytes - DirectoryInfoHeaderBytes)
                            throw new InvalidOperationException("Evidence directory metadata is malformed.");
                        uint nextOffset = unchecked((uint)Marshal.ReadInt32(buffer, offset));
                        uint nameBytes = unchecked((uint)Marshal.ReadInt32(buffer, offset + 60));
                        if ((nameBytes & 1) != 0 ||
                            nameBytes > DirectoryInfoBufferBytes - offset - DirectoryInfoHeaderBytes)
                        {
                            throw new InvalidOperationException("Evidence directory metadata exceeds its bounded buffer.");
                        }

                        entryCount = checked(entryCount + 1);
                        if (entryCount > MaximumDirectoryEntries)
                            throw new InvalidOperationException("Evidence directory exceeds its bounded entry contract.");
                        string name = Marshal.PtrToStringUni(
                            IntPtr.Add(buffer, offset + DirectoryInfoHeaderBytes),
                            checked((int)(nameBytes / 2)));
                        if (string.Equals(name, expectedLeaf, StringComparison.Ordinal))
                        {
                            for (int index = 0; index < expectedIdentity.Length; index++)
                            {
                                if (Marshal.ReadByte(buffer, offset + 72 + index) != expectedIdentity[index])
                                    throw new InvalidOperationException("Pending evidence path does not name the bound file identity.");
                            }
                            return;
                        }

                        if (nextOffset == 0) break;
                        if (nextOffset < DirectoryInfoHeaderBytes + nameBytes ||
                            nextOffset > DirectoryInfoBufferBytes - offset)
                        {
                            throw new InvalidOperationException("Evidence directory metadata has an invalid next offset.");
                        }
                        offset = checked(offset + (int)nextOffset);
                    }
                }
                throw new InvalidOperationException("Pending evidence is not the exact direct child of the bound evidence directory.");
            }
            finally
            {
                Marshal.FreeHGlobal(buffer);
            }
        }

        private static byte[] GetFileId(SafeFileHandle file)
        {
            const int FileIdInfoBytes = 24;
            IntPtr information = Marshal.AllocHGlobal(FileIdInfoBytes);
            try
            {
                if (!GetFileInformationByHandleEx(file, FileIdInfo, information, FileIdInfoBytes))
                    throw NativeError("read the pending evidence file identity", Marshal.GetLastWin32Error());
                byte[] identity = new byte[16];
                Marshal.Copy(IntPtr.Add(information, 8), identity, 0, identity.Length);
                return identity;
            }
            finally
            {
                Marshal.FreeHGlobal(information);
            }
        }

        private void ThrowIfDisposedOrMutated()
        {
            if (disposed) throw new ObjectDisposedException(nameof(BoundPendingFile));
            if (mutationComplete) throw new InvalidOperationException("Pending evidence identity was already mutated.");
        }

        private void ThrowIfMutationNotAllowed()
        {
            if (!mutationAllowed)
                throw new InvalidOperationException("Published evidence bindings are read-only.");
        }

        private static Win32Exception NativeError(string operation, int error)
        {
            return new Win32Exception(error, "Could not " + operation + ".");
        }
    }
}
'@
}

& {
    $nativeEvidenceType = 'ScribeEvidenceNative.BoundPendingFile' -as [type]
    $verifiedEvidenceType = 'ScribeEvidenceNative.BoundVerifiedEvidence' -as [type]
    $strictJsonType = 'ScribeEvidenceNative.StrictEvidenceJson' -as [type]
    $publicStatic = [Reflection.BindingFlags]::Public -bor [Reflection.BindingFlags]::Static
    $publicInstance = [Reflection.BindingFlags]::Public -bor [Reflection.BindingFlags]::Instance
    $versionField = if ($null -eq $nativeEvidenceType) { $null } else { $nativeEvidenceType.GetField('ConsumerApiVersion', $publicStatic) }
    $version = if ($null -eq $versionField -or -not $versionField.IsLiteral) { $null } else { $versionField.GetRawConstantValue() }
    $staticNativeMethods = if ($null -eq $nativeEvidenceType) { @() } else { @($nativeEvidenceType.GetMethods($publicStatic) | ForEach-Object { $_.Name }) }
    $instanceNativeMethods = if ($null -eq $nativeEvidenceType) { @() } else { @($nativeEvidenceType.GetMethods($publicInstance) | ForEach-Object { $_.Name }) }
    $strictJsonMethods = if ($null -eq $strictJsonType) { @() } else { @($strictJsonType.GetMethods($publicStatic) | ForEach-Object { $_.Name }) }
    $hasOpenPublished = $staticNativeMethods -ccontains 'OpenPublished'
    $hasReadAllAndVerify = $instanceNativeMethods -ccontains 'ReadAllAndVerify'
    $hasStrictValidate = $strictJsonMethods -ccontains 'Validate'
    if ($version -ne 2 -or
        $null -eq $verifiedEvidenceType -or
        -not $hasOpenPublished -or
        -not $hasReadAllAndVerify -or
        -not $hasStrictValidate) {
        throw 'Restart PowerShell/session: incompatible native evidence type is already loaded.'
    }
}

function Assert-ScribeEvidenceNoReparse([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { throw 'Could not find an existing non-reparse ancestor.' }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Evidence path crosses a reparse point.'
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) { break }
        $current = $parent
    }
}

function Get-ScribeEvidencePhysicalDirectory([string]$Path, [string]$Label) {
    Assert-ScribeEvidenceNoReparse $Path
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a physical non-reparse directory."
    }
    return $item
}

function Assert-ScribeEvidenceFile([string]$Path, [string]$Label, [UInt64]$MaxBytes) {
    $full = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    Assert-ScribeEvidenceNoReparse $full
    if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { throw "$Label is missing." }
    $item = Get-Item -LiteralPath $full -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 -or $item.Length -eq 0 -or $item.Length -gt $MaxBytes) {
        throw "$Label is not a bounded regular non-reparse file."
    }
    return $full
}

function Assert-ScribeEvidenceSingleLinkFile([string]$Path, [string]$Label, [UInt64]$MaxBytes, [string]$FsutilPath) {
    $full = Assert-ScribeEvidenceFile $Path $Label $MaxBytes
    $links = @(& $FsutilPath hardlink list $full)
    if ($LASTEXITCODE -ne 0 -or $links.Count -ne 1) { throw "$Label must have exactly one hard link." }
    return $full
}

function Assert-ScribeEvidenceExactProperties([psobject]$Value, [string[]]$Names, [string]$Label) {
    if ($null -eq $Value) { throw "$Label is missing." }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Assert-ScribeEvidenceDirectChildPath(
    [string]$Path,
    [string]$Root,
    [string]$ExpectedLeaf,
    [string]$Label
) {
    if ($ExpectedLeaf -cnotmatch '^[a-z0-9][a-z0-9._-]{0,127}\.json$') {
        throw "$Label has an unsafe expected leaf."
    }
    $rootItem = Get-ScribeEvidencePhysicalDirectory $Root "$Label root"
    $canonicalRoot = $rootItem.FullName.TrimEnd([char[]]@('\', '/'))
    $full = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    if ((Split-Path -Leaf $full) -cne $ExpectedLeaf -or
        (Split-Path -Parent $full).TrimEnd([char[]]@('\', '/')) -cne $canonicalRoot) {
        throw "$Label must be the exact direct child of its evidence root."
    }
    Assert-ScribeEvidenceNoReparse $full
    return $full
}

function Assert-ScribeEvidenceUnsignedInteger([object]$Value, [string]$Label) {
    $text = [Convert]::ToString($Value, [Globalization.CultureInfo]::InvariantCulture)
    [UInt64]$parsed = 0
    if ($text -cnotmatch '^[0-9]+$' -or
        -not [UInt64]::TryParse($text, [Globalization.NumberStyles]::None, [Globalization.CultureInfo]::InvariantCulture, [ref]$parsed)) {
        throw "$Label must be an unsigned bounded integer."
    }
}

function Assert-ScribeEvidenceMetadataString([object]$Value, [int]$MaximumLength, [string]$Label) {
    $text = [string]$Value
    if ([string]::IsNullOrWhiteSpace($text) -or
        $text.Length -gt $MaximumLength -or
        $text.IndexOfAny([char[]]@('\', '/', "`r", "`n", [char]0)) -ge 0 -or
        $text.ToCharArray().Where({ [int]$_ -lt 0x20 -or [int]$_ -gt 0x7e }).Count -ne 0) {
        throw "$Label is not bounded printable metadata."
    }
}

function Assert-ScribeEvidenceRunSet([psobject]$Value, [int]$ExpectedCount, [bool]$Cold, [string]$Label) {
    Assert-ScribeEvidenceExactProperties $Value @(
        'end_to_end', 'end_to_end_ms', 'backend_processing',
        'backend_processing_ms', 'model_load', 'model_load_ms'
    ) $Label
    foreach ($statisticsName in @('end_to_end', 'backend_processing')) {
        Assert-ScribeEvidenceExactProperties $Value.$statisticsName @('p50_ms', 'p95_ms') "$Label $statisticsName"
        Assert-ScribeEvidenceUnsignedInteger $Value.$statisticsName.p50_ms "$Label $statisticsName p50_ms"
        Assert-ScribeEvidenceUnsignedInteger $Value.$statisticsName.p95_ms "$Label $statisticsName p95_ms"
    }
    foreach ($samplesName in @('end_to_end_ms', 'backend_processing_ms')) {
        if (@($Value.$samplesName).Count -ne $ExpectedCount) {
            throw "$Label $samplesName has an unexpected sample count."
        }
        foreach ($sample in @($Value.$samplesName)) {
            Assert-ScribeEvidenceUnsignedInteger $sample "$Label $samplesName sample"
        }
    }
    if ($Cold) {
        Assert-ScribeEvidenceExactProperties $Value.model_load @('p50_ms', 'p95_ms') "$Label model_load"
        if (@($Value.model_load_ms).Count -ne $ExpectedCount) {
            throw "$Label model_load_ms has an unexpected sample count."
        }
        Assert-ScribeEvidenceUnsignedInteger $Value.model_load.p50_ms "$Label model_load p50_ms"
        Assert-ScribeEvidenceUnsignedInteger $Value.model_load.p95_ms "$Label model_load p95_ms"
        foreach ($sample in @($Value.model_load_ms)) {
            Assert-ScribeEvidenceUnsignedInteger $sample "$Label model_load_ms sample"
        }
    }
    elseif ($null -ne $Value.model_load -or $null -ne $Value.model_load_ms) {
        throw "$Label contains unexpected warm-run model-load evidence."
    }
}

function Assert-ScribeEvidenceReportBytes([byte[]]$Bytes) {
    if ($null -eq $Bytes -or $Bytes.Length -eq 0 -or $Bytes.Length -gt 1MB) {
        throw 'Pending evidence report bytes violate the bounded file-size contract.'
    }
    try {
        $utf8Json = [Text.UTF8Encoding]::new($false, $true).GetString($Bytes)
    }
    catch {
        throw 'Pending evidence report is not strict UTF-8.'
    }
    Assert-ScribeEvidenceReportJson $utf8Json
}

function Assert-ScribeEvidenceReportJson([string]$Utf8Json) {
    try {
        [ScribeEvidenceNative.StrictEvidenceJson]::Validate($Utf8Json)
    }
    catch {
        throw 'Evidence report violates the strict JSON schema.'
    }
    try {
        $report = $Utf8Json | ConvertFrom-Json
    }
    catch {
        throw 'Evidence report is not strict JSON.'
    }
    Assert-ScribeEvidenceExactProperties $report @(
        'schema_version', 'fixture_only', 'untrusted', 'auto_eligible',
        'source_revision', 'pack', 'model_sha256', 'wav_sha256', 'gpu',
        'nvidia_baseline', 'cold_runs_per_backend', 'warm_runs_per_backend',
        'cpu', 'vulkan', 'expected_phrase_present_every_run',
        'normalized_transcript_parity', 'same_device_internally_verified'
    ) 'Pending evidence report'
    if ($report.schema_version -ne 1 -or
        $report.fixture_only -ne $true -or
        $report.untrusted -ne $true -or
        $report.auto_eligible -ne $false -or
        [string]$report.source_revision -cnotmatch '^[0-9a-f]{40}$' -or
        [string]$report.model_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$report.wav_sha256 -cnotmatch '^[0-9a-f]{64}$' -or
        $report.cold_runs_per_backend -ne 5 -or
        $report.warm_runs_per_backend -ne 20 -or
        $report.expected_phrase_present_every_run -ne $true -or
        $report.normalized_transcript_parity -ne $true -or
        $report.same_device_internally_verified -ne $true) {
        throw 'Pending evidence report violates the fixture-only metadata contract.'
    }
    Assert-ScribeEvidenceExactProperties $report.pack @('id', 'version', 'digest', 'security_epoch', 'runtime_abi') 'Pending evidence pack'
    Assert-ScribeEvidenceExactProperties $report.gpu @('backend', 'provider', 'vendor', 'device_class', 'driver', 'memory_total_bytes') 'Pending evidence GPU'
    Assert-ScribeEvidenceExactProperties $report.nvidia_baseline @('product', 'driver', 'memory_total_bytes', 'memory_used_bytes', 'gpu_utilization_percent') 'Pending evidence NVIDIA baseline'
    Assert-ScribeEvidenceMetadataString $report.pack.id 128 'Pending evidence pack id'
    Assert-ScribeEvidenceMetadataString $report.pack.version 128 'Pending evidence pack version'
    if ([string]$report.pack.digest -cnotmatch '^[0-9a-f]{64}$') { throw 'Pending evidence pack digest is not canonical.' }
    Assert-ScribeEvidenceUnsignedInteger $report.pack.security_epoch 'Pending evidence pack security epoch'
    Assert-ScribeEvidenceUnsignedInteger $report.pack.runtime_abi 'Pending evidence pack runtime ABI'
    if ([string]$report.gpu.backend -cne 'vulkan' -or
        [string]$report.gpu.provider -cne 'transcribe-cpp-ggml-vulkan' -or
        [string]$report.gpu.vendor -cne 'nvidia' -or
        [string]$report.gpu.device_class -cne 'discrete_gpu') {
        throw 'Pending evidence GPU identity is outside the exact fixture contract.'
    }
    Assert-ScribeEvidenceMetadataString $report.gpu.driver 128 'Pending evidence GPU driver'
    Assert-ScribeEvidenceUnsignedInteger $report.gpu.memory_total_bytes 'Pending evidence GPU memory'
    Assert-ScribeEvidenceMetadataString $report.nvidia_baseline.product 256 'Pending evidence NVIDIA product'
    Assert-ScribeEvidenceMetadataString $report.nvidia_baseline.driver 128 'Pending evidence NVIDIA driver'
    Assert-ScribeEvidenceUnsignedInteger $report.nvidia_baseline.memory_total_bytes 'Pending evidence NVIDIA total memory'
    Assert-ScribeEvidenceUnsignedInteger $report.nvidia_baseline.memory_used_bytes 'Pending evidence NVIDIA used memory'
    Assert-ScribeEvidenceUnsignedInteger $report.nvidia_baseline.gpu_utilization_percent 'Pending evidence NVIDIA utilization'
    [UInt64]$gpuMemory = $report.gpu.memory_total_bytes
    [UInt64]$baselineTotal = $report.nvidia_baseline.memory_total_bytes
    [UInt64]$baselineUsed = $report.nvidia_baseline.memory_used_bytes
    [UInt64]$baselineUtilization = $report.nvidia_baseline.gpu_utilization_percent
    if ($gpuMemory -eq 0 -or
        $baselineTotal -eq 0 -or
        $baselineUsed -gt $baselineTotal -or
        $baselineUtilization -gt 10 -or
        $baselineUsed -gt ($baselineTotal / 4)) {
        throw 'Pending evidence NVIDIA metadata violates the bounded idle fixture contract.'
    }
    foreach ($backendName in @('cpu', 'vulkan')) {
        Assert-ScribeEvidenceExactProperties $report.$backendName @('cold', 'warm') "Pending evidence $backendName"
        Assert-ScribeEvidenceRunSet $report.$backendName.cold 5 $true "Pending evidence $backendName cold"
        Assert-ScribeEvidenceRunSet $report.$backendName.warm 20 $false "Pending evidence $backendName warm"
    }
}

function Read-ScribeVerifiedEvidenceReport([string]$EvidencePath, [string]$ExpectedSha256) {
    if ($ExpectedSha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Expected evidence SHA-256 must be exactly 64 lowercase hexadecimal characters.'
    }
    $fullPath = [IO.Path]::GetFullPath($EvidencePath)
    $evidenceRoot = [IO.Path]::GetDirectoryName($fullPath)
    $evidenceLeaf = [IO.Path]::GetFileName($fullPath)
    if ([string]::IsNullOrEmpty($evidenceRoot) -or [string]::IsNullOrEmpty($evidenceLeaf)) {
        throw 'Published evidence path must name one bounded file below an evidence root.'
    }

    $binding = [ScribeEvidenceNative.BoundPendingFile]::OpenPublished(
        $evidenceRoot,
        $fullPath,
        $evidenceLeaf,
        1MB
    )
    try {
        $verified = $binding.ReadAllAndVerify($ExpectedSha256)
        Assert-ScribeEvidenceReportJson $verified.Utf8Json
        return $verified
    }
    finally {
        if ($null -ne $binding) { $binding.Dispose() }
    }
}

function Remove-ScribeEvidencePendingReport(
    [string]$PendingPath,
    [string]$EvidenceRoot,
    [string]$PendingLeaf
) {
    $binding = [ScribeEvidenceNative.BoundPendingFile]::Open(
        $EvidenceRoot,
        $PendingPath,
        $PendingLeaf,
        1MB,
        $true,
        $true
    )
    if ($null -eq $binding) { return }
    try {
        $binding.Delete()
    }
    finally {
        $binding.Dispose()
    }
}

function Add-ScribeEvidenceSecondaryFailures([System.Exception]$Primary, [System.Exception[]]$Secondary) {
    for ($index = 0; $index -lt @($Secondary).Count; $index++) {
        $Primary.Data["ScribeEvidenceSecondaryFailure$index"] = @($Secondary)[$index].Message
    }
}

function Complete-ScribeEvidencePendingReport(
    [string]$PendingPath,
    [string]$FinalPath,
    [string]$EvidenceRoot,
    [string]$PendingLeaf,
    [string]$FinalLeaf,
    [System.Exception]$PrimaryFailure,
    [System.Exception[]]$SecondaryFailures
) {
    $failures = [System.Collections.Generic.List[System.Exception]]::new()
    foreach ($failure in @($SecondaryFailures)) {
        if ($null -ne $failure) { $failures.Add($failure) }
    }
    if ($null -ne $PrimaryFailure -or $failures.Count -gt 0) {
        try {
            Remove-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf
        }
        catch {
            $failures.Add($_.Exception)
        }
        if ($null -ne $PrimaryFailure) {
            Add-ScribeEvidenceSecondaryFailures $PrimaryFailure $failures.ToArray()
            throw $PrimaryFailure
        }
        $cleanupPrimary = $failures[0]
        Add-ScribeEvidenceSecondaryFailures $cleanupPrimary @($failures.ToArray() | Select-Object -Skip 1)
        throw $cleanupPrimary
    }

    try {
        $binding = [ScribeEvidenceNative.BoundPendingFile]::Open(
            $EvidenceRoot,
            $PendingPath,
            $PendingLeaf,
            1MB,
            $false,
            $false
        )
        try {
            $read = $binding.ReadAllAndHash()
            Assert-ScribeEvidenceReportBytes $read.Bytes
            $final = $binding.GetFinalPath($FinalLeaf)
            if (-not [string]::Equals(
                [IO.Path]::GetFullPath($FinalPath),
                $final,
                [StringComparison]::OrdinalIgnoreCase
            )) {
                throw 'Final evidence path does not match the bound evidence directory and leaf.'
            }
            $result = [pscustomobject]@{ Path = $final; Digest = $read.Sha256; Identity = $binding.Identity }
            # This non-replacing handle operation is deliberately the final
            # fallible publication step. The exact bytes hashed above remain
            # locked against write/delete sharing until the handle is closed.
            $binding.RenameNoReplace($FinalLeaf)
            return $result
        }
        finally {
            if ($null -ne $binding) { $binding.Dispose() }
        }
    }
    catch {
        $publishFailure = $_.Exception
        try {
            Remove-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf
        }
        catch {
            Add-ScribeEvidenceSecondaryFailures $publishFailure @($_.Exception)
        }
        throw $publishFailure
    }
}

function Assert-ScribeEvidenceNoReparseDescendants([string]$Path) {
    $root = Get-ScribeEvidencePhysicalDirectory $Path 'CMake bootstrap build directory'
    $pending = [System.Collections.Generic.Stack[IO.DirectoryInfo]]::new()
    $pending.Push($root)
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        foreach ($entry in $directory.EnumerateFileSystemInfos()) {
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'CMake bootstrap build directory contains a reparse point.'
            }
            if ($entry -is [IO.DirectoryInfo]) { $pending.Push($entry) }
        }
    }
}

function Set-ScribeEvidenceWorkerBuildMode([bool]$BuildingWorker) {
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    if ($BuildingWorker) {
        $env:SCRIBE_BUILDING_WORKER = '1'
    }
    else {
        $env:SCRIBE_BUILDING_WORKER = $null
    }
}

function Set-ScribeEvidenceProcessEnvironment([System.Collections.IDictionary]$Environment) {
    $previous = [System.Collections.Generic.List[psobject]]::new()
    try {
        foreach ($entry in $Environment.GetEnumerator()) {
            $name = [string]$entry.Key
            $value = [string]$entry.Value
            if ($name -cnotmatch '^[A-Za-z_][A-Za-z0-9_]*$' -or
                [string]::IsNullOrWhiteSpace($value) -or
                $value.Length -gt 32767 -or
                $value.IndexOfAny([char[]]@("`r", "`n", [char]0)) -ge 0) {
                throw 'Pinned toolchain environment export is invalid.'
            }
            $current = Get-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
            $previous.Add([pscustomobject]@{
                    Name = $name
                    Exists = $null -ne $current
                    Value = if ($null -eq $current) { $null } else { [string]$current.Value }
                })
            [Environment]::SetEnvironmentVariable($name, $value, [EnvironmentVariableTarget]::Process)
        }
        return ,$previous.ToArray()
    }
    catch {
        Restore-ScribeEvidenceProcessEnvironment $previous.ToArray()
        throw
    }
}

function Restore-ScribeEvidenceProcessEnvironment([psobject[]]$Previous) {
    foreach ($entry in @($Previous)) {
        if ($entry.Exists) {
            [Environment]::SetEnvironmentVariable([string]$entry.Name, [string]$entry.Value, [EnvironmentVariableTarget]::Process)
        }
        else {
            Remove-Item -LiteralPath "Env:$($entry.Name)" -ErrorAction SilentlyContinue
        }
    }
}

function New-ScribeEvidenceFixturePackVersion([string]$Revision, [string]$Nonce) {
    if ($Revision -cnotmatch '^[0-9a-f]{40}$' -or $Nonce -cnotmatch '^[0-9a-f]{12}$') {
        throw 'Fixture pack version inputs are not canonical.'
    }
    $version = "fixture-$($Revision.Substring(0, 12))-$Nonce"
    $cargoLeaf = "vulkan-$version-cargo"
    if ($version -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$' -or $cargoLeaf.Length -gt 48) {
        throw 'Fixture pack version exceeds the bounded builder Cargo target leaf.'
    }
    return $version
}

if (-not ('ScribeEvidenceNative.SystemDirectory' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
namespace ScribeEvidenceNative {
  public static class SystemDirectory {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern uint GetSystemDirectory(StringBuilder buffer, uint size);
  }
}
'@
}

function Get-ScribeVulkanEvidenceActualSystem32 {
    $buffer = [Text.StringBuilder]::new(32768)
    $length = [ScribeEvidenceNative.SystemDirectory]::GetSystemDirectory($buffer, [uint32]$buffer.Capacity)
    if ($length -eq 0 -or $length -ge $buffer.Capacity) { throw 'GetSystemDirectoryW did not return a bounded System32 path.' }
    return $buffer.ToString()
}

function ConvertTo-ScribeVulkanEvidencePci([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw 'NVIDIA PCI identity is missing.'
    }
    if ($Value -cne $Value.Trim()) {
        throw 'NVIDIA PCI identity must not contain surrounding whitespace.'
    }
    $normalized = $Value.ToLowerInvariant()
    if ($normalized -match '^native:([0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7])$') {
        return $Matches[1]
    }
    if ($normalized -match '^00000000:([0-9a-f]{2}:[0-9a-f]{2}\.[0-7])$') {
        return "0000:$($Matches[1])"
    }
    if ($normalized -match '^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$') {
        return $normalized
    }
    throw 'NVIDIA PCI identity is not canonical.'
}

function ConvertTo-ScribeVulkanEvidenceUInt64([string]$Value, [string]$Label) {
    if ($Value -cnotmatch '^[0-9]+$') {
        throw "$Label must be an unsigned decimal integer."
    }
    try {
        return [UInt64]::Parse($Value, [Globalization.CultureInfo]::InvariantCulture)
    }
    catch {
        throw "$Label is outside UInt64 range."
    }
}

function Assert-ScribeVulkanEvidenceTrustedNvidiaSmi([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw 'Required trusted nvidia-smi.exe is missing from System32.'
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Required trusted nvidia-smi.exe must be a regular non-reparse file.'
    }
    return $item.FullName
}

function Get-ScribeVulkanEvidenceNvidiaBaseline([string]$ExpectedStableDevice, [string]$NvidiaSmiPath) {
    $query = 'pci.bus_id,name,driver_version,memory.total,memory.used,utilization.gpu'
    $rows = @(& $NvidiaSmiPath "--query-gpu=$query" '--format=csv,noheader,nounits')
    if ($LASTEXITCODE -ne 0) {
        throw 'nvidia-smi failed during Vulkan evidence preflight.'
    }
    $expectedPci = ConvertTo-ScribeVulkanEvidencePci $ExpectedStableDevice
    $parsed = @($rows | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
        ConvertFrom-Csv -Header 'pci_bus_id', 'product', 'driver', 'memory_total_mib', 'memory_used_mib', 'gpu_utilization_percent')
    $matching = @($parsed | Where-Object {
        (ConvertTo-ScribeVulkanEvidencePci ([string]$_.pci_bus_id)) -ceq $expectedPci
    })
    if ($matching.Count -ne 1) {
        throw 'nvidia-smi did not provide exactly one row for the expected Vulkan PCI device.'
    }
    $row = $matching[0]
    $totalMib = ConvertTo-ScribeVulkanEvidenceUInt64 ([string]$row.memory_total_mib) 'NVIDIA total memory'
    $usedMib = ConvertTo-ScribeVulkanEvidenceUInt64 ([string]$row.memory_used_mib) 'NVIDIA used memory'
    $utilization = ConvertTo-ScribeVulkanEvidenceUInt64 ([string]$row.gpu_utilization_percent) 'NVIDIA GPU utilization'
    if ([string]::IsNullOrWhiteSpace([string]$row.product) -or
        ([string]$row.product).Length -gt 256 -or
        [string]::IsNullOrWhiteSpace([string]$row.driver) -or
        ([string]$row.driver).Length -gt 128 -or
        $totalMib -eq 0 -or
        $usedMib -gt $totalMib -or
        $utilization -gt 10 -or
        $usedMib -gt ($totalMib / 4)) {
        throw 'NVIDIA Vulkan evidence preflight requires <=10% GPU utilization and <=25% used VRAM.'
    }
    [pscustomobject]@{
        product = ([string]$row.product).Trim()
        driver = ([string]$row.driver).Trim()
        memory_total_bytes = $totalMib * 1MB
        memory_used_bytes = $usedMib * 1MB
        gpu_utilization_percent = [byte]$utilization
    }
}
