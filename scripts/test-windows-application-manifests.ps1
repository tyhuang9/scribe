[CmdletBinding()]
param([string[]]$ExecutablePath = @())

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-application-manifest.ps1')
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$canonical = [IO.File]::ReadAllBytes((Join-Path $repositoryRoot 'resources/windows/application.manifest'))
$canonicalText = [Text.Encoding]::UTF8.GetString($canonical)
$script:ManifestAssertions = 0
function Assert-ManifestAccepted([byte[]]$Bytes) {
    Assert-WindowsLongPathAwareManifestXml $Bytes
    $script:ManifestAssertions++
}
function Assert-ManifestRejected([scriptblock]$Action, [string]$Label, [string]$Expected = '') {
    try { & $Action }
    catch {
        if ($Expected -and -not $_.Exception.Message.Contains($Expected)) {
            throw "$Label failed for the wrong reason: $($_.Exception.Message)"
        }
        $script:ManifestAssertions++
        return
    }
    throw "Manifest gate accepted $Label."
}
Assert-ManifestAccepted $canonical
Assert-ManifestAccepted ([byte[]](@(0xEF,0xBB,0xBF) + $canonical))
Assert-ManifestAccepted ([Text.Encoding]::UTF8.GetBytes($canonicalText.Replace('>true<', '> true <')))
foreach ($replacement in @(
    @('>true<', '>false<'), @('>true<', '>TRUE<'), @('>true<', '><'),
    @('level="asInvoker"', 'level="requireAdministrator"'),
    @('level="asInvoker"', 'level="highestAvailable"'),
    @('uiAccess="false"', 'uiAccess="true"'), @(' uiAccess="false"', ''),
    @('manifestVersion="1.0"', 'manifestVersion="2.0"'),
    @('http://schemas.microsoft.com/SMI/2016/WindowsSettings', 'urn:wrong'),
    @('<longPathAware ', '<longPathAware unexpected="true" '),
    @('</windowsSettings>', '<longPathAware xmlns="urn:wrong">true</longPathAware></windowsSettings>'),
    @('</requestedPrivileges>', '<requestedExecutionLevel level="asInvoker" uiAccess="false" /></requestedPrivileges>'),
    @('</assembly>', '<dependency /></assembly>'),
    @('encoding="UTF-8"', 'encoding="UTF-16"'),
    @('</assembly>', ''),
    @('?>', '?><!DOCTYPE assembly [<!ENTITY external SYSTEM "file:///must-not-read">]>'),
    @('?>', '?><?unreviewed instruction?>')
)) {
    $bytes = [Text.Encoding]::UTF8.GetBytes($canonicalText.Replace($replacement[0], $replacement[1]))
    Assert-ManifestRejected { Assert-WindowsLongPathAwareManifestXml $bytes } ($replacement -join ' -> ')
}
Assert-ManifestRejected { Assert-WindowsLongPathAwareManifestXml ([byte[]]@()) } 'empty XML'
Assert-ManifestRejected { Assert-WindowsLongPathAwareManifestXml ([byte[]]@(0xFF)) } 'invalid UTF-8'
Assert-ManifestRejected { Assert-WindowsLongPathAwareManifestXml ([byte[]]::new(65537)) } 'oversized XML'
Assert-ManifestRejected { Get-WindowsApplicationManifestBytes 'relative.exe' } 'relative executable' 'absolute executable path'

# These tiny resource-only PE fixtures contain no executable code or imports.
# They exercise the Windows resource loader, not just our static PE parser.
function Write-ManifestResourceFixture(
    [string]$Path, [byte[]]$Payload, [uint32]$ResourceId = 1,
    [uint16]$Language = 1033, [switch]$ExtraName, [switch]$StringName,
    [switch]$ExtraLanguage, [uint32]$ResourceType = 24
) {
    $rawSize = [int]([Math]::Ceiling((512 + $Payload.Length) / 512.0) * 512)
    $bytes = [byte[]]::new(512 + $rawSize)
    function Set-U16([int]$Offset, [uint16]$Value) { [BitConverter]::GetBytes($Value).CopyTo($bytes,$Offset) }
    function Set-U32([int]$Offset, [uint32]$Value) { [BitConverter]::GetBytes($Value).CopyTo($bytes,$Offset) }
    $bytes[0] = 0x4D; $bytes[1] = 0x5A
    Set-U32 0x3C 0x80; Set-U32 0x80 0x00004550
    Set-U16 0x84 0x8664; Set-U16 0x86 1; Set-U16 0x94 0xF0; Set-U16 0x96 0x22
    $optional = 0x98
    Set-U16 $optional 0x20B
    [BitConverter]::GetBytes([uint64]0x140000000).CopyTo($bytes,$optional+24)
    Set-U32 ($optional+32) 0x1000; Set-U32 ($optional+36) 0x200
    Set-U16 ($optional+40) 6; Set-U16 ($optional+48) 6
    Set-U32 ($optional+56) ([uint32]([Math]::Ceiling((4096+$rawSize)/4096.0)*4096))
    Set-U32 ($optional+60) 0x200; Set-U16 ($optional+68) 3; Set-U32 ($optional+108) 16
    Set-U32 ($optional+128) 0x1000; Set-U32 ($optional+132) (512+$Payload.Length)
    [Text.Encoding]::ASCII.GetBytes('.rsrc').CopyTo($bytes,0x188)
    Set-U32 0x190 $rawSize; Set-U32 0x194 0x1000; Set-U32 0x198 $rawSize
    Set-U32 0x19C 0x200; Set-U32 0x1AC 0x40000040
    $root = 0x200
    Set-U16 ($root+14) 1; Set-U32 ($root+16) $ResourceType; Set-U32 ($root+20) 0x80000020L
    if ($StringName) { Set-U16 ($root+0x2C) 1 } else { Set-U16 ($root+0x2E) $(if ($ExtraName) { 2 } else { 1 }) }
    Set-U32 ($root+0x30) $(if ($StringName) { 0x80000180L } else { $ResourceId })
    Set-U32 ($root+0x34) 0x80000060L
    if ($ExtraName) { Set-U32 ($root+0x38) 2; Set-U32 ($root+0x3C) 0x80000090L }
    Set-U16 ($root+0x6E) $(if ($ExtraLanguage) { 2 } else { 1 })
    Set-U32 ($root+0x70) $Language; Set-U32 ($root+0x74) 0xC0
    if ($ExtraLanguage) { Set-U32 ($root+0x78) 1041; Set-U32 ($root+0x7C) 0xD0 }
    if ($ExtraName) { Set-U16 ($root+0x9E) 1; Set-U32 ($root+0xA0) $Language; Set-U32 ($root+0xA4) 0xE0 }
    foreach ($offset in @(0xC0,0xD0,0xE0)) { Set-U32 ($root+$offset) 0x1200; Set-U32 ($root+$offset+4) $Payload.Length }
    if ($StringName) { Set-U16 ($root+0x180) 8; [Text.Encoding]::Unicode.GetBytes('manifest').CopyTo($bytes,$root+0x182) }
    $Payload.CopyTo($bytes,0x400)
    [IO.File]::WriteAllBytes($Path,$bytes)
}
$fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ('scribe-manifest-test-' + [guid]::NewGuid().ToString('N'))
try {
    $null = New-Item -ItemType Directory -Path $fixtureRoot
    foreach ($language in @(0,1031,1033)) {
        $path = Join-Path $fixtureRoot "valid-$language.exe"
        Write-ManifestResourceFixture $path $canonical -Language $language
        $before = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        Assert-WindowsLongPathAwareApplicationManifest $path
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -cne $before) { throw 'Resource inspection modified fixture bytes.' }
        $script:ManifestAssertions++
    }
    foreach ($case in @(
        @{ Name='xml-false'; Payload=[Text.Encoding]::UTF8.GetBytes($canonicalText.Replace('>true<', '>false<')); Expected='longPathAware=true' },
        @{ Name='xml-elevated'; Payload=[Text.Encoding]::UTF8.GetBytes($canonicalText.Replace('level="asInvoker"', 'level="requireAdministrator"')); Expected='asInvoker' },
        @{ Name='wrong-id'; ResourceId=2; Expected='integer ID 1' },
        @{ Name='wrong-type'; ResourceType=23; Expected='integer ID 1' },
        @{ Name='extra-name'; ExtraName=$true; Expected='integer ID 1' },
        @{ Name='string-name'; StringName=$true; Expected='integer ID 1' },
        @{ Name='extra-language'; ExtraLanguage=$true; Expected='one embedded application-manifest language' },
        @{ Name='empty'; Payload=[byte[]]@(); Expected='1..65536 bytes' },
        @{ Name='oversize'; Payload=[byte[]]::new(65537); Expected='1..65536 bytes' }
    )) {
        $parameters = @{} + $case
        $label = $parameters.Name; $parameters.Remove('Name')
        $expected = $parameters.Expected; $parameters.Remove('Expected')
        if (-not $parameters.ContainsKey('Payload')) { $parameters.Payload = $canonical }
        $path = Join-Path $fixtureRoot "$label.exe"
        Write-ManifestResourceFixture -Path $path @parameters
        Assert-ManifestRejected { Assert-WindowsLongPathAwareApplicationManifest $path } $label $expected
    }
    $invalid = Join-Path $fixtureRoot 'invalid.exe'
    [IO.File]::WriteAllBytes($invalid,[byte[]]@(0x4D,0x5A))
    Assert-ManifestRejected { Assert-WindowsLongPathAwareApplicationManifest $invalid } 'invalid PE' 'Cannot map application resources'
}
finally {
    $resolved = [IO.Path]::GetFullPath($fixtureRoot)
    $temporary = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\','/')) + [IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($temporary,[StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -cnotmatch '^scribe-manifest-test-[0-9a-f]{32}$') { throw 'Unsafe manifest fixture cleanup path.' }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}
if ($script:ManifestAssertions -ne 37) { throw "Expected 37 manifest assertions, discovered $script:ManifestAssertions." }
foreach ($path in $ExecutablePath) {
    $before = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    Assert-WindowsLongPathAwareApplicationManifest $path
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -cne $before) { throw 'Resource inspection changed executable bytes.' }
}
Write-Output "Windows application manifest tests passed ($script:ManifestAssertions assertions; $($ExecutablePath.Count) supplied executable resources; no executable launched)."
