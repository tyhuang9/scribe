# Build-time quality gate only. This does not change worker-pack trust or patch
# an executable. Historical frozen workers remain subject to their own contract.
function Get-WindowsApplicationManifestBytes([string]$Path) {
    if (-not [IO.Path]::IsPathFullyQualified($Path)) {
        throw 'Application manifest inspection requires an absolute executable path.'
    }
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'Application manifest input must be a regular executable file.'
    }
    # Deny writes and replacement throughout resource extraction.
    $lease = [IO.File]::Open($item.FullName, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        if (-not ('Scribe.Build.ApplicationManifestReader' -as [type])) {
            Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
namespace Scribe.Build {
    public static class ApplicationManifestReader {
        private delegate bool NameCallback(IntPtr module, IntPtr type, IntPtr name, IntPtr param);
        private delegate bool LanguageCallback(IntPtr module, IntPtr type, IntPtr name, ushort language, IntPtr param);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
        private static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
        private static extern bool EnumResourceNamesExW(IntPtr module, IntPtr type, NameCallback callback, IntPtr param, uint flags, ushort language);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
        private static extern bool EnumResourceLanguagesExW(IntPtr module, IntPtr type, IntPtr name, LanguageCallback callback, IntPtr param, uint flags, ushort language);
        [DllImport("kernel32.dll", CharSet=CharSet.Unicode, ExactSpelling=true, SetLastError=true)]
        private static extern IntPtr FindResourceExW(IntPtr module, IntPtr type, IntPtr name, ushort language);
        [DllImport("kernel32.dll", ExactSpelling=true, SetLastError=true)]
        private static extern uint SizeofResource(IntPtr module, IntPtr resource);
        [DllImport("kernel32.dll", ExactSpelling=true, SetLastError=true)]
        private static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
        [DllImport("kernel32.dll", ExactSpelling=true)]
        private static extern IntPtr LockResource(IntPtr resource);
        [DllImport("kernel32.dll", ExactSpelling=true, SetLastError=true)]
        private static extern bool FreeLibrary(IntPtr module);

        public static byte[] Read(string path) {
            // Resource-only mapping: no static imports, entry point or DllMain.
            IntPtr module = LoadLibraryExW(path, IntPtr.Zero, 0x60);
            if (module == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot map application resources.");
            byte[] result = null;
            Exception failure = null;
            try {
                if ((module.ToInt64() & 3) == 0) throw new InvalidOperationException("Mapping is not resource-only.");
                IntPtr type = new IntPtr(24), id = new IntPtr(1);
                int names = 0;
                bool expectedName = false;
                NameCallback nameCallback = delegate(IntPtr m, IntPtr t, IntPtr name, IntPtr p) {
                    names++;
                    expectedName = name == id;
                    return names < 2;
                };
                // LN + VALIDATE: never consult a neighboring .mui file.
                bool namesOk = EnumResourceNamesExW(module, type, nameCallback, IntPtr.Zero, 9, 0);
                GC.KeepAlive(nameCallback);
                if (!namesOk || names != 1 || !expectedName) throw new InvalidOperationException("Expected exactly one embedded RT_MANIFEST, integer ID 1.");
                int languages = 0;
                ushort selectedLanguage = 0;
                LanguageCallback languageCallback = delegate(IntPtr m, IntPtr t, IntPtr n, ushort language, IntPtr p) {
                    languages++;
                    selectedLanguage = language;
                    return languages < 2;
                };
                bool languagesOk = EnumResourceLanguagesExW(module, type, id, languageCallback, IntPtr.Zero, 9, 0);
                GC.KeepAlive(languageCallback);
                if (!languagesOk || languages != 1) throw new InvalidOperationException("Expected exactly one embedded application-manifest language.");
                IntPtr resource = FindResourceExW(module, type, id, selectedLanguage);
                if (resource == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot find embedded application manifest.");
                uint size = SizeofResource(module, resource);
                if (size == 0 || size > 65536) throw new InvalidOperationException("Embedded application manifest exceeds 1..65536 bytes.");
                IntPtr loaded = LoadResource(module, resource);
                if (loaded == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Cannot load embedded application manifest.");
                IntPtr bytes = LockResource(loaded);
                if (bytes == IntPtr.Zero) throw new InvalidOperationException("Cannot lock embedded application manifest.");
                result = new byte[(int)size];
                Marshal.Copy(bytes, result, 0, result.Length);
            } catch (Exception error) { failure = error; }
            if (!FreeLibrary(module)) {
                Exception cleanup = new Win32Exception(Marshal.GetLastWin32Error(), "Cannot release application resource mapping.");
                if (failure != null) throw new AggregateException(failure, cleanup);
                throw cleanup;
            }
            if (failure != null) throw failure;
            return result;
        }
    }
}
'@
        }
        return ,([Scribe.Build.ApplicationManifestReader]::Read($item.FullName))
    }
    finally { $lease.Dispose() }
}

function Assert-WindowsLongPathAwareManifestXml([byte[]]$Bytes) {
    if ($null -eq $Bytes -or $Bytes.Length -eq 0 -or $Bytes.Length -gt 65536) {
        throw 'Application manifest must contain 1..65536 bytes.'
    }
    $text = [Text.UTF8Encoding]::new($false, $true).GetString($Bytes)
    # The linker may serialize a single UTF-8 BOM. No other encoding is accepted.
    if ($text.Length -gt 0 -and $text[0] -ceq [char]0xFEFF) { $text = $text.Substring(1) }
    $settings = [Xml.XmlReaderSettings]::new()
    $settings.DtdProcessing = [Xml.DtdProcessing]::Prohibit
    $settings.XmlResolver = $null
    $settings.MaxCharactersInDocument = 65536
    $manifestInput = [IO.StringReader]::new($text)
    $reader = [Xml.XmlReader]::Create($manifestInput, $settings)
    try {
        $document = [Xml.XmlDocument]::new()
        $document.XmlResolver = $null
        $document.Load($reader)
    }
    finally { $reader.Dispose(); $manifestInput.Dispose() }
    $ns = [Xml.XmlNamespaceManager]::new($document.NameTable)
    $ns.AddNamespace('a', 'urn:schemas-microsoft-com:asm.v1')
    $ns.AddNamespace('v', 'urn:schemas-microsoft-com:asm.v3')
    $ns.AddNamespace('w', 'http://schemas.microsoft.com/SMI/2016/WindowsSettings')
    $level = $document.SelectNodes('/a:assembly/v:trustInfo/v:security/v:requestedPrivileges/v:requestedExecutionLevel', $ns)
    $longPath = $document.SelectNodes('/a:assembly/v:application/v:windowsSettings/w:longPathAware', $ns)
    if ($document.SelectNodes('//*').Count -ne 8 -or
        $document.SelectNodes('//*[local-name()="requestedExecutionLevel"]').Count -ne 1 -or
        $document.SelectNodes('//*[local-name()="longPathAware"]').Count -ne 1 -or
        $level.Count -ne 1 -or $longPath.Count -ne 1 -or
        $level[0].GetAttribute('level') -cne 'asInvoker' -or
        $level[0].GetAttribute('uiAccess') -cne 'false' -or
        $longPath[0].InnerText.Trim() -cne 'true') {
        throw 'Application manifest must declare only reviewed asInvoker/uiAccess=false and longPathAware=true settings.'
    }
    foreach ($element in $document.SelectNodes('//*')) {
        $attributes = @($element.Attributes | Where-Object NamespaceURI -CNE 'http://www.w3.org/2000/xmlns/')
        if ($element -eq $document.DocumentElement) {
            if ($attributes.Count -ne 1 -or $attributes[0].Name -cne 'manifestVersion' -or $attributes[0].Value -cne '1.0') {
                throw 'Application manifest assembly attributes differ from the reviewed contract.'
            }
        }
        elseif ($element -eq $level[0]) {
            if ($attributes.Count -ne 2 -or @($attributes | Where-Object { $_.NamespaceURI -cne '' -or $_.Name -cnotin @('level', 'uiAccess') }).Count -ne 0) {
                throw 'Application manifest execution-level attributes differ from the reviewed contract.'
            }
        }
        elseif ($attributes.Count -ne 0) { throw 'Application manifest contains unreviewed attributes.' }
    }
    if ($document.SelectNodes('//processing-instruction()').Count -ne 0) {
        throw 'Application manifest contains an unreviewed processing instruction.'
    }
    if ($document.FirstChild -is [Xml.XmlDeclaration] -and $document.FirstChild.Encoding -ine 'UTF-8') {
        throw 'Application manifest declaration must use UTF-8.'
    }
}

function Assert-WindowsLongPathAwareApplicationManifest([string]$Path) {
    Assert-WindowsLongPathAwareManifestXml (Get-WindowsApplicationManifestBytes $Path)
}
