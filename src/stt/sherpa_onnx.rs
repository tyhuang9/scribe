use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::config::{self, AppConfig};
use crate::models::{SttModelInfo, TranscriptResult, TranscriptSegment, default_model_catalog};

use super::SttBackend;

const MIN_SHERPA_ONNX_RUNNER_REVISION: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SherpaRuntimeSpec {
    pub backend: &'static str,
    pub runtime_id: &'static str,
    pub wrapper_name: &'static str,
    pub dev_env: &'static str,
}

const RUNTIME_SPECS: &[SherpaRuntimeSpec] = &[
    SherpaRuntimeSpec {
        backend: "sherpa-onnx",
        runtime_id: "sherpa_onnx",
        wrapper_name: "scribe-sherpa-onnx",
        dev_env: "SCRIBE_SHERPA_ONNX_CLI",
    },
    SherpaRuntimeSpec {
        backend: "Moonshine",
        runtime_id: "moonshine",
        wrapper_name: "scribe-moonshine",
        dev_env: "SCRIBE_MOONSHINE_CLI",
    },
    SherpaRuntimeSpec {
        backend: "Parakeet",
        runtime_id: "parakeet",
        wrapper_name: "scribe-parakeet",
        dev_env: "SCRIBE_PARAKEET_CLI",
    },
];

pub struct SherpaOnnxBackend {
    backend: String,
    executable_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct RunnerOutput {
    text: String,
    #[serde(default)]
    segments: Vec<RunnerSegment>,
    duration_ms: Option<u128>,
}

#[derive(Debug, Deserialize)]
struct RunnerSegment {
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    text: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    runtime_id: Option<String>,
    runner_revision: Option<u32>,
    versions: Option<HashMap<String, Option<String>>>,
}

impl SherpaOnnxBackend {
    pub fn new(backend: &str, executable_path: Option<PathBuf>) -> Self {
        Self {
            backend: backend.to_owned(),
            executable_path,
        }
    }
}

impl SttBackend for SherpaOnnxBackend {
    fn id(&self) -> &str {
        &self.backend
    }

    fn list_models(&self) -> Vec<SttModelInfo> {
        default_model_catalog()
            .into_iter()
            .filter(|model| model.backend == self.backend)
            .collect()
    }

    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult> {
        let spec = runtime_spec_for_backend(&self.backend)
            .ok_or_else(|| anyhow!("unsupported sherpa-onnx family backend: {}", self.backend))?;
        let executable = self.executable_path.clone().ok_or_else(|| {
            anyhow!(
                "{} runtime is not installed. Install the {} runtime from Models, or set {} for development.",
                spec.backend,
                spec.backend,
                spec.dev_env
            )
        })?;
        let model_path = model
            .local_path
            .clone()
            .ok_or_else(|| anyhow!("download {} before transcribing", model.name))?;

        if !executable.exists() {
            return Err(anyhow!(
                "{} runner does not exist: {}",
                spec.backend,
                executable.display()
            ));
        }
        if !model_path.exists() {
            return Err(anyhow!(
                "model directory does not exist for {}: {}",
                model.name,
                model_path.display()
            ));
        }
        if !config::is_valid_model_install_path(&model, &model_path) {
            return Err(anyhow!(
                "{} model directory is incomplete for {}: {}. Reinstall this model from Models.",
                spec.backend,
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
            .args(sherpa_onnx_args(spec.backend, &model_path, &audio_path))
            .output()
            .with_context(|| format!("failed to run {}", executable.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!(
                "{} failed with status {}\n{}",
                spec.backend,
                output.status,
                runner_error_message(&stderr)
            ));
        }

        let parsed = parse_runner_output(&stdout, spec.backend)?;
        let segments = parsed
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text,
            })
            .collect::<Vec<_>>();
        let text = parsed.text;
        let segments = if segments.is_empty() {
            vec![TranscriptSegment {
                start_ms: None,
                end_ms: None,
                text: text.clone(),
            }]
        } else {
            segments
        };

        Ok(TranscriptResult {
            model_id: model.id,
            model_name: model.name,
            backend: spec.backend.to_owned(),
            segments,
            text,
            duration_ms: parsed
                .duration_ms
                .or_else(|| Some(started.elapsed().as_millis())),
            stdout,
            stderr,
        })
    }
}

pub fn resolve_executable_for_backend(config: &AppConfig, backend: &str) -> Option<PathBuf> {
    let spec = runtime_spec_for_backend(backend)?;
    resolve_executable_from_candidates(
        spec.runtime_id,
        bundled_runtime_root(),
        managed_runtime_roots(config, backend),
        dev_runtime_paths(spec),
    )
}

fn bundled_runtime_root() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn managed_runtime_roots(config: &AppConfig, backend: &str) -> Vec<PathBuf> {
    [config::managed_runtime_path(config, backend)]
        .into_iter()
        .flatten()
        .collect()
}

fn dev_runtime_paths(spec: SherpaRuntimeSpec) -> Vec<PathBuf> {
    [env::var_os(spec.dev_env).map(PathBuf::from)]
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) fn resolve_executable_from_candidates(
    runtime_id: &str,
    bundled_roots: impl IntoIterator<Item = PathBuf>,
    managed_roots: impl IntoIterator<Item = PathBuf>,
    dev_paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let spec = runtime_spec_for_runtime_id(runtime_id)?;
    first_existing_path(
        spec,
        bundled_roots
            .into_iter()
            .flat_map(|root| runtime_candidates(spec, &root))
            .chain(
                managed_roots
                    .into_iter()
                    .flat_map(|root| runtime_candidates(spec, &root)),
            )
            .chain(dev_paths),
    )
}

fn runtime_candidates(spec: SherpaRuntimeSpec, root: &Path) -> Vec<PathBuf> {
    if root.as_os_str().is_empty() {
        return Vec::new();
    }
    if root.is_file() {
        return vec![root.to_path_buf()];
    }

    wrapper_names(spec)
        .into_iter()
        .flat_map(|binary_name| {
            [
                root.join("runtimes")
                    .join(spec.runtime_id)
                    .join("bin")
                    .join(&binary_name),
                root.join("bin").join(&binary_name),
                root.join(&binary_name),
            ]
        })
        .collect()
}

fn wrapper_names(spec: SherpaRuntimeSpec) -> Vec<String> {
    if cfg!(windows) {
        vec![
            format!("{}.exe", spec.wrapper_name),
            format!("{}.bat", spec.wrapper_name),
        ]
    } else {
        vec![spec.wrapper_name.to_owned()]
    }
}

fn first_existing_path(
    spec: SherpaRuntimeSpec,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let mut seen = Vec::new();
    for path in paths {
        if path.as_os_str().is_empty() || seen.iter().any(|seen_path| seen_path == &path) {
            continue;
        }
        seen.push(path.clone());
        if is_runtime_usable_for_spec(spec, &path) {
            return Some(path);
        }
    }
    None
}

pub(crate) fn is_sherpa_family_runtime_usable(runtime_id: &str, path: &Path) -> bool {
    runtime_spec_for_runtime_id(runtime_id)
        .is_some_and(|spec| is_runtime_usable_for_spec(spec, path))
}

fn is_runtime_usable_for_spec(spec: SherpaRuntimeSpec, path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    if !is_packaged_runner_path(spec, path) {
        return true;
    }
    let Some(runtime_root) = packaged_runtime_root(path) else {
        return false;
    };
    runtime_root
        .join("bin")
        .join("sherpa_onnx_runner.py")
        .is_file()
        && runtime_root.join(venv_python_relative_path()).is_file()
        && manifest_has_supported_runner(spec, &runtime_root)
}

fn is_packaged_runner_path(spec: SherpaRuntimeSpec, path: &Path) -> bool {
    path.parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|name| name == "bin")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| wrapper_names(spec).iter().any(|runner| runner == name))
}

fn packaged_runtime_root(path: &Path) -> Option<PathBuf> {
    path.parent()?.parent().map(Path::to_path_buf)
}

fn venv_python_relative_path() -> &'static Path {
    if cfg!(windows) {
        Path::new("venv/Scripts/python.exe")
    } else {
        Path::new("venv/bin/python")
    }
}

fn manifest_has_supported_runner(spec: SherpaRuntimeSpec, runtime_root: &Path) -> bool {
    fs::read_to_string(runtime_root.join("runtime-manifest.json"))
        .ok()
        .and_then(|contents| serde_json::from_str::<RuntimeManifest>(&contents).ok())
        .is_some_and(|manifest| {
            manifest
                .runner_revision
                .is_some_and(|revision| revision >= MIN_SHERPA_ONNX_RUNNER_REVISION)
                && manifest.runtime_id.as_deref() == Some(spec.runtime_id)
                && manifest_has_numpy(&manifest)
        })
}

fn manifest_has_numpy(manifest: &RuntimeManifest) -> bool {
    manifest
        .versions
        .as_ref()
        .and_then(|versions| versions.get("numpy"))
        .and_then(|version| version.as_deref())
        .is_some_and(|version| !version.trim().is_empty())
}

pub(crate) fn sherpa_onnx_args(
    backend: &str,
    model_path: &Path,
    audio_path: &Path,
) -> Vec<OsString> {
    vec![
        OsString::from("transcribe"),
        OsString::from("--backend"),
        OsString::from(backend),
        OsString::from("--model"),
        model_path.as_os_str().to_owned(),
        OsString::from("--audio"),
        audio_path.as_os_str().to_owned(),
    ]
}

fn parse_runner_output(stdout: &str, backend: &str) -> Result<RunnerOutput> {
    serde_json::from_str(stdout.trim())
        .with_context(|| format!("failed to parse {backend} JSON output"))
}

fn runner_error_message(stderr: &str) -> String {
    #[derive(Deserialize)]
    struct RunnerError {
        error: String,
    }

    stderr
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<RunnerError>(line).ok())
        .map(|payload| payload.error)
        .unwrap_or_else(|| stderr.trim().to_owned())
}

fn runtime_spec_for_backend(backend: &str) -> Option<SherpaRuntimeSpec> {
    RUNTIME_SPECS
        .iter()
        .copied()
        .find(|spec| spec.backend == backend)
}

fn runtime_spec_for_runtime_id(runtime_id: &str) -> Option<SherpaRuntimeSpec> {
    RUNTIME_SPECS
        .iter()
        .copied()
        .find(|spec| spec.runtime_id == runtime_id)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn args_include_backend_model_and_audio_paths() {
        let args = sherpa_onnx_args(
            "Moonshine",
            Path::new("/models/moonshine"),
            Path::new("/tmp/audio.wav"),
        )
        .into_iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--backend", "Moonshine"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "/models/moonshine"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--audio", "/tmp/audio.wav"])
        );
    }

    #[test]
    fn parses_runner_json() {
        let parsed = parse_runner_output(
            r#"{"text":"hello","segments":[{"start_ms":0,"end_ms":500,"text":"hello"}],"duration_ms":42}"#,
            "sherpa-onnx",
        )
        .unwrap();

        assert_eq!(parsed.text, "hello");
        assert_eq!(parsed.segments.len(), 1);
        assert_eq!(parsed.segments[0].end_ms, Some(500));
        assert_eq!(parsed.duration_ms, Some(42));
    }

    #[test]
    fn missing_runtime_mentions_backend_and_dev_env() {
        let mut model = default_model_catalog()
            .into_iter()
            .find(|model| model.backend == "Parakeet")
            .unwrap();
        model.local_path = Some(PathBuf::from("/tmp/scribe-missing-parakeet-model"));

        let err = SherpaOnnxBackend::new("Parakeet", None)
            .transcribe(PathBuf::from("/tmp/scribe-audio.wav"), model)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Parakeet runtime is not installed"));
        assert!(err.contains("SCRIBE_PARAKEET_CLI"));
    }

    #[test]
    fn resolver_prefers_bundled_then_managed_then_dev_runtime() {
        let root = temp_root("resolver-priority");
        let bundled_root = root.join("bundled");
        let managed_root = root.join("managed");
        let dev_runtime = root.join("dev").join("scribe-sherpa-dev");
        let bundled_runtime = write_packaged_runtime(&bundled_root, "sherpa_onnx");
        let _managed_runtime = write_packaged_runtime(&managed_root, "sherpa_onnx");
        fs::create_dir_all(dev_runtime.parent().unwrap()).unwrap();
        fs::write(&dev_runtime, b"dev sherpa runtime").unwrap();

        let resolved = resolve_executable_from_candidates(
            "sherpa_onnx",
            [bundled_root],
            [managed_root],
            [dev_runtime],
        );

        assert_eq!(resolved, Some(bundled_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_skips_broken_packaged_runtime_before_dev_runtime() {
        let root = temp_root("resolver-broken");
        let managed_root = root.join("managed");
        let spec = runtime_spec_for_runtime_id("moonshine").unwrap();
        let broken_runtime = managed_root.join("bin").join(&wrapper_names(spec)[0]);
        let dev_runtime = root.join("dev").join("scribe-moonshine-dev");
        fs::create_dir_all(broken_runtime.parent().unwrap()).unwrap();
        fs::create_dir_all(dev_runtime.parent().unwrap()).unwrap();
        fs::write(&broken_runtime, b"broken Moonshine runtime").unwrap();
        fs::write(&dev_runtime, b"dev Moonshine runtime").unwrap();

        let resolved = resolve_executable_from_candidates(
            "moonshine",
            [],
            [managed_root],
            [dev_runtime.clone()],
        );

        assert_eq!(resolved, Some(dev_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_skips_packaged_runtime_without_numpy_before_dev_runtime() {
        let root = temp_root("resolver-missing-numpy");
        let managed_root = root.join("managed");
        let broken_runtime = write_packaged_runtime_with_manifest(
            &managed_root,
            "parakeet",
            r#"{"runtime_id":"parakeet","runner_revision":2,"versions":{"numpy":null}}"#,
        );
        let dev_runtime = root.join("dev").join("scribe-parakeet-dev");
        fs::create_dir_all(dev_runtime.parent().unwrap()).unwrap();
        fs::write(&dev_runtime, b"dev Parakeet runtime").unwrap();

        let resolved = resolve_executable_from_candidates(
            "parakeet",
            [],
            [managed_root],
            [dev_runtime.clone()],
        );

        assert_ne!(resolved, Some(broken_runtime));
        assert_eq!(resolved, Some(dev_runtime));
        let _ = fs::remove_dir_all(root);
    }

    fn temp_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "scribe-sherpa-runtime-{name}-{}",
            std::process::id()
        ))
    }

    fn write_packaged_runtime(root: &Path, runtime_id: &str) -> PathBuf {
        write_packaged_runtime_with_manifest(
            root,
            runtime_id,
            &format!(
                r#"{{"runtime_id":"{runtime_id}","runner_revision":2,"versions":{{"numpy":"2.3.2"}}}}"#
            ),
        )
    }

    fn write_packaged_runtime_with_manifest(
        root: &Path,
        runtime_id: &str,
        manifest_contents: &str,
    ) -> PathBuf {
        let spec = runtime_spec_for_runtime_id(runtime_id).unwrap();
        let executable = root.join("bin").join(if cfg!(windows) {
            format!("{}.bat", spec.wrapper_name)
        } else {
            spec.wrapper_name.to_owned()
        });
        let runner = root.join("bin").join("sherpa_onnx_runner.py");
        let manifest = root.join("runtime-manifest.json");
        let python = root.join(venv_python_relative_path());
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(&executable, b"sherpa runtime").unwrap();
        fs::write(runner, b"runner").unwrap();
        fs::write(manifest, manifest_contents).unwrap();
        fs::write(python, b"python").unwrap();
        executable
    }
}
