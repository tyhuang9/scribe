use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};

use crate::models::{SttModelInfo, TranscriptResult, TranscriptSegment, default_model_catalog};

use super::SttBackend;

pub struct WhisperCppBackend {
    executable_path: Option<PathBuf>,
}

impl WhisperCppBackend {
    pub fn new(executable_path: Option<PathBuf>) -> Self {
        Self { executable_path }
    }
}

impl SttBackend for WhisperCppBackend {
    fn id(&self) -> &str {
        "whisper.cpp"
    }

    fn list_models(&self) -> Vec<SttModelInfo> {
        default_model_catalog()
            .into_iter()
            .filter(|model| model.backend == "whisper.cpp")
            .collect()
    }

    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult> {
        let executable = self
            .executable_path
            .clone()
            .ok_or_else(|| anyhow!("configure the whisper.cpp executable path first"))?;
        let model_path = model
            .local_path
            .clone()
            .ok_or_else(|| anyhow!("configure the model file path for {}", model.name))?;

        if !executable.exists() {
            return Err(anyhow!(
                "whisper.cpp executable does not exist: {}",
                executable.display()
            ));
        }
        if !model_path.exists() {
            return Err(anyhow!(
                "model file does not exist for {}: {}",
                model.name,
                model_path.display()
            ));
        }
        if !audio_path.exists() {
            return Err(anyhow!(
                "audio file does not exist: {}",
                audio_path.display()
            ));
        }

        let started = Instant::now();
        let output = Command::new(&executable)
            .arg("-m")
            .arg(&model_path)
            .arg("-f")
            .arg(&audio_path)
            .arg("-nt")
            .output()
            .with_context(|| format!("failed to run {}", executable.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!(
                "whisper.cpp failed with status {}\n{}",
                output.status,
                stderr.trim()
            ));
        }

        let text = parse_final_text(&stdout);
        let text = if text.trim().is_empty() {
            stdout.trim().to_owned()
        } else {
            text
        };

        Ok(TranscriptResult {
            model_id: model.id,
            model_name: model.name,
            backend: "whisper.cpp".to_owned(),
            segments: vec![TranscriptSegment {
                start_ms: None,
                end_ms: None,
                text: text.clone(),
            }],
            text,
            duration_ms: Some(started.elapsed().as_millis()),
            stdout,
            stderr,
        })
    }
}

pub(crate) fn parse_final_text(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("whisper_"))
        .map(strip_timestamp_prefix)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_timestamp_prefix(line: &str) -> String {
    if let Some(end) = line.find(']') {
        if line.starts_with('[') && line[..=end].contains("-->") {
            return line[end + 1..].trim().to_owned();
        }
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_final_text_removes_timestamps_and_diagnostics() {
        let stdout = r#"
            whisper_init_from_file_with_params_no_state: loading model
            [00:00:00.000 --> 00:00:01.000]  First sentence.
            [00:00:01.000 --> 00:00:02.000]  Second sentence.
        "#;

        assert_eq!(
            parse_final_text(stdout),
            "First sentence.\nSecond sentence."
        );
    }

    #[test]
    fn parse_final_text_keeps_plain_lines() {
        assert_eq!(parse_final_text("hello world"), "hello world");
    }
}
