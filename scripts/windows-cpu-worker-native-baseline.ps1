# The bundled Windows CPU worker is intentionally built in a fresh target so
# transcribe.cpp's conservative x86 option can establish its defaults before a
# prior CMake cache has a chance to retain host-specific SIMD settings.

function Get-WindowsCpuWorkerBaselineCmakeArgs {
    return '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded -DTRANSCRIBE_X86_CONSERVATIVE=ON -DGGML_NATIVE=OFF -DTRANSCRIBE_GGML_BACKEND_DL=OFF -DGGML_OPENMP=OFF'
}

function Get-WindowsCpuWorkerBaselineEnvironmentNames {
    return @(
        'CARGO_TARGET_DIR',
        'TRANSCRIBE_CMAKE_ARGS',
        'RUSTFLAGS'
    )
}

function Assert-WindowsCpuWorkerBaselineNoReparseAncestors([string]$Path) {
    $current = [System.IO.Path]::GetFullPath($Path)
    while (-not (Test-Path -LiteralPath $current)) {
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            throw "Could not resolve an existing ancestor for CPU worker baseline path: $Path"
        }
        $current = $parent
    }
    while ($current) {
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "CPU worker baseline paths cannot cross a symbolic link or reparse point: $current"
        }
        $parent = Split-Path -Parent $current
        if (-not $parent -or $parent -eq $current) {
            break
        }
        $current = $parent
    }
}

function Test-WindowsCpuWorkerBaselineUnsafeIsaOption([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return $false
    }
    # CL accepts a dash as well as a slash for option prefixes.
    if ($Value -match '(?i)(?:^|[\s>"''])-arch\s*[:=]\s*[^\s"'']+') {
        return $true
    }
    if ($Value -match '(?i)(?:^|[\s>"''])[-/]D\s*GGML_(?:NATIVE|SSE42|AVX(?:_VNNI)?|AVX2|BMI2|FMA|F16C|AVX512(?:_VBMI|_VNNI|_BF16)?)\b') {
        return $true
    }
    return $Value -match '(?i)(?:\bGGML_(?:NATIVE|SSE42|AVX(?:_VNNI)?|AVX2|BMI2|FMA|F16C|AVX512(?:_VBMI|_VNNI|_BF16)?)\s*[:=]\s*(?:ON|1|TRUE)\b|\bTRANSCRIBE_X86_CONSERVATIVE\s*[:=]\s*(?:OFF|0|FALSE)\b|(?:^|[\s"''])/(?:arch)\s*[:=]\s*[^\s"'']+|(?:^|[\s"''])-m(?:sse|avx|fma|f16c|bmi|xop|aes|pclmul|popcnt|lzcnt|sha|amx|arch|tune|cpu)[A-Za-z0-9_.=-]*\b|target-cpu\s*=?\s*(?:native|haswell|skylake|znver[0-9]*)\b|target-feature\s*=?\s*[^\s]*(?:\+(?:sse4\.2|avx|avx2|avx512[^,\s]*|fma|f16c|bmi2)))'
}

function Test-WindowsCpuWorkerBaselineUnsafeGeneratedFlags([string]$Value, [switch]$AllowMsBuildProperties) {
    if ($AllowMsBuildProperties) {
        # Raw projects contain standard MSBuild inheritance in includes,
        # definitions and output paths. The mandatory evaluated command log
        # below, not raw XML, proves those values after expansion.
        $Value = [regex]::Replace($Value, '(?:%|\$)\([^)]+\)', '')
    }
    return (Test-WindowsCpuWorkerBaselineUnsafeIsaOption $Value) -or
        $Value -match '(?i)(?:\bGGML_(?:NATIVE|SSE42|AVX(?:_VNNI)?|AVX2|BMI2|FMA|F16C|AVX512(?:_VBMI|_VNNI|_BF16)?)\b|(?:^|[\s"''])/(?:arch)\s*[:=]\s*[^\s"'']+|(?:^|[\s"''])-m(?:sse|avx|fma|f16c|bmi|xop|aes|pclmul|popcnt|lzcnt|sha|amx|arch|tune|cpu)[A-Za-z0-9_.=-]*\b|<EnhancedInstructionSet>\s*(?!(?:NotSet)\s*<)[^<]+<|@(?:"[^"]+"|[^\s<]+)|%\((?!AdditionalOptions\))[^)]+\)|\$\([^)]+\))'
}

function Assert-WindowsCpuWorkerBaselineAmbientEnvironment {
    foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
        $name = [string]$entry.Key
        $value = [string]$entry.Value
        if ([string]::IsNullOrWhiteSpace($value)) { continue }
        if ($name -match '^(?i:CMAKE_ARGS|TRANSCRIBE_CMAKE_ARGS)$') {
            throw "CPU worker baseline does not accept ambient CMake arguments: $name"
        }
        if ($name -match '^(?i:CMAKE_TOOLCHAIN_FILE(?:_.+)?|HOST_CMAKE_TOOLCHAIN_FILE|CMAKE_X86_64_PC_WINDOWS_MSVC(?:_.+)?)$') {
            throw "CPU worker baseline does not accept ambient CMake toolchain overrides: $name"
        }
        if ($name -match '^(?i:(?:C|CXX|CPP)FLAGS(?:_.+)?|CL|_CL_|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_RUSTFLAGS|CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS)$') {
            throw "CPU worker baseline does not accept ambient compiler flags: $name"
        }
    }
}

function New-WindowsCpuWorkerBaselineBuild([string]$TargetTriple) {
    Assert-WindowsCpuWorkerBaselineAmbientEnvironment
    $temporaryRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    Assert-WindowsCpuWorkerBaselineNoReparseAncestors $temporaryRoot
    $name = "scribe-windows-cpu-worker-baseline-$PID-$([guid]::NewGuid().ToString('N'))"
    $targetRoot = Join-Path $temporaryRoot $name
    if (Test-Path -LiteralPath $targetRoot) {
        throw "Fresh CPU worker baseline target unexpectedly already exists: $targetRoot"
    }
    $previousEnvironment = @{}
    foreach ($name in Get-WindowsCpuWorkerBaselineEnvironmentNames) {
        $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
    }
    $env:CARGO_TARGET_DIR = $targetRoot
    $env:TRANSCRIBE_CMAKE_ARGS = Get-WindowsCpuWorkerBaselineCmakeArgs
    # Cargo's environment-level RUSTFLAGS override target-specific Cargo
    # configuration. Keep the project's static CRT but deny host ISA tuning.
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    return [pscustomobject]@{
        TargetRoot = $targetRoot
        TargetTriple = $TargetTriple
        WorkerPath = Join-Path $targetRoot "$TargetTriple\release\scribe-inference-worker.exe"
        PreviousEnvironment = $previousEnvironment
        Restored = $false
    }
}

function Restore-WindowsCpuWorkerBaselineEnvironment([psobject]$Build) {
    if ($Build.Restored) { return }
    foreach ($name in Get-WindowsCpuWorkerBaselineEnvironmentNames) {
        [Environment]::SetEnvironmentVariable($name, $Build.PreviousEnvironment[$name])
    }
    $Build.Restored = $true
}

function Get-WindowsCpuWorkerBaselineCacheEntry([string]$CacheText, [string]$Name) {
    $matches = @([regex]::Matches($CacheText, "(?m)^$([regex]::Escape($Name)):(?:BOOL|STRING|PATH|FILEPATH|INTERNAL)=([^\r\n]*)\r?$"))
    if ($matches.Count -ne 1) {
        throw "CPU worker baseline evidence must contain exactly one $Name CMake cache entry."
    }
    return $matches[0].Groups[1].Value.Trim()
}

function Assert-WindowsCpuWorkerBaselineRegularFile([string]$Path) {
    Assert-WindowsCpuWorkerBaselineNoReparseAncestors $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "CPU worker baseline evidence file is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "CPU worker baseline evidence file cannot be a reparse point: $Path"
    }
    return $item
}

function Get-WindowsCpuWorkerBaselineNativeBuildRoot([psobject]$Build) {
    $buildParent = Join-Path $Build.TargetRoot "$($Build.TargetTriple)\release\build"
    Assert-WindowsCpuWorkerBaselineNoReparseAncestors $buildParent
    if (-not (Test-Path -LiteralPath $buildParent -PathType Container)) {
        throw 'CPU worker baseline Cargo build directory is missing after the native build.'
    }
    $candidates = @(Get-ChildItem -LiteralPath $buildParent -Directory -Force | Where-Object {
            $_.Name -match '^transcribe-cpp-sys-[0-9A-Fa-f]+$'
        })
    if ($candidates.Count -ne 1) {
        throw "CPU worker baseline requires exactly one transcribe-cpp-sys Cargo build directory; found $($candidates.Count)."
    }
    if (($candidates[0].Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw 'CPU worker baseline transcribe-cpp-sys Cargo build directory cannot be a reparse point.'
    }
    $nativeBuildRoot = Join-Path $candidates[0].FullName 'out\build'
    Assert-WindowsCpuWorkerBaselineNoReparseAncestors $nativeBuildRoot
    if (-not (Test-Path -LiteralPath $nativeBuildRoot -PathType Container)) {
        throw 'CPU worker baseline transcribe native build directory is missing after the native build.'
    }
    return $nativeBuildRoot
}

function Assert-WindowsCpuWorkerBaselineVisualStudioFlags([string]$ProjectPath, [string]$TlogPath) {
    $null = Assert-WindowsCpuWorkerBaselineRegularFile $ProjectPath
    try {
        [xml]$project = [System.IO.File]::ReadAllText($ProjectPath, [System.Text.UTF8Encoding]::new($false, $true))
    }
    catch {
        throw "CPU worker baseline Visual Studio ggml-cpu flags are not valid XML: $ProjectPath"
    }
    $compileNodes = @($project.SelectNodes("//*[local-name()='ItemDefinitionGroup']/*[local-name()='ClCompile']"))
    if ($compileNodes.Count -eq 0) {
        throw "CPU worker baseline Visual Studio ggml-cpu flags do not contain a ClCompile configuration: $ProjectPath"
    }
    $enhancedNodes = @($project.SelectNodes("//*[local-name()='EnhancedInstructionSet' or local-name()='EnableEnhancedInstructionSet']"))
    foreach ($node in $enhancedNodes) {
        if ($node.InnerText.Trim() -cnotin @('', 'NotSet')) {
            throw "CPU worker baseline Visual Studio ggml-cpu flags contain an unrecognized or higher ISA requirement: $ProjectPath"
        }
    }
    foreach ($node in $compileNodes) {
        if (Test-WindowsCpuWorkerBaselineUnsafeGeneratedFlags $node.InnerXml -AllowMsBuildProperties) {
            throw "CPU worker baseline Visual Studio ggml-cpu flags contain an unrecognized or higher ISA requirement: $ProjectPath"
        }
    }
    $null = Assert-WindowsCpuWorkerBaselineRegularFile $TlogPath
    $bytes = [System.IO.File]::ReadAllBytes($TlogPath)
    if ($bytes.Length -le 2 -or -not (($bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) -or ($bytes[0] -eq 0xFE -and $bytes[1] -eq 0xFF))) {
        throw "CPU worker baseline Visual Studio ggml-cpu command log must be nonempty UTF-16 with a byte-order mark: $TlogPath"
    }
    $encoding = [System.Text.UnicodeEncoding]::new(($bytes[0] -eq 0xFE), $false, $true)
    $text = $encoding.GetString($bytes, 2, $bytes.Length - 2)
    $lines = @($text -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $hasCommandRecord = $false
    for ($index = 0; $index -lt ($lines.Count - 1); $index++) {
        if ($lines[$index].StartsWith('^', [System.StringComparison]::Ordinal) -and -not $lines[$index + 1].StartsWith('^', [System.StringComparison]::Ordinal)) {
            $hasCommandRecord = $true
            break
        }
    }
    if (-not $hasCommandRecord) {
        throw "CPU worker baseline Visual Studio ggml-cpu command log does not contain a caret source and compiler-command record: $TlogPath"
    }
    if ((Test-WindowsCpuWorkerBaselineUnsafeGeneratedFlags $text) -or $text -match '(?:%|\$)\([^)]+\)') {
        throw "CPU worker baseline Visual Studio ggml-cpu command log contains an unresolved or higher ISA requirement: $TlogPath"
    }
}

function Assert-WindowsCpuWorkerBaselineEvidence([psobject]$Build) {
    if (-not (Test-Path -LiteralPath $Build.TargetRoot -PathType Container)) {
        throw 'CPU worker baseline target is missing after the native build.'
    }
    Assert-WindowsCpuWorkerBaselineNoReparseAncestors $Build.TargetRoot
    $nativeBuildRoot = Get-WindowsCpuWorkerBaselineNativeBuildRoot $Build
    $cachePath = Join-Path $nativeBuildRoot 'CMakeCache.txt'
    $null = Assert-WindowsCpuWorkerBaselineRegularFile $cachePath
    $cacheText = [System.IO.File]::ReadAllText($cachePath, [System.Text.UTF8Encoding]::new($false, $true))
    $expectedCache = [ordered]@{
        TRANSCRIBE_X86_CONSERVATIVE = 'ON'
        TRANSCRIBE_GGML_BACKEND_DL = 'OFF'
        GGML_NATIVE = 'OFF'
        GGML_BACKEND_DL = 'OFF'
        GGML_OPENMP = 'OFF'
        GGML_CPU_ALL_VARIANTS = 'OFF'
        GGML_SSE42 = 'OFF'
        GGML_AVX = 'OFF'
        GGML_AVX_VNNI = 'OFF'
        GGML_AVX2 = 'OFF'
        GGML_BMI2 = 'OFF'
        GGML_FMA = 'OFF'
        GGML_F16C = 'OFF'
        GGML_AVX512 = 'OFF'
        GGML_AVX512_VBMI = 'OFF'
        GGML_AVX512_VNNI = 'OFF'
        GGML_AVX512_BF16 = 'OFF'
    }
    foreach ($entry in $expectedCache.GetEnumerator()) {
        $actual = Get-WindowsCpuWorkerBaselineCacheEntry $cacheText $entry.Key
        if ($actual -cne $entry.Value) {
            throw "CPU worker baseline CMake cache requires $($entry.Key)=$($entry.Value); found $actual."
        }
    }
    $cacheCompilerFlags = [System.Collections.Generic.List[string]]::new()
    foreach ($name in @('CMAKE_C_FLAGS', 'CMAKE_CXX_FLAGS', 'CMAKE_C_FLAGS_RELEASE', 'CMAKE_CXX_FLAGS_RELEASE')) {
        $cacheCompilerFlags.Add((Get-WindowsCpuWorkerBaselineCacheEntry $cacheText $name))
    }
    if (Test-WindowsCpuWorkerBaselineUnsafeGeneratedFlags ($cacheCompilerFlags -join "`n")) {
        throw 'CPU worker baseline CMake compiler flags contain a native or higher ISA requirement.'
    }
    $generator = Get-WindowsCpuWorkerBaselineCacheEntry $cacheText 'CMAKE_GENERATOR'
    if ($generator -ceq 'NMake Makefiles') {
        $flagsPath = Join-Path $nativeBuildRoot 'ggml\src\CMakeFiles\ggml-cpu.dir\flags.make'
        $null = Assert-WindowsCpuWorkerBaselineRegularFile $flagsPath
        $flags = [System.IO.File]::ReadAllText($flagsPath, [System.Text.UTF8Encoding]::new($false, $true))
        if ($flags -notmatch '(?m)^(?:C|CXX)_FLAGS\s*=') {
            throw "CPU worker baseline generated ggml-cpu C/C++ flags format is not recognized: $flagsPath"
        }
        if (Test-WindowsCpuWorkerBaselineUnsafeGeneratedFlags $flags) {
            throw "CPU worker baseline generated ggml-cpu C/C++ flags contain a native or higher ISA requirement: $flagsPath"
        }
    }
    elseif ($generator -match '^Visual Studio [0-9]+ [0-9]{4}$') {
        Assert-WindowsCpuWorkerBaselineVisualStudioFlags `
            (Join-Path $nativeBuildRoot 'ggml\src\ggml-cpu.vcxproj') `
            (Join-Path $nativeBuildRoot 'ggml\src\ggml-cpu.dir\Release\ggml-cpu.tlog\CL.command.1.tlog')
    }
    else {
        throw "CPU worker baseline does not recognize the generated CMake generator: $generator"
    }
}
