param(
    [switch]$RequireScmIntegration
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$serviceName = 'ScribeGpuPromotionBroker'
$pipeName = 'ScribeGpuPromotionBroker.v1'
$serviceSid = 'S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137'
$policyPath = 'SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization'
$policyRegistryPath = 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifest = Join-Path $repositoryRoot 'tools\windows-gpu-promotion-broker\Cargo.toml'
$provisioner = Join-Path $repositoryRoot 'scripts\provision-windows-gpu-broker-client-policy.ps1'
$targetRoot = Join-Path ([IO.Path]::GetTempPath()) "scribe-gpu-broker-transport-$([guid]::NewGuid().ToString('N'))"
$previousCargoTarget = [Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR', 'Process')
$createdService = $false
$machineTarget = $null
$primaryFailure = $null
$cleanupFailures = [Collections.Generic.List[object]]::new()
$safeToRemoveMachineTarget = $false
$ownedPolicyState = $null
$initiallyMissingPolicyAncestors = @()
$ownedPolicyAncestors = @()
$policyAncestorPaths = @(
    'SOFTWARE\Scribe',
    'SOFTWARE\Scribe\GpuPromotionBroker',
    'SOFTWARE\Scribe\GpuPromotionBroker\v1'
)

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

function Wait-ServiceNotRunning([int]$TimeoutSeconds) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        $service = Get-BrokerService
        if ($null -eq $service -or $service.Status -eq [System.ServiceProcess.ServiceControllerStatus]::Stopped) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Service $serviceName remained active after a rejected startup."
}

function Get-PolicySecurityFingerprint([Microsoft.Win32.RegistryKey]$Key) {
    $security = $Key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
    return [Convert]::ToBase64String($security.GetSecurityDescriptorBinaryForm())
}

function New-ExpectedPolicyValues([string]$Sid) {
    return [ordered]@{
        'AuthorizedClientSid' = [pscustomobject]@{ Kind = [Microsoft.Win32.RegistryValueKind]::String; Value = $Sid }
        'SchemaVersion' = [pscustomobject]@{ Kind = [Microsoft.Win32.RegistryValueKind]::DWord; Value = [uint32]1 }
    }
}

function Assert-OwnedPolicyState([object]$State) {
    Assert-True ($null -ne $State) 'Policy ownership state is unavailable.'
    Assert-True ($policyRegistryPath -ceq 'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization') 'Policy cleanup target changed.'
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $false)
        Assert-True ($null -ne $key) 'Owned authorization policy disappeared; refusing cleanup.'
        try {
            Assert-True ($key.SubKeyCount -eq 0) 'Refusing to remove a policy containing subkeys.'
            $actualNames = @($key.GetValueNames() | Sort-Object -CaseSensitive)
            $expectedNames = @($State.Values.Keys | Sort-Object -CaseSensitive)
            Assert-True ($actualNames.Count -eq $expectedNames.Count) 'Policy value inventory changed; refusing cleanup.'
            for ($index = 0; $index -lt $expectedNames.Count; $index++) {
                Assert-True ($actualNames[$index] -ceq $expectedNames[$index]) 'Policy value inventory changed; refusing cleanup.'
                $name = $expectedNames[$index]
                $expected = $State.Values[$name]
                Assert-True ($key.GetValueKind($name) -eq $expected.Kind) "Policy value type changed for $name; refusing cleanup."
                $actual = $key.GetValue($name)
                if ($expected.Kind -eq [Microsoft.Win32.RegistryValueKind]::DWord) {
                    Assert-True ([uint32]$actual -eq [uint32]$expected.Value) "Policy value changed for $name; refusing cleanup."
                }
                else {
                    Assert-True ([string]$actual -ceq [string]$expected.Value) "Policy value changed for $name; refusing cleanup."
                }
            }
            Assert-True ((Get-PolicySecurityFingerprint -Key $key) -ceq $State.SecurityFingerprint) 'Policy security descriptor changed; refusing cleanup.'
            if ($State.RequireCanonicalAcl) { Assert-ExactPolicyAclForKey -Key $key }
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Remove-OwnedPolicy {
    if ($null -eq $ownedPolicyState) { return }
    $state = $ownedPolicyState
    Assert-OwnedPolicyState -State $state
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try { $base.DeleteSubKey($policyPath, $false) }
    finally { $base.Dispose() }
    Assert-True (-not (Test-Path -LiteralPath $policyRegistryPath)) 'Owned authorization policy remained after cleanup.'
    $script:ownedPolicyState = $null
}

function Remove-OwnedPolicyAncestors {
    if ($ownedPolicyAncestors.Count -eq 0) { return }
    Assert-True (@($ownedPolicyAncestors | Where-Object { $_ -cnotin $policyAncestorPaths }).Count -eq 0) 'Policy ancestor cleanup target changed.'
    $paths = @($ownedPolicyAncestors)
    [array]::Reverse($paths)
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        foreach ($path in $paths) {
            $key = $base.OpenSubKey($path, $false)
            Assert-True ($null -ne $key) "Owned policy ancestor $path disappeared; refusing cleanup."
            try {
                Assert-True ($key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0) "Owned policy ancestor $path is no longer empty; refusing cleanup."
                Assert-ExactPolicyAclForKey -Key $key
            }
            finally { $key.Dispose() }
            $base.DeleteSubKey($path, $false)
        }
    }
    finally { $base.Dispose() }
    $script:ownedPolicyAncestors = @()
}

function Set-OwnedPolicyState([System.Collections.IDictionary]$Values, [bool]$RequireCanonicalAcl) {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $false)
        Assert-True ($null -ne $key) 'Successfully provisioned authorization policy is unavailable.'
        try {
            $state = [pscustomobject]@{
                Values = $Values
                RequireCanonicalAcl = $RequireCanonicalAcl
                SecurityFingerprint = Get-PolicySecurityFingerprint -Key $key
            }
            if ($RequireCanonicalAcl) { Assert-ExactPolicyAclForKey -Key $key }
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    Assert-OwnedPolicyState -State $state
    $script:ownedPolicyState = $state
}

function New-ProtectedPolicy([string]$Sid) {
    $powerShell = (Get-Process -Id $PID).Path
    $result = Invoke-Process -FilePath $powerShell -Arguments @('-NoProfile', '-File', $provisioner, '-AuthorizedClientSid', $Sid) -TimeoutSeconds 30 -AllowFailure
    if ($result.ExitCode -eq 0) {
        Assert-True ($null -eq $ownedPolicyState) 'Provisioner succeeded while another policy was owned by the harness.'
        Set-OwnedPolicyState -Values (New-ExpectedPolicyValues -Sid $Sid) -RequireCanonicalAcl $true
        if ($ownedPolicyAncestors.Count -eq 0) {
            $script:ownedPolicyAncestors = @($initiallyMissingPolicyAncestors)
        }
    }
    return $result
}

function New-WeakPolicy([string]$Sid) {
    $result = New-ProtectedPolicy -Sid $Sid
    Assert-True ($result.ExitCode -eq 0) "Could not provision the weak-DACL fixture: $($result.Stderr)"
    Assert-OwnedPolicyState -State $ownedPolicyState
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Owned authorization policy disappeared before weak-DACL setup.'
        try {
            $security = $key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
            $broadRead = [Security.AccessControl.RegistryAccessRule]::new(
                [Security.Principal.SecurityIdentifier]::new('S-1-5-11'),
                [Security.AccessControl.RegistryRights]::ReadKey,
                [Security.AccessControl.InheritanceFlags]::None,
                [Security.AccessControl.PropagationFlags]::None,
                [Security.AccessControl.AccessControlType]::Allow
            )
            [void]$security.AddAccessRule($broadRead)
            $key.SetAccessControl($security)
        }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    Set-OwnedPolicyState -Values (New-ExpectedPolicyValues -Sid $Sid) -RequireCanonicalAcl $false
}

function Set-PolicyValue([string]$Name, [object]$Value, [Microsoft.Win32.RegistryValueKind]$Kind) {
    Assert-True ($null -ne $ownedPolicyState) 'Refusing to mutate a policy not created by this harness.'
    Assert-OwnedPolicyState -State $ownedPolicyState
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        if ($null -eq $key) { throw 'Owned authorization policy disappeared.' }
        try { $key.SetValue($Name, $Value, $Kind); $key.Flush() }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    $values = [ordered]@{}
    foreach ($existingName in $ownedPolicyState.Values.Keys) { $values[$existingName] = $ownedPolicyState.Values[$existingName] }
    $values[$Name] = [pscustomobject]@{ Kind = $Kind; Value = $Value }
    Set-OwnedPolicyState -Values $values -RequireCanonicalAcl $ownedPolicyState.RequireCanonicalAcl
}

function Assert-ExactPolicyAclForKey([Microsoft.Win32.RegistryKey]$Key) {
    $acl = $Key.GetAccessControl([Security.AccessControl.AccessControlSections]'Access, Owner')
    Assert-True $acl.AreAccessRulesProtected 'Authorization policy DACL is not protected.'
    Assert-True ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ceq 'S-1-5-18') 'Authorization policy owner is not SYSTEM.'
    $rules = @($acl.GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]))
    Assert-True ($rules.Count -eq 3) 'Authorization policy does not have exactly three ACEs.'
    $expected = @{
        'S-1-5-18' = [uint32][Security.AccessControl.RegistryRights]::FullControl
        'S-1-5-32-544' = [uint32][Security.AccessControl.RegistryRights]::FullControl
        $serviceSid = [uint32][Security.AccessControl.RegistryRights]::ReadKey
    }
    foreach ($rule in $rules) {
        Assert-True ($expected.ContainsKey($rule.IdentityReference.Value)) 'Authorization policy contains an unexpected SID.'
        Assert-True (-not $rule.IsInherited -and $rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Allow) 'Authorization policy contains inherited or deny access.'
        Assert-True ([uint32]$rule.RegistryRights -eq $expected[$rule.IdentityReference.Value]) 'Authorization policy ACE mask is noncanonical.'
        [void]$expected.Remove($rule.IdentityReference.Value)
    }
    Assert-True ($expected.Count -eq 0) 'Authorization policy is missing a required SID.'
}

function Assert-ExactPolicyAcl {
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $false)
        Assert-True ($null -ne $key) 'Authorization policy is unavailable.'
        try { Assert-ExactPolicyAclForKey -Key $key }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
}

function Assert-RejectedServiceStartup([string]$Label, [string]$Client, [string[]]$ClientArguments) {
    [void](Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure)
    Wait-ServiceNotRunning -TimeoutSeconds 10
    $probe = Invoke-Process -FilePath $Client -Arguments $ClientArguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($probe.ExitCode -eq 78) "$Label did not leave the fixed pipe unavailable."
    Assert-True ($probe.Stderr.Contains('broker is unavailable', [StringComparison]::Ordinal)) "$Label exposed a pipe after rejected startup."
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

    if (Test-Path -LiteralPath $policyRegistryPath) {
        throw 'Refusing to modify a pre-existing fixed Windows GPU broker client policy.'
    }
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $missing = [Collections.Generic.List[string]]::new()
        foreach ($path in $policyAncestorPaths) {
            $ancestor = $base.OpenSubKey($path, $false)
            if ($null -eq $ancestor) { [void]$missing.Add($path) }
            else { $ancestor.Dispose() }
        }
        $initiallyMissingPolicyAncestors = @($missing)
    }
    finally { $base.Dispose() }
    foreach ($rejectedSid in @('BUILTIN\Users', 'S-1-5-11', 'S-1-5-20', $serviceSid, 'S-1-5-21-1-2-3-500')) {
        $rejectedProvision = New-ProtectedPolicy -Sid $rejectedSid
        Assert-True ($rejectedProvision.ExitCode -ne 0) "Provisioner accepted dangerous client identity $rejectedSid."
        Assert-True (-not (Test-Path -LiteralPath $policyRegistryPath)) 'Rejected provisioning created a policy key.'
        Assert-True ($null -eq $ownedPolicyState) 'Rejected provisioning established destructive policy ownership.'
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

    Assert-RejectedServiceStartup -Label 'Missing policy' -Client $client -ClientArguments $arguments
    Assert-True (-not (Test-Path -LiteralPath $handoff)) 'Missing-policy startup touched the handoff path.'
    Assert-True (-not (Test-Path -LiteralPath $output)) 'Missing-policy startup touched the output path.'

    New-WeakPolicy -Sid $identity.User.Value
    Assert-RejectedServiceStartup -Label 'Weak broad-read policy DACL' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $identity.User.Value
    Assert-True ($provisioned.ExitCode -eq 0) "Protected policy provisioning failed: $($provisioned.Stderr)"
    Assert-ExactPolicyAcl
    $duplicateProvision = New-ProtectedPolicy -Sid $identity.User.Value
    Assert-True ($duplicateProvision.ExitCode -ne 0) 'Provisioner modified a pre-existing policy.'

    # A failed provision never establishes ownership, and cleanup must also
    # refuse an owned key whose exact fixture state changed unexpectedly.
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Owned policy disappeared before adversarial cleanup proof.'
        try { $key.SetValue('CleanupTamper', 'foreign', [Microsoft.Win32.RegistryValueKind]::String); $key.Flush() }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    $cleanupRejected = $false
    try { Remove-OwnedPolicy }
    catch { $cleanupRejected = $true }
    Assert-True $cleanupRejected 'Policy cleanup accepted a changed same-name key.'
    Assert-True (Test-Path -LiteralPath $policyRegistryPath) 'Policy cleanup deleted a changed same-name key.'
    $base = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, [Microsoft.Win32.RegistryView]::Registry64)
    try {
        $key = $base.OpenSubKey($policyPath, $true)
        Assert-True ($null -ne $key) 'Changed policy disappeared during adversarial cleanup proof.'
        try { $key.DeleteValue('CleanupTamper', $true); $key.Flush() }
        finally { $key.Dispose() }
    }
    finally { $base.Dispose() }
    Assert-OwnedPolicyState -State $ownedPolicyState

    Set-PolicyValue -Name 'UnexpectedValue' -Value 'forbidden' -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Extra policy value' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $identity.User.Value
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not reprovision the malformed-schema fixture.'
    Set-PolicyValue -Name 'SchemaVersion' -Value '1' -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Malformed policy schema' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $identity.User.Value
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not reprovision the malformed-policy fixture.'
    Set-PolicyValue -Name 'AuthorizedClientSid' -Value 'S-1-5-11' -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    Assert-RejectedServiceStartup -Label 'Broad malformed policy SID' -Client $client -ClientArguments $arguments
    Remove-OwnedPolicy

    $orphanSid = 'S-1-5-21-4294967290-4294967291-4294967292-4294967293'
    $provisioned = New-ProtectedPolicy -Sid $orphanSid
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not provision the orphan-SID denial fixture.'
    Assert-ExactPolicyAcl
    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) "SCM rejected the syntactically valid orphan-SID policy: $($start.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))
    $wrongIdentity = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($wrongIdentity.ExitCode -eq 74) 'Wrong TokenUser SID did not fail closed without a response.'
    Assert-True ($wrongIdentity.Stderr.Contains('transport was rejected', [StringComparison]::Ordinal)) 'Wrong TokenUser SID did not emit the rejected-transport diagnostic.'
    Assert-True ((Get-Service -Name $serviceName).Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Orphan SID denial stopped the healthy service.'
    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
    $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
    Assert-True ($stop.ExitCode -eq 0) 'SCM rejected the orphan-policy stop request.'
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))
    Remove-OwnedPolicy

    $provisioned = New-ProtectedPolicy -Sid $identity.User.Value
    Assert-True ($provisioned.ExitCode -eq 0) 'Could not provision the current TokenUser SID.'
    Assert-ExactPolicyAcl
    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) "SCM rejected the current-user policy: $($start.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))

    $overbroad = [IO.Pipes.NamedPipeClientStream]::new(
        '.',
        $pipeName,
        [IO.Pipes.PipeAccessRights]::FullControl,
        [IO.Pipes.PipeOptions]::Asynchronous,
        [Security.Principal.TokenImpersonationLevel]::Identification,
        [IO.HandleInheritability]::None
    )
    try {
        $overbroadDenied = $false
        try { $overbroad.Connect(2000) }
        catch [UnauthorizedAccessException] { $overbroadDenied = $true }
        Assert-True $overbroadDenied 'Client received generic write, pipe-instance, or ACL authority beyond 0x00100183.'
    }
    finally { $overbroad.Dispose() }

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
    Set-PolicyValue -Name 'AuthorizedClientSid' -Value $orphanSid -Kind ([Microsoft.Win32.RegistryValueKind]::String)
    $roundTrip = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($roundTrip.ExitCode -eq 78) 'Authenticated service response did not map to NotProvisioned.'
    Assert-True ($roundTrip.Stdout.Length -eq 0) 'Broker client wrote protocol data to stdout.'
    Assert-True ($roundTrip.Stderr.Trim() -ceq 'Protected Windows GPU promotion broker authenticated; production authority is not provisioned, and no filesystem, ledger, or signing authority was accessed.') 'Broker client did not emit its fixed authenticated NotProvisioned diagnostic.'
    Assert-True ((Get-Service -Name $serviceName).Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Broker service did not remain running after the authenticated round trip.'
    Assert-True (-not (Test-Path -LiteralPath $handoff)) 'No-authority service touched the handoff path.'
    Assert-True (-not (Test-Path -LiteralPath $output)) 'No-authority service touched the output path.'

    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
    $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
    Assert-True ($stop.ExitCode -eq 0) "SCM rejected the snapshot-policy stop request: $($stop.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))

    $start = Invoke-Sc -Arguments @('start', $serviceName) -AllowFailure
    Assert-True ($start.ExitCode -eq 0) 'SCM rejected restart with the mutated, syntactically valid orphan SID.'
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(10))
    $afterRestart = Invoke-Process -FilePath $client -Arguments $arguments -TimeoutSeconds 20 -AllowFailure
    Assert-True ($afterRestart.ExitCode -eq 74) 'Service restart did not load the mutated authorization SID.'
    Assert-True ((Get-Service -Name $serviceName).Status -eq [System.ServiceProcess.ServiceControllerStatus]::Running) 'Restarted orphan-policy service was not healthy after denial.'
    [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
    $stop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
    Assert-True ($stop.ExitCode -eq 0) "SCM rejected the final stop request: $($stop.Stderr)"
    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))
    Write-Output 'Windows GPU broker transport contract tests passed.'
}
catch { $primaryFailure = $_ }
finally {
    try {
        if ($null -ne $ownedPolicyState) { Remove-OwnedPolicy }
    }
    catch { [void]$cleanupFailures.Add($_) }

    try { Remove-OwnedPolicyAncestors }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($createdService) {
            $existing = Get-BrokerService
            if ($null -ne $existing) {
                [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
                if ($existing.Status -ne [System.ServiceProcess.ServiceControllerStatus]::Stopped) {
                    $cleanupStop = Invoke-Sc -Arguments @('stop', $serviceName) -AllowFailure
                    Assert-True ($cleanupStop.ExitCode -eq 0) "SCM rejected cleanup stop: $($cleanupStop.Stderr)"
                    (Get-Service -Name $serviceName).WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(10))
                }
                [void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)
                $cleanupDelete = Invoke-Sc -Arguments @('delete', $serviceName) -AllowFailure
                Assert-True ($cleanupDelete.ExitCode -eq 0) "SCM rejected cleanup delete: $($cleanupDelete.Stderr)"
            }
            Wait-ServiceAbsent -TimeoutSeconds 10
        }
        $safeToRemoveMachineTarget = $true
    }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        if ($safeToRemoveMachineTarget -and $null -ne $machineTarget) {
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
    }
    catch { [void]$cleanupFailures.Add($_) }

    try { [Environment]::SetEnvironmentVariable('CARGO_TARGET_DIR', $previousCargoTarget, 'Process') }
    catch { [void]$cleanupFailures.Add($_) }

    try {
        $resolvedTemp = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
        $resolvedTarget = [IO.Path]::GetFullPath($targetRoot)
        if ($resolvedTarget.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase) -and
            [IO.Path]::GetFileName($resolvedTarget).StartsWith('scribe-gpu-broker-transport-', [StringComparison]::Ordinal)) {
            Remove-Item -LiteralPath $resolvedTarget -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
    catch { [void]$cleanupFailures.Add($_) }

    if ($null -eq $primaryFailure -and $cleanupFailures.Count -gt 0) {
        throw $cleanupFailures[0]
    }
}

if ($null -ne $primaryFailure) {
    foreach ($cleanupFailure in $cleanupFailures) {
        Write-Warning "Non-destructive broker test cleanup was incomplete: $($cleanupFailure.Exception.Message)"
    }
    throw $primaryFailure
}
