//! Test-only local diagnosis for the known public CPU/CUDA parity mismatch.
//!
//! This module is compiled only from `#[cfg(test)]` in its parent. It has no
//! production route, report field, worker authority, or persistent transcript
//! output. The ignored hardware test emits normalized public-fixture text only
//! after the ordinary collector has completed and every metadata comparison has
//! succeeded.

use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ};

use super::{
    CommandOptions, MAX_MODEL_BYTES, MAX_REPORT_BYTES, MAX_WAV_BYTES, canonical_sha256, hex,
    reject_reparse_components, require_retained_digest, run_single_capture, verify_input,
};

const FIXTURE_DIRECTORY: &str = "public-parity-fixture";
const MODEL_FILENAME: &str = "whisper-base.en-Q8_0.gguf";
const WAV_FILENAME: &str = "jfk.wav";
const FAILED_REPORT_FILENAME: &str = "failed-observation.json";
const DIAGNOSTIC_REPORT_FILENAME: &str = "diagnostic-observation.json";
const PUBLIC_MODEL_SHA256: &str =
    "3b46ca40bccbf7609c68d88a36d96077a04ca7c87f2060ede06f129fac3e7652";
const PUBLIC_WAV_SHA256: &str = "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e";
const PUBLIC_CPU_TRANSCRIPT_SHA256: &str =
    "eab257991a173cc2fa69e4ef034d2d27a9b435c2f00976629a90a9559d5c5e67";
const PUBLIC_GPU_TRANSCRIPT_SHA256: &str =
    "4dd743f8668427f2825c4492d6f017b2bb8d142ffeb63f5e52f343f02c741d80";
const CUDA_BACKEND: &str = "cuda";
const CUDA_PACK_ID: &str = "scribe-cuda-windows-x64";
const CUDA_PROVIDER: &str = "transcribe-cpp-ggml-cuda";
const CUDA_PACK_VERSION: &str = "frozen-r-c03e125";
const CUDA_PACK_SHA256: &str = "fc1f003b290209ebdd285820befcc74ddaa9d4582e1ae8ac86e983779f72db70";
const CUDA_PACK_SECURITY_EPOCH: u64 = 1;
const CUDA_RUNTIME_ABI: u64 = 1;
const MAX_NORMALIZED_TRANSCRIPT_BYTES: usize = 8 * 1024;
const MAX_RENDERED_DIAGNOSTIC_BYTES: usize = 256 * 1024;

std::thread_local! {
    static NORMALIZED_TRANSCRIPTS: RefCell<Option<TranscriptSink>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct TranscriptSink {
    cpu: Option<String>,
    gpu: Option<String>,
}

struct CapturedTranscripts {
    cpu: String,
    gpu: String,
}

struct TranscriptSinkGuard {
    active: bool,
}

impl TranscriptSinkGuard {
    fn enable() -> Result<Self> {
        NORMALIZED_TRANSCRIPTS.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                bail!("public fixture transcript sink is already active")
            }
            *slot = Some(TranscriptSink::default());
            Ok(Self { active: true })
        })
    }

    fn take(mut self) -> Result<CapturedTranscripts> {
        let sink = NORMALIZED_TRANSCRIPTS.with(|slot| slot.borrow_mut().take());
        self.active = false;
        let sink = sink.ok_or_else(|| anyhow!("public fixture transcript sink was not active"))?;
        let cpu = sink
            .cpu
            .ok_or_else(|| anyhow!("public fixture transcript sink omitted CPU output"))?;
        let gpu = sink
            .gpu
            .ok_or_else(|| anyhow!("public fixture transcript sink omitted GPU output"))?;
        Ok(CapturedTranscripts { cpu, gpu })
    }
}

impl Drop for TranscriptSinkGuard {
    fn drop(&mut self) {
        if self.active {
            NORMALIZED_TRANSCRIPTS.with(|slot| {
                *slot.borrow_mut() = None;
            });
        }
    }
}

pub(super) fn record_normalized_transcript(gpu: bool, normalized: &str) -> Result<()> {
    NORMALIZED_TRANSCRIPTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(sink) = slot.as_mut() else {
            return Ok(());
        };
        if normalized.len() > MAX_NORMALIZED_TRANSCRIPT_BYTES {
            bail!("public fixture transcript sink exceeded its byte limit")
        }
        let target = if gpu { &mut sink.gpu } else { &mut sink.cpu };
        if target.is_some() {
            bail!("public fixture transcript sink received a duplicate target")
        }
        *target = Some(normalized.to_owned());
        Ok(())
    })
}

#[derive(Eq, PartialEq)]
struct GpuBinding {
    backend: String,
    provider: String,
    stable_device: String,
    driver: String,
    device_class: String,
    vendor: String,
    memory_total_bytes: u64,
    pack_id: String,
    pack_version: String,
    pack_sha256: String,
    pack_security_epoch: u64,
    runtime_abi: u64,
}

struct ReportMetadata {
    model_sha256: String,
    wav_sha256: String,
    cpu_transcript_sha256: String,
    gpu_transcript_sha256: String,
    transcript_parity: bool,
    gpu_binding: GpuBinding,
}

fn fixture_root() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("could not locate diagnostic test binary")?;
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow!("diagnostic test binary has no parent directory"))?;
    let root = parent.join(FIXTURE_DIRECTORY);
    reject_reparse_components(&root)?;
    let metadata = std::fs::symlink_metadata(&root)
        .context("public parity fixture directory is unavailable beside the test binary")?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("public parity fixture directory is not a physical directory")
    }
    Ok(root)
}

fn verify_public_inputs(model: &Path, wav: &Path) -> Result<()> {
    let mut model = verify_input(
        model,
        PUBLIC_MODEL_SHA256,
        MAX_MODEL_BYTES,
        "public fixture model",
    )?;
    require_retained_digest(&mut model)?;
    let mut wav = verify_input(wav, PUBLIC_WAV_SHA256, MAX_WAV_BYTES, "public fixture WAV")?;
    require_retained_digest(&mut wav)?;
    Ok(())
}

fn read_bounded_regular_report(path: &Path, label: &str) -> Result<Vec<u8>> {
    reject_reparse_components(path)?;
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("{label} is unavailable"))?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.len() == 0
        || metadata.len() > MAX_REPORT_BYTES as u64
    {
        bail!("{label} is not a bounded regular non-reparse file")
    }
    let mut options = OpenOptions::new();
    options.read(true).share_mode(FILE_SHARE_READ);
    let file = options
        .open(path)
        .with_context(|| format!("could not read {label}"))?;
    read_bounded_file(file, metadata.len(), label)
}

fn read_bounded_file(mut file: File, expected_len: u64, label: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(expected_len as usize);
    file.by_ref()
        .take(MAX_REPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {label}"))?;
    if bytes.len() > MAX_REPORT_BYTES || bytes.len() as u64 != expected_len {
        bail!("{label} changed or exceeded its byte bound while being read")
    }
    Ok(bytes)
}

fn report_metadata(bytes: &[u8], label: &str) -> Result<ReportMetadata> {
    let value: Value = serde_json::from_slice(bytes)
        .with_context(|| format!("{label} is not valid JSON metadata"))?;
    reject_raw_transcript_fields(&value, label)?;
    let report = value
        .as_object()
        .ok_or_else(|| anyhow!("{label} must be a JSON object"))?;
    if required_string(report, "kind", label)? != "windows_gpu_capture_observation" {
        bail!("{label} is not a Windows GPU capture observation")
    }
    let inputs = required_object(report, "inputs", label)?;
    let model_sha256 = required_sha256(inputs, "model_sha256", label)?;
    let wav_sha256 = required_sha256(inputs, "wav_sha256", label)?;
    let cpu = required_object(report, "cpu", label)?;
    let gpu = required_object(report, "gpu", label)?;
    let gpu_identity = required_object(report, "gpu_identity", label)?;
    let transcript_parity = report
        .get("transcript_parity")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("{label} omitted transcript parity metadata"))?;
    Ok(ReportMetadata {
        model_sha256,
        wav_sha256,
        cpu_transcript_sha256: required_sha256(cpu, "normalized_transcript_sha256", label)?,
        gpu_transcript_sha256: required_sha256(gpu, "normalized_transcript_sha256", label)?,
        transcript_parity,
        gpu_binding: GpuBinding {
            backend: required_selector(gpu_identity, "backend", label)?,
            provider: required_selector(gpu_identity, "provider", label)?,
            stable_device: required_selector(gpu_identity, "stable_device", label)?,
            driver: required_selector(gpu_identity, "driver", label)?,
            device_class: required_selector(gpu_identity, "device_class", label)?,
            vendor: required_selector(gpu_identity, "vendor", label)?,
            memory_total_bytes: required_u64(gpu_identity, "memory_total_bytes", label)?,
            pack_id: required_selector(gpu_identity, "pack_id", label)?,
            pack_version: required_selector(gpu_identity, "pack_version", label)?,
            pack_sha256: required_sha256(gpu_identity, "pack_sha256", label)?,
            pack_security_epoch: required_u64(gpu_identity, "pack_security_epoch", label)?,
            runtime_abi: required_u64(gpu_identity, "runtime_abi", label)?,
        },
    })
}

fn reject_raw_transcript_fields(value: &Value, label: &str) -> Result<()> {
    match value {
        Value::Array(values) => {
            for value in values {
                reject_raw_transcript_fields(value, label)?;
            }
        }
        Value::Object(values) => {
            for (field, value) in values {
                if field.contains("transcript")
                    && field != "normalized_transcript_sha256"
                    && field != "transcript_parity"
                {
                    bail!("{label} is not metadata-only transcript output")
                }
                reject_raw_transcript_fields(value, label)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn required_object<'a>(
    value: &'a serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{label} omitted required object metadata"))
}

fn required_string(
    value: &serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<String> {
    let value = value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{label} omitted required string metadata"))?;
    if value.is_empty()
        || value.len() > 1024
        || value != value.trim()
        || value.chars().any(char::is_control)
    {
        bail!("{label} contains noncanonical string metadata")
    }
    Ok(value.to_owned())
}

fn required_selector(
    value: &serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<String> {
    let value = required_string(value, field, label)?;
    if value.len() > 256 {
        bail!("{label} contains oversized selector metadata")
    }
    Ok(value)
}

fn required_sha256(
    value: &serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> Result<String> {
    let value = required_string(value, field, label)?;
    canonical_sha256(&value).with_context(|| format!("{label} contains invalid SHA-256 metadata"))
}

fn required_u64(value: &serde_json::Map<String, Value>, field: &str, label: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{label} omitted required integer metadata"))
}

fn transcript_sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn require_prior_cuda_reproduction_metadata(report: &ReportMetadata) -> Result<()> {
    if report.model_sha256 != PUBLIC_MODEL_SHA256 || report.wav_sha256 != PUBLIC_WAV_SHA256 {
        bail!("failed observation does not describe the pinned public inputs")
    }
    if report.gpu_binding.backend != CUDA_BACKEND
        || report.gpu_binding.pack_id != CUDA_PACK_ID
        || report.gpu_binding.provider != CUDA_PROVIDER
        || report.gpu_binding.pack_version != CUDA_PACK_VERSION
        || report.gpu_binding.pack_sha256 != CUDA_PACK_SHA256
        || report.gpu_binding.pack_security_epoch != CUDA_PACK_SECURITY_EPOCH
        || report.gpu_binding.runtime_abi != CUDA_RUNTIME_ABI
    {
        bail!("failed observation does not describe the exact CUDA pack binding")
    }
    if report.transcript_parity
        || report.cpu_transcript_sha256 == report.gpu_transcript_sha256
        || report.cpu_transcript_sha256 != PUBLIC_CPU_TRANSCRIPT_SHA256
        || report.gpu_transcript_sha256 != PUBLIC_GPU_TRANSCRIPT_SHA256
    {
        bail!("failed observation does not preserve the strict CPU/GPU mismatch")
    }
    Ok(())
}

fn require_reproduction(
    prior: &ReportMetadata,
    current: &ReportMetadata,
    captured: &CapturedTranscripts,
) -> Result<()> {
    if current.model_sha256 != PUBLIC_MODEL_SHA256 || current.wav_sha256 != PUBLIC_WAV_SHA256 {
        bail!("ordinary diagnostic report does not retain the pinned public inputs")
    }
    if current.gpu_binding != prior.gpu_binding {
        bail!("ordinary diagnostic report changed the authenticated CUDA binding")
    }
    let cpu_hash = transcript_sha256(&captured.cpu);
    let gpu_hash = transcript_sha256(&captured.gpu);
    if current.cpu_transcript_sha256 != cpu_hash || current.gpu_transcript_sha256 != gpu_hash {
        bail!("ordinary diagnostic report transcript hashes do not match the bounded sink")
    }
    if current.cpu_transcript_sha256 != prior.cpu_transcript_sha256
        || current.gpu_transcript_sha256 != prior.gpu_transcript_sha256
    {
        bail!("ordinary diagnostic report did not reproduce the prior transcript hashes")
    }
    if current.transcript_parity || captured.cpu == captured.gpu {
        bail!("ordinary diagnostic report unexpectedly passed strict transcript parity")
    }
    Ok(())
}

fn require_report_omits_captured_text(report: &[u8], captured: &CapturedTranscripts) -> Result<()> {
    let report = std::str::from_utf8(report).context("ordinary diagnostic report is not UTF-8")?;
    if report.contains(&captured.cpu) || report.contains(&captured.gpu) {
        bail!("ordinary diagnostic report retained normalized transcript text")
    }
    Ok(())
}

fn first_differing_token<'a>(
    cpu: &'a str,
    gpu: &'a str,
) -> Option<(usize, Option<&'a str>, Option<&'a str>)> {
    let cpu = cpu.split(' ').map(Some).chain(std::iter::once(None));
    let gpu = gpu.split(' ').map(Some).chain(std::iter::once(None));
    for (index, (cpu_token, gpu_token)) in cpu.zip(gpu).enumerate() {
        if cpu_token != gpu_token {
            return Some((index, cpu_token, gpu_token));
        }
    }
    None
}

fn first_differing_character(cpu: &str, gpu: &str) -> Option<(usize, Option<char>, Option<char>)> {
    let cpu = cpu.chars().map(Some).chain(std::iter::once(None));
    let gpu = gpu.chars().map(Some).chain(std::iter::once(None));
    for (index, (cpu_character, gpu_character)) in cpu.zip(gpu).enumerate() {
        if cpu_character != gpu_character {
            return Some((index, cpu_character, gpu_character));
        }
    }
    None
}

fn emit_bounded_diagnostic(captured: &CapturedTranscripts) -> Result<()> {
    let cpu = serde_json::to_string(&captured.cpu).context("could not escape CPU transcript")?;
    let gpu = serde_json::to_string(&captured.gpu).context("could not escape GPU transcript")?;
    let (token_index, cpu_token, gpu_token) =
        first_differing_token(&captured.cpu, &captured.gpu)
            .ok_or_else(|| anyhow!("strict transcript mismatch had no differing token"))?;
    let (character_index, cpu_character, gpu_character) =
        first_differing_character(&captured.cpu, &captured.gpu)
            .ok_or_else(|| anyhow!("strict transcript mismatch had no differing character"))?;
    let cpu_token = serde_json::to_string(&cpu_token).context("could not escape CPU token")?;
    let gpu_token = serde_json::to_string(&gpu_token).context("could not escape GPU token")?;
    let cpu_character =
        serde_json::to_string(&cpu_character).context("could not escape CPU character")?;
    let gpu_character =
        serde_json::to_string(&gpu_character).context("could not escape GPU character")?;
    let output = format!(
        "public fixture normalized CPU ({} UTF-8 bytes): {cpu}\n\
         public fixture normalized GPU ({} UTF-8 bytes): {gpu}\n\
         first differing token #{token_index}: CPU={cpu_token} GPU={gpu_token}\n\
         first differing character #{character_index}: CPU={cpu_character} GPU={gpu_character}\n",
        captured.cpu.len(),
        captured.gpu.len(),
    );
    if output.len() > MAX_RENDERED_DIAGNOSTIC_BYTES {
        bail!("escaped public fixture diagnostic exceeded its byte limit")
    }
    print!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink_is_empty() -> bool {
        NORMALIZED_TRANSCRIPTS.with(|slot| slot.borrow().is_none())
    }

    #[test]
    fn disabled_sink_stores_nothing() {
        record_normalized_transcript(false, "normalized CPU fixture").unwrap();
        record_normalized_transcript(true, "normalized GPU fixture").unwrap();
        assert!(sink_is_empty());
    }

    #[test]
    fn sink_rejects_duplicates_and_overflow_without_truncation() {
        let guard = TranscriptSinkGuard::enable().unwrap();
        record_normalized_transcript(false, "cpu").unwrap();
        assert!(record_normalized_transcript(false, "replacement").is_err());
        record_normalized_transcript(true, "gpu").unwrap();
        let captured = guard.take().unwrap();
        assert_eq!(captured.cpu.len(), 3);
        assert_eq!(captured.gpu.len(), 3);
        assert_eq!(
            transcript_sha256(&captured.cpu),
            format!("{:x}", Sha256::digest(b"cpu"))
        );
        assert_eq!(
            transcript_sha256(&captured.gpu),
            format!("{:x}", Sha256::digest(b"gpu"))
        );

        let guard = TranscriptSinkGuard::enable().unwrap();
        assert!(
            record_normalized_transcript(true, &"x".repeat(MAX_NORMALIZED_TRANSCRIPT_BYTES + 1))
                .is_err()
        );
        drop(guard);
        assert!(sink_is_empty());
    }

    fn fail_after_capture() -> Result<()> {
        let _guard = TranscriptSinkGuard::enable()?;
        record_normalized_transcript(false, "captured before failure")?;
        bail!("forced diagnostic failure")
    }

    #[test]
    fn sink_raii_clears_after_failure() {
        assert!(fail_after_capture().is_err());
        assert!(sink_is_empty());
    }

    fn panic_after_capture() {
        let _guard = TranscriptSinkGuard::enable().unwrap();
        record_normalized_transcript(false, "captured before unwind").unwrap();
        panic!("forced diagnostic unwind")
    }

    #[test]
    fn sink_raii_clears_after_unwind_and_nested_guard_rejection() {
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(panic_after_capture));
        assert!(unwind.is_err());
        assert!(sink_is_empty());

        let outer = TranscriptSinkGuard::enable().unwrap();
        assert!(TranscriptSinkGuard::enable().is_err());
        drop(outer);
        assert!(sink_is_empty());
    }

    #[test]
    fn sink_accepts_exact_multibyte_byte_bound() {
        let exact = "é".repeat(MAX_NORMALIZED_TRANSCRIPT_BYTES / "é".len());
        assert_eq!(exact.len(), MAX_NORMALIZED_TRANSCRIPT_BYTES);
        let guard = TranscriptSinkGuard::enable().unwrap();
        record_normalized_transcript(false, &exact).unwrap();
        record_normalized_transcript(true, &exact).unwrap();
        let captured = guard.take().unwrap();
        assert_eq!(captured.cpu.len(), MAX_NORMALIZED_TRANSCRIPT_BYTES);
        assert_eq!(captured.gpu.len(), MAX_NORMALIZED_TRANSCRIPT_BYTES);
    }

    #[test]
    fn wrong_public_wav_is_rejected_before_sink_activation() {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("scribe-public-fixture-wav-{}", hex(&nonce)));
        std::fs::create_dir(&root).unwrap();
        let wav = root.join(WAV_FILENAME);
        std::fs::write(&wav, b"not the public fixture WAV").unwrap();
        assert!(
            verify_input(&wav, PUBLIC_WAV_SHA256, MAX_WAV_BYTES, "public fixture WAV",).is_err()
        );
        assert!(sink_is_empty());
        std::fs::remove_file(wav).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn captured_normalized_strings_hash_like_ordinary_report() {
        let guard = TranscriptSinkGuard::enable().unwrap();
        record_normalized_transcript(false, "hello world").unwrap();
        record_normalized_transcript(true, "hello wor1d").unwrap();
        let captured = guard.take().unwrap();
        assert_eq!(
            transcript_sha256(&captured.cpu),
            format!("{:x}", Sha256::digest(b"hello world"))
        );
        assert_eq!(
            transcript_sha256(&captured.gpu),
            format!("{:x}", Sha256::digest(b"hello wor1d"))
        );
    }

    fn fixture_binding() -> GpuBinding {
        GpuBinding {
            backend: CUDA_BACKEND.to_owned(),
            provider: "fixture-provider".to_owned(),
            stable_device: "native:luid:0102030405060708".to_owned(),
            driver: "fixture-driver".to_owned(),
            device_class: "discrete_gpu".to_owned(),
            vendor: "nvidia".to_owned(),
            memory_total_bytes: 1,
            pack_id: CUDA_PACK_ID.to_owned(),
            pack_version: "fixture-version".to_owned(),
            pack_sha256: "c".repeat(64),
            pack_security_epoch: 1,
            runtime_abi: 1,
        }
    }

    fn fixture_report(cpu_hash: String, gpu_hash: String) -> ReportMetadata {
        ReportMetadata {
            model_sha256: PUBLIC_MODEL_SHA256.to_owned(),
            wav_sha256: PUBLIC_WAV_SHA256.to_owned(),
            cpu_transcript_sha256: cpu_hash,
            gpu_transcript_sha256: gpu_hash,
            transcript_parity: false,
            gpu_binding: fixture_binding(),
        }
    }

    #[test]
    fn mismatched_ordinary_and_prior_hashes_fail_reproduction() {
        let captured = CapturedTranscripts {
            cpu: "cpu fixture".to_owned(),
            gpu: "gpu fixture".to_owned(),
        };
        let current = fixture_report(
            transcript_sha256(&captured.cpu),
            transcript_sha256(&captured.gpu),
        );
        let prior = fixture_report("a".repeat(64), current.gpu_transcript_sha256.clone());
        assert!(require_reproduction(&prior, &current, &captured).is_err());
    }

    #[test]
    fn first_difference_is_finite_for_equal_text_and_tracks_unicode() {
        assert!(first_differing_token("same text", "same text").is_none());
        assert!(first_differing_character("same text", "same text").is_none());

        let (token_index, _, _) = first_differing_token("alpha écho", "alpha êcho").unwrap();
        let (character_index, _, _) =
            first_differing_character("alpha écho", "alpha êcho").unwrap();
        assert_eq!(token_index, 1);
        assert_eq!(character_index, 6);
    }

    #[test]
    #[ignore = "requires a fresh verified CUDA public-parity-fixture beside the compiled test binary"]
    fn reproduces_public_fixture_cpu_cuda_parity_mismatch() -> Result<()> {
        let root = fixture_root()?;
        let model = root
            .parent()
            .ok_or_else(|| anyhow!("public parity fixture has no bundle parent"))?
            .join(MODEL_FILENAME);
        let wav = root.join(WAV_FILENAME);
        let failed_report = root.join(FAILED_REPORT_FILENAME);
        let output = root.join(DIAGNOSTIC_REPORT_FILENAME);
        verify_public_inputs(&model, &wav)?;
        let prior = report_metadata(
            &read_bounded_regular_report(&failed_report, "failed public observation report")?,
            "failed public observation report",
        )?;
        require_prior_cuda_reproduction_metadata(&prior)?;
        if std::fs::symlink_metadata(&output).is_ok() {
            bail!("diagnostic public observation output already exists")
        }

        let sink = TranscriptSinkGuard::enable()?;
        run_single_capture(CommandOptions {
            model,
            model_sha256: PUBLIC_MODEL_SHA256.to_owned(),
            wav,
            wav_sha256: PUBLIC_WAV_SHA256.to_owned(),
            gpu_pack_id: prior.gpu_binding.pack_id.clone(),
            gpu_backend: prior.gpu_binding.backend.clone(),
            gpu_device: prior.gpu_binding.stable_device.clone(),
            output: output.clone(),
            campaign_power: None,
        })?;
        let captured = sink.take()?;
        let ordinary_report =
            read_bounded_regular_report(&output, "ordinary diagnostic observation report")?;
        let current = report_metadata(&ordinary_report, "ordinary diagnostic observation report")?;
        require_reproduction(&prior, &current, &captured)?;
        require_report_omits_captured_text(&ordinary_report, &captured)?;
        emit_bounded_diagnostic(&captured)?;
        bail!(
            "strict public CPU/CUDA transcript parity remains failed; diagnostic output is not acceptance"
        )
    }
}
