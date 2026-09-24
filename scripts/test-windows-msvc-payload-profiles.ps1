[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$tokens = $null; $parseErrors = $null
$builder = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'build-windows-gpu-worker-pack.ps1'), [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'GPU pack builder does not parse.' }
# Load only the production matcher and schema helpers, never the builder body.
foreach ($name in @('Assert-ExactProperties', 'Assert-MsvcPayloadProfileContract', 'Resolve-PinnedMsvcPayloadProfile')) {
    $functions = @($builder.FindAll({
        param($node)
        $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name
    }, $false))
    if ($functions.Count -ne 1) { throw "Expected one production $name definition." }
    . ([scriptblock]::Create($functions[0].Extent.Text))
}
$profiles = @((Get-Content -LiteralPath (Join-Path $repositoryRoot 'runtime-manifests/gpu-worker-toolchain-windows-x64.json') -Raw | ConvertFrom-Json).msvc.payload_profiles)
$script:ProfileTestCount = 0
function Assert-ProfileTest([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
    $script:ProfileTestCount++
}
function Copy-ProfileValue($Value) {
    return ($Value | ConvertTo-Json -Depth 8 -Compress | ConvertFrom-Json)
}
# The sole physical-file seam: no installed compiler, native process or network.
function Get-PinnedMsvcToolIdentity([string]$Path, [string]$ExpectedFilename, [string]$Label) {
    $name = [IO.Path]::GetFileNameWithoutExtension($ExpectedFilename)
    $tool = $script:ObservedTools.$name
    return [pscustomobject]@{ Path = $Path; Filename = $tool.filename; FileVersion = $tool.file_version; Sha256 = $tool.sha256 }
}
function Assert-ProfileRejected([object[]]$Candidates, [string]$Label) {
    $rejected = $false
    try { Resolve-PinnedMsvcPayloadProfile $PSScriptRoot $Candidates | Out-Null } catch { $rejected = $true }
    Assert-ProfileTest $rejected "MSVC matcher accepted $Label."
}
foreach ($required in @('msvc-14.44.35227-local', 'msvc-14.44.35228-hosted', 'msvc-14.44.35229-hosted')) {
    Assert-ProfileTest (@($profiles | Where-Object profile_id -CEQ $required).Count -eq 1) "Missing or duplicate reviewed profile: $required."
}
foreach ($profile in $profiles) {
    $script:ObservedTools = Copy-ProfileValue $profile.tools
    $resolved = Resolve-PinnedMsvcPayloadProfile $PSScriptRoot $profiles
    Assert-ProfileTest ($resolved.ProfileId -ceq $profile.profile_id) 'Exact reviewed compiler set did not resolve uniquely.'
    foreach ($tool in @('cl', 'link', 'lib', 'nmake')) {
        foreach ($field in @('filename', 'file_version', 'sha256')) {
            $script:ObservedTools = Copy-ProfileValue $profile.tools
            $script:ObservedTools.$tool.$field = if ($field -ceq 'sha256') { '0' * 64 } else { 'unapproved' }
            Assert-ProfileRejected $profiles "changed $tool $field"
        }
        foreach ($other in @($profiles | Where-Object profile_id -CNE $profile.profile_id)) {
            $script:ObservedTools = Copy-ProfileValue $profile.tools
            $script:ObservedTools.$tool = Copy-ProfileValue $other.tools.$tool
            Assert-ProfileRejected $profiles "mixed $tool from $($other.profile_id)"
        }
    }
}
$script:ObservedTools = Copy-ProfileValue $profiles[0].tools
Assert-ProfileRejected @() 'empty profiles'
Assert-ProfileRejected (@($profiles[0]) * 9) 'excessive profile count'
Assert-ProfileRejected @($profiles[0], $profiles[0]) 'duplicate profile IDs'
$ambiguous = Copy-ProfileValue $profiles[0]
$ambiguous.profile_id = 'duplicate-payload-different-id'
Assert-ProfileRejected @($profiles[0], $ambiguous) 'ambiguous matching payloads'
$malformed = Copy-ProfileValue $profiles[0]
$malformed.tools.PSObject.Properties.Remove('nmake')
Assert-ProfileRejected @($malformed) 'missing required tool'
Assert-ProfileTest ($script:ProfileTestCount -ge 71) 'Expected MSVC profile tests were not discovered.'
Write-Output "Windows MSVC payload profile tests passed ($script:ProfileTestCount cases; offline, no compiler execution)."
