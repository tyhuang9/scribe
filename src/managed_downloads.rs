use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::config;
use crate::models::{
    SttModelInfo, sherpa_model_download_url, vosk_model_download_url, whisper_cpp_download_url,
};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelDownloadProgress {
    pub(crate) model_id: String,
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: Option<u64>,
    pub(crate) bytes_per_second: Option<u64>,
}

pub(crate) fn download_whisper_cpp_model(
    model_name: &str,
    destination: &Path,
    model_id: &str,
    expected_total_bytes: Option<u64>,
    progress: &dyn Fn(ModelDownloadProgress),
) -> Result<PathBuf, String> {
    let url = whisper_cpp_download_url(model_name);
    download_model_file(&url, destination, model_id, expected_total_bytes, progress)
}

pub(crate) fn download_faster_whisper_model(
    runner: &Path,
    model_name: &str,
    destination: &Path,
    model_id: &str,
    expected_total_bytes: Option<u64>,
    progress: &dyn Fn(ModelDownloadProgress),
) -> Result<PathBuf, String> {
    download_runner_model(RunnerModelDownload {
        runner,
        model_name,
        destination,
        model_id,
        expected_total_bytes,
        backend_label: "faster-whisper",
        stdout_label: "faster-whisper stdout",
        valid_install: &config::is_faster_whisper_model_dir,
        parse_path: parse_faster_whisper_download_path,
        progress,
    })
}

pub(crate) fn download_vosk_model(
    runner: &Path,
    model_name: &str,
    destination: &Path,
    model_id: &str,
    expected_total_bytes: Option<u64>,
    progress: &dyn Fn(ModelDownloadProgress),
) -> Result<PathBuf, String> {
    if vosk_model_download_url(model_name).is_none() {
        return Err(format!("unsupported Vosk model download: {model_name}"));
    }

    download_runner_model(RunnerModelDownload {
        runner,
        model_name,
        destination,
        model_id,
        expected_total_bytes,
        backend_label: "Vosk",
        stdout_label: "Vosk stdout",
        valid_install: &config::is_vosk_model_dir,
        parse_path: parse_vosk_download_path,
        progress,
    })
}

pub(crate) fn download_sherpa_model(
    runner: &Path,
    model: &SttModelInfo,
    model_name: &str,
    destination: &Path,
    model_id: &str,
    expected_total_bytes: Option<u64>,
    progress: &dyn Fn(ModelDownloadProgress),
) -> Result<PathBuf, String> {
    if sherpa_model_download_url(model_name).is_none() {
        return Err(format!(
            "unsupported {} model download: {model_name}",
            model.backend
        ));
    }

    let backend = model.backend.as_str();
    download_runner_model(RunnerModelDownload {
        runner,
        model_name,
        destination,
        model_id,
        expected_total_bytes,
        backend_label: backend,
        stdout_label: "sherpa-onnx stdout",
        valid_install: &|path| config::is_valid_model_install_path(model, path),
        parse_path: parse_sherpa_download_path,
        progress,
    })
}

fn download_model_file(
    url: &str,
    destination: &Path,
    model_id: &str,
    expected_total_bytes: Option<u64>,
    progress: &dyn Fn(ModelDownloadProgress),
) -> Result<PathBuf, String> {
    if destination.exists() {
        return Ok(destination.to_path_buf());
    }

    let partial_path = destination.with_extension("bin.partial");
    let result = (|| {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }

        let response = ureq::get(url)
            .call()
            .map_err(|err| format!("request failed for {url}: {err}"))?;
        let total_bytes = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok())
            .or(expected_total_bytes);
        let mut reader = response.into_reader();
        let mut file = fs::File::create(&partial_path)
            .map_err(|err| format!("failed to create {}: {err}", partial_path.display()))?;
        let mut downloaded_bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        let started_at = Instant::now();
        let mut last_progress_at = started_at;

        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|err| format!("download read failed: {err}"))?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|err| format!("failed to write {}: {err}", partial_path.display()))?;
            downloaded_bytes += read as u64;
            let now = Instant::now();
            if now.duration_since(last_progress_at) >= PROGRESS_INTERVAL {
                last_progress_at = now;
                emit_progress(
                    progress,
                    model_id,
                    downloaded_bytes,
                    total_bytes,
                    started_at,
                    now,
                );
            }
        }

        let finished_at = Instant::now();
        emit_progress(
            progress,
            model_id,
            downloaded_bytes,
            total_bytes,
            started_at,
            finished_at,
        );
        file.sync_all()
            .map_err(|err| format!("failed to finish {}: {err}", partial_path.display()))?;
        fs::rename(&partial_path, destination).map_err(|err| {
            format!(
                "failed to move {} to {}: {err}",
                partial_path.display(),
                destination.display()
            )
        })?;
        Ok(destination.to_path_buf())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&partial_path);
    }

    result
}

struct RunnerModelDownload<'a> {
    runner: &'a Path,
    model_name: &'a str,
    destination: &'a Path,
    model_id: &'a str,
    expected_total_bytes: Option<u64>,
    backend_label: &'a str,
    stdout_label: &'a str,
    valid_install: &'a dyn Fn(&Path) -> bool,
    parse_path: fn(&str) -> Option<PathBuf>,
    progress: &'a dyn Fn(ModelDownloadProgress),
}

fn download_runner_model(spec: RunnerModelDownload<'_>) -> Result<PathBuf, String> {
    if prepare_install_destination(spec.destination, spec.backend_label, spec.valid_install)? {
        return Ok(spec.destination.to_path_buf());
    }

    let started_at = Instant::now();
    let mut child = Command::new(spec.runner)
        .args(["download-model", "--model", spec.model_name, "--output"])
        .arg(spec.destination)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run {}: {err}", spec.runner.display()))?;
    let stdout = child.stdout.take().map(read_stream_to_string);
    let stderr = child.stderr.take().map(read_stream_to_string);
    let mut last_progress_at = started_at;
    let mut last_downloaded_bytes = 0_u64;
    let status = loop {
        match child
            .try_wait()
            .map_err(|err| format!("failed to poll {}: {err}", spec.runner.display()))?
        {
            Some(status) => break status,
            None => {
                let now = Instant::now();
                if now.duration_since(last_progress_at) >= PROGRESS_INTERVAL {
                    last_progress_at = now;
                    last_downloaded_bytes =
                        installed_path_size(spec.destination).unwrap_or(last_downloaded_bytes);
                    emit_progress(
                        spec.progress,
                        spec.model_id,
                        last_downloaded_bytes,
                        spec.expected_total_bytes,
                        started_at,
                        now,
                    );
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    };

    let finished_at = Instant::now();
    let downloaded_bytes = installed_path_size(spec.destination).unwrap_or(last_downloaded_bytes);
    emit_progress(
        spec.progress,
        spec.model_id,
        downloaded_bytes,
        spec.expected_total_bytes,
        started_at,
        finished_at,
    );
    let stdout = join_stream_reader(stdout, spec.stdout_label)?;
    let stderr = join_stream_reader(stderr, &format!("{} stderr", spec.backend_label))?;

    if !status.success() {
        let _ = remove_incomplete_destination(spec.destination);
        return Err(format!(
            "{} model download failed with status {}: {}",
            spec.backend_label,
            status,
            stderr.trim()
        ));
    }

    let runner_path = (spec.parse_path)(&stdout).unwrap_or_else(|| spec.destination.to_path_buf());
    if (spec.valid_install)(&runner_path)
        && (runner_path == spec.destination || runner_path.starts_with(spec.destination))
    {
        Ok(runner_path)
    } else if (spec.valid_install)(spec.destination) {
        Ok(spec.destination.to_path_buf())
    } else {
        Err(format!(
            "{} runner finished but did not create a complete model at {}",
            spec.backend_label,
            spec.destination.display()
        ))
    }
}

fn prepare_install_destination(
    destination: &Path,
    backend_label: &str,
    valid_install: &dyn Fn(&Path) -> bool,
) -> Result<bool, String> {
    if destination.exists() {
        if valid_install(destination) {
            return Ok(true);
        }
        remove_incomplete_destination(destination).map_err(|err| {
            format!(
                "failed to replace incomplete {} model at {}: {err}",
                backend_label,
                destination.display()
            )
        })?;
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    Ok(false)
}

fn remove_incomplete_destination(destination: &Path) -> std::io::Result<()> {
    if destination.is_dir() {
        fs::remove_dir_all(destination)
    } else {
        fs::remove_file(destination)
    }
}

fn emit_progress(
    progress: &dyn Fn(ModelDownloadProgress),
    model_id: &str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    started_at: Instant,
    measured_at: Instant,
) {
    progress(ModelDownloadProgress {
        model_id: model_id.to_owned(),
        downloaded_bytes,
        total_bytes,
        bytes_per_second: download_speed(downloaded_bytes, started_at, measured_at),
    });
}

fn read_stream_to_string<R>(mut reader: R) -> JoinHandle<Result<String, String>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = String::new();
        reader
            .read_to_string(&mut output)
            .map_err(|err| format!("failed to read child process output: {err}"))?;
        Ok(output)
    })
}

fn join_stream_reader(
    handle: Option<JoinHandle<Result<String, String>>>,
    label: &str,
) -> Result<String, String> {
    match handle {
        Some(handle) => handle
            .join()
            .map_err(|_| format!("{label} reader panicked"))?,
        None => Ok(String::new()),
    }
}

fn download_speed(downloaded_bytes: u64, started_at: Instant, measured_at: Instant) -> Option<u64> {
    let elapsed = measured_at.duration_since(started_at).as_secs_f64();
    if downloaded_bytes == 0 || elapsed <= 0.0 {
        None
    } else {
        Some((downloaded_bytes as f64 / elapsed).round() as u64)
    }
}

fn installed_path_size(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }

    let metadata =
        fs::metadata(path).map_err(|err| format!("failed to inspect {}: {err}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut total = 0_u64;
    let entries =
        fs::read_dir(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|err| format!("failed to read entry in {}: {err}", path.display()))?;
        total = total.saturating_add(installed_path_size(&entry.path())?);
    }
    Ok(total)
}

#[derive(Debug, Deserialize)]
struct FasterWhisperDownloadOutput {
    path: PathBuf,
}

fn parse_faster_whisper_download_path(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<FasterWhisperDownloadOutput>(line.trim()).ok())
        .map(|output| output.path)
}

#[derive(Debug, Deserialize)]
struct VoskDownloadOutput {
    path: PathBuf,
}

fn parse_vosk_download_path(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<VoskDownloadOutput>(line.trim()).ok())
        .map(|output| output.path)
}

#[derive(Debug, Deserialize)]
struct SherpaDownloadOutput {
    path: PathBuf,
}

fn parse_sherpa_download_path(stdout: &str) -> Option<PathBuf> {
    stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<SherpaDownloadOutput>(line.trim()).ok())
        .map(|output| output.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "scribe-managed-downloads-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[test]
    fn parses_faster_whisper_download_path_from_runner_json() {
        let path = parse_faster_whisper_download_path(
            r#"{"model":"small.en","path":"/tmp/scribe-models/faster-whisper/small"}"#,
        )
        .unwrap();

        assert_eq!(
            path,
            PathBuf::from("/tmp/scribe-models/faster-whisper/small")
        );
    }

    #[test]
    fn parses_vosk_download_path_from_runner_json() {
        let path = parse_vosk_download_path(
            r#"{"model":"vosk-model-small-en-us-0.15","path":"/tmp/scribe-models/vosk/vosk_small_en"}"#,
        )
        .unwrap();

        assert_eq!(path, PathBuf::from("/tmp/scribe-models/vosk/vosk_small_en"));
    }

    #[test]
    fn parses_sherpa_download_path_from_runner_json() {
        let path = parse_sherpa_download_path(
            r#"{"model":"sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27","path":"/tmp/scribe-models/moonshine/moonshine"}"#,
        )
        .unwrap();

        assert_eq!(
            path,
            PathBuf::from("/tmp/scribe-models/moonshine/moonshine")
        );
    }

    #[test]
    fn prepare_install_destination_returns_existing_valid_install() {
        let destination = unique_temp_path("valid-existing");
        fs::create_dir_all(&destination).unwrap();

        let result = prepare_install_destination(&destination, "test", &|path| path.is_dir());

        assert_eq!(result, Ok(true));
        assert!(destination.exists());

        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn prepare_install_destination_removes_incomplete_path() {
        let parent = unique_temp_path("incomplete-parent");
        let destination = parent.join("model");
        fs::create_dir_all(&parent).unwrap();
        fs::write(&destination, b"incomplete").unwrap();

        let result = prepare_install_destination(&destination, "test", &|_| false);

        assert_eq!(result, Ok(false));
        assert!(parent.exists());
        assert!(!destination.exists());

        fs::remove_dir_all(parent).unwrap();
    }
}
