[CmdletBinding()]
param(
    [string]$ManifestPath,
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Assert-Condition([bool]$Condition, [string]$Message) {
    if (-not $Condition) {
        throw $Message
    }
}

function Assert-ExactKeys([hashtable]$Object, [string[]]$Expected, [string]$Subject) {
    $actual = @($Object.Keys | Sort-Object)
    $expectedSorted = @($Expected | Sort-Object)
    Assert-Condition ($actual.Count -eq $expectedSorted.Count) "$Subject has unexpected or missing fields."
    for ($index = 0; $index -lt $expectedSorted.Count; $index++) {
        Assert-Condition ($actual[$index] -ceq $expectedSorted[$index]) "$Subject has unexpected or missing fields."
    }
}

function Assert-JsonString($Value, [string]$Subject) {
    Assert-Condition ($Value -is [string]) "$Subject must be a JSON string."
}

function Assert-JsonInteger($Value, [string]$Subject) {
    Assert-Condition ($Value -is [long]) "$Subject must be a JSON integer."
}

function Assert-JsonBoolean($Value, [string]$Subject) {
    Assert-Condition ($Value -is [bool]) "$Subject must be a JSON boolean."
}

function Test-Sha256([string]$Value) {
    return $Value -cmatch '^[0-9a-f]{64}$'
}

function Test-Identifier([string]$Value, [int]$MaximumLength) {
    return -not [string]::IsNullOrWhiteSpace($Value) -and
        $Value.Length -le $MaximumLength -and
        $Value -cmatch '^[a-z0-9._:-]+$'
}

function Test-StoreComponent([string]$Value) {
    return $Value.Length -le 96 -and $Value -cmatch '^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$'
}

function Test-DriverValue([string]$Value) {
    return $Value.Length -le 128 -and $Value -cmatch '^[\x20-\x7e]+$' -and -not $Value.Contains('\')
}

function Assert-Entry([hashtable]$Entry) {
    Assert-ExactKeys $Entry @(
        'pack', 'model_digest', 'backend', 'provider_id', 'vendor', 'device_class',
        'minimum_total_memory_bytes', 'driver', 'evidence'
    ) 'qualification entry'
    Assert-Condition ($Entry.pack -is [hashtable]) 'Qualification pack binding must be an object.'
    Assert-ExactKeys $Entry.pack @(
        'pack_id', 'pack_version', 'pack_digest', 'security_epoch', 'runtime_abi'
    ) 'qualification pack binding'
    foreach ($field in @('pack_id', 'pack_version', 'pack_digest')) {
        Assert-JsonString $Entry.pack[$field] "Qualification pack $field"
    }
    Assert-JsonInteger $Entry.pack.security_epoch 'Qualification pack security_epoch'
    Assert-JsonInteger $Entry.pack.runtime_abi 'Qualification pack runtime_abi'
    Assert-Condition (Test-StoreComponent $Entry.pack.pack_id) 'Qualification pack ID is not canonical.'
    Assert-Condition (Test-StoreComponent $Entry.pack.pack_version) 'Qualification pack version is not canonical.'
    Assert-Condition (Test-Sha256 $Entry.pack.pack_digest) 'Qualification pack digest is invalid.'
    Assert-Condition ($Entry.pack.security_epoch -gt 0) 'Qualification pack security epoch must be positive.'
    Assert-Condition ($Entry.pack.runtime_abi -gt 0 -and $Entry.pack.runtime_abi -le [uint16]::MaxValue) 'Qualification pack runtime ABI must be positive and bounded.'
    foreach ($field in @('model_digest', 'backend', 'provider_id', 'vendor', 'device_class')) {
        Assert-JsonString $Entry[$field] "Qualification $field"
    }
    Assert-Condition (Test-Sha256 $Entry.model_digest) 'Qualification model digest is invalid.'
    Assert-Condition (@('cuda', 'vulkan') -ccontains $Entry.backend) 'Qualification backend is unsupported for Windows Auto.'
    Assert-Condition (Test-Identifier $Entry.provider_id 128) 'Qualification provider identity is invalid.'
    Assert-Condition (@('nvidia', 'amd', 'intel') -ccontains $Entry.vendor) 'Qualification vendor is invalid.'
    Assert-Condition (@('discrete_gpu', 'integrated_gpu', 'unified_gpu') -ccontains $Entry.device_class) 'Qualification device class is invalid.'
    Assert-Condition (
        ($Entry.backend -ceq 'cuda' -and $Entry.provider_id -ceq 'transcribe-cpp-ggml-cuda' -and $Entry.vendor -ceq 'nvidia') -or
        ($Entry.backend -ceq 'vulkan' -and $Entry.provider_id -ceq 'transcribe-cpp-ggml-vulkan' -and @('nvidia', 'amd', 'intel') -ccontains $Entry.vendor)
    ) 'Qualification backend, provider, and vendor binding is invalid.'
    Assert-JsonInteger $Entry.minimum_total_memory_bytes 'Qualification minimum_total_memory_bytes'
    Assert-Condition ($Entry.minimum_total_memory_bytes -gt 0) 'Qualification minimum total memory must be positive.'
    Assert-Condition ($Entry.driver -is [hashtable]) 'Qualification driver constraint must be an object.'
    Assert-ExactKeys $Entry.driver @('kind', 'value') 'qualification driver constraint'
    Assert-JsonString $Entry.driver.kind 'Qualification driver constraint kind'
    Assert-JsonString $Entry.driver.value 'Qualification driver constraint value'
    Assert-Condition ($Entry.driver.kind -ceq 'exact') 'Qualification driver constraint kind must be exact for Stage 5.'
    Assert-Condition (Test-DriverValue $Entry.driver.value) 'Qualification driver constraint value is invalid.'
    Assert-Condition ($Entry.evidence -is [hashtable]) 'Qualification evidence must be an object.'
    Assert-ExactKeys $Entry.evidence @(
        'id', 'cold_runs', 'warm_runs', 'gpu_p95_ms', 'cpu_p95_ms',
        'correctness_verified', 'reliability_verified', 'cold_evidence_sha256',
        'warm_evidence_sha256', 'transcript_parity_evidence_sha256'
    ) 'qualification evidence'
    foreach ($field in @('id', 'cold_evidence_sha256', 'warm_evidence_sha256', 'transcript_parity_evidence_sha256')) {
        Assert-JsonString $Entry.evidence[$field] "Qualification evidence $field"
    }
    foreach ($field in @('cold_runs', 'warm_runs', 'gpu_p95_ms', 'cpu_p95_ms')) {
        Assert-JsonInteger $Entry.evidence[$field] "Qualification evidence $field"
    }
    Assert-JsonBoolean $Entry.evidence.correctness_verified 'Qualification correctness_verified'
    Assert-JsonBoolean $Entry.evidence.reliability_verified 'Qualification reliability_verified'
    Assert-Condition (Test-Identifier $Entry.evidence.id 160) 'Qualification evidence ID is invalid.'
    Assert-Condition ($Entry.evidence.cold_runs -ge 5 -and $Entry.evidence.cold_runs -le [uint16]::MaxValue) 'Qualification evidence requires at least five bounded cold runs.'
    Assert-Condition ($Entry.evidence.warm_runs -ge 20 -and $Entry.evidence.warm_runs -le [uint16]::MaxValue) 'Qualification evidence requires at least twenty bounded warm runs.'
    Assert-Condition ($Entry.evidence.gpu_p95_ms -gt 0 -and $Entry.evidence.cpu_p95_ms -gt 0) 'Qualification p95 values must be positive.'
    Assert-Condition (([decimal]$Entry.evidence.gpu_p95_ms * 100) -le ([decimal]$Entry.evidence.cpu_p95_ms * 110)) 'Qualification GPU p95 exceeds 110 percent of CPU p95.'
    Assert-Condition $Entry.evidence.correctness_verified 'Qualification correctness evidence is required.'
    Assert-Condition $Entry.evidence.reliability_verified 'Qualification reliability evidence is required.'
    foreach ($digest in @(
        $Entry.evidence.cold_evidence_sha256,
        $Entry.evidence.warm_evidence_sha256,
        $Entry.evidence.transcript_parity_evidence_sha256
    )) {
        Assert-Condition (Test-Sha256 $digest) 'Qualification evidence digest is invalid.'
    }
}

function ConvertTo-CanonicalQualificationJson([hashtable]$Manifest) {
    $entries = @(
        foreach ($entry in @($Manifest.entries)) {
            [ordered]@{
                pack = [ordered]@{
                    pack_id = $entry.pack.pack_id
                    pack_version = $entry.pack.pack_version
                    pack_digest = $entry.pack.pack_digest
                    security_epoch = $entry.pack.security_epoch
                    runtime_abi = $entry.pack.runtime_abi
                }
                model_digest = $entry.model_digest
                backend = $entry.backend
                provider_id = $entry.provider_id
                vendor = $entry.vendor
                device_class = $entry.device_class
                minimum_total_memory_bytes = $entry.minimum_total_memory_bytes
                driver = [ordered]@{
                    kind = $entry.driver.kind
                    value = $entry.driver.value
                }
                evidence = [ordered]@{
                    id = $entry.evidence.id
                    cold_runs = $entry.evidence.cold_runs
                    warm_runs = $entry.evidence.warm_runs
                    gpu_p95_ms = $entry.evidence.gpu_p95_ms
                    cpu_p95_ms = $entry.evidence.cpu_p95_ms
                    correctness_verified = $entry.evidence.correctness_verified
                    reliability_verified = $entry.evidence.reliability_verified
                    cold_evidence_sha256 = $entry.evidence.cold_evidence_sha256
                    warm_evidence_sha256 = $entry.evidence.warm_evidence_sha256
                    transcript_parity_evidence_sha256 = $entry.evidence.transcript_parity_evidence_sha256
                }
            }
        }
    )
    $canonical = [ordered]@{
        schema_version = $Manifest.schema_version
        mode = $Manifest.mode
        target_os = $Manifest.target_os
        target_arch = $Manifest.target_arch
        entries = $entries
    }
    return $canonical | ConvertTo-Json -Compress -Depth 16
}

$repositoryRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if ([string]::IsNullOrWhiteSpace($ManifestPath)) {
    $ManifestPath = Join-Path $repositoryRoot 'runtime-manifests\gpu-auto-qualification-windows-x64.json'
}
$manifestItem = Get-Item -LiteralPath $ManifestPath -Force
Assert-Condition (-not $manifestItem.PSIsContainer) 'GPU Auto qualification manifest must be a regular file.'
Assert-Condition (($manifestItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) 'GPU Auto qualification manifest must not be a reparse point.'
Assert-Condition ($manifestItem.Length -le 512KB) 'GPU Auto qualification manifest is oversized.'
$raw = [System.IO.File]::ReadAllText($manifestItem.FullName, [System.Text.UTF8Encoding]::new($false))
try {
    $manifest = $raw | ConvertFrom-Json -AsHashtable -Depth 16
}
catch {
    throw "GPU Auto qualification manifest is invalid JSON: $($_.Exception.Message)"
}
Assert-ExactKeys $manifest @('schema_version', 'mode', 'target_os', 'target_arch', 'entries') 'GPU Auto qualification manifest'
Assert-JsonInteger $manifest.schema_version 'GPU Auto qualification schema_version'
foreach ($field in @('mode', 'target_os', 'target_arch')) {
    Assert-JsonString $manifest[$field] "GPU Auto qualification $field"
}
Assert-Condition ($manifest.schema_version -eq 1) 'GPU Auto qualification schema_version must be 1.'
Assert-Condition ($manifest.mode -ceq 'default_deny') 'GPU Auto qualification mode must be default_deny.'
Assert-Condition ($manifest.target_os -ceq 'windows' -and $manifest.target_arch -ceq 'x86_64') 'GPU Auto qualification manifest must target Windows x64.'
Assert-Condition ($manifest.entries -is [System.Array]) 'GPU Auto qualification entries must be an array.'

$canonicalEntries = @()
foreach ($entry in @($manifest.entries)) {
    Assert-Condition ($entry -is [hashtable]) 'GPU Auto qualification entry must be an object.'
    Assert-Entry $entry
    $canonicalEntries += ($entry | ConvertTo-Json -Compress -Depth 16)
}
$documentInput = if ($raw.EndsWith("`n")) { $raw.Substring(0, $raw.Length - 1) } else { $raw }
$canonicalDocument = ConvertTo-CanonicalQualificationJson $manifest
Assert-Condition ($canonicalDocument -ceq $documentInput) 'GPU Auto qualification manifest is not canonical.'
for ($index = 1; $index -lt $canonicalEntries.Count; $index++) {
    Assert-Condition ($canonicalEntries[$index - 1] -clt $canonicalEntries[$index]) 'GPU Auto qualification entries must be strictly sorted and unique.'
}

$manifestDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $manifestItem.FullName).Hash.ToLowerInvariant()
$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('Scribe Windows GPU Auto qualification evidence')
$lines.Add("manifest_sha256: $manifestDigest")
$lines.Add("schema_version: $($manifest.schema_version)")
$lines.Add("mode: $($manifest.mode)")
$lines.Add("target: $($manifest.target_os)/$($manifest.target_arch)")
$lines.Add("qualified_entries: $($canonicalEntries.Count)")
if ($canonicalEntries.Count -eq 0) {
    $lines.Add('status: default-deny; no GPU backend is eligible for Auto')
}
else {
    foreach ($entry in @($manifest.entries)) {
        $lines.Add((
            'entry: backend={0} pack={1}@{2} digest={3} model={4} evidence={5} cold={6} warm={7} gpu_p95_ms={8} cpu_p95_ms={9}' -f `
            $entry.backend, $entry.pack.pack_id, $entry.pack.pack_version, $entry.pack.pack_digest,
            $entry.model_digest, $entry.evidence.id, $entry.evidence.cold_runs, $entry.evidence.warm_runs,
            $entry.evidence.gpu_p95_ms, $entry.evidence.cpu_p95_ms
        ))
    }
}
$report = $lines -join [Environment]::NewLine
Write-Output $report
if (-not [string]::IsNullOrWhiteSpace($OutputPath)) {
    $fullOutput = [System.IO.Path]::GetFullPath($OutputPath)
    $parent = Split-Path -Parent $fullOutput
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    [System.IO.File]::WriteAllText($fullOutput, $report + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
}
