[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Offline boundary test for the raw artifact acquisition step.  The resolver,
# its hashing, its staging file, and its HttpClient request construction are
# real; only the authenticated metadata result and HTTP transport are fake.
$repositoryRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$testRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-windows-ci-cpu-worker-download-$([guid]::NewGuid().ToString('N'))"
$script:Assertions = 0
$script:Utf8 = [Text.UTF8Encoding]::new($false)
$script:Handlers = [Collections.Generic.List[object]]::new()
$script:PreflightCalls = 0

$savedGhToken = [Environment]::GetEnvironmentVariable('GH_TOKEN')
$lastExitVariable = Get-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue
$hadSavedLastExitCode = $null -ne $lastExitVariable
$savedLastExitCode = if ($hadSavedLastExitCode) { [int]$lastExitVariable.Value } else { $null }

function Assert-Test([bool]$Condition, [string]$Message) {
    $script:Assertions++
    if (-not $Condition) { throw "TEST FAILED: $Message" }
}

function Assert-Equal($Actual, $Expected, [string]$Message) {
    $script:Assertions++
    if ($Actual -cne $Expected) {
        throw "TEST FAILED: $Message Expected '$Expected', got '$Actual'."
    }
}

function Assert-True([bool]$Value, [string]$Message) {
    $script:Assertions++
    if (-not $Value) { throw "TEST FAILED: $Message" }
}

function Assert-Rejected([string]$Name, [scriptblock]$Action) {
    $script:Assertions++
    try {
        $null = & $Action
    }
    catch {
        if ($_.Exception.Message.StartsWith('TEST FAILED:', [StringComparison]::Ordinal)) { throw }
        return
    }
    throw "TEST FAILED: $Name was accepted."
}

function Get-TestHash([byte[]]$Bytes) {
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($Bytes)).ToLowerInvariant()
}

function Write-TestBytes([string]$Path, [byte[]]$Bytes) {
    [IO.Directory]::CreateDirectory((Split-Path -Parent $Path)) | Out-Null
    [IO.File]::WriteAllBytes($Path, $Bytes)
}

function Remove-OwnedTestRoot([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $root = [IO.Path]::GetFullPath($Path).TrimEnd([char[]]@('\', '/'))
    $temp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd([char[]]@('\', '/'))
    Assert-Test ($root.StartsWith($temp + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -and
        (Split-Path -Leaf $root) -cmatch '^scribe-windows-ci-cpu-worker-download-[0-9a-f]{32}$') `
        'Refused download fixture cleanup outside its exact temporary root.'
    $current = $root
    while ($true) {
        $item = Get-Item -LiteralPath $current -Force
        Assert-Test (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused download fixture cleanup through a reparse point.'
        if ([string]::Equals($current, $temp, [StringComparison]::OrdinalIgnoreCase)) { break }
        $parent = Split-Path -Parent $current
        Assert-Test (-not [string]::IsNullOrWhiteSpace($parent) -and $parent -cne $current) 'Could not prove download fixture cleanup ancestry.'
        $current = $parent
    }
    foreach ($item in @(Get-ChildItem -LiteralPath $root -Recurse -Force)) {
        Assert-Test (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Refused download fixture cleanup containing a reparse point.'
    }
    [IO.Directory]::Delete("\\?\$root", $true)
}

if ($null -eq ('Scribe.CiCpuWorkerDownloadFixtureHandler' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;

namespace Scribe {
    public sealed class CiCpuWorkerDownloadFixtureHandler : HttpMessageHandler {
        public bool Authenticated;
        public int StatusCode;
        public string Location;
        public byte[] Body;
        public long DeclaredLength = -1;
        public int HeaderBytes;
        public bool HangRead;
        public bool HangingReadEntered;
        public bool TokenWasCancelable;
        public int CallCount;
        public string RequestUri;
        public string Authorization;

        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) {
            CallCount++;
            TokenWasCancelable = cancellationToken.CanBeCanceled;
            RequestUri = request.RequestUri.AbsoluteUri;
            Authorization = request.Headers.Authorization == null ? "" : request.Headers.Authorization.ToString();
            var response = new HttpResponseMessage((HttpStatusCode)StatusCode);
            if (Authenticated && Location != null) {
                response.Headers.Location = new Uri(Location, UriKind.Absolute);
            }
            if (!Authenticated) {
                response.Content = HangRead
                    ? (HttpContent)new CiCpuWorkerHangingContent(this)
                    : new ByteArrayContent(Body ?? Array.Empty<byte>());
                if (DeclaredLength >= 0) response.Content.Headers.ContentLength = DeclaredLength;
                if (HeaderBytes > 0) response.Headers.TryAddWithoutValidation("X-Fixture-Header", new String('x', HeaderBytes));
            }
            return Task.FromResult(response);
        }
    }

    public sealed class CiCpuWorkerHangingContent : HttpContent {
        private readonly CiCpuWorkerDownloadFixtureHandler handler;

        public CiCpuWorkerHangingContent(CiCpuWorkerDownloadFixtureHandler handler) {
            this.handler = handler;
        }

        protected override bool TryComputeLength(out long length) {
            length = handler.DeclaredLength;
            return length >= 0;
        }

        protected override Task SerializeToStreamAsync(Stream stream, TransportContext context) {
            return Task.CompletedTask;
        }

        protected override Task<Stream> CreateContentReadStreamAsync() {
            return Task.FromResult<Stream>(new CiCpuWorkerHangingReadStream(handler));
        }
    }

    public sealed class CiCpuWorkerHangingReadStream : Stream {
        private readonly CiCpuWorkerDownloadFixtureHandler handler;

        public CiCpuWorkerHangingReadStream(CiCpuWorkerDownloadFixtureHandler handler) {
            this.handler = handler;
        }

        public override bool CanRead { get { return true; } }
        public override bool CanSeek { get { return false; } }
        public override bool CanWrite { get { return false; } }
        public override long Length { get { throw new NotSupportedException(); } }
        public override long Position { get { throw new NotSupportedException(); } set { throw new NotSupportedException(); } }
        public override void Flush() { }
        public override long Seek(long offset, SeekOrigin origin) { throw new NotSupportedException(); }
        public override void SetLength(long value) { throw new NotSupportedException(); }
        public override void Write(byte[] buffer, int offset, int count) { throw new NotSupportedException(); }

        public override int Read(byte[] buffer, int offset, int count) {
            handler.HangingReadEntered = true;
            throw new NotSupportedException("Fixture requires the cancellation-aware ReadAsync overload.");
        }

        public override Task<int> ReadAsync(byte[] buffer, int offset, int count, CancellationToken cancellationToken) {
            handler.HangingReadEntered = true;
            if (cancellationToken.IsCancellationRequested) {
                return Task.FromCanceled<int>(cancellationToken);
            }
            var completion = new TaskCompletionSource<int>(TaskCreationOptions.RunContinuationsAsynchronously);
            var registration = cancellationToken.Register(() => completion.TrySetCanceled(cancellationToken));
            return AwaitCancellation(completion.Task, registration);
        }

        private static async Task<int> AwaitCancellation(Task<int> task, CancellationTokenRegistration registration) {
            try {
                return await task.ConfigureAwait(false);
            }
            finally {
                registration.Dispose();
            }
        }
    }
}
'@
}

. (Join-Path $PSScriptRoot 'resolve-windows-cpu-worker-inputs.ps1') -FunctionsOnly

$savedPreflight = ${function:Invoke-WindowsCpuWorkerInputPreflight}
$savedFactory = ${function:New-WindowsCpuWorkerInputHttpClient}
$savedDeadline = $script:WindowsCpuInputDownloadDeadline

function New-DownloadFixture {
    $bytes = $script:Utf8.GetBytes("offline CI CPU worker ZIP bytes $([guid]::NewGuid().ToString('N'))")
    $script:Fixture = [ordered]@{
        SourceRevision = 'b' * 40
        WorkerSourceRevision = 'a' * 40
        ProducerRunId = '8001'
        ProducerRunAttempt = '2'
        ArtifactId = '8101'
        Bytes = $bytes
        Digest = Get-TestHash $bytes
        Size = [int64]$bytes.Length
        ArchivePath = Join-Path $testRoot "worker-$([guid]::NewGuid().ToString('N')).zip"
        RedirectLocation = 'https://fixture-download.example.invalid/signed/worker.zip?signature=credential-free'
        ApiStatus = [int][Net.HttpStatusCode]::Found
        DownloadStatus = [int][Net.HttpStatusCode]::OK
        ContentLength = [int64]$bytes.Length
        HeaderBytes = 0
        HangRead = $false
        PreflightDigest = $null
        PreflightSize = $null
        PreflightTupleMutation = $null
        CreateDestinationDuringDownload = $false
    }
    $script:Handlers.Clear()
    $script:PreflightCalls = 0
}

function New-TestPreflightResult {
    $f = $script:Fixture
    $digest = if ($null -ne $f.PreflightDigest) { [string]$f.PreflightDigest } else { [string]$f.Digest }
    $size = if ($null -ne $f.PreflightSize) { [int64]$f.PreflightSize } else { [int64]$f.Size }
    $source = $f.SourceRevision
    $worker = $f.WorkerSourceRevision
    $run = $f.ProducerRunId
    $attempt = $f.ProducerRunAttempt
    $artifact = $f.ArtifactId
    if ($null -ne $f.PreflightTupleMutation) {
        switch ([string]$f.PreflightTupleMutation) {
            'source' { $source = 'c' * 40 }
            'worker' { $worker = 'c' * 40 }
            'run' { $run = '8002' }
            'attempt' { $attempt = '3' }
            'artifact' { $artifact = '8102' }
            default { throw "Unknown preflight tuple mutation: $($f.PreflightTupleMutation)" }
        }
    }
    return [pscustomobject]@{
        SourceRevision = $source
        WorkerSourceRevision = $worker
        ProducerRunId = $run
        ProducerRunAttempt = $attempt
        ArtifactId = $artifact
        ArtifactSha256 = $digest
        ArtifactSizeBytes = $size
        RepositoryRoot = $null
        Context = $null
    }
}

function Invoke-WindowsCpuWorkerInputPreflight {
    param(
        [string]$SourceRevision,
        [string]$WorkerSourceRevision,
        [string]$ProducerRunId,
        [string]$ProducerRunAttempt,
        [string]$ArtifactId
    )
    $script:PreflightCalls++
    Assert-Equal $SourceRevision $script:Fixture.SourceRevision 'Download did not pin the installer source revision.'
    Assert-Equal $WorkerSourceRevision $script:Fixture.WorkerSourceRevision 'Download did not pin the worker source revision.'
    Assert-Equal $ProducerRunId $script:Fixture.ProducerRunId 'Download did not pin the producer run.'
    Assert-Equal $ProducerRunAttempt $script:Fixture.ProducerRunAttempt 'Download did not pin the producer attempt.'
    Assert-Equal $ArtifactId $script:Fixture.ArtifactId 'Download did not pin the artifact ID.'
    return New-TestPreflightResult
}

function New-WindowsCpuWorkerInputHttpClient([bool]$Authenticated) {
    $f = $script:Fixture
    if (-not $Authenticated -and $f.CreateDestinationDuringDownload) {
        Write-TestBytes $f.ArchivePath $script:Utf8.GetBytes('late destination collision')
    }
    $handler = [Scribe.CiCpuWorkerDownloadFixtureHandler]::new()
    $handler.Authenticated = $Authenticated
    $handler.StatusCode = if ($Authenticated) { $f.ApiStatus } else { $f.DownloadStatus }
    $handler.Location = if ($Authenticated) { $f.RedirectLocation } else { $null }
    $handler.Body = if ($Authenticated) { $null } else { [byte[]]$f.Bytes }
    $handler.DeclaredLength = if ($Authenticated) { -1 } else { [int64]$f.ContentLength }
    $handler.HeaderBytes = if ($Authenticated) { 0 } else { [int]$f.HeaderBytes }
    $handler.HangRead = if ($Authenticated) { $false } else { [bool]$f.HangRead }
    $client = [Net.Http.HttpClient]::new($handler, $true)
    $client.Timeout = [TimeSpan]::FromSeconds(10)
    if ($Authenticated) {
        $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', 'fixture-gh-token')
    }
    $script:Handlers.Add($handler)
    return $client
}

function Invoke-TestDownload {
    return Invoke-WindowsCpuWorkerInputDownload `
        -SourceRevision $script:Fixture.SourceRevision `
        -WorkerSourceRevision $script:Fixture.WorkerSourceRevision `
        -ProducerRunId $script:Fixture.ProducerRunId `
        -ProducerRunAttempt $script:Fixture.ProducerRunAttempt `
        -ArtifactId $script:Fixture.ArtifactId `
        -ExpectedArtifactSha256 $script:Fixture.Digest `
        -ArchivePath $script:Fixture.ArchivePath
}

try {
    [IO.Directory]::CreateDirectory($testRoot) | Out-Null
    $env:GH_TOKEN = 'fixture-gh-token'
    $global:LASTEXITCODE = 73

    New-DownloadFixture
    $download = Invoke-TestDownload
    Assert-Equal $script:PreflightCalls 1 'Download did not perform exactly one independent preflight.'
    Assert-Equal $script:Handlers.Count 2 'Download did not use isolated API and signed-location clients.'
    $api = $script:Handlers[0]
    $signed = $script:Handlers[1]
    Assert-Equal $api.CallCount 1 'Authenticated GitHub artifact endpoint was not requested exactly once.'
    Assert-Equal $api.RequestUri 'https://api.github.com/repos/tyhuang9/scribe/actions/artifacts/8101/zip' 'Download did not use the fixed GitHub artifact ZIP endpoint.'
    Assert-Equal $api.Authorization 'Bearer fixture-gh-token' 'Authenticated GitHub artifact endpoint did not receive the fixture token.'
    Assert-True $api.TokenWasCancelable 'Authenticated artifact request did not receive the bounded deadline token.'
    Assert-Equal $signed.CallCount 1 'Signed artifact location was not requested exactly once.'
    Assert-Equal $signed.RequestUri $script:Fixture.RedirectLocation 'Signed artifact request did not use the exact one-hop redirect location.'
    Assert-Equal $signed.Authorization '' 'Credential leaked from GitHub API request to signed artifact location.'
    Assert-True $signed.TokenWasCancelable 'Signed artifact request did not receive the bounded deadline token.'
    Assert-Equal $download.ArchivePath ([IO.Path]::GetFullPath($script:Fixture.ArchivePath)) 'Download returned an unexpected archive path.'
    Assert-Equal $download.ArtifactSha256 $script:Fixture.Digest 'Download returned a changed digest.'
    Assert-Equal ([IO.File]::ReadAllBytes($script:Fixture.ArchivePath).Length) $script:Fixture.Bytes.Length 'Download changed raw archive bytes.'
    Assert-Equal (Get-TestHash ([IO.File]::ReadAllBytes($script:Fixture.ArchivePath))) $script:Fixture.Digest 'Download did not write the expected raw archive digest.'

    foreach ($case in @(
        [pscustomobject]@{ Name = 'artifact endpoint status'; Change = { $script:Fixture.ApiStatus = [int][Net.HttpStatusCode]::OK } },
        [pscustomobject]@{ Name = 'non-HTTPS signed redirect'; Change = { $script:Fixture.RedirectLocation = 'http://fixture-download.example.invalid/worker.zip' } },
        [pscustomobject]@{ Name = 'signed redirect user info'; Change = { $script:Fixture.RedirectLocation = 'https://token@fixture-download.example.invalid/worker.zip' } },
        [pscustomobject]@{ Name = 'second redirect'; Change = { $script:Fixture.DownloadStatus = [int][Net.HttpStatusCode]::Found } },
        [pscustomobject]@{ Name = 'download status'; Change = { $script:Fixture.DownloadStatus = [int][Net.HttpStatusCode]::Forbidden } },
        [pscustomobject]@{ Name = 'declared length mismatch'; Change = { $script:Fixture.ContentLength = [int64]($script:Fixture.Size + 1) } },
        [pscustomobject]@{ Name = 'truncated stream'; Change = { $script:Fixture.Bytes = $script:Fixture.Bytes[0..($script:Fixture.Bytes.Length - 2)]; $script:Fixture.ContentLength = [int64]$script:Fixture.Size } },
        [pscustomobject]@{ Name = 'oversized stream'; Change = { $script:Fixture.Bytes = $script:Fixture.Bytes + [byte[]](0x99); $script:Fixture.ContentLength = [int64]$script:Fixture.Size } },
        [pscustomobject]@{ Name = 'digest mismatch'; Change = { $script:Fixture.Bytes[0] = $script:Fixture.Bytes[0] -bxor 0xFF } },
        [pscustomobject]@{ Name = 'oversized headers'; Change = { $script:Fixture.HeaderBytes = 70000 } },
        [pscustomobject]@{ Name = 'preflight digest drift'; Change = { $script:Fixture.PreflightDigest = '0' * 64 } },
        [pscustomobject]@{ Name = 'preflight size drift'; Change = { $script:Fixture.PreflightSize = [int64]($script:Fixture.Size + 1) } },
        [pscustomobject]@{ Name = 'preflight source tuple drift'; Change = { $script:Fixture.PreflightTupleMutation = 'source' } },
        [pscustomobject]@{ Name = 'preflight worker tuple drift'; Change = { $script:Fixture.PreflightTupleMutation = 'worker' } },
        [pscustomobject]@{ Name = 'preflight run tuple drift'; Change = { $script:Fixture.PreflightTupleMutation = 'run' } },
        [pscustomobject]@{ Name = 'preflight attempt tuple drift'; Change = { $script:Fixture.PreflightTupleMutation = 'attempt' } },
        [pscustomobject]@{ Name = 'preflight artifact tuple drift'; Change = { $script:Fixture.PreflightTupleMutation = 'artifact' } },
        [pscustomobject]@{ Name = 'late archive destination race'; Change = { $script:Fixture.CreateDestinationDuringDownload = $true } }
    )) {
        New-DownloadFixture
        & $case.Change
        Assert-Rejected $case.Name { Invoke-TestDownload }
        Assert-Test (-not (Test-Path -LiteralPath $script:Fixture.ArchivePath) -or $script:Fixture.CreateDestinationDuringDownload) "$($case.Name) left an unexpected final archive."
        $staging = @(Get-ChildItem -LiteralPath $testRoot -Force | Where-Object { $_.Name -like ".$(Split-Path -Leaf $script:Fixture.ArchivePath).download-*" })
        Assert-Equal $staging.Count 0 "$($case.Name) left an owned download staging file."
    }

    New-DownloadFixture
    $script:Fixture.HangRead = $true
    $fixtureDeadline = $script:WindowsCpuInputDownloadDeadline
    try {
        # The stream completes only from the resolver's cancellation token; no
        # network request or fixture sleep is involved in the deadline test.
        $script:WindowsCpuInputDownloadDeadline = [TimeSpan]::FromMilliseconds(250)
        Assert-Rejected 'hanging signed download stream deadline' { Invoke-TestDownload }
        Assert-Equal $script:Handlers.Count 2 'Hanging stream did not reach the signed download client.'
        Assert-True $script:Handlers[1].HangingReadEntered 'Hanging stream deadline did not enter the cancellation-aware ReadAsync call.'
        Assert-True (-not (Test-Path -LiteralPath $script:Fixture.ArchivePath)) 'Hanging stream deadline published a final archive.'
        $staging = @(Get-ChildItem -LiteralPath $testRoot -Force | Where-Object { $_.Name -like ".$(Split-Path -Leaf $script:Fixture.ArchivePath).download-*" })
        Assert-Equal $staging.Count 0 'Hanging stream deadline left an owned download staging file.'
    }
    finally {
        $script:WindowsCpuInputDownloadDeadline = $fixtureDeadline
    }

    New-DownloadFixture
    Write-TestBytes $script:Fixture.ArchivePath $script:Utf8.GetBytes('preserve destination')
    Assert-Rejected 'existing raw archive destination' { Invoke-TestDownload }
    Assert-Equal ([Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($script:Fixture.ArchivePath))) 'preserve destination' 'Rejected destination collision changed existing archive bytes.'

    # Entry-mode validation happens before any HTTP client is constructed. This
    # proves Download has no resolve-only output directory escape hatch.
    Assert-Rejected 'Download mode with output directory' {
        & (Join-Path $PSScriptRoot 'resolve-windows-cpu-worker-inputs.ps1') `
            -Mode Download -SourceRevision ('b' * 40) -WorkerSourceRevision ('a' * 40) `
            -ProducerRunId 8001 -ProducerRunAttempt 2 -ArtifactId 8101 `
            -ExpectedArtifactSha256 ('0' * 64) -ArchivePath (Join-Path $testRoot 'entry-mode.zip') `
            -OutputDirectory (Join-Path $testRoot 'must-not-be-accepted')
    }

    Assert-Test ($script:Assertions -ge 55) 'CI CPU worker download fixture discovery coverage unexpectedly shrank.'
}
finally {
    Set-Item -Path Function:Invoke-WindowsCpuWorkerInputPreflight -Value $savedPreflight
    Set-Item -Path Function:New-WindowsCpuWorkerInputHttpClient -Value $savedFactory
    $script:WindowsCpuInputDownloadDeadline = $savedDeadline
    [Environment]::SetEnvironmentVariable('GH_TOKEN', $savedGhToken)
    if ($hadSavedLastExitCode) { $global:LASTEXITCODE = $savedLastExitCode } else { Remove-Variable -Name LASTEXITCODE -Scope Global -ErrorAction SilentlyContinue }
    Remove-OwnedTestRoot $testRoot
}

Write-Output "Windows CI CPU worker download tests passed ($script:Assertions assertions)."
