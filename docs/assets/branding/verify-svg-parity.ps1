[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$canonicalPath = Join-Path $repositoryRoot 'assets\branding\scribe-mark.svg'
$canonicalXml = [xml](Get-Content -Raw -LiteralPath $canonicalPath)

function Get-BarGeometry([xml]$document) {
    @($document.SelectNodes('//*[local-name()="g"]')) |
        Where-Object { $_.GetAttribute('fill') -in '#2D979C', '#7CCBC9', '#ACDBD9' } |
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
$canonicalPathNode = $canonicalXml.SelectSingleNode('//*[local-name()="path"]')
$targets = @(
    @{ Path = 'assets\branding\scribe-lockup-light.svg'; Primary = '#2D979C' },
    @{ Path = 'assets\branding\scribe-lockup-dark.svg'; Primary = '#7CCBC9' },
    @{ Path = 'docs\assets\branding\scribe-mark.svg'; Primary = '#2D979C' },
    @{ Path = 'website\public\brand\scribe-mark.svg'; Primary = '#2D979C' },
    @{ Path = 'docs\assets\branding\scribe-header-light.svg'; Primary = '#2D979C' },
    @{ Path = 'website\src\assets\scribe-header-light.svg'; Primary = '#2D979C' },
    @{ Path = 'docs\assets\branding\scribe-header-dark.svg'; Primary = '#7CCBC9' },
    @{ Path = 'website\src\assets\scribe-header-dark.svg'; Primary = '#7CCBC9' },
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

    $groups = @($document.SelectNodes('//*[local-name()="g"]'))
    $primaryBars = @($groups | Where-Object { $_.fill -eq $target.Primary } | ForEach-Object { @($_.rect) })
    $secondaryBars = @($groups | Where-Object { $_.fill -eq '#ACDBD9' } | ForEach-Object { @($_.rect) })
    if ($primaryBars.Count -ne 5 -or $secondaryBars.Count -ne 2) {
        throw "$($target.Path) must contain five primary bars and two Soft Aqua bars."
    }
    Assert-Equal @($secondaryBars | Sort-Object { [double]$_.x } | ForEach-Object x) @('30', '87') "$($target.Path) Soft Aqua bars must be the outer-adjacent pair."

    $pathNode = $document.SelectSingleNode('//*[local-name()="path"]')
    foreach ($attribute in 'd', 'stroke-width', 'stroke-linecap', 'stroke-linejoin') {
        if ($pathNode.$attribute -ne $canonicalPathNode.$attribute) {
            throw "$($target.Path) S-path $attribute differs from the canonical mark."
        }
    }
}

$tagline = 'Lightning-fast local transcription that stays out of your way.'
$canonicalLockupContracts = @(
    @{
        Path = 'assets\branding\scribe-lockup-light.svg'
        RequiredFills = @('#fff', '#08233A', '#2D979C')
    },
    @{
        Path = 'assets\branding\scribe-lockup-dark.svg'
        RequiredFills = @('#061C2E', '#08233A', '#EAF5F5', '#7CCBC9')
    }
)

foreach ($contract in $canonicalLockupContracts) {
    $lockupPath = Join-Path $repositoryRoot $contract.Path
    $rawLockup = Get-Content -Raw -LiteralPath $lockupPath
    $lockupXml = [xml]$rawLockup
    $title = $lockupXml.SelectSingleNode('//*[local-name()="title"]')
    $description = $lockupXml.SelectSingleNode('//*[local-name()="desc"]')
    $visibleText = @($lockupXml.SelectNodes('//*[local-name()="text"]') | ForEach-Object InnerText)
    $expectedText = @('scribe', 'Lightning-fast local transcription', 'that stays out of your way.')

    if ($title.InnerText -ne 'scribe' -or $description.InnerText -notlike "*$tagline*") {
        throw "$($contract.Path) must expose the lowercase wordmark and exact tagline to assistive technology."
    }
    if (($visibleText -join '|') -ne ($expectedText -join '|')) {
        throw "$($contract.Path) must visibly contain the lowercase wordmark and exact two-line tagline."
    }
    foreach ($fill in $contract.RequiredFills) {
        if (-not $rawLockup.Contains("fill=`"$fill`"")) {
            throw "$($contract.Path) is missing required theme fill $fill."
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
Assert-HashSet @(
    'docs\assets\branding\scribe-header-light.svg',
    'website\src\assets\scribe-header-light.svg'
) 'Light compact header'
Assert-HashSet @(
    'docs\assets\branding\scribe-header-dark.svg',
    'website\src\assets\scribe-header-dark.svg'
) 'Dark compact header'

foreach ($headerPath in @(
    'docs\assets\branding\scribe-header-light.svg',
    'website\src\assets\scribe-header-light.svg',
    'docs\assets\branding\scribe-header-dark.svg',
    'website\src\assets\scribe-header-dark.svg'
)) {
    $headerXml = [xml](Get-Content -Raw -LiteralPath (Join-Path $repositoryRoot $headerPath))
    $visibleText = @($headerXml.SelectNodes('//*[local-name()="text"]'))
    if ($visibleText.Count -ne 1 -or $visibleText[0].InnerText -ne 'scribe') {
        throw "$headerPath must contain only the lowercase wordmark and no tagline."
    }
}

Write-Output "Brand SVG parity verified for $($targets.Count) documentation and website assets."
