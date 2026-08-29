[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$CatalogPath,
    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$catalogFile = [System.IO.Path]::GetFullPath($CatalogPath)
$catalogItem = Get-Item -LiteralPath $catalogFile -Force
if ($catalogItem.PSIsContainer -or
    ($catalogItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0 -or
    $catalogItem.Length -gt 4MB) {
    throw 'Worker-pack catalog must be a bounded regular non-reparse file.'
}
$catalog = Get-Content -LiteralPath $catalogFile -Raw | ConvertFrom-Json
if ($catalog.schema_version -ne 1 -or @($catalog.packs).Count -gt 8) {
    throw 'Worker-pack catalog has an unsupported schema or pack count.'
}
$reportPacks = foreach ($pack in @($catalog.packs)) {
    if ([string]$pack.pack_id -cnotmatch '^[a-z0-9][a-z0-9._-]{0,95}$' -or
        [string]$pack.pack_version -cnotmatch '^[a-z0-9][a-z0-9._-]{0,95}$' -or
        [string]$pack.pack_digest -cnotmatch '^[0-9a-f]{64}$' -or
        [string]$pack.backend -cnotmatch '^(cuda|vulkan)$' -or
        [int64]$pack.installed_size_bytes -lt 0 -or
        [int64]$pack.compressed_size_bytes -lt 0 -or
        @($pack.files).Count -gt 1024) {
        throw 'Worker-pack catalog contains invalid size-report evidence.'
    }
    Write-Host (
        'GPU worker pack {0} {1} ({2}): installed={3} compressed={4} files={5}' -f
        $pack.pack_id,
        $pack.pack_version,
        $pack.backend,
        $pack.installed_size_bytes,
        $pack.compressed_size_bytes,
        @($pack.files).Count
    )
    [ordered]@{
        pack_id = [string]$pack.pack_id
        pack_version = [string]$pack.pack_version
        pack_digest = [string]$pack.pack_digest
        backend = [string]$pack.backend
        installed_size_bytes = [int64]$pack.installed_size_bytes
        compressed_size_bytes = [int64]$pack.compressed_size_bytes
        file_count = [int]@($pack.files).Count
    }
}
$report = [ordered]@{
    schema_version = 1
    packs = @($reportPacks)
}
$destination = [System.IO.Path]::GetFullPath($OutputPath)
$parent = Split-Path -Parent $destination
if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
    New-Item -ItemType Directory -Path $parent -Force | Out-Null
}
[System.IO.File]::WriteAllText(
    $destination,
    ($report | ConvertTo-Json -Depth 5),
    [System.Text.UTF8Encoding]::new($false)
)
