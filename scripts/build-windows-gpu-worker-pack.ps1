[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('Cuda', 'Vulkan')]
    [string]$Backend,
    [Parameter(Mandatory = $true)]
    [string]$PackVersion,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [ValidateSet('Production', 'Fixture')]
    [string]$SigningMode = 'Production',
    [string]$ProductionPrivateKeyPath,
    [string]$ProductionKeyId,
    [string]$ToolchainManifestPath,
    [string]$NativeArchiveDirectory,
    [string]$CargoTargetDirectory,
    [switch]$ToolchainCheckOnly,
    [switch]$ExportPinnedMsvcEnvironment
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-cuda-sdk-inventory.ps1')

if ($ExportPinnedMsvcEnvironment -and -not $ToolchainCheckOnly) {
    throw 'Pinned MSVC environment export is only available with ToolchainCheckOnly.'
}

if (-not $ToolchainCheckOnly -and
    $SigningMode -eq 'Production' -and
    ([string]::IsNullOrWhiteSpace($ProductionPrivateKeyPath) -or
    [string]::IsNullOrWhiteSpace($ProductionKeyId))) {
    throw 'Production pack builds require an externally supplied PKCS#8 private key path and reviewed key ID.'
}

function Get-NormalizedFullPath([string]$Path) {
    $full = [System.IO.Path]::GetFullPath($Path)
    $root = [System.IO.Path]::GetPathRoot($full)
    if ([string]::Equals($full, $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $root
    }
    return $full.TrimEnd([char[]]@('\', '/'))
}

function Assert-ExactProperties([psobject]$Value, [string[]]$Names, [string]$Label) {
    if ($null -eq $Value) {
        throw "$Label is missing."
    }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object)
    $expected = @($Names | Sort-Object)
    if ($actual.Count -ne $expected.Count -or
        (Compare-Object -ReferenceObject $expected -DifferenceObject $actual -CaseSensitive)) {
        throw "$Label has unknown or missing fields."
    }
}

function Assert-RegularNonReparseFile([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a regular non-reparse file: $Path"
    }
    return $item
}

function Assert-NoReparseAncestors([string]$Path) {
    $current = Get-NormalizedFullPath $Path
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing ancestor for path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "GPU worker-pack build path cannot cross a link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Resolve-ShortCargoTargetDirectory(
    [string]$RequestedPath,
    [string]$BackendName,
    [string]$PackVersion
) {
    $localAppData = [System.Environment]::GetFolderPath(
        [System.Environment+SpecialFolder]::LocalApplicationData
    )
    if ([string]::IsNullOrWhiteSpace($localAppData)) {
        throw 'Windows LocalApplicationData is unavailable for the bounded native build cache.'
    }
    $cacheRoot = Get-NormalizedFullPath (Join-Path $localAppData 'sgp')
    Assert-NoReparseAncestors $cacheRoot
    $candidate = if ([string]::IsNullOrWhiteSpace($RequestedPath)) {
        Join-Path $cacheRoot "$BackendName-$PackVersion-cargo"
    } else {
        Get-NormalizedFullPath $RequestedPath
    }
    $target = Get-NormalizedFullPath $candidate
    $expectedPrefix = "$cacheRoot\"
    if (-not $target.StartsWith($expectedPrefix, [System.StringComparison]::OrdinalIgnoreCase) -or
        -not [string]::Equals(
            (Split-Path -Parent $target),
            $cacheRoot,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        throw "GPU worker Cargo target must be one direct child of the short LocalApplicationData build root: $cacheRoot"
    }
    if ((Split-Path -Leaf $target) -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,46}[a-z0-9])?$' -or
        $target.Length -gt 96) {
        throw 'GPU worker Cargo target leaf must be a bounded canonical build identifier.'
    }
    Assert-NoReparseAncestors $target
    if (Test-Path -LiteralPath $target) {
        throw "GPU worker Cargo target must be fresh to prevent feature/output reuse: $target"
    }
    $buildEnvironment = Get-NormalizedFullPath (
        Join-Path $cacheRoot "$BackendName-$PackVersion-env"
    )
    if ($buildEnvironment.Length -gt 96) {
        throw 'GPU worker native build environment path exceeds the bounded short-path contract.'
    }
    Assert-NoReparseAncestors $buildEnvironment
    if (Test-Path -LiteralPath $buildEnvironment) {
        throw "GPU worker native build environment must be fresh: $buildEnvironment"
    }
    return [pscustomobject]@{
        BuildEnvironment = $buildEnvironment
        Target = $target
    }
}

function Invoke-NativeProcess(
    [string]$Executable,
    [string[]]$Arguments,
    [string]$FailureMessage
) {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Executable
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($argument in $Arguments) {
        $startInfo.ArgumentList.Add($argument)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw $FailureMessage
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $process.WaitForExit()
        $output = $stdout.GetAwaiter().GetResult()
        $errorOutput = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            $detail = $errorOutput.Trim()
            if (-not $detail) {
                $detail = $output.Trim()
            }
            throw "$FailureMessage Exit code $($process.ExitCode). $detail"
        }
        return [pscustomobject]@{
            Stdout = $output
            Stderr = $errorOutput
        }
    }
    finally {
        $process.Dispose()
    }
}

function Enable-ValidatedCmakeBuildJunction(
    [string]$BuildEnvironment,
    [string]$CargoTarget
) {
    $tcsRoot = Join-Path $BuildEnvironment 'tcs'
    if (-not (Test-Path -LiteralPath $tcsRoot -PathType Container)) {
        throw 'The isolated transcribe-cpp native-build junction root was not created.'
    }
    $entries = @(Get-ChildItem -LiteralPath $tcsRoot -Force)
    if ($entries.Count -ne 1) {
        throw 'The isolated transcribe-cpp native-build root has an unexpected inventory.'
    }
    $shortOut = $entries[0]
    if (-not $shortOut.PSIsContainer -or
        ($shortOut.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0 -or
        $shortOut.LinkType -cne 'Junction' -or
        @($shortOut.Target).Count -ne 1) {
        throw 'The transcribe-cpp short OUT_DIR is not one exact NTFS junction.'
    }
    $outDirectory = Get-NormalizedFullPath ([string]@($shortOut.Target)[0])
    $relativeOut = [System.IO.Path]::GetRelativePath($CargoTarget, $outDirectory).Replace('\', '/')
    if ($relativeOut -cnotmatch '^release/build/transcribe-cpp-sys-[0-9a-f]{16}/out$') {
        throw 'The transcribe-cpp short OUT_DIR junction escaped the exact fresh Cargo target.'
    }
    $outItem = Get-Item -LiteralPath $outDirectory -Force
    if (-not $outItem.PSIsContainer -or
        ($outItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'The transcribe-cpp OUT_DIR target is not a physical directory.'
    }
    $buildDirectory = Join-Path $outDirectory 'build'
    if (Test-Path -LiteralPath $buildDirectory) {
        $buildItem = Get-Item -LiteralPath $buildDirectory -Force
        if (-not $buildItem.PSIsContainer -or
            ($buildItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
            -not [string]::Equals(
                (Split-Path -Parent $buildItem.FullName),
                $outDirectory,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw 'Refusing to replace an unexpected transcribe-cpp native build path.'
        }
        Remove-Item -LiteralPath $buildDirectory -Recurse -Force
    }
    $nativeBuild = Join-Path $BuildEnvironment 'native'
    if (Test-Path -LiteralPath $nativeBuild) {
        throw 'The isolated short native build directory was unexpectedly preexisting.'
    }
    New-Item -ItemType Directory -Path $nativeBuild | Out-Null
    $nativeItem = Get-Item -LiteralPath $nativeBuild -Force
    if (-not $nativeItem.PSIsContainer -or
        ($nativeItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'The isolated short native build directory is not physical.'
    }
    New-Item -ItemType Junction -Path $buildDirectory -Target $nativeBuild | Out-Null
    $junction = Get-Item -LiteralPath $buildDirectory -Force
    if (-not $junction.PSIsContainer -or
        ($junction.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0 -or
        $junction.LinkType -cne 'Junction' -or
        @($junction.Target).Count -ne 1 -or
        -not [string]::Equals(
            (Get-NormalizedFullPath ([string]@($junction.Target)[0])),
            $nativeBuild,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        throw 'Could not verify the isolated transcribe-cpp native build junction.'
    }
}

function Assert-ExactHash([string]$Path, [string]$Expected, [string]$Label) {
    $null = Assert-RegularNonReparseFile $Path $Label
    $actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -cne $Expected) {
        throw "$Label SHA-256 mismatch: expected $Expected, got $actual"
    }
}

function Assert-LockedPackage(
    [string]$CargoLock,
    [string]$Name,
    [string]$Version,
    [string]$Checksum
) {
    if ($CargoLock.IndexOf([char]0) -ge 0) {
        throw 'Cargo.lock contains a NUL byte.'
    }
    # actions/checkout may materialize the text lockfile with CRLF on Windows,
    # while developer worktrees commonly retain LF. Normalize text newlines
    # before applying the exact package-block contract; package values remain
    # byte-for-byte compared below.
    $normalizedCargoLock = $CargoLock.Replace("`r`n", "`n").Replace("`r", "`n")
    $escapedName = [regex]::Escape($Name)
    $escapedVersion = [regex]::Escape($Version)
    $escapedChecksum = [regex]::Escape($Checksum)
    $blockPattern = '(?ms)^\[\[package\]\]\n(?<body>.*?)(?=^\[\[package\]\]|\z)'
    $matches = @([regex]::Matches($normalizedCargoLock, $blockPattern) | Where-Object {
        $_.Groups['body'].Value -match "(?m)^name = `"$escapedName`"$" -and
        $_.Groups['body'].Value -match "(?m)^version = `"$escapedVersion`"$" -and
        $_.Groups['body'].Value -match "(?m)^checksum = `"$escapedChecksum`"$"
    })
    if ($matches.Count -ne 1) {
        throw "Cargo.lock does not contain the pinned $Name $Version checksum."
    }
}

function Get-CommandPath([string]$Name, [string]$FailureMessage) {
    $command = Get-Command $Name -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -eq $command) {
        throw $FailureMessage
    }
    return $command.Source
}

function Assert-PhysicalDirectory([string]$Path, [string]$Label) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Label is missing: $Path"
    }
    Assert-NoReparseAncestors $Path
    $item = Get-Item -LiteralPath $Path -Force
    if (-not $item.PSIsContainer -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "$Label must be a physical directory: $Path"
    }
    return $item
}

function Get-PinnedMsvcVsWhereArguments([string]$ComponentId) {
    if ($ComponentId -cnotmatch '^Microsoft\.VisualStudio\.Component\.VC\.[A-Za-z0-9.]+\.x86\.x64$') {
        throw 'MSVC component ID is not canonical.'
    }
    return @(
        '-all', '-products', '*', '-requires', $ComponentId,
        '-property', 'installationPath'
    )
}

function ConvertFrom-VsWhereInstallationPaths([string]$Output, [string]$ComponentId) {
    if ($Output.Length -gt 32768) {
        throw "Visual Studio locator output for $ComponentId is oversized."
    }
    $paths = [System.Collections.Generic.List[string]]::new()
    foreach ($line in @($Output -split "`r?`n")) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $candidate = $line.Trim()
        if ($candidate.Length -gt 240 -or
            -not [System.IO.Path]::IsPathFullyQualified($candidate) -or
            $candidate -cnotmatch '^[A-Za-z]:[\\/]' -or
            $candidate.IndexOfAny([char[]]@('"', '&', '|', '<', '>', '^', '%', '!', "`r", "`n")) -ge 0) {
            throw "Visual Studio locator returned an unsafe installation path for $ComponentId."
        }
        $paths.Add((Get-NormalizedFullPath $candidate))
        if ($paths.Count -gt 16) {
            throw "Visual Studio locator returned too many installations for $ComponentId."
        }
    }
    if ($paths.Count -eq 0) {
        return
    }
    return $paths.ToArray()
}

function Get-PinnedMsvcToolIdentity(
    [string]$Path,
    [string]$ExpectedFilename,
    [string]$Label
) {
    if ($ExpectedFilename -cnotmatch '^[a-z]+\.exe$' -or
        -not [string]::Equals(
            (Split-Path -Leaf $Path),
            $ExpectedFilename,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        throw "$Label filename contract is not canonical."
    }
    Assert-NoReparseAncestors $Path
    $item = Assert-RegularNonReparseFile $Path $Label
    $fileVersion = [string]$item.VersionInfo.FileVersion
    if ($fileVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$') {
        throw "$Label has a noncanonical file version."
    }
    return [pscustomobject]@{
        Path = Get-NormalizedFullPath $Path
        Filename = $ExpectedFilename
        FileVersion = $fileVersion
        Sha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

function Assert-MsvcPayloadProfileContract([psobject]$Profile) {
    Assert-ExactProperties $Profile @('profile_id', 'tools') 'MSVC payload profile'
    if ([string]$Profile.profile_id -cnotmatch '^[a-z0-9][a-z0-9._-]{0,63}$') {
        throw 'MSVC payload profile ID is not canonical.'
    }
    Assert-ExactProperties $Profile.tools @('cl', 'link', 'lib', 'nmake') 'MSVC payload profile tools'
    foreach ($toolName in @('cl', 'link', 'lib', 'nmake')) {
        $tool = $Profile.tools.$toolName
        Assert-ExactProperties $tool @('filename', 'file_version', 'sha256') "MSVC $toolName payload identity"
        if ([string]$tool.filename -cnotmatch '^[a-z]+\.exe$' -or
            [string]$tool.file_version -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or
            [string]$tool.sha256 -cnotmatch '^[0-9a-f]{64}$') {
            throw "MSVC $toolName payload identity is not canonical."
        }
    }
}

function Resolve-PinnedMsvcPayloadProfile(
    [string]$ToolBin,
    [object[]]$Profiles
) {
    $profilesArray = @($Profiles)
    if ($profilesArray.Count -lt 1 -or $profilesArray.Count -gt 8) {
        throw 'MSVC payload profile count is invalid.'
    }
    $profileIds = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    foreach ($profile in $profilesArray) {
        Assert-MsvcPayloadProfileContract $profile
        if (-not $profileIds.Add([string]$profile.profile_id)) {
            throw 'MSVC payload profile IDs must be unique.'
        }
    }

    $identities = [ordered]@{
        cl = Get-PinnedMsvcToolIdentity (Join-Path $ToolBin 'cl.exe') 'cl.exe' 'Pinned MSVC compiler'
        link = Get-PinnedMsvcToolIdentity (Join-Path $ToolBin 'link.exe') 'link.exe' 'Pinned MSVC linker'
        lib = Get-PinnedMsvcToolIdentity (Join-Path $ToolBin 'lib.exe') 'lib.exe' 'Pinned MSVC librarian'
        nmake = Get-PinnedMsvcToolIdentity (Join-Path $ToolBin 'nmake.exe') 'nmake.exe' 'Pinned MSVC build driver'
    }
    $matchingProfiles = [System.Collections.Generic.List[psobject]]::new()
    foreach ($profile in $profilesArray) {
        $matches = $true
        foreach ($toolName in @('cl', 'link', 'lib', 'nmake')) {
            $expected = $profile.tools.$toolName
            $actual = $identities[$toolName]
            if (-not [string]::Equals(
                    [string]$actual.Filename,
                    [string]$expected.filename,
                    [System.StringComparison]::OrdinalIgnoreCase
                ) -or
                [string]$actual.FileVersion -cne [string]$expected.file_version -or
                [string]$actual.Sha256 -cne [string]$expected.sha256) {
                $matches = $false
                break
            }
        }
        if ($matches) {
            $matchingProfiles.Add($profile)
        }
    }
    if ($matchingProfiles.Count -ne 1) {
        $observed = @(
            foreach ($toolName in @('cl', 'link', 'lib', 'nmake')) {
                $identity = $identities[$toolName]
                "$toolName=$($identity.FileVersion):$($identity.Sha256)"
            }
        ) -join ','
        throw "MSVC tool payload does not match exactly one approved profile; observed $observed"
    }
    return [pscustomobject]@{
        ProfileId = [string]$matchingProfiles[0].profile_id
        Cl = [string]$identities.cl.Path
        Link = [string]$identities.link.Path
        Lib = [string]$identities.lib.Path
        NMake = [string]$identities.nmake.Path
    }
}

function ConvertFrom-VcVarsEnvironmentOutput([string]$Output) {
    if ($Output.Length -gt 1048576) {
        throw 'vcvarsall returned an oversized environment.'
    }
    $environment = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($line in @($Output -split "`r?`n")) {
        if ([string]::IsNullOrWhiteSpace($line) -or $line.StartsWith('=')) {
            continue
        }
        $separator = $line.IndexOf('=')
        if ($separator -lt 1) {
            throw 'vcvarsall returned a malformed environment line.'
        }
        $name = $line.Substring(0, $separator)
        $value = $line.Substring($separator + 1)
        if ($name -cnotmatch '^[^=\x00-\x1f]{1,255}$' -or
            $value.Length -gt 32767 -or
            $value.IndexOfAny([char[]]@("`0", "`r", "`n")) -ge 0 -or
            -not $environment.TryAdd($name, $value)) {
            throw 'vcvarsall returned a malformed or duplicate environment variable.'
        }
        if ($environment.Count -gt 4096) {
            throw 'vcvarsall returned too many environment variables.'
        }
    }
    return ,$environment
}

function Get-RequiredEnvironmentValue(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$Name
) {
    if (-not $Environment.ContainsKey($Name) -or
        [string]::IsNullOrWhiteSpace($Environment[$Name])) {
        throw "vcvarsall did not set required environment variable $Name."
    }
    return $Environment[$Name]
}

function Assert-ExactEnvironmentText(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$Name,
    [string]$Expected
) {
    $actual = Get-RequiredEnvironmentValue $Environment $Name
    if ($actual -cne $Expected) {
        throw "vcvarsall selected unexpected ${Name}: expected $Expected, found $actual."
    }
}

function Assert-ExactEnvironmentPath(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$Name,
    [string]$Expected
) {
    $actual = Get-NormalizedFullPath (Get-RequiredEnvironmentValue $Environment $Name)
    if (-not [string]::Equals(
        $actual,
        (Get-NormalizedFullPath $Expected),
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw "vcvarsall selected an unexpected path for $Name."
    }
}

function Get-EnvironmentPathEntries(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$Name
) {
    $entries = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in @((Get-RequiredEnvironmentValue $Environment $Name) -split ';')) {
        if (-not [string]::IsNullOrWhiteSpace($entry)) {
            $entries.Add((Get-NormalizedFullPath $entry.Trim()))
        }
    }
    if ($entries.Count -eq 0 -or $entries.Count -gt 256) {
        throw "vcvarsall returned an invalid $Name path list."
    }
    return $entries.ToArray()
}

function Assert-EnvironmentPathListContains(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$Name,
    [string]$Expected
) {
    $normalizedExpected = Get-NormalizedFullPath $Expected
    $found = @(
        Get-EnvironmentPathEntries $Environment $Name | Where-Object {
            [string]::Equals(
                $_,
                $normalizedExpected,
                [System.StringComparison]::OrdinalIgnoreCase
            )
        }
    ).Count -eq 1
    if (-not $found) {
        throw "vcvarsall $Name does not contain the exact required path: $normalizedExpected"
    }
}

function Resolve-EnvironmentExecutable(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$Filename
) {
    foreach ($directory in @(Get-EnvironmentPathEntries $Environment 'Path')) {
        $candidate = Join-Path $directory $Filename
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $null = Assert-RegularNonReparseFile $candidate "vcvarsall-selected $Filename"
            return Get-NormalizedFullPath $candidate
        }
    }
    throw "vcvarsall PATH does not resolve $Filename."
}

function Invoke-PinnedVcVarsEnvironment(
    [string]$VcVarsAll,
    [string]$WindowsSdkVersion,
    [string]$ToolsetVersion
) {
    foreach ($version in @($WindowsSdkVersion, $ToolsetVersion)) {
        if ($version -cnotmatch '^\d+(?:\.\d+){2,3}$') {
            throw 'vcvarsall selection contains a noncanonical version.'
        }
    }
    if ($VcVarsAll.IndexOfAny([char[]]@('"', '&', '|', '<', '>', '^', '%', '!', "`r", "`n")) -ge 0) {
        throw 'vcvarsall path contains a command-shell metacharacter.'
    }
    $windowsRoot = [System.Environment]::GetFolderPath(
        [System.Environment+SpecialFolder]::Windows
    )
    $commandProcessor = Join-Path $windowsRoot 'System32\cmd.exe'
    $null = Assert-RegularNonReparseFile $commandProcessor 'Windows command processor'
    $commandLine = '/d /s /c ""{0}" x64 {1} -vcvars_ver={2} >nul && set"' -f `
        $VcVarsAll, $WindowsSdkVersion, $ToolsetVersion
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $commandProcessor
    $startInfo.Arguments = $commandLine
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    foreach ($name in @($startInfo.Environment.Keys)) {
        if ($name.StartsWith('VSCMD_', [System.StringComparison]::OrdinalIgnoreCase)) {
            $null = $startInfo.Environment.Remove($name)
        }
    }
    foreach ($name in @(
        'CC', 'CXX', 'AR', 'CL', '_CL_', 'LINK', 'LIB', 'LIBPATH', 'INCLUDE',
        'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsVersion', 'VSINSTALLDIR',
        'WindowsSdkDir', 'WindowsSDKVersion', 'WindowsSdkBinPath',
        'WindowsSdkVerBinPath', 'UniversalCRTSdkDir', 'UCRTVersion'
    )) {
        $null = $startInfo.Environment.Remove($name)
    }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) {
            throw 'Could not start the pinned vcvarsall environment probe.'
        }
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill($true)
            throw 'Pinned vcvarsall environment probe timed out.'
        }
        $output = $stdout.GetAwaiter().GetResult()
        $errorOutput = $stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) {
            throw "Pinned vcvarsall environment probe failed with exit code $($process.ExitCode): $($errorOutput.Trim())"
        }
        return ConvertFrom-VcVarsEnvironmentOutput $output
    }
    finally {
        $process.Dispose()
    }
}

function Assert-PinnedMsvcEnvironment(
    [System.Collections.Generic.Dictionary[string, string]]$Environment,
    [string]$InstallationPath,
    [string]$ToolRoot,
    [psobject]$Tools,
    [string]$WindowsSdkRoot,
    [string]$WindowsSdkVersion
) {
    Assert-ExactEnvironmentPath $Environment 'VSINSTALLDIR' $InstallationPath
    Assert-ExactEnvironmentPath $Environment 'VCINSTALLDIR' (Join-Path $InstallationPath 'VC')
    Assert-ExactEnvironmentPath $Environment 'VCToolsInstallDir' $ToolRoot
    Assert-ExactEnvironmentText $Environment 'VCToolsVersion' (Split-Path -Leaf $ToolRoot)
    Assert-ExactEnvironmentPath $Environment 'WindowsSdkDir' $WindowsSdkRoot
    Assert-ExactEnvironmentPath $Environment 'UniversalCRTSdkDir' $WindowsSdkRoot
    Assert-ExactEnvironmentText $Environment 'WindowsSDKVersion' "$WindowsSdkVersion\"
    Assert-ExactEnvironmentText $Environment 'UCRTVersion' $WindowsSdkVersion
    Assert-ExactEnvironmentPath $Environment 'WindowsSdkBinPath' (Join-Path $WindowsSdkRoot 'bin')
    Assert-ExactEnvironmentPath $Environment 'WindowsSdkVerBinPath' (Join-Path $WindowsSdkRoot "bin\$WindowsSdkVersion")
    foreach ($selection in @(
        @('Platform', 'x64'),
        @('VSCMD_ARG_HOST_ARCH', 'x64'),
        @('VSCMD_ARG_TGT_ARCH', 'x64'),
        @('VSCMD_ARG_VCVARS_VER', (Split-Path -Leaf $ToolRoot)),
        @('VSCMD_ARG_winsdk', $WindowsSdkVersion)
    )) {
        Assert-ExactEnvironmentText $Environment $selection[0] $selection[1]
    }

    $toolBin = Get-NormalizedFullPath (Split-Path -Parent $Tools.Cl)
    $pathEntries = @(Get-EnvironmentPathEntries $Environment 'Path')
    if (-not [string]::Equals(
        $pathEntries[0],
        $toolBin,
        [System.StringComparison]::OrdinalIgnoreCase
    )) {
        throw 'vcvarsall did not place the exact pinned MSVC binary directory first on PATH.'
    }
    foreach ($toolName in @('Cl', 'Link', 'Lib', 'NMake')) {
        $expected = [string]$Tools.$toolName
        $resolved = Resolve-EnvironmentExecutable $Environment (Split-Path -Leaf $expected)
        if (-not [string]::Equals(
            $resolved,
            $expected,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
            throw "vcvarsall resolved an unexpected $toolName executable."
        }
    }

    foreach ($directory in @(
        (Join-Path $ToolRoot 'include'),
        (Join-Path $ToolRoot 'lib\x64'),
        (Join-Path $WindowsSdkRoot "bin\$WindowsSdkVersion\x64"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\ucrt"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\um"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\shared"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\winrt"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\cppwinrt"),
        (Join-Path $WindowsSdkRoot "lib\$WindowsSdkVersion\ucrt\x64"),
        (Join-Path $WindowsSdkRoot "lib\$WindowsSdkVersion\um\x64")
    )) {
        $null = Assert-PhysicalDirectory $directory 'Pinned MSVC/Windows SDK directory'
    }
    foreach ($directory in @(
        (Join-Path $ToolRoot 'include'),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\ucrt"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\um"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\shared"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\winrt"),
        (Join-Path $WindowsSdkRoot "include\$WindowsSdkVersion\cppwinrt")
    )) {
        Assert-EnvironmentPathListContains $Environment 'INCLUDE' $directory
    }
    foreach ($directory in @(
        (Join-Path $ToolRoot 'lib\x64'),
        (Join-Path $WindowsSdkRoot "lib\$WindowsSdkVersion\ucrt\x64"),
        (Join-Path $WindowsSdkRoot "lib\$WindowsSdkVersion\um\x64")
    )) {
        Assert-EnvironmentPathListContains $Environment 'LIB' $directory
    }
}

function Resolve-PinnedMsvcToolchain($Contract) {
    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    Assert-NoReparseAncestors $vswhere
    $null = Assert-RegularNonReparseFile $vswhere 'Visual Studio locator'
    $componentIds = @(
        [string]$Contract.msvc.preferred_component_id,
        [string]$Contract.msvc.fallback_discovery_component_id
    )
    $candidateComponents = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($componentId in $componentIds) {
        $arguments = @(Get-PinnedMsvcVsWhereArguments $componentId)
        $output = (Invoke-NativeProcess $vswhere $arguments "Could not query Visual Studio component $componentId.").Stdout
        foreach ($path in @(ConvertFrom-VsWhereInstallationPaths $output $componentId)) {
            if ([string]::IsNullOrWhiteSpace([string]$path)) {
                continue
            }
            if (-not $candidateComponents.ContainsKey($path)) {
                $candidateComponents.Add($path, $componentId)
            }
        }
    }
    if ($candidateComponents.Count -eq 0) {
        throw 'No Visual Studio installation exposes the reviewed MSVC compatibility or discovery component.'
    }

    $failures = [System.Collections.Generic.List[string]]::new()
    foreach ($installationPath in @($candidateComponents.Keys | Sort-Object)) {
        try {
            $null = Assert-PhysicalDirectory $installationPath 'Visual Studio installation'
            $vcvars = Join-Path $installationPath 'VC\Auxiliary\Build\vcvarsall.bat'
            Assert-NoReparseAncestors $vcvars
            $null = Assert-RegularNonReparseFile $vcvars 'Visual Studio vcvarsall'
            $toolRoot = Get-NormalizedFullPath (
                Join-Path $installationPath "VC\Tools\MSVC\$($Contract.msvc.toolset_version)"
            )
            $toolBin = Join-Path $toolRoot 'bin\Hostx64\x64'
            $null = Assert-PhysicalDirectory $toolBin 'Pinned MSVC x64 tool directory'
            $tools = Resolve-PinnedMsvcPayloadProfile `
                $toolBin `
                @($Contract.msvc.payload_profiles)
            $sdkRoot = Get-NormalizedFullPath (([string]$Contract.msvc.windows_sdk_root).Replace('/', '\'))
            $null = Assert-PhysicalDirectory $sdkRoot 'Pinned Windows SDK root'
            $environment = Invoke-PinnedVcVarsEnvironment `
                $vcvars `
                ([string]$Contract.msvc.windows_sdk_version) `
                ([string]$Contract.msvc.toolset_version)
            Assert-PinnedMsvcEnvironment `
                $environment `
                $installationPath `
                $toolRoot `
                $tools `
                $sdkRoot `
                ([string]$Contract.msvc.windows_sdk_version)
            $buildEnvironment = [ordered]@{}
            foreach ($name in @(
                'Path', 'INCLUDE', 'LIB', 'LIBPATH', 'VCINSTALLDIR',
                'VCToolsInstallDir', 'VCToolsVersion', 'VSINSTALLDIR',
                'WindowsSdkDir', 'WindowsSDKVersion', 'WindowsSdkBinPath',
                'WindowsSdkVerBinPath', 'UniversalCRTSdkDir', 'UCRTVersion',
                'Platform', 'VSCMD_ARG_HOST_ARCH', 'VSCMD_ARG_TGT_ARCH',
                'VSCMD_ARG_VCVARS_VER', 'VSCMD_ARG_winsdk'
            )) {
                $buildEnvironment[$name] = Get-RequiredEnvironmentValue $environment $name
            }
            $buildEnvironment['CC'] = $tools.Cl
            $buildEnvironment['CXX'] = $tools.Cl
            $buildEnvironment['AR'] = $tools.Lib
            $buildEnvironment['CC_x86_64_pc_windows_msvc'] = $tools.Cl
            $buildEnvironment['CXX_x86_64_pc_windows_msvc'] = $tools.Cl
            $buildEnvironment['AR_x86_64_pc_windows_msvc'] = $tools.Lib
            $buildEnvironment['CMAKE_C_COMPILER'] = $tools.Cl
            $buildEnvironment['CMAKE_CXX_COMPILER'] = $tools.Cl
            $buildEnvironment['CMAKE_LINKER'] = $tools.Link
            $buildEnvironment['CMAKE_AR'] = $tools.Lib
            $buildEnvironment['CMAKE_MAKE_PROGRAM'] = $tools.NMake
            $buildEnvironment['CMAKE_GENERATOR'] = 'NMake Makefiles'
            $buildEnvironment['CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER'] = $tools.Link
            return [pscustomobject]@{
                InstallationPath = $installationPath
                DiscoveryComponent = $candidateComponents[$installationPath]
                ToolRoot = $toolRoot
                Tools = $tools
                WindowsSdkRoot = $sdkRoot
                WindowsSdkVersion = [string]$Contract.msvc.windows_sdk_version
                Environment = $environment
                BuildEnvironment = $buildEnvironment
            }
        }
        catch {
            $failures.Add("$installationPath => $($_.Exception.Message)")
        }
    }
    throw "No discovered Visual Studio installation passed the exact MSVC payload and vcvars contract: $($failures -join ' | ')"
}

function Set-PinnedMsvcBuildEnvironment($Toolchain) {
    $previous = [System.Collections.Generic.List[psobject]]::new()
    try {
        foreach ($entry in $Toolchain.BuildEnvironment.GetEnumerator()) {
            $current = Get-Item -LiteralPath "Env:$($entry.Key)" -ErrorAction SilentlyContinue
            $previous.Add([pscustomobject]@{
                Name = [string]$entry.Key
                Exists = $null -ne $current
                Value = if ($null -eq $current) { $null } else { [string]$current.Value }
            })
            [System.Environment]::SetEnvironmentVariable(
                [string]$entry.Key,
                [string]$entry.Value,
                [System.EnvironmentVariableTarget]::Process
            )
        }
        return ,$previous.ToArray()
    }
    catch {
        Restore-ProcessEnvironment $previous.ToArray()
        throw
    }
}

function Restore-ProcessEnvironment([psobject[]]$Previous) {
    foreach ($entry in @($Previous)) {
        if ($entry.Exists) {
            [System.Environment]::SetEnvironmentVariable(
                [string]$entry.Name,
                [string]$entry.Value,
                [System.EnvironmentVariableTarget]::Process
            )
        }
        else {
            Remove-Item -LiteralPath "Env:$($entry.Name)" -ErrorAction SilentlyContinue
        }
    }
}

function Assert-ActivePinnedMsvcEnvironment($Toolchain) {
    $active = [System.Collections.Generic.Dictionary[string, string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($entry in $Toolchain.BuildEnvironment.GetEnumerator()) {
        $value = [System.Environment]::GetEnvironmentVariable([string]$entry.Key, 'Process')
        if ([string]::IsNullOrWhiteSpace($value)) {
            throw "Pinned MSVC build environment is missing $($entry.Key)."
        }
        $active.Add([string]$entry.Key, $value)
        if ($value -cne [string]$entry.Value) {
            throw "Pinned MSVC build environment changed after activation: $($entry.Key)."
        }
    }
    Assert-PinnedMsvcEnvironment `
        $active `
        $Toolchain.InstallationPath `
        $Toolchain.ToolRoot `
        $Toolchain.Tools `
        $Toolchain.WindowsSdkRoot `
        $Toolchain.WindowsSdkVersion
    foreach ($toolName in @('Cl', 'Link', 'Lib', 'NMake')) {
        $resolved = Get-CommandPath (Split-Path -Leaf $Toolchain.Tools.$toolName) "Pinned $toolName is not active."
        if (-not [string]::Equals(
            (Get-NormalizedFullPath $resolved),
            [string]$Toolchain.Tools.$toolName,
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
            throw "Active process resolves an unexpected MSVC $toolName."
        }
    }
}

function Assert-NoAmbientToolchainOverrides {
    $blocked = @(
        'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CMAKE_ARGS', 'CFLAGS', 'CXXFLAGS',
        'CC', 'CXX', 'AR', 'CL', '_CL_', 'LINK', 'LIB', 'LIBPATH', 'INCLUDE',
        'CMAKE_C_COMPILER', 'CMAKE_CXX_COMPILER', 'CMAKE_LINKER', 'CMAKE_AR',
        'CMAKE_MAKE_PROGRAM', 'CMAKE_GENERATOR', 'CMAKE_TOOLCHAIN_FILE',
        'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER',
        'CC_x86_64_pc_windows_msvc', 'CXX_x86_64_pc_windows_msvc',
        'AR_x86_64_pc_windows_msvc', 'VCINSTALLDIR', 'VCToolsInstallDir',
        'VCToolsVersion', 'VSINSTALLDIR', 'WindowsSdkDir', 'WindowsSDKVersion',
        'WindowsSdkBinPath', 'WindowsSdkVerBinPath', 'UniversalCRTSdkDir',
        'UCRTVersion', 'NVCC_PREPEND_FLAGS', 'NVCC_APPEND_FLAGS'
    )
    foreach ($ambientName in $blocked) {
        $ambient = Get-Item -LiteralPath "Env:$ambientName" -ErrorAction SilentlyContinue
        if ($null -ne $ambient -and -not [string]::IsNullOrWhiteSpace([string]$ambient.Value)) {
            throw "GPU worker release builds reject ambient toolchain override $ambientName."
        }
    }
    foreach ($ambient in @(Get-ChildItem Env:)) {
        if ($ambient.Name.StartsWith('VSCMD_', [System.StringComparison]::OrdinalIgnoreCase) -and
            -not [string]::IsNullOrWhiteSpace([string]$ambient.Value)) {
            throw "GPU worker release builds reject ambient toolchain override $($ambient.Name)."
        }
    }
}

function Assert-BaseToolchain($Contract, [string]$RepositoryRoot) {
    if ($env:OS -ne 'Windows_NT' -or
        -not [System.Environment]::Is64BitOperatingSystem -or
        -not [System.Environment]::Is64BitProcess) {
        throw 'Windows GPU worker packs require a native Windows x64 build process.'
    }
    $rustc = Get-CommandPath 'rustc.exe' 'Pinned Rust 1.96.0 is not installed or not on PATH.'
    $rustVersion = (Invoke-NativeProcess $rustc @('-Vv') 'Could not inspect rustc.').Stdout
    foreach ($required in @(
        "release: $($Contract.rust.release)",
        "commit-hash: $($Contract.rust.commit)",
        "host: $($Contract.rust.host)"
    )) {
        if (-not $rustVersion.Contains($required)) {
            throw "rustc does not match the pinned worker-pack toolchain ($required)."
        }
    }
    $rustup = Get-CommandPath 'rustup.exe' 'rustup is required to verify the pinned Windows target.'
    $installedTargets = (Invoke-NativeProcess $rustup @('target', 'list', '--installed') 'Could not inspect installed Rust targets.').Stdout
    if (-not @($installedTargets -split "`r?`n").Contains([string]$Contract.target_triple)) {
        throw "Pinned Rust target $($Contract.target_triple) is not installed."
    }

    $cmake = Get-CommandPath 'cmake.exe' "Pinned CMake $($Contract.msvc.cmake_version) is not installed or not on PATH."
    $cmakeVersion = (Invoke-NativeProcess $cmake @('--version') 'Could not inspect CMake.').Stdout
    if (-not $cmakeVersion.StartsWith("cmake version $($Contract.msvc.cmake_version)`n") -and
        -not $cmakeVersion.StartsWith("cmake version $($Contract.msvc.cmake_version)`r`n")) {
        throw "CMake does not match pinned version $($Contract.msvc.cmake_version)."
    }

    $msvcToolchain = Resolve-PinnedMsvcToolchain $Contract

    $cargoLockPath = Join-Path $RepositoryRoot 'Cargo.lock'
    $cargoLock = Get-Content -LiteralPath $cargoLockPath -Raw
    Assert-LockedPackage $cargoLock 'transcribe-cpp' `
        ([string]$Contract.native_source.transcribe_cpp_version) `
        ([string]$Contract.native_source.transcribe_cpp_checksum)
    Assert-LockedPackage $cargoLock 'transcribe-cpp-sys' `
        ([string]$Contract.native_source.transcribe_cpp_sys_version) `
        ([string]$Contract.native_source.transcribe_cpp_sys_checksum)
    Assert-LockedPackage $cargoLock 'ash' `
        ([string]$Contract.native_source.ash_version) `
        ([string]$Contract.native_source.ash_checksum)
    $toolchain = Get-Content -LiteralPath (Join-Path $RepositoryRoot 'rust-toolchain.toml') -Raw
    if ($toolchain -notmatch "channel\s*=\s*`"$([regex]::Escape([string]$Contract.rust.release))`"") {
        throw 'rust-toolchain.toml does not match the worker-pack toolchain contract.'
    }
    $cargoConfig = Get-Content -LiteralPath (Join-Path $RepositoryRoot '.cargo\config.toml') -Raw
    foreach ($required in @('+crt-static', 'CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded')) {
        if (-not $cargoConfig.Contains($required)) {
            throw "Windows worker build lost required static-runtime setting $required."
        }
    }
    foreach ($manifest in @(
        (Join-Path $RepositoryRoot 'Cargo.toml'),
        (Join-Path $RepositoryRoot 'tools\worker-pack-author\Cargo.toml')
    )) {
        $manifestText = Get-Content -LiteralPath $manifest -Raw
        if ($manifestText -notmatch "(?m)^version\s*=\s*`"$([regex]::Escape([string]$Contract.app_version))`"\s*$") {
            throw "Package version in $manifest does not match the worker-pack app version contract."
        }
    }
    $rootManifest = Get-Content -LiteralPath (Join-Path $RepositoryRoot 'Cargo.toml') -Raw
    foreach ($required in @(
        'vulkan-acceleration = ["inference-worker", "transcribe-cpp/vulkan", "dep:ash"]',
        'ash = { version = "=0.37.3", optional = true }'
    )) {
        if (-not $rootManifest.Contains($required)) {
            throw "Cargo.toml lost the pinned worker-only Vulkan identity dependency: $required"
        }
    }
    return $msvcToolchain
}

function Resolve-VulkanSdk($Contract) {
    $expectedVersion = [string]$Contract.vulkan.sdk_version
    $candidate = if (-not [string]::IsNullOrWhiteSpace($env:VULKAN_SDK)) {
        $env:VULKAN_SDK
    } else {
        "C:\VulkanSDK\$expectedVersion"
    }
    $root = Get-NormalizedFullPath $candidate
    if ((Split-Path -Leaf $root) -cne $expectedVersion -or
        -not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "Pinned Vulkan SDK $expectedVersion is missing. Set VULKAN_SDK to its exact installation root."
    }
    foreach ($required in @($Contract.vulkan.required_files)) {
        Assert-ExactProperties $required @('path', 'sha256') 'Vulkan SDK file contract'
        $relative = ([string]$required.path).Replace('/', '\')
        Assert-ExactHash (Join-Path $root $relative) ([string]$required.sha256) "Pinned Vulkan SDK file $($required.path)"
    }
    return $root
}

function Resolve-CudaSdk($Contract, [string]$BuildSigningMode) {
    $requiredAuthenticatedPaths = @(
        @($Contract.cuda.required_files) +
        @($Contract.cuda.packaged_runtime_imports | ForEach-Object { "bin/$_" })
    )
    if ($BuildSigningMode -ceq 'Production') {
        $null = ConvertTo-AuthenticatedCudaInventory `
            @($Contract.cuda.production_inventory) `
            $requiredAuthenticatedPaths
    }
    $expectedDirectoryVersion = [string]$Contract.cuda.sdk_directory_version
    $candidate = if (-not [string]::IsNullOrWhiteSpace($env:CUDA_PATH)) {
        $env:CUDA_PATH
    } else {
        "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v$expectedDirectoryVersion"
    }
    $root = Get-NormalizedFullPath $candidate
    if ((Split-Path -Leaf $root) -cne "v$expectedDirectoryVersion" -or
        -not (Test-Path -LiteralPath $root -PathType Container)) {
        throw "Pinned CUDA Toolkit $expectedDirectoryVersion is missing. Install that exact toolkit or set CUDA_PATH to its exact installation root."
    }
    Assert-NoReparseAncestors $root
    foreach ($required in @($Contract.cuda.required_files)) {
        $path = Join-Path $root (([string]$required).Replace('/', '\'))
        $null = Assert-RegularNonReparseFile $path "Pinned CUDA Toolkit file $required"
    }
    if ($BuildSigningMode -ceq 'Production') {
        Assert-AuthenticatedCudaSdkInventory `
            $root `
            @($Contract.cuda.production_inventory) `
            $requiredAuthenticatedPaths
    }
    $nvcc = Join-Path $root 'bin\nvcc.exe'
    $nvccVersion = (Invoke-NativeProcess $nvcc @('--version') 'Could not inspect the pinned CUDA compiler.').Stdout
    if (-not $nvccVersion.Contains("V$($Contract.cuda.nvcc_version)")) {
        throw "CUDA nvcc must be exactly V$($Contract.cuda.nvcc_version)."
    }
    return $root
}

function Copy-ReviewedGpuWorkerDependencyClosure(
    [string]$WorkerPath,
    [string]$SdkRoot,
    [string]$PackBin,
    [string[]]$SystemDriverImports,
    [string[]]$PackagedRuntimeImports
) {
    . (Join-Path $PSScriptRoot 'windows-pe-imports.ps1')
    $system = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $systemDrivers = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($name in @($ReviewedWindowsSystemDlls) + @($SystemDriverImports)) {
        if ([string]$name -cnotmatch '^[a-z0-9._-]+\.dll$') {
            throw "System dependency allowlist contains an unsafe DLL name: $name"
        }
        $null = $system.Add([string]$name)
    }
    foreach ($name in $SystemDriverImports) {
        $null = $systemDrivers.Add([string]$name)
    }
    $packaged = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($name in $PackagedRuntimeImports) {
        if ([string]$name -cnotmatch '^[a-z0-9._-]+\.dll$' -or
            $system.Contains([string]$name) -or
            -not $packaged.Add([string]$name)) {
            throw "Packaged dependency allowlist contains an unsafe or duplicate DLL name: $name"
        }
    }

    $pending = [System.Collections.Generic.Queue[string]]::new()
    $pending.Enqueue($WorkerPath)
    $processed = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    $observedProviderDependency = $false
    while ($pending.Count -gt 0) {
        $binary = $pending.Dequeue()
        $binaryName = [System.IO.Path]::GetFileName($binary)
        if (-not $processed.Add($binaryName)) {
            continue
        }
        $report = Get-WindowsPeImportReport $binary
        if ($report.Machine -ne 0x8664) {
            throw "GPU pack dependency is not an AMD64 PE: $binaryName"
        }
        if ([string]::Equals($binary, $WorkerPath, [System.StringComparison]::OrdinalIgnoreCase) -and
            $report.Subsystem -ne 3) {
            throw 'GPU inference worker must use the Windows console subsystem.'
        }
        foreach ($import in @($report.NormalImports) + @($report.DelayImports)) {
            if ($system.Contains([string]$import)) {
                if ($systemDrivers.Contains([string]$import)) {
                    $observedProviderDependency = $true
                }
                continue
            }
            if (-not $packaged.Contains([string]$import)) {
                throw "GPU pack contains an undeclared native dependency: $binaryName imports $import"
            }
            $observedProviderDependency = $true
            $destination = Join-Path $PackBin ([string]$import)
            if (-not (Test-Path -LiteralPath $destination -PathType Leaf)) {
                $source = Join-Path $SdkRoot "bin\$import"
                $null = Assert-RegularNonReparseFile $source "Pinned provider runtime $import"
                Copy-Item -LiteralPath $source -Destination $destination
                $null = Assert-RegularNonReparseFile $destination "Materialized provider runtime $import"
            }
            $pending.Enqueue($destination)
        }
    }
    if (-not $observedProviderDependency) {
        throw 'GPU worker dependency closure does not contain the selected provider runtime or driver interface.'
    }
}

$repositoryRoot = Get-NormalizedFullPath (Split-Path -Parent $PSScriptRoot)
if (-not $ToolchainManifestPath) {
    $ToolchainManifestPath = Join-Path $repositoryRoot 'runtime-manifests\gpu-worker-toolchain-windows-x64.json'
}
$contractPath = Get-NormalizedFullPath $ToolchainManifestPath
$null = Assert-RegularNonReparseFile $contractPath 'GPU worker-pack toolchain manifest'
$contract = Get-Content -LiteralPath $contractPath -Raw | ConvertFrom-Json
Assert-ExactProperties $contract @('schema_version', 'app_version', 'target_triple', 'rust', 'native_source', 'msvc', 'build', 'vulkan', 'cuda') 'GPU worker-pack toolchain manifest'
Assert-ExactProperties $contract.rust @('release', 'commit', 'host') 'Rust toolchain contract'
Assert-ExactProperties $contract.native_source @('transcribe_cpp_version', 'transcribe_cpp_checksum', 'transcribe_cpp_sys_version', 'transcribe_cpp_sys_checksum', 'ash_version', 'ash_checksum', 'source_revision', 'sherpa_onnx_archive') 'Native source contract'
Assert-ExactProperties $contract.native_source.sherpa_onnx_archive @('filename', 'size_bytes', 'sha256') 'Sherpa ONNX archive contract'
Assert-ExactProperties $contract.msvc @('preferred_component_id', 'fallback_discovery_component_id', 'toolset_version', 'platform_toolset', 'windows_sdk_version', 'windows_sdk_root', 'payload_profiles', 'cmake_version', 'runtime', 'reproducible_flag') 'MSVC toolchain contract'
$payloadProfiles = @($contract.msvc.payload_profiles)
if ($payloadProfiles.Count -lt 1 -or $payloadProfiles.Count -gt 8) {
    throw 'MSVC payload profile count is invalid.'
}
foreach ($payloadProfile in $payloadProfiles) {
    Assert-MsvcPayloadProfileContract $payloadProfile
}
Assert-ExactProperties $contract.build @('profile', 'static_cpu_scheduling', 'dynamic_backends', 'openmp') 'Worker build contract'
Assert-ExactProperties $contract.vulkan @('sdk_version', 'provider', 'required_files', 'system_driver_imports', 'packaged_runtime_imports') 'Vulkan provider contract'
Assert-ExactProperties $contract.cuda @('sdk_directory_version', 'nvcc_version', 'provider', 'cmake_architectures', 'required_files', 'production_inventory', 'system_driver_imports', 'packaged_runtime_imports') 'CUDA provider contract'
if ($contract.schema_version -ne 1 -or
    $contract.app_version -cnotmatch '^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$' -or
    $contract.target_triple -cne 'x86_64-pc-windows-msvc' -or
    $contract.msvc.preferred_component_id -cne 'Microsoft.VisualStudio.Component.VC.14.44.17.14.x86.x64' -or
    $contract.msvc.fallback_discovery_component_id -cne 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64' -or
    $contract.msvc.toolset_version -cne '14.44.35207' -or
    $contract.msvc.platform_toolset -cne 'v143' -or
    $contract.msvc.windows_sdk_version -cne '10.0.26100.0' -or
    $contract.msvc.windows_sdk_root -cne 'C:/Program Files (x86)/Windows Kits/10' -or
    $contract.msvc.runtime -cne 'MultiThreaded' -or
    $contract.msvc.reproducible_flag -cne '/Brepro' -or
    $contract.build.profile -cne 'release' -or
    -not $contract.build.static_cpu_scheduling -or
    $contract.build.dynamic_backends -or
    $contract.build.openmp) {
    throw 'GPU worker-pack toolchain manifest violates the reviewed Windows x64 static-runtime contract.'
}

Assert-NoAmbientToolchainOverrides
$msvcToolchain = Assert-BaseToolchain $contract $repositoryRoot
$archiveContract = $contract.native_source.sherpa_onnx_archive
if (-not $NativeArchiveDirectory) {
    $NativeArchiveDirectory = Join-Path $repositoryRoot '.ci-native'
}
$nativeArchiveRoot = Get-NormalizedFullPath $NativeArchiveDirectory
Assert-NoReparseAncestors $nativeArchiveRoot
$nativeArchive = Join-Path $nativeArchiveRoot ([string]$archiveContract.filename)
$archiveItem = Assert-RegularNonReparseFile $nativeArchive 'Pinned Sherpa ONNX archive'
if ($archiveItem.Length -ne [int64]$archiveContract.size_bytes) {
    throw 'Pinned Sherpa ONNX archive size mismatch.'
}
Assert-ExactHash $nativeArchive ([string]$archiveContract.sha256) 'Pinned Sherpa ONNX archive'
$backendName = $Backend.ToLowerInvariant()
$providerContract = if ($Backend -eq 'Vulkan') { $contract.vulkan } else { $contract.cuda }
$sdkRoot = if ($Backend -eq 'Vulkan') {
    Resolve-VulkanSdk $contract
} else {
    Resolve-CudaSdk $contract $SigningMode
}
if ($ToolchainCheckOnly) {
    $toolchainEnvironmentState = $null
    try {
        $toolchainEnvironmentState = Set-PinnedMsvcBuildEnvironment $msvcToolchain
        Assert-ActivePinnedMsvcEnvironment $msvcToolchain
        if ($ExportPinnedMsvcEnvironment) {
            [pscustomobject]@{
                schema_version = 1
                environment = $msvcToolchain.BuildEnvironment
            } | ConvertTo-Json -Depth 4 -Compress | Write-Output
        }
        else {
            Write-Output "$Backend worker-pack toolchain matches the pinned contract."
        }
    }
    finally {
        if ($null -ne $toolchainEnvironmentState) {
            Restore-ProcessEnvironment $toolchainEnvironmentState
        }
    }
    return
}
if ($PackVersion -cnotmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$') {
    throw 'PackVersion must be a canonical lowercase immutable store component.'
}
$outputRoot = Get-NormalizedFullPath $OutputDirectory
Assert-NoReparseAncestors $outputRoot
if (Test-Path -LiteralPath $outputRoot) {
    throw "GPU worker-pack output already exists: $outputRoot"
}
$shortBuild = Resolve-ShortCargoTargetDirectory $CargoTargetDirectory $backendName $PackVersion
$cargoTarget = $shortBuild.Target

$git = Get-CommandPath 'git.exe' 'Git is required to bind worker packs to an exact source revision.'
$revision = (Invoke-NativeProcess $git @('-C', $repositoryRoot, 'rev-parse', '--verify', 'HEAD') 'Could not resolve source revision.').Stdout.Trim()
if ($revision -cnotmatch '^[0-9a-f]{40}$') {
    throw 'Git returned a noncanonical source revision.'
}
$sourceDateEpoch = (Invoke-NativeProcess $git @('-C', $repositoryRoot, 'show', '-s', '--format=%ct', $revision) 'Could not resolve source revision timestamp.').Stdout.Trim()
if ($sourceDateEpoch -cnotmatch '^[1-9][0-9]{8,11}$') {
    throw 'Git returned a noncanonical source revision timestamp.'
}
$null = Invoke-NativeProcess $git @('-C', $repositoryRoot, 'diff', '--quiet', '--exit-code') 'GPU worker release builds require a clean worktree.'
$null = Invoke-NativeProcess $git @('-C', $repositoryRoot, 'diff', '--cached', '--quiet', '--exit-code') 'GPU worker release builds require a clean index.'

$manifestPath = Join-Path $repositoryRoot 'Cargo.toml'
$authoringManifestPath = Join-Path $repositoryRoot 'tools\worker-pack-author\Cargo.toml'
$cargo = Get-CommandPath 'cargo.exe' 'Cargo is required to build GPU worker packs.'
$previousCargoTarget = $env:CARGO_TARGET_DIR
$previousRevision = $env:SCRIBE_BUILD_REVISION
$previousWorkerDigest = $env:SCRIBE_BUNDLED_WORKER_SHA256
$previousBuildingWorker = $env:SCRIBE_BUILDING_WORKER
$previousVulkanSdk = $env:VULKAN_SDK
$previousCudaPath = $env:CUDA_PATH
$previousCmakeArguments = $env:TRANSCRIBE_CMAKE_ARGS
$previousSherpaArchiveRoot = $env:SHERPA_ONNX_ARCHIVE_DIR
$previousSourceDateEpoch = $env:SOURCE_DATE_EPOCH
$previousMsvcFlags = $env:_CL_
$previousLocalAppData = $env:LOCALAPPDATA
$previousPinnedMsvcEnvironment = $null
$stagingRoot = "$outputRoot.staging-$([guid]::NewGuid().ToString('N'))"
$stagingCreated = $false

try {
    $previousPinnedMsvcEnvironment = Set-PinnedMsvcBuildEnvironment $msvcToolchain
    Assert-ActivePinnedMsvcEnvironment $msvcToolchain
    New-Item -ItemType Directory -Path $shortBuild.BuildEnvironment | Out-Null
    $buildEnvironmentItem = Get-Item -LiteralPath $shortBuild.BuildEnvironment -Force
    if (-not $buildEnvironmentItem.PSIsContainer -or
        ($buildEnvironmentItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'GPU worker native build environment is not a physical directory.'
    }
    $env:CARGO_TARGET_DIR = $cargoTarget
    # transcribe-cpp-sys creates its bounded native-build junction beneath this
    # fresh build-specific shell-folder path. It never shares or reclaims the
    # user's ambient tcs junction namespace.
    $env:LOCALAPPDATA = $shortBuild.BuildEnvironment
    $env:SCRIBE_BUILD_REVISION = $revision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $null
    $env:SCRIBE_BUILDING_WORKER = '1'
    $env:SHERPA_ONNX_ARCHIVE_DIR = $nativeArchiveRoot
    $env:SOURCE_DATE_EPOCH = $sourceDateEpoch
    $env:_CL_ = [string]$contract.msvc.reproducible_flag
    $env:TRANSCRIBE_CMAKE_ARGS = '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded -DTRANSCRIBE_GGML_BACKEND_DL=OFF -DGGML_OPENMP=OFF'
    if ($Backend -eq 'Vulkan') {
        $env:VULKAN_SDK = $sdkRoot
        $env:CUDA_PATH = $null
    } else {
        $env:CUDA_PATH = $sdkRoot
        $env:VULKAN_SDK = $null
        $env:TRANSCRIBE_CMAKE_ARGS += " -DCMAKE_CUDA_ARCHITECTURES=$($contract.cuda.cmake_architectures)"
    }

    $null = Invoke-NativeProcess $cargo @(
        'build', '--locked', '--offline', '--release',
        '--bin', 'scribe-worker-pack-tool',
        '--manifest-path', $authoringManifestPath
    ) 'Worker-pack authoring tool build failed.'
    $authoringTool = Join-Path $cargoTarget 'release\scribe-worker-pack-tool.exe'
    $null = Assert-RegularNonReparseFile $authoringTool 'Worker-pack authoring tool'

    if ($SigningMode -eq 'Production') {
        $privateKey = Get-NormalizedFullPath $ProductionPrivateKeyPath
        $null = Invoke-NativeProcess $authoringTool @(
            'check-production-key', '--key-id', $ProductionKeyId,
            '--private-key', $privateKey
        ) 'Production GPU pack signing is not provisioned.'
    }

    $feature = "$backendName-acceleration"
    $workerBuildArguments = @(
        'build', '--locked', '--offline', '--release',
        '--bin', 'scribe-inference-worker', '--features', $feature,
        '--manifest-path', $manifestPath
    )
    try {
        $null = Invoke-NativeProcess $cargo $workerBuildArguments "$Backend inference worker build failed."
    }
    catch {
        $failure = $_.Exception.Message
        $isKnownShortPathBootstrap = $failure.Contains('transcribe-cpp-sys') -and (
            $failure.Contains('The directory name is invalid. (os error 267)') -or
            $failure.Contains('Could not open file for write in copy operation')
        )
        if (-not $isKnownShortPathBootstrap) {
            throw
        }
        Enable-ValidatedCmakeBuildJunction $shortBuild.BuildEnvironment $cargoTarget
        Write-Warning 'Retrying the pinned native build through its validated isolated CMake build junction.'
        $null = Invoke-NativeProcess $cargo $workerBuildArguments "$Backend inference worker build failed after short-path bootstrap."
    }
    $worker = Join-Path $cargoTarget 'release\scribe-inference-worker.exe'
    $null = Assert-RegularNonReparseFile $worker "$Backend inference worker"

    New-Item -ItemType Directory -Path (Join-Path $stagingRoot 'bin') | Out-Null
    $stagingCreated = $true
    $stagedWorker = Join-Path $stagingRoot 'bin\scribe-inference-worker.exe'
    Copy-Item -LiteralPath $worker -Destination $stagedWorker
    $null = Assert-RegularNonReparseFile $stagedWorker 'Materialized GPU inference worker'
    Copy-ReviewedGpuWorkerDependencyClosure `
        $stagedWorker `
        $sdkRoot `
        (Join-Path $stagingRoot 'bin') `
        @($providerContract.system_driver_imports) `
        @($providerContract.packaged_runtime_imports)

    $packId = "scribe-$backendName-windows-x64"
    $authorArguments = @(
        'author',
        '--backend', $backendName,
        '--pack-id', $packId,
        '--pack-root', $stagingRoot,
        '--pack-version', $PackVersion,
        '--provider', ([string]$providerContract.provider),
        '--security-epoch', '1',
        '--worker-path', 'bin/scribe-inference-worker.exe'
    )
    if ($SigningMode -eq 'Fixture') {
        $authorArguments += '--fixture-signing'
    } else {
        $authorArguments += @(
            '--key-id', $ProductionKeyId,
            '--private-key', (Get-NormalizedFullPath $ProductionPrivateKeyPath)
        )
    }
    $authored = Invoke-NativeProcess $authoringTool $authorArguments 'GPU worker-pack authoring failed.'
    try {
        $descriptor = $authored.Stdout | ConvertFrom-Json
    }
    catch {
        throw "Worker-pack authoring returned invalid JSON: $($_.Exception.Message)"
    }
    Assert-ExactProperties $descriptor @('pack_id', 'pack_version', 'pack_digest', 'key_id', 'payload_files', 'installed_payload_bytes') 'Authored worker-pack descriptor'
    if ($descriptor.pack_id -cne $packId -or
        $descriptor.pack_version -cne $PackVersion -or
        [string]$descriptor.pack_digest -cnotmatch '^[0-9a-f]{64}$') {
        throw 'Worker-pack authoring returned a mismatched identity.'
    }
    Move-Item -LiteralPath $stagingRoot -Destination $outputRoot
    $stagingCreated = $false
    [pscustomobject]@{
        Backend = $Backend
        PackRoot = $outputRoot
        PackId = [string]$descriptor.pack_id
        PackVersion = [string]$descriptor.pack_version
        PackDigest = [string]$descriptor.pack_digest
        SigningKeyId = [string]$descriptor.key_id
        PayloadFiles = [int]$descriptor.payload_files
        InstalledPayloadBytes = [int64]$descriptor.installed_payload_bytes
        SourceRevision = $revision
    }
}
finally {
    if ($null -ne $previousPinnedMsvcEnvironment) {
        Restore-ProcessEnvironment $previousPinnedMsvcEnvironment
    }
    $env:CARGO_TARGET_DIR = $previousCargoTarget
    $env:SCRIBE_BUILD_REVISION = $previousRevision
    $env:SCRIBE_BUNDLED_WORKER_SHA256 = $previousWorkerDigest
    $env:SCRIBE_BUILDING_WORKER = $previousBuildingWorker
    $env:VULKAN_SDK = $previousVulkanSdk
    $env:CUDA_PATH = $previousCudaPath
    $env:TRANSCRIBE_CMAKE_ARGS = $previousCmakeArguments
    $env:SHERPA_ONNX_ARCHIVE_DIR = $previousSherpaArchiveRoot
    $env:SOURCE_DATE_EPOCH = $previousSourceDateEpoch
    $env:_CL_ = $previousMsvcFlags
    $env:LOCALAPPDATA = $previousLocalAppData
    if ($stagingCreated -and (Test-Path -LiteralPath $stagingRoot)) {
        $expectedParent = Get-NormalizedFullPath (Split-Path -Parent $outputRoot)
        $observedParent = Get-NormalizedFullPath (Split-Path -Parent $stagingRoot)
        if ($observedParent -cne $expectedParent -or
            -not (Split-Path -Leaf $stagingRoot).StartsWith("$(Split-Path -Leaf $outputRoot).staging-", [System.StringComparison]::Ordinal)) {
            throw 'Refusing to clean a staging path outside the exact GPU worker-pack output parent.'
        }
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
}
