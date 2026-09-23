$ErrorActionPreference='Stop'; Set-StrictMode -Version Latest
$testRoot=Join-Path ([IO.Path]::GetTempPath()) ('scribe-cuda-contract-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
try {
 . (Join-Path $PSScriptRoot 'windows-cuda-evidence-preflight.ps1')
 $stats=[ordered]@{p50_ms=1;p95_ms=1}; $cold=[ordered]@{end_to_end_ms=@(1)*5;end_to_end=$stats;backend_processing_ms=@(1)*5;backend_processing=$stats;model_load_ms=@(1)*5;model_load=$stats}; $warm=[ordered]@{end_to_end_ms=@(1)*20;end_to_end=$stats;backend_processing_ms=@(1)*20;backend_processing=$stats;model_load_ms=$null;model_load=$null}
 $report=[ordered]@{schema_version=1;evidence_kind='windows-cuda-fixture-performance';fixture_only=$true;untrusted=$true;auto_eligible=$false;source_revision=('a'*40);cpu_worker_sha256=('b'*64);pack=[ordered]@{id='scribe-cuda-windows-x64';version='fixture-a';digest=('c'*64);security_epoch=1;runtime_abi=1};model_sha256=('d'*64);wav_sha256=('e'*64);gpu=[ordered]@{backend='cuda';provider='transcribe-cpp-ggml-cuda';vendor='nvidia';device_class='discrete_gpu';driver='fixture';memory_total_bytes=1};nvidia_baseline=[ordered]@{product='fixture';driver='fixture';memory_total_bytes=1;memory_used_bytes=0;gpu_utilization_percent=0};cold_runs_per_backend=5;warm_runs_per_backend=20;cpu=[ordered]@{cold=$cold;warm=$warm};cuda=[ordered]@{cold=$cold;warm=$warm};expected_phrase_present_every_run=$true;normalized_transcript_parity=$true;same_device_internally_verified=$true}
 function Write-Case($value,$leaf) {$path=Join-Path $testRoot $leaf;[IO.File]::WriteAllText($path,($value|ConvertTo-Json -Depth 12 -Compress),[Text.UTF8Encoding]::new($false));$digest=(Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant();@($path,$digest)}
 $valid=Write-Case $report 'valid.json'; $verified=Read-ScribeVerifiedCudaEvidenceReport $valid[0] $valid[1]; if($verified.Sha256-cne$valid[1]){throw 'Valid CUDA report lost digest binding.'}
 $pending=Write-Case $report 'pending.json';$published=Complete-ScribeCudaEvidencePendingReport $pending[0] (Join-Path $testRoot 'published.json') $testRoot 'pending.json' 'published.json' $null @();if(-not(Test-Path -LiteralPath $published.Path)-or$published.Digest-cne$pending[1]){throw 'CUDA retained-handle publication failed.'};$null=Read-ScribeVerifiedCudaEvidenceReport $published.Path $published.Digest
 $failedPending=Write-Case $report 'failed-pending.json';$failedFinal=Join-Path $testRoot 'failed-final.json';$primary=[InvalidOperationException]::new('fixture child failed');$caught=$null;try{Complete-ScribeCudaEvidencePendingReport $failedPending[0] $failedFinal $testRoot 'failed-pending.json' 'failed-final.json' $primary @()|Out-Null}catch{$caught=$_.Exception};if($null-eq$caught-or$caught.Message-cne$primary.Message-or(Test-Path -LiteralPath $failedPending[0])-or(Test-Path -LiteralPath $failedFinal)){throw 'CUDA primary child failure did not prevent publication and remove pending evidence.'}
 $mutations=@(
  {param($r)$r.gpu.backend='vulkan'}, {param($r)$r.gpu.provider='wrong'}, {param($r)$r.gpu.vendor='amd'}, {param($r)$r.gpu.device_class='integrated_gpu'},
  {param($r)$r.fixture_only=$false}, {param($r)$r.auto_eligible=$true}, {param($r)$r.source_revision='A'*40}, {param($r)$r.cpu.cold.end_to_end_ms=@(1)*4},
  {param($r)$r.cuda.warm.model_load_ms=@(1)*20}, {param($r)$r.cpu.cold.model_load_ms[0]=-1}, {param($r)$r.cuda.cold.model_load.p95_ms='1'},
  {param($r)$r.gpu.driver="fixture$([char]27)escape"}, {param($r)$r.pack=@{}}, {param($r)$r.nvidia_baseline=@{}}, {param($r)$r.gpu.PSObject.Properties.Remove('driver')},
  {param($r)$r.gpu|Add-Member extra 1}, {param($r)$r|Add-Member extra 1}
 )
 $index=0; foreach($mutation in $mutations){$copy=($report|ConvertTo-Json -Depth 12|ConvertFrom-Json);&$mutation $copy;$case=Write-Case $copy "bad-$index.json";$rejected=$false;try{Read-ScribeVerifiedCudaEvidenceReport $case[0] $case[1]|Out-Null}catch{$rejected=$true};if(-not$rejected){throw "CUDA reader accepted mutation $index"};$index++}
 $wrong=$false;try{Read-ScribeVerifiedCudaEvidenceReport $valid[0] ('0'*64)|Out-Null}catch{$wrong=$true};if(-not$wrong){throw 'CUDA reader accepted wrong digest.'}
 $duplicate=Join-Path $testRoot 'duplicate.json';[IO.File]::WriteAllText($duplicate,'{"schema_version":1,"schema_version":1}',[Text.UTF8Encoding]::new($false));$dd=(Get-FileHash $duplicate -Algorithm SHA256).Hash.ToLowerInvariant();$rejected=$false;try{Read-ScribeVerifiedCudaEvidenceReport $duplicate $dd|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'CUDA reader accepted duplicate JSON names.'}
 # Keep the immutable v1 report contract while requiring all v2 startup records.
 $v2 = $report | ConvertTo-Json -Depth 12 | ConvertFrom-Json
 $v2.schema_version = 2
 foreach ($backend in @('cpu', 'cuda')) {
     $records = @(1..5 | ForEach-Object {
         [pscustomobject]@{resolve_ms=1;executable_revalidation_ms=0;spawn_ms=0;hello_ms=0}
     })
     $v2.$backend.cold | Add-Member worker_startup_ms $records
     $v2.$backend.warm | Add-Member worker_startup_ms $null
 }
 $validV2 = Write-Case $v2 'valid-v2.json'
 $null = Read-ScribeVerifiedCudaEvidenceReport $validV2[0] $validV2[1]
 $pendingV2 = Write-Case $v2 'pending-v2.json'
 $publishedV2 = Complete-ScribeCudaEvidencePendingReport $pendingV2[0] (Join-Path $testRoot 'published-v2.json') $testRoot 'pending-v2.json' 'published-v2.json' $null @()
 if ($publishedV2.Digest -cne $pendingV2[1]) { throw 'CUDA v2 publication lost digest binding.' }
 $null = Read-ScribeVerifiedCudaEvidenceReport $publishedV2.Path $publishedV2.Digest
 $v2Mutations = @(
     {param($r) $r.schema_version=1},
     {param($r) $r.schema_version=3},
     {param($r) $r.cpu.cold.PSObject.Properties.Remove('worker_startup_ms')},
     {param($r) $r.cuda.warm.PSObject.Properties.Remove('worker_startup_ms')},
     {param($r) $r.cpu.cold.worker_startup_ms=$null},
     {param($r) $r.cuda.cold.worker_startup_ms=@()},
     {param($r) $r.cpu.cold.worker_startup_ms=@($r.cpu.cold.worker_startup_ms[0]) * 4},
     {param($r) $r.cuda.cold.worker_startup_ms=@($r.cuda.cold.worker_startup_ms[0]) * 6},
     {param($r) $r.cpu.warm.worker_startup_ms=@()},
     {param($r) $r.cuda.warm.worker_startup_ms=@($r.cuda.cold.worker_startup_ms[0]) * 20},
     {param($r) $r.cpu.cold.worker_startup_ms[0]=$null},
     {param($r) $r.cuda.cold.worker_startup_ms[0].PSObject.Properties.Remove('hello_ms')},
     {param($r) $r.cpu.cold.worker_startup_ms[0] | Add-Member extra 0},
     {param($r) $r.cpu.cold.worker_startup_ms[0].resolve_ms='1'},
     {param($r) $r.cuda.cold.worker_startup_ms[0].executable_revalidation_ms=-1},
     {param($r) $r.cpu.cold.worker_startup_ms[0].spawn_ms=0.5},
     {param($r) $r.cuda.cold.worker_startup_ms[0].hello_ms=$false},
     {param($r) $r.cpu.cold.worker_startup_ms[4].hello_ms=1},
     {param($r) $r.cuda.cold.end_to_end_ms[4]=0},
     {param($r) $r.cuda.cold.worker_startup_ms[0].resolve_ms=[uint64]::MaxValue}
 )
 $v2Index=0
 foreach ($mutation in $v2Mutations) {
     $copy=$v2 | ConvertTo-Json -Depth 12 | ConvertFrom-Json
     & $mutation $copy
     $case=Write-Case $copy "bad-v2-$v2Index.json"
     $rejected=$false
     try { Read-ScribeVerifiedCudaEvidenceReport $case[0] $case[1] | Out-Null } catch { $rejected=$true }
     if (-not $rejected) { throw "CUDA reader accepted v2 mutation $v2Index" }
     $v2Index++
 }
 $v1MissingV2=$report | ConvertTo-Json -Depth 12 | ConvertFrom-Json
 $v1MissingV2.schema_version=2
 $rejected=$false
 try { Assert-ScribeCudaEvidenceReportJson ($v1MissingV2 | ConvertTo-Json -Depth 12 -Compress) } catch { $rejected=$true }
 if (-not $rejected) { throw 'CUDA v2 accepted missing startup measurements.' }
 $v2Json=$v2 | ConvertTo-Json -Depth 12 -Compress
 $rawMutations=@(
     '"resolve_ms":1,"resolve_ms":1',
     '"resolve_ms":18446744073709551616',
     '"resolve_ms":1.0',
     '"resolve_ms":1e0'
 )
 foreach ($replacement in $rawMutations) {
     $rejected=$false
     try { Assert-ScribeCudaEvidenceReportJson ($v2Json.Replace('"resolve_ms":1', $replacement)) } catch { $rejected=$true }
     if (-not $rejected) { throw "CUDA v2 accepted duplicate or noncanonical startup value: $replacement" }
 }
 # Exact u64 arithmetic: accept equality at the maximum, then reject a sum one above it.
 $large=$v2 | ConvertTo-Json -Depth 12 | ConvertFrom-Json
 $large.cpu.cold.end_to_end_ms[0]=[uint64]::MaxValue
 $large.cpu.cold.worker_startup_ms[0].resolve_ms=[uint64]::MaxValue-1
 $large.cpu.cold.worker_startup_ms[0].hello_ms=1
 Assert-ScribeCudaEvidenceReportJson ($large | ConvertTo-Json -Depth 12 -Compress)
 $large.cpu.cold.worker_startup_ms[0].hello_ms=2
 $rejected=$false
 try { Assert-ScribeCudaEvidenceReportJson ($large | ConvertTo-Json -Depth 12 -Compress) } catch { $rejected=$true }
 if (-not $rejected) { throw 'CUDA v2 startup sum overflow was accepted.' }
 $invalidPending=Write-Case $large 'invalid-v2-pending.json'
 $invalidFinal=Join-Path $testRoot 'invalid-v2-final.json'
 $rejected=$false
 try { Complete-ScribeCudaEvidencePendingReport $invalidPending[0] $invalidFinal $testRoot 'invalid-v2-pending.json' 'invalid-v2-final.json' $null @() | Out-Null } catch { $rejected=$true }
 if (-not $rejected -or (Test-Path -LiteralPath $invalidPending[0]) -or (Test-Path -LiteralPath $invalidFinal)) {
     throw 'CUDA v2 invalid startup report was published or not cleaned up.'
 }
 $runner=Get-Content (Join-Path $PSScriptRoot 'run-windows-cuda-evidence.ps1') -Raw
 foreach($required in @('[string]$EvidenceDirectory','[string]$NativeArchiveDirectory','[string]$CudaToolkitDirectory','Production signing/release input is forbidden','Get-ScribeVulkanEvidenceActualSystem32','Assert-ScribeEvidenceSingleLinkFile','Get-ScribeEvidencePinnedMsvcEnvironment','Invoke-ScribeEvidenceCargoWithCmakeRetry','-Backend Cuda','-SigningMode Fixture','CUDA_PATH = $previousCudaPath','--no-run','local_transcriber-[0-9a-f]{16}',"Invoke-ScribeEvidence `$testExecutable",'[Diagnostics.ProcessStartInfo]::new()','UseShellExecute = $false','ArgumentList.Add($argument)','WaitForExit()','Native process exit code:','process.Dispose()','SCRIBE_BUNDLED_WORKER_SHA256 = $cpuWorkerDigest','changed before CUDA evidence test precompilation','changed before exact CUDA evidence execution','status --porcelain=v1 --untracked-files=all','Complete-ScribeCudaEvidencePendingReport','Windows Auto manifest changed')){if(-not$runner.Contains($required)){throw "CUDA runner missing hardened contract: $required"}}
 if($runner-match 'SigningMode Production|ProductionPrivateKeyPath'){throw 'CUDA runner references production signing.'}
 $tokens=$null;$errors=$null;$runnerAst=[Management.Automation.Language.Parser]::ParseInput($runner,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'CUDA runner does not parse.'}
 $invokeDefinitions=@($runnerAst.FindAll({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'Invoke-ScribeEvidence'},$true));if($invokeDefinitions.Count-ne1){throw 'CUDA runner must define exactly one native invocation helper.'};Invoke-Expression $invokeDefinitions[0].Extent.Text
 $childScript=Join-Path $testRoot 'child process.ps1';[IO.File]::WriteAllText($childScript,'param([string]$Output,[string]$First,[string]$Second,[int]$Code)[IO.File]::WriteAllText($Output,$First+"`n"+$Second,[Text.UTF8Encoding]::new($false));exit $Code',[Text.UTF8Encoding]::new($false));$childOutput=Join-Path $testRoot 'waited child.txt';$pwsh=(Get-Command pwsh.exe -ErrorAction Stop).Source;Invoke-ScribeEvidence $pwsh @('-NoProfile','-File',$childScript,$childOutput,'first value with spaces',"second value's apostrophe",'0') 'waited child failed';if(-not(Test-Path -LiteralPath $childOutput)-or([IO.File]::ReadAllText($childOutput)-cne"first value with spaces`nsecond value's apostrophe")){throw 'CUDA native invocation helper returned before its child completed or changed argument boundaries.'}
 $nonzero=$null;try{Invoke-ScribeEvidence $pwsh @('-NoProfile','-File',$childScript,(Join-Path $testRoot 'failed child.txt'),'one','two','23') 'expected child failure'}catch{$nonzero=$_.Exception};if($null-eq$nonzero-or$nonzero.Message-cnotmatch'^expected child failure Native process exit code: 23\.$'){throw 'CUDA native invocation helper swallowed or misreported a child failure.'}
 Write-Output "Windows CUDA strict reader rejected $index semantic mutations plus duplicate/digest cases."
 Write-Output "Windows CUDA v1/v2 contracts passed, including $v2Index v2 mutations, exact u64 accounting, and fail-closed publication."
 Write-Output 'Windows CUDA native invocation waits, preserves argument boundaries, and rejects child failures.'
 Write-Output 'Windows CUDA runner hardened static contracts passed.'
} finally {Remove-Item -LiteralPath $testRoot -Recurse -Force}
