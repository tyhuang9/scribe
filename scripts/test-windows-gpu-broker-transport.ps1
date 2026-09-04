param(
    [switch]$RequireScmIntegration
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$serviceName = 'ScribeGpuPromotionBroker'
$pipeName = 'ScribeGpuPromotionBroker.v1'
$serviceSid = 'S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifest = Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\Cargo.toml'
$targetRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-gpu-broker-transport-$([guid]::NewGuid().ToString('N'))"
$previousCargoTarget = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process')
$createdService = $false
$machineTarget = $null

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Invoke-Process {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @(),
        [ValidateRange(1, 3600)][int]$TimeoutSeconds = 300,
        [switch]$AllowFailure
    )
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $FilePath
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $repositoryRoot
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::Start($start)
    if ($null -eq $process) { throw "Failed to start $FilePath." }
    try {
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            $process.Kill($true)
            $process.WaitForExit()
            throw "$FilePath did not exit within $TimeoutSeconds seconds."
        }
        $stdout = $stdoutTask.GetAwaiter().GetResult()
        $stderr = $stderrTask.GetAwaiter().GetResult()
        $result = [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdout
            Stderr = $stderr
        }
    }
    finally { $process.Dispose() }
    if (-not $AllowFailure -and $result.ExitCode -ne 0) {
        throw "$FilePath failed with exit $($result.ExitCode): $($result.Stderr)"
    }
    return $result
}

function Invoke-Sc([string[]]$Arguments, [switch]$AllowFailure) {
    return Invoke-Process -FilePath (Join-Path $env:SystemRoot 'System32\sc.exe') -Arguments $Arguments -TimeoutSeconds 30 -AllowFailure:$AllowFailure
}

function Get-BrokerService {
    return Get-Service -Name $serviceName -ErrorAction SilentlyContinue
}

function Test-RestrictedServiceSidType([string]$ScOutput) {
    $sidTypeMatches = [regex]::Matches(
        $ScOutput,
        '(?m)^\s*SERVICE_SID_TYPE\s*:\s*(?<value>\S+)\s*$',
        [Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    return $sidTypeMatches.Count -eq 1 -and $sidTypeMatches[0].Groups['value'].Value -ceq 'RESTRICTED'
}

function Assert-OwnedBrokerService([string]$ExpectedPath) {
    $config = Get-CimInstance -ClassName Win32_Service -Filter "Name='$serviceName'"
    Assert-True ($null -ne $config) 'Temporary service configuration is unavailable.'
    Assert-True ($config.StartName -ceq 'NT AUTHORITY\LocalService') 'Service account is not LocalService.'
    Assert-True ($config.ServiceType -ceq 'Own Process') 'Service is not configured as an own-process service.'
    Assert-True ([IO.Path]::GetFullPath($config.PathName.Trim('"')) -ceq [IO.Path]::GetFullPath($ExpectedPath)) 'SCM service path no longer matches the protected freshly built binary; refusing destructive cleanup.'
    $queriedSidType = Invoke-Sc -Arguments @('qsidtype', $serviceName)
    Assert-True (Test-RestrictedServiceSidType -ScOutput $queriedSidType.Stdout) 'SCM no longer reports the restricted service SID type; refusing destructive cleanup.'
    return $config
}

function Wait-ServiceAbsent([int]$TimeoutSeconds) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        if ($null -eq (Get-BrokerService)) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Service $serviceName was not deleted within $TimeoutSeconds seconds."
}

function New-ValidClientArguments([string]$HandoffRoot, [string]$OutputRoot) {
    return @(
        'promote-windows-pack-set',
        '--handoff-root', $HandoffRoot,
        '--output-root', $OutputRoot,
        '--source-repository', 'tyhuang9/scribe',
        '--source-ref', 'refs/heads/main',
        '--source-revision', ('a' * 40),
        '--workflow-ref', 'tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main',
        '--workflow-source-sha', ('a' * 40),
        '--run-id', '1001',
        '--run-attempt', '1',
        '--artifact-id', '2002',
        '--artifact-digest', ('b' * 64),
        '--handoff-sha256', ('c' * 64),
        '--release-set-digest', ('d' * 64),
        '--toolchain-manifest-sha256', ('e' * 64),
        '--pack-version', '0.1.0-transport-fixture',
        '--minimum-security-epoch', '1',
        '--require-unused-release-set'
    )
}

try {
    $goldenRequest = '{"schema_version":1,"command":"promote-windows-pack-set","client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","promotion_intent_sha256":"bf7b4002065dcc87c6d7abd70899c76a23c880c82e1869c4ba2bdbf39dcebe3c","intent":{"schema_version":1,"policy_namespace":"scribe-windows-gpu-production-v1","source_repository":"owner/repo","source_ref":"refs/heads/main","source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","workflow_ref":"owner/repo/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main","workflow_source_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","run_id":"123","run_attempt":"1","artifact_id":"456","artifact_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","handoff_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","release_set_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","toolchain_manifest_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","pack_version":"0.1.0","minimum_security_epoch":1,"require_unused_release_set":true}}'
    $goldenMaterial = [Text.Encoding]::UTF8.GetBytes("scribe-windows-gpu-promotion-request-v1`0$goldenRequest")
    $goldenDigest = ([BitConverter]::ToString([Security.Cryptography.SHA256]::HashData($goldenMaterial))).Replace('-', '').ToLowerInvariant()
    Assert-True ($goldenDigest -ceq '3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083') 'PowerShell and Rust disagree on the canonical broker request digest.'
    $goldenResponse = '{"schema_version":1,"client_nonce":"1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a","promotion_intent_sha256":"bf7b4002065dcc87c6d7abd70899c76a23c880c82e1869c4ba2bdbf39dcebe3c","request_sha256":"3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083","outcome":{"status":"not_provisioned","code":"production_authority_not_provisioned"}}'
    $goldenResponseMaterial = [Text.Encoding]::UTF8.GetBytes("scribe-windows-gpu-promotion-response-v1`0$goldenResponse")
    $goldenResponseDigest = ([BitConverter]::ToString([Security.Cryptography.SHA256]::HashData($goldenResponseMaterial))).Replace('-', '').ToLowerInvariant()
    Assert-True ($goldenResponseDigest -ceq '7d4774c4ad2c0f59d57079e33d3729863a2a679739845f21b4a023207b580143') 'PowerShell and Rust disagree on the canonical broker response digest.'
    Assert-True (Test-RestrictedServiceSidType -ScOutput "[SC] QueryServiceConfig2 SUCCESS`r`n`r`nSERVICE_NAME: $serviceName`r`n        SERVICE_SID_TYPE :  RESTRICTED`r`n") 'The SCM SID parser rejected representative aligned qsidtype output.'
    Assert-True (-not (Test-RestrictedServiceSidType -ScOutput 'SERVICE_SID_TYPE: UNRESTRICTED')) 'The SCM SID parser accepted a non-restricted service.'
    Assert-True (-not (Test-RestrictedServiceSidType -ScOutput "SERVICE_SID_TYPE: RESTRICTED`nSERVICE_SID_TYPE: RESTRICTED")) 'The SCM SID parser accepted ambiguous duplicate fields.'

    if (Get-BrokerService) {
        throw "Refusing to modify the pre-existing fixed-name service $serviceName."
    }

    New-Item -ItemType Directory -Path $targetRoot | Out-Null
    $env:CARGO_TARGET_DIR = Join-Path $targetRoot 'cargo-target'
    Invoke-Process -FilePath 'cargo' -Arguments @('build', '--release', '--locked', '--offline', '--manifest-path', $manifest, '--bins') | Out-Null
    $client = Join-Path $env:CARGO_TARGET_DIR 'release\scribe-windows-gpu-promotion-client.exe'
    $builtService = Join-Path $env:CARGO_TARGET_DIR 'release\scribe-windows-gpu-promotion-service.exe'
    Assert-True (Test-Path -LiteralPath $client -PathType Leaf) 'Release broker client was not built.'
    Assert-True (Test-Path -LiteralPath $builtService -PathType Leaf) 'Release broker service was not built.'

    $handoff = Join-Path $targetRoot 'untrusted-handoff-must-not-exist'
    $output = Join-Path $targetRoot 'publication-must-not-exist'
    $arguments = New-ValidClientArguments -HandoffRoot $handoff -OutputRoot $output

    $missing = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($missing.ExitCode -eq 78) 'An absent broker did not map to the fail-closed unprovisioned exit.'
    Assert-True ($missing.Stderr.Trim() -ceq 'Protected Windows GPU promotion broker is unavailable and production authority is not provisioned; no filesystem, ledger, or signing authority was accessed.') 'An absent broker did not emit its fixed unavailable diagnostic.'
    Assert-True (-not (Test-Path -LiteralPath $handoff)) 'Absent-service handling touched the handoff path.'
    Assert-True (-not (Test-Path -LiteralPath $output)) 'Absent-service handling touched the output path.'

    $console = Invoke-Process -FilePath $builtService -TimeoutSeconds 20 -AllowFailure
    Assert-True ($console.ExitCode -eq 78) 'The SCM-only service did not reject an interactive console launch with its fixed exit.'

    $squatter = [IO.Pipes.NamedPipeServerStream]::new(
        $pipeName,
        [IO.Pipes.PipeDirection]::InOut,
        1,
        [IO.Pipes.PipeTransmissionMode]::Message,
        [IO.Pipes.PipeOptions]::Asynchronous
    )
    try {
        $connected = $squatter.WaitForConnectionAsync()
        $spoofed = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
        Assert-True ($spoofed.ExitCode -eq 74) 'The client did not classify a same-name user-process pipe server as rejected authentication.'
        Assert-True ($spoofed.Stderr.Contains('transport was rejected', [StringComparison]::Ordinal)) 'The client did not emit its fixed rejected-transport diagnostic.'
        Assert-True ($connected.Wait(5000)) 'The client did not reach the fixed-name squatter.'
        $buffer = [byte[]]::new(1)
        $read = $squatter.ReadAsync($buffer, 0, 1)
        Assert-True ($read.Wait(5000)) 'The squatter did not observe the client closing its authenticated connection.'
        Assert-True ($read.Result -eq 0) 'The client sent request bytes before authenticating the service.'
    }
    finally { $squatter.Dispose() }

    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $isElevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    if (-not $isElevated) {
        if ($RequireScmIntegration) { throw 'Restricted-service integration requires an elevated disposable Windows host.' }
        Write-Output 'Restricted-service integration skipped: current process is not elevated.'
        Write-Output 'Windows GPU broker transport contract tests passed.'
        return
    }

    $commonAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)
    Assert-True (-not [string]::IsNullOrWhiteSpace($commonAppData)) 'Windows did not provide the machine-wide application-data root.'
    $machineTarget = Join-Path $commonAppData "scribe-gpu-broker-transport-$([guid]::NewGuid().ToString('N'))"
    New-Item -ItemType Directory -Path $machineTarget | Out-Null
    $machineAcl = Get-Acl -LiteralPath $machineTarget
    $machineAcl.SetAccessRuleProtection($true, $false)
    foreach ($rule in @($machineAcl.Access)) { [void]$machineAcl.RemoveAccessRuleSpecific($rule) }
    $inheritance = [Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit'
    $propagation = [Security.AccessControl.PropagationFlags]::None
    $allow = [Security.AccessControl.AccessControlType]::Allow
    foreach ($entry in @(
        @('S-1-5-18', [Security.AccessControl.FileSystemRights]::FullControl),
        @('S-1-5-32-544', [Security.AccessControl.FileSystemRights]::FullControl),
        @($serviceSid, [Security.AccessControl.FileSystemRights]::ReadAndExecute)
    )) {
        $identitySid = [Security.Principal.SecurityIdentifier]::new([string]$entry[0])
        $accessRule = [Security.AccessControl.FileSystemAccessRule]::new($identitySid, $entry[1], $inheritance, $propagation, $allow)
        $machineAcl.AddAccessRule($accessRule)
    }
    Set-Acl -LiteralPath $machineTarget -AclObject $machineAcl
    $verifiedAcl = Get-Acl -LiteralPath $machineTarget
    Assert-True $verifiedAcl.AreAccessRulesProtected 'SCM test staging inherited an ambient writable DACL.'
    $verifiedRules = @($verifiedAcl.GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]))
    $allowedSids = @('S-1-5-18', 'S-1-5-32-544', $serviceSid)
    Assert-True ($verifiedRules.Count -eq 3) 'SCM test staging contains an unexpected access rule.'
    Assert-True (-not ($verifiedRules | Where-Object { $_.AccessControlType -ne $allow -or $_.IdentityReference.Value -notin $allowedSids })) 'SCM test staging contains unexpected identity or deny rules.'
    $serviceRules = @($verifiedRules | Where-Object { $_.IdentityReference.Value -ceq $serviceSid })
    Assert-True ($serviceRules.Count -eq 1) 'SCM test staging does not have one exact service-SID access rule.'
    Assert-True (($serviceRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::Write) -eq 0) 'The test service SID can modify its staged binary.'
    Assert-True (($serviceRules[0].FileSystemRights -band [Security.AccessControl.FileSystemRights]::ReadAndExecute) -eq [Security.AccessControl.FileSystemRights]::ReadAndExecute) 'The test service SID cannot read and execute its staged binary.'
    $serviceForScm = Join-Path $machineTarget 'scribe-windows-gpu-promotion-service.exe'
    Copy-Item -LiteralPath $builtService -Destination $serviceForScm
    Assert-True (Test-Path -LiteralPath $serviceForScm -PathType Leaf) 'Protected SCM service staging failed.'
    $stagedItem = Get-Item -LiteralPath $serviceForScm -Force
    Assert-True (($stagedItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) 'Protected SCM service staging produced a reparse point.'
    Assert-True ((Get-FileHash -Algorithm SHA256 -LiteralPath $serviceForScm).Hash -ceq (Get-FileHash -Algorithm SHA256 -LiteralPath $builtService).Hash) 'Protected SCM service staging changed the built service bytes.'

    $shownSid = Invoke-Sc -Arguments @('showsid', $serviceName)
    Assert-True ($shownSid.Stdout.Contains($serviceSid, [StringComparison]::Ordinal)) 'Windows derived an unexpected fixed service SID.'

    $quotedService = '"' + $serviceForScm + '"'
    $create = Invoke-Sc -Arguments @(
        'create', $serviceName,
        'type=', 'own',
        'start=', 'demand',
        'obj=', 'NT AUTHORITY\LocalService',
        'binPath=', $quotedService,
        'DisplayName=', 'Scribe GPU Promotion Broker Transport Test'
    ) -AllowFailure
    Assert-True ($create.ExitCode -eq 0) "Failed to create the temporary broker service: $($create.Stderr)"
    $createdService = $true

    $sidType = Invoke-Sc -Arguments @('sidtype', $serviceName, 'restricted') -AllowFailure
    Assert-True ($sidType.ExitCode -eq 0) "Failed to configure the restricted service SID: $($sidType.Stderr)"
    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)

    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) "SCM rejected the first service start: $($start.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))
    $stalledClientRights = [IO.Pipes.PipeAccessRights](
        [uint32][IO.Pipes.PipeAccessRights]::ReadData -bor
        [uint32][IO.Pipes.PipeAccessRights]::WriteData -bor
        [uint32][IO.Pipes.PipeAccessRights]::ReadAttributes -bor
        [uint32][IO.Pipes.PipeAccessRights]::WriteAttributes -bor
        [uint32][IO.Pipes.PipeAccessRights]::Synchronize
    )
    Assert-True ([uint32]$stalledClientRights -eq 0x00100183) 'The stalled-client probe no longer requests the production client access mask.'
    $stalled = [IO.Pipes.NamedPipeClientStream]::new(
        '.',
        $pipeName,
        $stalledClientRights,
        [IO.Pipes.PipeOptions]::Asynchronous,
        [Security.Principal.TokenImpersonationLevel]::Identification,
        [IO.HandleInheritability]::None
    )
    try {
        $stopProof = [Diagnostics.Stopwatch]::StartNew()
        $stalled.Connect(5000)
        [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
        $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
        Assert-True ($stop.ExitCode -eq 0) "SCM rejected the bounded-stop request: $($stop.Stderr)"
        (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(4))
        $stopProof.Stop()
        Assert-True ($stopProof.Elapsed.TotalMilliseconds -lt 4500) 'SCM stop did not cancel the stalled broker read materially before its five-second natural timeout.'
    }
    finally { $stalled.Dispose() }

    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) "SCM rejected the second service start: $($start.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))
    $roundTrip = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($roundTrip.ExitCode -eq 78) 'Authenticated service response did not map to NotProvisioned.'
    Assert-True ($roundTrip.Stdout.Length -eq 0) 'Broker client wrote protocol data to stdout.'
    Assert-True ($roundTrip.Stderr.Trim() -ceq 'Protected Windows GPU promotion broker authenticated; production authority is not provisioned, and no filesystem, ledger, or signing authority was accessed.') 'Broker client did not emit its fixed authenticated NotProvisioned diagnostic.'
    Assert-True ((Get-Service -Name $serviceName).Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Broker service did not remain running after the authenticated round trip.'
    Assert-True (-not (Test-Path -LiteralPath $handoff)) 'No-authority service touched the handoff path.'
    Assert-True (-not (Test-Path -LiteralPath $output)) 'No-authority service touched the output path.'

    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
    $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
    Assert-True ($stop.ExitCode -eq 0) "SCM rejected the final stop request: $($stop.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))
    Write-Output 'Windows GPU broker transport contract tests passed.'
}
finally {
    if ($createdService) {
        $existing = Get-BrokerService
        if ($null -ne $existing) {
            [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
            if ($existing.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Stopped) {
                Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure | Out-Null
                (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))
            }
            [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
            Invoke-Sc -Arguments @('delete', $serviceName) -AllowFailure | Out-Null
        }
        Wait-ServiceAbsent -TimeoutSeconds 10
    }
    if ($null -ne $machineTarget) {
        $resolvedCommonAppData = [IO.Path]::GetFullPath([Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)).TrimEnd('\') + '\'
        $resolvedMachineTarget = [IO.Path]::GetFullPath($machineTarget)
        if ($resolvedMachineTarget.StartsWith($resolvedCommonAppData, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedMachineTarget).StartsWith('scribe-gpu-broker-transport-', [StringComparison]::Ordinal)) {
            $machineItem = Get-Item -LiteralPath $resolvedMachineTarget -Force -ErrorAction SilentlyContinue
            if ($null -ne $machineItem -and ($machineItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
                Remove-Item -LiteralPath $resolvedMachineTarget -Recurse -Force -ErrorAction SilentlyContinue
            }
        }
    }
    [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $previousCargoTarget, 'Process')
    $resolvedTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    $resolvedTarget = [IO.Path]::GetFullPath($targetRoot)
    if ($resolvedTarget.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase) -and
        [IO.Path]::GetFileName($resolvedTarget).StartsWith('scribe-gpu-broker-transport-', [StringComparison]::Ordinal)) {
        Remove-Item -LiteralPath $resolvedTarget -Recurse -Force -ErrorAction SilentlyContinue
    }
}
