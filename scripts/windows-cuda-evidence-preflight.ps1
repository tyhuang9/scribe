[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'windows-vulkan-evidence-preflight.ps1')

function Assert-ScribeCudaJsonProperties($Element,[string[]]$Expected,[string]$Label) {
    $actual=@($Element.EnumerateObject()|ForEach-Object Name)
    if($actual.Count-ne$Expected.Count -or @($Expected|Where-Object{$_ -cnotin $actual}).Count){throw "$Label has unknown, missing, or duplicate properties."}
}
function Assert-ScribeCudaCanonicalUInt($Element,[string]$Label) {
    $raw=$Element.GetRawText(); if($raw-cnotmatch '\A(?:0|[1-9][0-9]*)\z'){throw "$Label is not a canonical unsigned integer."};$null=[uint64]::Parse($raw,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture)
}

function Assert-ScribeCudaEvidenceReportJson([string]$Utf8Json) {
    $options=[Text.Json.JsonDocumentOptions]@{AllowTrailingCommas=$false;CommentHandling=[Text.Json.JsonCommentHandling]::Disallow};$document=[Text.Json.JsonDocument]::Parse($Utf8Json,$options)
    try {$root=$document.RootElement;$required=@('schema_version','evidence_kind','fixture_only','untrusted','auto_eligible','source_revision','cpu_worker_sha256','pack','model_sha256','wav_sha256','gpu','nvidia_baseline','cold_runs_per_backend','warm_runs_per_backend','cpu','cuda','expected_phrase_present_every_run','normalized_transcript_parity','same_device_internally_verified');Assert-ScribeCudaJsonProperties $root $required 'CUDA root'
        $pack=$root.GetProperty('pack');$gpu=$root.GetProperty('gpu');$base=$root.GetProperty('nvidia_baseline');Assert-ScribeCudaJsonProperties $pack @('id','version','digest','security_epoch','runtime_abi') 'CUDA pack';Assert-ScribeCudaJsonProperties $gpu @('backend','provider','vendor','device_class','driver','memory_total_bytes') 'CUDA GPU';Assert-ScribeCudaJsonProperties $base @('product','driver','memory_total_bytes','memory_used_bytes','gpu_utilization_percent') 'CUDA baseline'
        if($root.GetProperty('schema_version').GetInt32()-ne1-or$root.GetProperty('evidence_kind').GetString()-cne'windows-cuda-fixture-performance'-or-not$root.GetProperty('fixture_only').GetBoolean()-or-not$root.GetProperty('untrusted').GetBoolean()-or$root.GetProperty('auto_eligible').GetBoolean()-or$root.GetProperty('cold_runs_per_backend').GetInt32()-ne5-or$root.GetProperty('warm_runs_per_backend').GetInt32()-ne20-or-not$root.GetProperty('expected_phrase_present_every_run').GetBoolean()-or-not$root.GetProperty('normalized_transcript_parity').GetBoolean()-or-not$root.GetProperty('same_device_internally_verified').GetBoolean()-or$gpu.GetProperty('backend').GetString()-cne'cuda'-or$gpu.GetProperty('provider').GetString()-cne'transcribe-cpp-ggml-cuda'-or$gpu.GetProperty('vendor').GetString()-cne'nvidia'-or$gpu.GetProperty('device_class').GetString()-cne'discrete_gpu'){throw 'CUDA semantic contract failed.'}
        foreach($entry in @(@($pack,'id',128),@($pack,'version',96),@($gpu,'driver',128),@($base,'product',256),@($base,'driver',128))){$text=$entry[0].GetProperty($entry[1]).GetString();if([string]::IsNullOrWhiteSpace($text)-or$text.Length-gt$entry[2]-or$text.IndexOfAny([char[]]@("`r","`n",[char]0,'\','/'))-ge0-or$text.ToCharArray().Where({[int]$_-lt32-or[int]$_-gt126},'First').Count-ne0){throw 'CUDA metadata is unbounded or non-printable.'}}
        foreach($digest in @($root.GetProperty('cpu_worker_sha256').GetString(),$root.GetProperty('model_sha256').GetString(),$root.GetProperty('wav_sha256').GetString(),$pack.GetProperty('digest').GetString())){if($digest-cnotmatch'\A[0-9a-f]{64}\z'){throw 'CUDA digest is noncanonical.'}};if($root.GetProperty('source_revision').GetString()-cnotmatch'\A[0-9a-f]{40}\z'){throw 'CUDA revision is noncanonical.'}
        foreach($backend in @('cpu','cuda')){$b=$root.GetProperty($backend);Assert-ScribeCudaJsonProperties $b @('cold','warm') "CUDA $backend";foreach($phase in @('cold','warm')){$set=$b.GetProperty($phase);$count=if($phase-ceq'cold'){5}else{20};Assert-ScribeCudaJsonProperties $set @('end_to_end_ms','end_to_end','backend_processing_ms','backend_processing','model_load_ms','model_load') "CUDA $backend $phase";foreach($series in @('end_to_end_ms','backend_processing_ms')){if($set.GetProperty($series).GetArrayLength()-ne$count){throw 'CUDA timing cardinality mismatch.'};foreach($v in $set.GetProperty($series).EnumerateArray()){Assert-ScribeCudaCanonicalUInt $v 'CUDA timing'}};foreach($stat in @('end_to_end','backend_processing')){Assert-ScribeCudaJsonProperties $set.GetProperty($stat) @('p50_ms','p95_ms') 'CUDA stats';foreach($n in @('p50_ms','p95_ms')){Assert-ScribeCudaCanonicalUInt $set.GetProperty($stat).GetProperty($n) 'CUDA stat'}};if($phase-ceq'cold'){if($set.GetProperty('model_load_ms').GetArrayLength()-ne5){throw 'CUDA cold model cardinality mismatch.'};foreach($v in $set.GetProperty('model_load_ms').EnumerateArray()){Assert-ScribeCudaCanonicalUInt $v 'CUDA model-load timing'};Assert-ScribeCudaJsonProperties $set.GetProperty('model_load') @('p50_ms','p95_ms') 'CUDA model stats';foreach($n in @('p50_ms','p95_ms')){Assert-ScribeCudaCanonicalUInt $set.GetProperty('model_load').GetProperty($n) 'CUDA model-load statistic'}}elseif($set.GetProperty('model_load_ms').ValueKind-ne[Text.Json.JsonValueKind]::Null-or$set.GetProperty('model_load').ValueKind-ne[Text.Json.JsonValueKind]::Null){throw 'CUDA warm model fields must be null.'}}}
        foreach($value in @($root.GetProperty('schema_version'),$root.GetProperty('cold_runs_per_backend'),$root.GetProperty('warm_runs_per_backend'),$pack.GetProperty('security_epoch'),$pack.GetProperty('runtime_abi'),$gpu.GetProperty('memory_total_bytes'),$base.GetProperty('memory_total_bytes'),$base.GetProperty('memory_used_bytes'),$base.GetProperty('gpu_utilization_percent'))){Assert-ScribeCudaCanonicalUInt $value 'CUDA integer'}
        $total=$base.GetProperty('memory_total_bytes').GetUInt64();if($gpu.GetProperty('memory_total_bytes').GetUInt64()-eq0-or$total-eq0-or$base.GetProperty('memory_used_bytes').GetUInt64()-gt($total/4)-or$base.GetProperty('gpu_utilization_percent').GetUInt64()-gt10){throw 'CUDA idle metadata is invalid.'}
    } finally {$document.Dispose()}
}

function Read-ScribeVerifiedCudaEvidenceReport {
    [CmdletBinding()]
    param([Parameter(Mandatory)][string]$LiteralPath,[Parameter(Mandatory)][ValidatePattern('\A[0-9a-f]{64}\z')][string]$ExpectedSha256)
    $fullPath=[IO.Path]::GetFullPath($LiteralPath); $root=[IO.Path]::GetDirectoryName($fullPath); $leaf=[IO.Path]::GetFileName($fullPath)
    $binding=[ScribeEvidenceNative.BoundPendingFile]::OpenPublished($root,$fullPath,$leaf,1MB)
    try { $verified=$binding.ReadAllAndVerify($ExpectedSha256)
        Assert-ScribeCudaEvidenceReportJson $verified.Utf8Json
        return $verified
    } finally { if($null-ne$binding){$binding.Dispose()} }
}

function Complete-ScribeCudaEvidencePendingReport([string]$PendingPath,[string]$FinalPath,[string]$EvidenceRoot,[string]$PendingLeaf,[string]$FinalLeaf,[System.Exception]$PrimaryFailure,[System.Exception[]]$SecondaryFailures) {
    $failures=[Collections.Generic.List[System.Exception]]::new();foreach($failure in @($SecondaryFailures)){if($null-ne$failure){$failures.Add($failure)}}
    if($null-ne$PrimaryFailure -or $failures.Count){try{Remove-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf}catch{$failures.Add($_.Exception)};if($null-ne$PrimaryFailure){Add-ScribeEvidenceSecondaryFailures $PrimaryFailure $failures.ToArray();throw $PrimaryFailure};$first=$failures[0];Add-ScribeEvidenceSecondaryFailures $first @($failures.ToArray()|Select-Object -Skip 1);throw $first}
    try {
        $binding=[ScribeEvidenceNative.BoundPendingFile]::Open($EvidenceRoot,$PendingPath,$PendingLeaf,1MB,$false,$false)
        try {
            $read=$binding.ReadAllAndHash();$decoded=[Text.UTF8Encoding]::new($false,$true).GetString($read.Bytes)
            Assert-ScribeCudaEvidenceReportJson $decoded
            $final=$binding.GetFinalPath($FinalLeaf);if(-not[string]::Equals([IO.Path]::GetFullPath($FinalPath),$final,[StringComparison]::OrdinalIgnoreCase)){throw 'Final CUDA evidence path does not match its bound directory and leaf.'}
            $result=[pscustomobject]@{Path=$final;Digest=$read.Sha256;Identity=$binding.Identity};$binding.RenameNoReplace($FinalLeaf);return $result
        } finally {if($null-ne$binding){$binding.Dispose()}}
    } catch {$publishFailure=$_.Exception;try{Remove-ScribeEvidencePendingReport $PendingPath $EvidenceRoot $PendingLeaf}catch{Add-ScribeEvidenceSecondaryFailures $publishFailure @($_.Exception)};throw $publishFailure}
}
