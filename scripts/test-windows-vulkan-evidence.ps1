$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1')

if ((ConvertTo-ScribeVulkanEvidencePci 'native:0000:01:00.0') -cne '0000:01:00.0') { throw 'Native PCI parsing regressed.' }
if ((ConvertTo-ScribeVulkanEvidencePci '00000000:01:00.0') -cne '0000:01:00.0') { throw 'nvidia-smi PCI parsing regressed.' }
foreach ($value in @('native:0000:01:00.8', 'native:0000:01:00.0 ', 'uuid:secret')) {
    $accepted = $false
    try { $null = ConvertTo-ScribeVulkanEvidencePci $value; $accepted = $true } catch {}
    if ($accepted) { throw "Malformed PCI identity was accepted: $value" }
}
$runner = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'run-windows-vulkan-evidence.ps1') -Raw
foreach ($required in @('--locked', '--offline', '-SigningMode Fixture', '--ignored', '--exact', '--test-threads=1', '--no-run', 'onnx_worker::tests::windows_vulkan_fixture_evidence_captures_five_cold_and_twenty_warm_runs', 'gpu-auto-qualification-windows-x64.json', 'Production signing/release input is forbidden', 'Resolve-ScribeEvidenceFreshDirectory', 'Evidence output may not be under source')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing required source contract: $required" }
}
foreach ($required in @('Get-FileHash -LiteralPath $model', 'Get-FileHash -LiteralPath $wav', 'Test-ScribeEvidenceActivationPath', 'Test-ScribeEvidenceWithin', 'New-ScribeEvidenceShortCargoTarget', 'Assert-ScribeEvidenceSingleLinkFile', 'Assert-ScribeVulkanEvidenceTrustedNvidiaSmi', 'fixture-evidence-$($revision.Substring(0, 12))-$([guid]::NewGuid()', 'Fixture-only untrusted Vulkan evidence', 'previousEvidenceEnvironment')) {
    if ($runner -notmatch [regex]::Escape($required)) { throw "Runner is missing required safety contract: $required" }
}
$compileAt = $runner.IndexOf("'--no-run'")
$baselineAt = $runner.IndexOf('Get-ScribeVulkanEvidenceNvidiaBaseline')
if ($compileAt -lt 0 -or $baselineAt -lt 0 -or $compileAt -ge $baselineAt) { throw 'Runner must precompile before NVIDIA baseline capture.' }
if ($runner -match 'SigningMode Production|ProductionPrivateKeyPath|ProductionKeyId') { throw 'Fixture runner references a production signing input.' }
$preflight = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1') -Raw
foreach ($required in @('nvidia-smi.exe', 'matching.Count -ne 1', '$utilization -gt 10', '$usedMib -gt ($totalMib / 4)', 'pci.bus_id')) {
    if ($preflight -notmatch [regex]::Escape($required)) { throw "Preflight is missing required source contract: $required" }
}
Write-Output 'Windows Vulkan fixture-evidence script contracts passed.'
