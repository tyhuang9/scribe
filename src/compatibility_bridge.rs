//! Private bridge for pre-revamp model/runtime management.
//!
//! Application UI receives opaque provider handles and neutral status data.
//! Concrete legacy adapters remain confined here and in `stt` until their
//! artifacts are migrated or retired in Phase 11.

use std::path::{Path, PathBuf};

use crate::config::{self, AppConfig};
use crate::models::{ModelInstallStatus, ModelRuntimeStatus, SttModelInfo};
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

pub(crate) fn provider_for_model(model: &SttModelInfo) -> Option<ProviderHandle> {
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

    pub(crate) fn can_install_model(self, model: &SttModelInfo) -> bool {
        self.adapter.can_install_model(model)
    }

    pub(crate) fn can_uninstall_model(self, model: &SttModelInfo) -> bool {
        self.adapter.can_uninstall_model(model)
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

    pub(crate) fn same_provider(self, model: &SttModelInfo) -> bool {
        model.backend == self.adapter.backend
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

#[cfg(test)]
pub(crate) fn model_storage_estimate(model: &SttModelInfo) -> &'static str {
    runtime_catalog::model_storage_estimate(&model.id)
}

pub(crate) fn model_download_total_bytes(model: &SttModelInfo) -> Option<u64> {
    runtime_catalog::model_download_total_bytes(&model.id)
}

pub(crate) fn runtime_storage_estimate(model: &SttModelInfo) -> &'static str {
    runtime_catalog::backend_spec(&model.backend)
        .map(|spec| spec.runtime_storage_estimate)
        .unwrap_or("varies")
}

pub(crate) fn runtime_storage_detail(model: &SttModelInfo) -> &'static str {
    runtime_catalog::backend_spec(&model.backend)
        .map(|spec| spec.runtime_storage_detail)
        .unwrap_or("varies")
}

pub(crate) fn entrypoint_is_usable(provider_id: &str, path: &Path) -> bool {
    runtime_catalog::runtime_entrypoint_is_usable(provider_id, path)
}
