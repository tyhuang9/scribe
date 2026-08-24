param(
    [Parameter(Mandatory = $true)]
    [string]$BundlePath,
    [string]$PortableZipPath,
    [string]$InstallerPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Get-NormalizedPath([string]$Path) {
    return [System.IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
}

function Get-RelativeBundlePath([string]$Root, [string]$Path) {
    $rootUri = [System.Uri]::new((Get-NormalizedPath $Root) + [System.IO.Path]::DirectorySeparatorChar)
    $pathUri = [System.Uri]::new((Get-NormalizedPath $Path))
    return [System.Uri]::UnescapeDataString($rootUri.MakeRelativeUri($pathUri).ToString()).Replace('\', '/')
}

function Assert-Bundle([string]$Root) {
    $root = Get-NormalizedPath $Root
    $inventoryPath = Join-Path $root "bundle-inventory.json"
    if (-not (Test-Path -LiteralPath $inventoryPath -PathType Leaf)) {
        throw "Bundle inventory is missing from $root."
    }
    $inventory = Get-Content -LiteralPath $inventoryPath -Raw | ConvertFrom-Json
    if ($inventory.schema_version -ne 1 -or $inventory.platform_triple -ne "x86_64-pc-windows-msvc") {
        throw "Bundle inventory has an unexpected schema or platform."
    }
    $expected = @($inventory.files.path) + @("bundle-inventory.json") | Sort-Object
    $actual = @(Get-ChildItem -LiteralPath $root -Recurse -File -Force | ForEach-Object {
        Get-RelativeBundlePath $root $_.FullName
    } | Sort-Object)
    if ($expected.Count -ne $actual.Count -or (Compare-Object -ReferenceObject $expected -DifferenceObject $actual)) {
        throw "Release payload differs from its explicit inventory."
    }
    foreach ($entry in $inventory.files) {
        $path = Join-Path $root ($entry.path -replace '/', '\\')
        $item = Get-Item -LiteralPath $path -Force
        if ($item.Length -ne [int64]$entry.size_bytes) {
            throw "Bundle inventory size mismatch for $($entry.path)."
        }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($hash -ne $entry.sha256.ToLowerInvariant()) {
            throw "Bundle inventory SHA-256 mismatch for $($entry.path)."
        }
    }
}

$bundle = Get-NormalizedPath $BundlePath
Assert-Bundle $bundle

$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) "scribe-release-verification-$PID-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
try {
    if ($PortableZipPath) {
        if (-not (Test-Path -LiteralPath $PortableZipPath -PathType Leaf)) {
            throw "Portable ZIP is missing: $PortableZipPath"
        }
        $zipRoot = Join-Path $temporaryRoot "portable"
        Expand-Archive -LiteralPath $PortableZipPath -DestinationPath $zipRoot
        Assert-Bundle $zipRoot
    }

    if ($InstallerPath) {
        if (-not (Test-Path -LiteralPath $InstallerPath -PathType Leaf)) {
            throw "Windows installer is missing: $InstallerPath"
        }
        $installedRoot = Join-Path $temporaryRoot "installed"
        & $InstallerPath /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- "/DIR=$installedRoot"
        if ($LASTEXITCODE -ne 0) {
            throw "Silent installer verification failed with exit code $LASTEXITCODE."
        }
        Assert-Bundle $installedRoot
    }
}
finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}

Write-Output "Windows release payload inventory verification passed."
