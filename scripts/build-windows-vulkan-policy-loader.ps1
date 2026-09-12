#requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$SourceArchiveDirectory,
    [Parameter(Mandatory = $true)][string]$BuildDirectory,
    [string]$NativeArchiveDirectory,
    [switch]$VerifyInputsOnly
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $IsWindows -or
    [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture -ne [Runtime.InteropServices.Architecture]::X64) {
    throw 'The Vulkan policy loader build requires Windows x64.'
}
. (Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1')
. (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')

function Assert-LoaderProperties($Value, [string[]]$Expected) {
    if ((@($Value.PSObject.Properties.Name | Sort-Object) -join '|') -cne
        (@($Expected | Sort-Object) -join '|')) {
        throw 'Unexpected Vulkan policy source manifest properties.'
    }
}

function Open-LoaderInput([string]$Path, [string]$Sha256, [long]$Size = -1) {
    Assert-ScribeGpuWorkerNoReparse $Path
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or $item.Length -gt 33554432 -or
        ($Size -ge 0 -and $item.Length -ne $Size) -or
        $Sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Vulkan policy input size, type, or digest syntax is invalid.'
    }
    # Retain the exact authenticated bytes against replacement through build.
    $stream = [IO.File]::Open($item.FullName, [IO.FileMode]::Open,
        [IO.FileAccess]::Read, [IO.FileShare]::Read)
    try {
        $sha = [Security.Cryptography.SHA256]::Create()
        try { $actual = [Convert]::ToHexString($sha.ComputeHash($stream)).ToLowerInvariant() }
        finally { $sha.Dispose() }
        if ($actual -cne $Sha256) { throw 'Vulkan policy input SHA-256 mismatch.' }
        $stream.Position = 0
        return $stream
    } catch { $stream.Dispose(); throw }
}

function Expand-LoaderSource($Stream, [string]$RootName, [string]$Destination) {
    $zip = [IO.Compression.ZipArchive]::new($Stream, [IO.Compression.ZipArchiveMode]::Read, $true)
    try {
        $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $nodes = [Collections.Generic.Dictionary[string,object]]::new([StringComparer]::OrdinalIgnoreCase)
        $total = 0L
        if ($zip.Entries.Count -lt 1 -or $zip.Entries.Count -gt 4096) {
            throw 'Vulkan source archive inventory exceeds its bound.'
        }
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName
            $parts = $name.TrimEnd('/').Split('/')
            if (-not $name.StartsWith("$RootName/", [StringComparison]::Ordinal) -or $name.Contains('//') -or
                -not $names.Add($name.TrimEnd('/')) -or $name -match '[:\\<>"|?*\x00-\x1f]') {
                throw 'Unsafe or case-colliding Vulkan source path.'
            }
            foreach ($part in $parts) {
                if ($part -in @('', '.', '..') -or $part.EndsWith('.') -or $part.EndsWith(' ') -or
                    $part -match '^(?i:con|prn|aux|nul|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)') {
                    throw 'Unsafe Vulkan source path component.'
                }
            }
            # Include implicit parent directories, so file/descendant conflicts
            # and differently cased parent names fail before creating anything,
            # regardless of the archive's entry order.
            for ($index = 0; $index -lt $parts.Length; $index++) {
                $nodePath = $parts[0..$index] -join '/'
                $isDirectory = $index -lt $parts.Length - 1 -or $name.EndsWith('/')
                if ($nodes.ContainsKey($nodePath)) {
                    $prior = $nodes[$nodePath]
                    if ($prior.Name -cne $nodePath -or $prior.IsDirectory -ne $isDirectory) {
                        throw 'Conflicting Vulkan source path nodes.'
                    }
                } else {
                    $nodes.Add($nodePath, [pscustomobject]@{ Name = $nodePath; IsDirectory = $isDirectory })
                }
            }
            $unixType = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if ($unixType -notin @(0, 0x8000, 0x4000) -or
                ($unixType -eq 0x4000 -and -not $name.EndsWith('/')) -or
                ($unixType -eq 0x8000 -and $name.EndsWith('/')) -or
                ($name.EndsWith('/') -and $entry.Length -ne 0)) {
                throw 'Vulkan source link, special file, or conflicting entry type is forbidden.'
            }
            $total += $entry.Length
            if ($entry.Length -lt 0 -or $entry.Length -gt 33554432 -or $total -gt 134217728) {
                throw 'Vulkan source expansion exceeds its byte bound.'
            }
        }
        # Validate the complete archive before creating any source entry. The
        # destination is a fresh, physical build directory, never a reused tree.
        foreach ($entry in $zip.Entries) {
            $path = [IO.Path]::GetFullPath((Join-Path $Destination $entry.FullName))
            if (-not $path.StartsWith("$Destination\", [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Vulkan source entry escaped its build directory.'
            }
            if ($entry.FullName.EndsWith('/')) {
                [IO.Directory]::CreateDirectory($path) | Out-Null
                continue
            }
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
            $output = [IO.File]::Open($path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $input = $entry.Open()
                try {
                    $buffer = [byte[]]::new(65536)
                    $remaining = $entry.Length
                    while ($remaining -gt 0) {
                        $read = $input.Read($buffer, 0, [int][Math]::Min($remaining, $buffer.Length))
                        if ($read -eq 0) { throw 'Truncated Vulkan source entry.' }
                        $output.Write($buffer, 0, $read)
                        $remaining -= $read
                    }
                    if ($input.ReadByte() -ne -1) { throw 'Vulkan source entry exceeded its declared size.' }
                } finally { $input.Dispose() }
                if ($output.Length -ne $entry.Length) { throw 'Vulkan source extraction size mismatch.' }
            } finally { $output.Dispose() }
        }
    } finally { $zip.Dispose() }
}

function Invoke-LoaderNative([string]$Executable, [string[]]$Arguments) {
    $result = Invoke-ScribeGpuWorkerBoundedNativeProcess $Executable $Arguments 'Vulkan policy native build failed.'
    Write-Verbose $result.Stdout
    if ($result.Stderr) { Write-Verbose $result.Stderr }
}

function Assert-NoLoaderBuildOverrides([Collections.IDictionary]$Environment) {
    # The shared preflight rejects compiler/SDK overrides. Also reject CMake's
    # environment initializers (including compiler/linker launchers), before a
    # fresh configure can execute them. Rejection never mutates the caller.
    foreach ($entry in $Environment.GetEnumerator()) {
        if (([string]$entry.Key -match '^(?i:CMAKE_|CTEST_)' -or
            [string]$entry.Key -in @('ASMFLAGS', 'ASM_MASMFLAGS', 'RCFLAGS', 'LDFLAGS',
                '_LINK_', 'MAKEFLAGS', 'MFLAGS', 'NMAKEFLAGS')) -and
            -not [string]::IsNullOrEmpty([string]$entry.Value)) {
            throw "Vulkan policy builds reject ambient native override $($entry.Key)."
        }
    }
}

$repo = Split-Path -Parent $PSScriptRoot
$policyRoot = Join-Path $repo 'native\vulkan-policy-loader'
$manifestPath = Join-Path $policyRoot 'source-manifest.json'
Assert-ScribeGpuWorkerNoReparse $manifestPath
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
Assert-LoaderProperties $manifest @('schema_version', 'policy_id', 'target_triple', 'loader', 'headers', 'patch', 'artifact', 'build')
if ($manifest.schema_version -ne 1 -or $manifest.policy_id -cne 'scribe-windows-vulkan-no-layers-v1' -or
    $manifest.target_triple -cne 'x86_64-pc-windows-msvc') {
    throw 'Unsupported Vulkan policy source manifest.'
}
Assert-LoaderProperties $manifest.patch @('file', 'sha256', 'patched_files')
if ($manifest.patch.file -cne 'no-layers-or-settings.patch' -or @($manifest.patch.patched_files).Count -ne 3) {
    throw 'Unexpected Vulkan policy patch inventory.'
}
$patchedPaths = @($manifest.patch.patched_files.path | Sort-Object)
if (($patchedPaths -join '|') -cne 'CMakeLists.txt|loader/loader.c|loader/settings.c') {
    throw 'Unexpected patched Vulkan source paths.'
}
foreach ($patched in $manifest.patch.patched_files) {
    Assert-LoaderProperties $patched @('path', 'sha256')
}
Assert-LoaderProperties $manifest.artifact @('filename', 'size_bytes', 'sha256')
if ($manifest.artifact.filename -cne 'vulkan-1.dll' -or $manifest.artifact.size_bytes -le 0 -or
    $manifest.artifact.size_bytes -gt 33554432 -or $manifest.artifact.sha256 -cnotmatch '^[0-9a-f]{64}$') {
    throw 'Invalid pinned Vulkan policy artifact.'
}
Assert-LoaderProperties $manifest.build @('generator', 'configuration', 'target', 'runtime', 'warnings_as_errors', 'update_dependencies', 'code_generation', 'upstream_tests', 'normal_imports', 'delay_imports')
if ($manifest.build.generator -cne 'NMake Makefiles' -or $manifest.build.configuration -cne 'Release' -or
    $manifest.build.target -cne 'vulkan' -or $manifest.build.runtime -cne 'MultiThreaded' -or
    $manifest.build.warnings_as_errors -ne $true -or $manifest.build.update_dependencies -ne $false -or
    $manifest.build.code_generation -ne $false -or $manifest.build.upstream_tests -ne $false -or
    (@($manifest.build.normal_imports) -join '|') -cne 'advapi32.dll|cfgmgr32.dll|kernel32.dll' -or
    @($manifest.build.delay_imports).Count -ne 0) {
    throw 'Unsupported Vulkan policy native build options.'
}

$archives = (Get-ScribeGpuWorkerPhysicalDirectory $SourceArchiveDirectory 'Vulkan source archives').FullName
$buildRoot = [IO.Path]::GetFullPath($BuildDirectory).TrimEnd('\', '/')
Assert-ScribeGpuWorkerNoReparse $buildRoot
if (Test-Path -LiteralPath $buildRoot) { throw 'Vulkan policy build directory must be fresh.' }
if ($buildRoot.Length -gt 96 -or [IO.Path]::GetFileName($buildRoot) -cnotmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$') {
    throw 'Vulkan policy build directory must have a bounded short path.'
}
$streams = [Collections.Generic.List[IO.Stream]]::new()
$savedEnvironment = @{}
try {
    foreach ($kind in @('loader', 'headers')) {
        $spec = $manifest.$kind
        $properties = @('repository', 'version', 'commit', 'archive', 'url', 'size_bytes', 'sha256', 'root')
        if ($kind -eq 'loader') { $properties += @('license', 'license_sha256') }
        Assert-LoaderProperties $spec $properties
        $project = if ($kind -eq 'loader') { 'Vulkan-Loader' } else { 'Vulkan-Headers' }
        $expectedRoot = "$project-$($spec.commit)"
        if ($spec.commit -cnotmatch '^[0-9a-f]{40}$' -or $spec.version -cne '1.4.357' -or
            $spec.repository -cne "https://github.com/KhronosGroup/$project" -or
            $spec.root -cne $expectedRoot -or $spec.archive -cne "$expectedRoot.zip" -or
            $spec.url -cne "https://codeload.github.com/KhronosGroup/$project/zip/$($spec.commit)") {
            throw 'Vulkan source provenance or archive path mismatch.'
        }
        $streams.Add((Open-LoaderInput (Join-Path $archives $spec.archive) $spec.sha256 $spec.size_bytes))
    }
    $patchPath = Join-Path $policyRoot $manifest.patch.file
    $streams.Add((Open-LoaderInput $patchPath $manifest.patch.sha256))
    if ($VerifyInputsOnly) {
        Write-Output 'Vulkan policy source archives and patch verified; no build or download performed.'
        return
    }

    # Reuse the existing pinned Rust/MSVC/SDK/CMake preflight, including its
    # payload profiles. Its environment export has no persistent side effects.
    Assert-NoLoaderBuildOverrides ([Environment]::GetEnvironmentVariables('Process'))
    $exportArgs = @{
        Backend = 'Vulkan'; PackVersion = '0.1.0-policy-loader-build';
        OutputDirectory = (Join-Path $buildRoot 'unused-pack'); SigningMode = 'Fixture';
        ToolchainCheckOnly = $true; ExportPinnedMsvcEnvironment = $true
    }
    if ($NativeArchiveDirectory) { $exportArgs.NativeArchiveDirectory = $NativeArchiveDirectory }
    $pinned = (& (Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1') @exportArgs) | ConvertFrom-Json
    foreach ($property in $pinned.environment.PSObject.Properties) {
        $savedEnvironment[$property.Name] = [Environment]::GetEnvironmentVariable($property.Name, 'Process')
        [Environment]::SetEnvironmentVariable($property.Name, [string]$property.Value, 'Process')
    }
    foreach ($name in @('_CL_', 'SOURCE_DATE_EPOCH')) {
        if (-not $savedEnvironment.ContainsKey($name)) {
            $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        }
    }
    $env:_CL_ = '/Brepro'
    $env:SOURCE_DATE_EPOCH = '0'
    $cmake = (Get-Command cmake.exe -CommandType Application | Select-Object -First 1).Source
    $git = (Get-Command git.exe -CommandType Application | Select-Object -First 1).Source
    [IO.Directory]::CreateDirectory($buildRoot) | Out-Null
    $null = Get-ScribeGpuWorkerPhysicalDirectory $buildRoot 'Fresh Vulkan policy build root'
    Expand-LoaderSource $streams[0] $manifest.loader.root $buildRoot
    Expand-LoaderSource $streams[1] $manifest.headers.root $buildRoot
    $loader = Join-Path $buildRoot $manifest.loader.root
    $headers = Join-Path $buildRoot $manifest.headers.root
    Invoke-LoaderNative $git @('-C', $loader, 'apply', '--check', $patchPath)
    Invoke-LoaderNative $git @('-C', $loader, 'apply', $patchPath)
    foreach ($patched in $manifest.patch.patched_files) {
        $verified = Open-LoaderInput (Join-Path $loader $patched.path) $patched.sha256
        $verified.Dispose()
    }
    if ($manifest.loader.license -cne 'LICENSE.txt') { throw 'Unexpected loader license path.' }
    $license = Open-LoaderInput (Join-Path $loader 'LICENSE.txt') $manifest.loader.license_sha256
    $license.Dispose()
    $headerBuild = Join-Path $buildRoot 'headers-build'
    $headerInstall = Join-Path $buildRoot 'headers-install'
    $loaderBuild = Join-Path $buildRoot 'loader-build'
    Invoke-LoaderNative $cmake @('-S', $headers, '-B', $headerBuild, '-G', 'NMake Makefiles',
        '-DCMAKE_BUILD_TYPE=Release', '-DVULKAN_HEADERS_ENABLE_TESTS=OFF', '-DVULKAN_HEADERS_ENABLE_MODULE=OFF',
        '-DVULKAN_HEADERS_ENABLE_INSTALL=ON', "-DCMAKE_INSTALL_PREFIX=$headerInstall")
    Invoke-LoaderNative $cmake @('--install', $headerBuild, '--config', 'Release')
    Invoke-LoaderNative $cmake @('-S', $loader, '-B', $loaderBuild, '-G', 'NMake Makefiles',
        '-DCMAKE_BUILD_TYPE=Release', "-DVULKAN_HEADERS_INSTALL_DIR=$headerInstall", '-DUPDATE_DEPS=OFF',
        '-DLOADER_CODEGEN=OFF', '-DBUILD_TESTS=OFF', '-DBUILD_WERROR=ON',
        '-DCMAKE_SHARED_LINKER_FLAGS=/Brepro /INCREMENTAL:NO')
    Invoke-LoaderNative $cmake @('--build', $loaderBuild, '--config', 'Release', '--target', 'vulkan')
    $dll = Join-Path $loaderBuild 'loader\vulkan-1.dll'
    Assert-ScribeGpuWorkerNoReparse $dll
    $artifactLease = Open-LoaderInput $dll $manifest.artifact.sha256 $manifest.artifact.size_bytes
    $streams.Add($artifactLease)
    $report = Get-WindowsPeImportReport $dll
    if ($report.Machine -ne 0x8664 -or
        (@($report.NormalImports | Sort-Object) -join '|') -cne (@($manifest.build.normal_imports | Sort-Object) -join '|') -or
        @($report.DelayImports).Count -ne 0) {
        throw 'Vulkan policy loader has an unexpected PE architecture or dependency closure.'
    }
    [pscustomobject]@{
        PolicyId = $manifest.policy_id
        DllPath = $dll
        DllSha256 = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
        InstalledBytes = (Get-Item -LiteralPath $dll).Length
        SourceManifestSha256 = (Get-FileHash -LiteralPath $manifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
        LicensePath = (Join-Path $loader 'LICENSE.txt')
        LicenseDirectory = (Join-Path $loader 'LICENSES')
    } | ConvertTo-Json -Compress
} finally {
    foreach ($stream in $streams) { $stream.Dispose() }
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, 'Process')
    }
}
