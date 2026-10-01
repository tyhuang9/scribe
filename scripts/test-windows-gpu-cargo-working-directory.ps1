[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Get-RecordLines([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return @() }
    return @([IO.File]::ReadAllLines($Path, [Text.UTF8Encoding]::new($false)))
}

function Assert-CallerLocations([string]$ExpectedPowerShell, [string]$ExpectedDotNet) {
    Assert-True (
        (Get-Location).Path -ceq $ExpectedPowerShell
    ) 'Bounded native-process invocation changed the caller PowerShell location.'
    Assert-True (
        [Environment]::CurrentDirectory -ceq $ExpectedDotNet
    ) 'Bounded native-process invocation changed the caller .NET current directory.'
}

$repositoryRoot = (Get-Item -LiteralPath (Join-Path $PSScriptRoot '..') -Force).FullName.TrimEnd([char[]]@('\', '/'))
$bootstrapScript = Join-Path $PSScriptRoot 'windows-gpu-worker-cmake-bootstrap.ps1'
$builderScript = Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'
$runnerScript = Join-Path $PSScriptRoot 'run-windows-vulkan-evidence.ps1'
. $bootstrapScript

$builderTokens = $null
$builderParseErrors = $null
$builderAst = [System.Management.Automation.Language.Parser]::ParseFile(
    $builderScript,
    [ref]$builderTokens,
    [ref]$builderParseErrors
)
Assert-True ($builderParseErrors.Count -eq 0) 'Windows GPU worker-pack builder could not be parsed.'
$builderNativeProcessFunctions = @($builderAst.FindAll({
    param($Ast)
    $Ast -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $Ast.Name -ceq 'Invoke-NativeProcess'
}, $true))
Assert-True ($builderNativeProcessFunctions.Count -eq 1) 'Windows GPU worker-pack builder has an ambiguous native-process wrapper.'
$builderNativeProcessFunction = $builderNativeProcessFunctions[0]

$runnerTokens = $null
$runnerParseErrors = $null
$runnerAst = [System.Management.Automation.Language.Parser]::ParseFile(
    $runnerScript,
    [ref]$runnerTokens,
    [ref]$runnerParseErrors
)
Assert-True ($runnerParseErrors.Count -eq 0) 'Windows Vulkan evidence runner could not be parsed.'
$runnerRetryFunction = $runnerAst.Find({
    param($Ast)
    $Ast -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
    $Ast.Name -ceq 'Invoke-ScribeEvidenceCargoWithCmakeRetry'
}, $true)
Assert-True ($null -ne $runnerRetryFunction) 'Windows Vulkan evidence runner lost its Cargo retry function.'
Assert-True (
    @($runnerAst.FindAll({
        param($Ast)
        $Ast -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $Ast.Name -ceq 'Invoke-ScribeEvidenceCargoWithCmakeRetry'
    }, $true)).Count -eq 1
) 'Windows Vulkan evidence runner has an ambiguous Cargo retry function.'
Assert-True (
    @($runnerRetryFunction.Body.FindAll({
        param($Ast)
        $Ast -is [System.Management.Automation.Language.CommandAst] -and
        $Ast.GetCommandName() -ceq 'Invoke-ScribeGpuWorkerBoundedNativeProcess' -and
        $Ast.Extent.Text.Contains('-WorkingDirectory $repositoryRoot')
    }, $true)).Count -eq 2
) 'Both evidence Cargo attempts must use the verified repository-root working directory.'

# Evaluate only the actual retry function extracted above. This avoids running
# the evidence runner or its GPU/SDK preflight while exercising its real retry
# control flow against a harmless PowerShell child.
Invoke-Expression $runnerRetryFunction.Extent.Text

$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-gpu-cargo-working-directory-$([guid]::NewGuid().ToString('N'))"
$previousPowerShellLocation = (Get-Location).Path
$previousDotNetDirectory = [Environment]::CurrentDirectory
$environmentNames = @(
    'SCRIBE_GPU_CARGO_WD_RECORD',
    'SCRIBE_GPU_CARGO_WD_COUNTER',
    'SCRIBE_GPU_CARGO_WD_TARGET',
    'SCRIBE_GPU_CARGO_WD_MODE'
)
$previousEnvironment = @{}
foreach ($name in $environmentNames) {
    $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}

try {
    $source = Join-Path $fixtureRoot 'source'
    $outsidePowerShell = Join-Path $fixtureRoot 'outside-powershell'
    $outsideDotNet = Join-Path $fixtureRoot 'outside-dotnet'
    $target = Join-Path $fixtureRoot 'cargo-target'
    $buildEnvironment = Join-Path $fixtureRoot 'build-environment'
    New-Item -ItemType Directory -Path $source, $outsidePowerShell, $outsideDotNet, $target, $buildEnvironment -Force | Out-Null
    [IO.File]::WriteAllText(
        (Join-Path $source 'source-relative-sentinel.txt'),
        'source-relative-sentinel',
        [Text.UTF8Encoding]::new($false)
    )

    $child = Join-Path $fixtureRoot 'fake-cargo.ps1'
    [IO.File]::WriteAllText($child, @'
param([switch]$RequireSourceSentinel, [switch]$RetryContract)
$cwd = (Get-Location).Path.TrimEnd([char[]]@('\', '/'))
$sentinel = Join-Path $cwd 'source-relative-sentinel.txt'
if ($RequireSourceSentinel -and -not (Test-Path -LiteralPath $sentinel -PathType Leaf)) {
    [Console]::Error.WriteLine('relative source sentinel is unavailable')
    exit 71
}
$sentinelValue = if ($RequireSourceSentinel) { [IO.File]::ReadAllText($sentinel) } else { 'omitted' }
[IO.File]::AppendAllText($env:SCRIBE_GPU_CARGO_WD_RECORD, ("{0}|{1}" -f $cwd, $sentinelValue) + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
if (-not $RetryContract) { exit 0 }
$attempt = 0
if (Test-Path -LiteralPath $env:SCRIBE_GPU_CARGO_WD_COUNTER -PathType Leaf) {
    $attempt = [int][IO.File]::ReadAllText($env:SCRIBE_GPU_CARGO_WD_COUNTER)
}
$attempt++
[IO.File]::WriteAllText($env:SCRIBE_GPU_CARGO_WD_COUNTER, [string]$attempt, [Text.UTF8Encoding]::new($false))
if ($attempt -eq 1) {
    $hash = '0123456789abcdef'
    $warning = Join-Path $env:SCRIBE_GPU_CARGO_WD_TARGET "release\build\transcribe-cpp-sys-$hash\out\build\e\src\vulkan-shaders-gen-build\CMakeFiles\CMakeScratch\TryCompile-Ab12Cd\CMakeLists.txt"
    [Console]::Out.WriteLine('error: failed to run custom build command for `transcribe-cpp-sys v0.1.3`')
    [Console]::Out.WriteLine("CMake Warning in ${warning}:")
    [Console]::Out.WriteLine('  characters (see CMAKE_OBJECT_PATH_MAX). Object file')
    [Console]::Out.WriteLine("LINK : fatal error LNK1104: cannot open file 'CMakeFiles\cmTC_1a2B3c.dir/intermediate.manifest'")
    exit 17
}
if ($env:SCRIBE_GPU_CARGO_WD_MODE -ceq 'retry-failure') {
    [Console]::Error.WriteLine('sensitive retry child diagnostic')
    exit 19
}
exit 0
'@, [Text.UTF8Encoding]::new($false))

    $pwsh = Join-Path $PSHOME 'pwsh.exe'
    Assert-True (Test-Path -LiteralPath $pwsh -PathType Leaf) 'PowerShell test child is unavailable.'
    $directRecord = Join-Path $fixtureRoot 'direct-record.txt'
    $env:SCRIBE_GPU_CARGO_WD_RECORD = $directRecord
    Set-Location $outsidePowerShell
    [Environment]::CurrentDirectory = $outsideDotNet
    $expectedPowerShellLocation = (Get-Location).Path
    $expectedDotNetDirectory = [Environment]::CurrentDirectory
    Assert-True ($expectedPowerShellLocation -cne $expectedDotNetDirectory) 'Working-directory fixture did not separate caller PowerShell and .NET locations.'

    $null = Invoke-ScribeGpuWorkerBoundedNativeProcess `
        $pwsh `
        @('-NoProfile', '-File', $child, '-RequireSourceSentinel') `
        'Accepted working-directory child failed.' `
        -WorkingDirectory $source
    Assert-CallerLocations $expectedPowerShellLocation $expectedDotNetDirectory
    $acceptedLines = @(Get-RecordLines $directRecord)
    Assert-True ($acceptedLines.Count -eq 1) 'Accepted working-directory child did not execute exactly once.'
    Assert-True (
        $acceptedLines[0] -ceq ((Get-Item -LiteralPath $source -Force).FullName.TrimEnd([char[]]@('\', '/')) + '|source-relative-sentinel')
    ) 'Accepted working-directory child did not run at the physical source root or read its relative sentinel.'

    $null = Invoke-ScribeGpuWorkerBoundedNativeProcess `
        $pwsh `
        @('-NoProfile', '-File', $child) `
        'Omitted working-directory child failed.'
    Assert-CallerLocations $expectedPowerShellLocation $expectedDotNetDirectory
    $omittedLines = @(Get-RecordLines $directRecord)
    Assert-True ($omittedLines.Count -eq 2) 'Omitted working-directory compatibility child did not execute exactly once.'
    Assert-True (
        $omittedLines[1] -ceq ($expectedDotNetDirectory.TrimEnd([char[]]@('\', '/')) + '|omitted')
    ) 'Omitted working-directory invocation no longer inherits the caller .NET current directory.'

    $wrapperRecord = Join-Path $fixtureRoot 'builder-wrapper-record.txt'
    $env:SCRIBE_GPU_CARGO_WD_RECORD = $wrapperRecord
    & {
        param(
            [string]$WrapperSource,
            [string]$Executable,
            [string]$Child,
            [string]$Source,
            [string]$ExpectedPowerShell,
            [string]$ExpectedDotNet
        )
        # This scope receives only the exact AST-extracted builder wrapper. It
        # resolves the already imported real bounded helper, so both calls are
        # real child launches rather than a test stub.
        Invoke-Expression $WrapperSource
        $null = Invoke-NativeProcess `
            $Executable `
            @('-NoProfile', '-File', $Child, '-RequireSourceSentinel') `
            'Builder wrapper supplied working-directory child failed.' `
            -WorkingDirectory $Source
        Assert-CallerLocations $ExpectedPowerShell $ExpectedDotNet
        $null = Invoke-NativeProcess `
            $Executable `
            @('-NoProfile', '-File', $Child) `
            'Builder wrapper omitted working-directory child failed.'
        Assert-CallerLocations $ExpectedPowerShell $ExpectedDotNet
    } $builderNativeProcessFunction.Extent.Text `
        $pwsh `
        $child `
        $source `
        $expectedPowerShellLocation `
        $expectedDotNetDirectory
    Assert-CallerLocations $expectedPowerShellLocation $expectedDotNetDirectory
    $wrapperLines = @(Get-RecordLines $wrapperRecord)
    Assert-True ($wrapperLines.Count -eq 2) 'Actual builder native-process wrapper did not execute its supplied and omitted children exactly once.'
    Assert-True (
        $wrapperLines[0] -ceq ((Get-Item -LiteralPath $source -Force).FullName.TrimEnd([char[]]@('\', '/')) + '|source-relative-sentinel')
    ) 'Actual builder native-process wrapper did not forward its supplied source working directory.'
    Assert-True (
        $wrapperLines[1] -ceq ($expectedDotNetDirectory.TrimEnd([char[]]@('\', '/')) + '|omitted')
    ) 'Actual builder native-process wrapper changed omitted working-directory inheritance.'
    $env:SCRIBE_GPU_CARGO_WD_RECORD = $directRecord

    $fileWorkingDirectory = Join-Path $fixtureRoot 'not-a-directory.txt'
    [IO.File]::WriteAllText($fileWorkingDirectory, 'not a directory', [Text.UTF8Encoding]::new($false))
    $junctionWorkingDirectory = Join-Path $fixtureRoot 'source-junction'
    New-Item -ItemType Junction -Path $junctionWorkingDirectory -Target $source | Out-Null
    $reparseTarget = Join-Path $fixtureRoot 'reparse-target'
    New-Item -ItemType Directory -Path (Join-Path $reparseTarget 'child') -Force | Out-Null
    $reparseAncestor = Join-Path $fixtureRoot 'reparse-ancestor'
    New-Item -ItemType Junction -Path $reparseAncestor -Target $reparseTarget | Out-Null
    $invalidWorkingDirectories = @(
        [pscustomobject]@{ Label = 'empty'; Value = '' },
        [pscustomobject]@{ Label = 'null'; Value = $null },
        [pscustomobject]@{ Label = 'relative'; Value = '.' },
        [pscustomobject]@{ Label = 'missing'; Value = (Join-Path $fixtureRoot 'missing') },
        [pscustomobject]@{ Label = 'file'; Value = $fileWorkingDirectory },
        [pscustomobject]@{ Label = 'junction'; Value = $junctionWorkingDirectory },
        [pscustomobject]@{ Label = 'reparse ancestor'; Value = (Join-Path $reparseAncestor 'child') }
    )
    foreach ($invalid in $invalidWorkingDirectories) {
        $before = @(Get-RecordLines $directRecord).Count
        $rejected = $false
        try {
            $invoke = @{
                Executable = $pwsh
                # Do not let a missing source sentinel mask an accidental launch:
                # this child always records execution, even outside the source.
                Arguments = @('-NoProfile', '-File', $child)
                FailureMessage = "Invalid $($invalid.Label) working-directory child unexpectedly started."
                WorkingDirectory = $invalid.Value
            }
            $null = Invoke-ScribeGpuWorkerBoundedNativeProcess @invoke
        }
        catch {
            $rejected = $true
        }
        Assert-True $rejected "Explicit $($invalid.Label) working directory was accepted."
        Assert-True (@(Get-RecordLines $directRecord).Count -eq $before) "Explicit $($invalid.Label) working directory launched the child before rejection."
        Assert-CallerLocations $expectedPowerShellLocation $expectedDotNetDirectory
    }

    $crateHash = '0123456789abcdef'
    $outDirectory = Join-Path $target "release\build\transcribe-cpp-sys-$crateHash\out"
    New-Item -ItemType Directory -Path $outDirectory -Force | Out-Null
    $tcs = Join-Path $buildEnvironment 'tcs'
    New-Item -ItemType Directory -Path $tcs -Force | Out-Null
    New-Item -ItemType Junction -Path (Join-Path $tcs 'a8206a5e1a40df03') -Target $outDirectory | Out-Null

    $retryRecord = Join-Path $fixtureRoot 'retry-record.txt'
    $retryCounter = Join-Path $fixtureRoot 'retry-counter.txt'
    $env:SCRIBE_GPU_CARGO_WD_RECORD = $retryRecord
    $env:SCRIBE_GPU_CARGO_WD_COUNTER = $retryCounter
    $env:SCRIBE_GPU_CARGO_WD_TARGET = $target
    $env:SCRIBE_GPU_CARGO_WD_MODE = 'success'
    $cargo = $pwsh
    $repositoryRoot = $source
    [int]$bootstrapInvocations = 0
    function Enable-ScribeEvidenceCmakeBootstrap([string]$CargoTarget, [string]$BuildEnvironment) {
        $script:bootstrapInvocations++
    }
    Invoke-ScribeEvidenceCargoWithCmakeRetry `
        @('-NoProfile', '-File', $child, '-RequireSourceSentinel', '-RetryContract') `
        'Fake Cargo failed.' `
        $target `
        $buildEnvironment
    $retryLines = @(Get-RecordLines $retryRecord)
    Assert-True ($bootstrapInvocations -eq 1) 'Evidence Cargo retry did not perform exactly one bounded bootstrap mutation.'
    Assert-True ($retryLines.Count -eq 2) 'Evidence Cargo retry did not launch exactly the initial and retry children.'
    $expectedRetryLine = (Get-Item -LiteralPath $source -Force).FullName.TrimEnd([char[]]@('\', '/')) + '|source-relative-sentinel'
    Assert-True (@($retryLines | Where-Object { $_ -cne $expectedRetryLine }).Count -eq 0) 'Evidence Cargo initial or retry child lost the verified source working directory or relative sentinel.'
    Assert-CallerLocations $expectedPowerShellLocation $expectedDotNetDirectory

    Remove-Item -LiteralPath $retryRecord, $retryCounter -Force -ErrorAction SilentlyContinue
    $env:SCRIBE_GPU_CARGO_WD_MODE = 'retry-failure'
    [int]$bootstrapInvocations = 0
    $retryFailure = $null
    try {
        Invoke-ScribeEvidenceCargoWithCmakeRetry `
            @('-NoProfile', '-File', $child, '-RequireSourceSentinel', '-RetryContract') `
            'Fake Cargo failed.' `
            $target `
            $buildEnvironment
    }
    catch {
        $retryFailure = $_.Exception
    }
    Assert-True ($null -ne $retryFailure -and
        $retryFailure.Message -ceq 'Fake Cargo failed. after validated CMake bootstrap retry.' -and
        -not $retryFailure.Message.Contains('sensitive retry child diagnostic')) 'Evidence retry failure lost its bounded sanitized error.'
    Assert-True ($bootstrapInvocations -eq 1) 'Evidence retry failure performed more than one bootstrap mutation.'
    Assert-True (@(Get-RecordLines $retryRecord).Count -eq 2) 'Evidence retry failure did not remain bounded to two child launches.'
    Assert-CallerLocations $expectedPowerShellLocation $expectedDotNetDirectory
}
finally {
    Set-Location $previousPowerShellLocation
    [Environment]::CurrentDirectory = $previousDotNetDirectory
    foreach ($name in $environmentNames) {
        [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], 'Process')
    }
    if (Test-Path -LiteralPath $fixtureRoot) {
        $fixtureItem = Get-ScribeGpuWorkerPhysicalDirectory $fixtureRoot 'Owned working-directory test fixture'
        Assert-True ($fixtureItem.FullName -ceq $fixtureRoot) 'Owned working-directory fixture identity changed before cleanup.'
        Remove-Item -LiteralPath $fixtureItem.FullName -Recurse -Force
        Assert-True (-not (Test-Path -LiteralPath $fixtureRoot)) 'Owned working-directory fixture survived cleanup.'
    }
}

Write-Output 'Windows GPU Cargo working-directory tests passed.'
