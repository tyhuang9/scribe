[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$canonicalPath = Join-Path $repositoryRoot 'assets\branding\scribe-mark.svg'
$canonicalXml = [xml](Get-Content -Raw -LiteralPath $canonicalPath)

function Get-BarGeometry([xml]$document) {
    @($document.svg.g) |
        ForEach-Object { @($_.rect) } |
        Sort-Object { [double]$_.x } |
        ForEach-Object { '{0},{1},{2},{3},{4}' -f $_.x, $_.y, $_.width, $_.height, $_.rx }
}

function Assert-Equal($actual, $expected, [string]$message) {
    if (Compare-Object -ReferenceObject @($expected) -DifferenceObject @($actual)) {
        throw $message
    }
}

function Assert-HashSet([string[]]$relativePaths, [string]$label) {
    $hashes = $relativePaths | ForEach-Object {
        (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $repositoryRoot $_)).Hash
    } | Select-Object -Unique
    if (@($hashes).Count -ne 1) {
        throw "$label SVG copies are not byte-identical."
    }
}

$canonicalGeometry = Get-BarGeometry $canonicalXml
$canonicalPathNode = @($canonicalXml.svg.path)[0]
$targets = @(
    @{ Path = 'docs\assets\branding\scribe-mark.svg'; Primary = '#2D979C' },
    @{ Path = 'website\public\brand\scribe-mark.svg'; Primary = '#2D979C' },
    @{ Path = 'docs\assets\branding\scribe-lockup-light.svg'; Primary = '#2D979C' },
    @{ Path = 'website\public\brand\scribe-lockup-light.svg'; Primary = '#2D979C' },
    @{ Path = 'website\src\assets\scribe-lockup-light.svg'; Primary = '#2D979C' },
    @{ Path = 'docs\assets\branding\scribe-lockup-dark.svg'; Primary = '#7CCBC9' },
    @{ Path = 'website\public\brand\scribe-lockup-dark.svg'; Primary = '#7CCBC9' },
    @{ Path = 'website\src\assets\scribe-lockup-dark.svg'; Primary = '#7CCBC9' },
    @{ Path = 'website\public\favicon.svg'; Primary = '#7CCBC9' }
)

foreach ($target in $targets) {
    $path = Join-Path $repositoryRoot $target.Path
    $document = [xml](Get-Content -Raw -LiteralPath $path)
    Assert-Equal (Get-BarGeometry $document) $canonicalGeometry "$($target.Path) bar geometry differs from the canonical mark."

    $groups = @($document.svg.g)
    $primaryBars = @($groups | Where-Object { $_.fill -eq $target.Primary } | ForEach-Object { @($_.rect) })
    $secondaryBars = @($groups | Where-Object { $_.fill -eq '#ACDBD9' } | ForEach-Object { @($_.rect) })
    if ($primaryBars.Count -ne 5 -or $secondaryBars.Count -ne 2) {
        throw "$($target.Path) must contain five primary bars and two Soft Aqua bars."
    }
    Assert-Equal @($secondaryBars | Sort-Object { [double]$_.x } | ForEach-Object x) @('30', '87') "$($target.Path) Soft Aqua bars must be the outer-adjacent pair."

    $pathNode = @($document.svg.path)[0]
    foreach ($attribute in 'd', 'stroke-width', 'stroke-linecap', 'stroke-linejoin') {
        if ($pathNode.$attribute -ne $canonicalPathNode.$attribute) {
            throw "$($target.Path) S-path $attribute differs from the canonical mark."
        }
    }
}

Assert-HashSet @(
    'assets\branding\scribe-mark.svg',
    'docs\assets\branding\scribe-mark.svg',
    'website\public\brand\scribe-mark.svg'
) 'Canonical mark'
Assert-HashSet @(
    'docs\assets\branding\scribe-lockup-light.svg',
    'website\public\brand\scribe-lockup-light.svg',
    'website\src\assets\scribe-lockup-light.svg'
) 'Light lockup'
Assert-HashSet @(
    'docs\assets\branding\scribe-lockup-dark.svg',
    'website\public\brand\scribe-lockup-dark.svg',
    'website\src\assets\scribe-lockup-dark.svg'
) 'Dark lockup'

Write-Output "Brand SVG parity verified for $($targets.Count) documentation and website assets."
