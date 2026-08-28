//! Private bridge for the retained whisper.cpp compatibility runtime.
//!
//! Normalized GGUF, receipt-backed ONNX, remote GGUF, and imported GGUF models
//! are owned by the common native runtime and never enter this bridge. The
//! opaque provider handle remains only for legacy GGML/DLL/CLI maintenance
//! until that final compatibility surface is retired.

use std::path::{Path, PathBuf};

use crate::config::{self, AppConfig};
use crate::models::{ModelInstallStatus, ModelRuntimeStatus, SttModelInfo};
use crate::transcription::ModelId;
use crate::{runtime_catalog, stt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderHandle {
    adapter: &'static stt::SttProviderAdapter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DevelopmentPackageSpec {
    pub(crate) script_name: &'static str,
    pub(crate) destination_env: &'static str,
    pub(crate) executable_relative_path: &'static str,
}

pub(crate) fn provider_for_legacy_model(
    config: &AppConfig,
    model: &SttModelInfo,
) -> Option<ProviderHandle> {
    let model_id = ModelId::new(&model.id);
    if crate::model_catalog::normalized_install_artifact(&model_id).is_some()
        || config::remote_gguf_artifact(config, &model.id).is_some()
        || config::imported_gguf_artifact(config, &model.id).is_some()
    {
        return None;
    }
    stt::provider_for_backend(&model.backend).map(|adapter| ProviderHandle { adapter })
}

impl ProviderHandle {
    pub(crate) fn id(self) -> &'static str {
        self.adapter.runtime_id
    }

    pub(crate) fn runtime_install_supported(self) -> bool {
        self.adapter.runtime_install_supported
    }

    pub(crate) fn runtime_status(self, config: &AppConfig) -> ModelRuntimeStatus {
        self.adapter.runtime_status(config)
    }

    pub(crate) fn model_install_status(self, model: &SttModelInfo) -> ModelInstallStatus {
        self.adapter.model_install_status(model)
    }

    pub(crate) fn managed_root(self, config: &AppConfig) -> Option<PathBuf> {
        config::managed_runtime_path(config, self.adapter.backend)
    }

    pub(crate) fn available_version(self) -> Option<&'static str> {
        runtime_catalog::runtime_version_for_runtime_id(self.id())
    }

    pub(crate) fn development_package(self) -> Option<DevelopmentPackageSpec> {
        runtime_catalog::development_runtime_spec(self.id()).map(|spec| DevelopmentPackageSpec {
            script_name: spec.script_name,
            destination_env: spec.destination_env,
            executable_relative_path: spec.executable_relative_path,
        })
    }

    pub(crate) fn resolve_entrypoint(
        self,
        roots: impl IntoIterator<Item = PathBuf>,
    ) -> Option<PathBuf> {
        runtime_catalog::resolve_runtime_entrypoint(self.id(), roots)
    }
}

pub(crate) fn record_selected_provider(config: &mut AppConfig, model: &SttModelInfo) {
    config.general.last_used_backend.clone_from(&model.backend);
}

pub(crate) fn primary_runtime_entrypoint(config: &AppConfig) -> Option<PathBuf> {
    stt::whisper_cpp::resolve_whisper_cpp_executable(config)
}

pub(crate) fn primary_bundled_runtime_package_root() -> Option<PathBuf> {
    stt::whisper_cpp::bundled_runtime_package_root()
}

pub(crate) fn model_download_total_bytes(model: &SttModelInfo) -> Option<u64> {
    runtime_catalog::model_download_total_bytes(&model.id)
}

pub(crate) fn entrypoint_is_usable(provider_id: &str, path: &Path) -> bool {
    runtime_catalog::runtime_entrypoint_is_usable(provider_id, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_models_cannot_resolve_the_legacy_provider() {
        let config = AppConfig::default();
        let catalog = crate::models::default_model_catalog();
        for model in &catalog {
            assert!(
                provider_for_legacy_model(&config, model).is_none(),
                "normalized model {} resolved a compatibility provider",
                model.id
            );
        }

        let mut receipt_model = catalog
            .iter()
            .find(|model| model.id == "moonshine-tiny-en-int8-onnx")
            .unwrap()
            .clone();
        receipt_model.backend = "whisper.cpp".to_owned();
        assert!(provider_for_legacy_model(&config, &receipt_model).is_none());

        let mut legacy_model = catalog
            .iter()
            .find(|model| model.backend == "whisper.cpp")
            .unwrap()
            .clone();
        legacy_model.id = "legacy-whisper-ggml".to_owned();
        assert_eq!(
            provider_for_legacy_model(&config, &legacy_model).map(ProviderHandle::id),
            Some("whisper_cpp")
        );
    }
}
