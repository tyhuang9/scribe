//! Trusted, backend-owned Hugging Face catalog discovery.
//!
//! The UI never queries Hugging Face or constructs artifact URLs. This module
//! discovers only a constrained public publisher namespace, resolves a full
//! commit before accepting a GGUF, and persists a conservative cache for
//! offline use. It intentionally returns experimental candidates until the
//! runtime compatibility gate has enough release evidence to promote one.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const API_ORIGIN: &str = "https://huggingface.co";
const TRUSTED_ORGANIZATION: &str = "handy-computer";
const CACHE_SCHEMA_VERSION: u16 = 1;
const CATALOG_PAGE_SIZE: usize = 100;
const MAX_CATALOG_PAGES: usize = 100;
const REQUIRED_TAGS: [&str; 2] = ["gguf", "transcribe.cpp"];
const ASR_PIPELINE_TAG: &str = "automatic-speech-recognition";
const METADATA_OVERRIDES_JSON: &str = include_str!("../resources/model_metadata_overrides.json");

/// The publisher gate that allowed a model to appear in this catalog.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelTrust {
    TrustedPublisher,
}

impl ModelTrust {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::TrustedPublisher => "Trusted publisher",
        }
    }
}

/// Product compatibility is deliberately separate from repository trust.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "reason")]
pub(crate) enum ModelCompatibility {
    Experimental(String),
}

impl ModelCompatibility {
    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::Experimental(reason) => reason,
        }
    }
}

/// A safe-to-display remote variant. It deliberately does not carry a URL.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteModelVariant {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) size_bytes: u64,
    pub(crate) expected_sha256: String,
}

/// A revision-pinned model discovered through the trusted backend service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct RemoteModel {
    pub(crate) id: String,
    pub(crate) revision: String,
    pub(crate) display_name: String,
    pub(crate) description: String,
    pub(crate) languages: Vec<String>,
    pub(crate) recommended: bool,
    pub(crate) trust: ModelTrust,
    pub(crate) compatibility: ModelCompatibility,
    pub(crate) variants: Vec<RemoteModelVariant>,
}

/// The only information an installer needs to resolve a selected artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrustedArtifact {
    pub(crate) model_id: String,
    pub(crate) revision: String,
    pub(crate) filename: String,
    pub(crate) size_bytes: u64,
    pub(crate) expected_sha256: String,
}

impl RemoteModel {
    pub(crate) fn artifact_for(&self, variant_id: &str) -> Option<TrustedArtifact> {
        let variant = self
            .variants
            .iter()
            .find(|variant| variant.id == variant_id)?;
        Some(TrustedArtifact {
            model_id: self.id.clone(),
            revision: self.revision.clone(),
            filename: variant.filename.clone(),
            size_bytes: variant.size_bytes,
            expected_sha256: variant.expected_sha256.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogSource {
    Network,
    BundledFallback,
}

impl CatalogSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Network => "Live Hugging Face catalog",
            Self::BundledFallback => "Bundled offline catalog fallback",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelInventorySnapshot {
    revision: u64,
    source: CatalogSource,
    fetched_at_unix_seconds: u64,
    models: Arc<[RemoteModel]>,
}

impl ModelInventorySnapshot {
    pub(crate) fn bundled() -> Self {
        Self::validated(1, CatalogSource::BundledFallback, 0, bundled_fallback())
            .expect("bundled model inventory must remain valid")
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn source(&self) -> CatalogSource {
        self.source
    }

    pub(crate) fn models(&self) -> &[RemoteModel] {
        &self.models
    }

    #[cfg(test)]
    pub(crate) fn from_trusted_records(
        revision: u64,
        source: CatalogSource,
        fetched_at_unix_seconds: u64,
        models: Vec<RemoteModel>,
    ) -> Result<Self, CatalogError> {
        Self::validated(revision, source, fetched_at_unix_seconds, models)
    }

    #[cfg(test)]
    pub(crate) fn shares_records_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.models, &other.models)
    }

    fn validated(
        revision: u64,
        source: CatalogSource,
        fetched_at_unix_seconds: u64,
        models: Vec<RemoteModel>,
    ) -> Result<Self, CatalogError> {
        validate_inventory(&models)?;
        Ok(Self {
            revision,
            source,
            fetched_at_unix_seconds,
            models: models.into(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CatalogError {
    #[error("Hugging Face catalog request failed: {0}")]
    Network(String),
    #[error("Hugging Face catalog response was invalid: {0}")]
    InvalidResponse(String),
    #[error("Hugging Face catalog cache failed: {0}")]
    Cache(String),
}

/// Narrow HTTP boundary so discovery tests never call the network.
pub(crate) trait HubHttpClient {
    fn get_json(&self, url: &str) -> Result<HubJsonResponse, CatalogError>;
}

/// The catalog list API communicates pagination through its HTTP `Link`
/// header. The next URL remains opaque to callers, but it is validated before
/// another request can be issued.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HubJsonResponse {
    pub(crate) body: String,
    pub(crate) next_page: Option<String>,
}

#[derive(Default)]
pub(crate) struct UreqHubHttpClient;

impl HubHttpClient for UreqHubHttpClient {
    fn get_json(&self, url: &str) -> Result<HubJsonResponse, CatalogError> {
        let response = ureq::get(url)
            .set("Accept", "application/json")
            .call()
            .map_err(|error| CatalogError::Network(error.to_string()))?;
        let next_page = response
            .header("Link")
            .map(next_catalog_page_from_link_header)
            .transpose()?
            .flatten();
        let body = response
            .into_string()
            .map_err(|error| CatalogError::Network(error.to_string()))?;
        Ok(HubJsonResponse { body, next_page })
    }
}

/// Strict discovery and cache service. All network endpoints are constructed
/// internally from the fixed Hugging Face origin and trusted organization.
pub(crate) struct HuggingFaceCatalogService<C = UreqHubHttpClient> {
    client: C,
    cache_path: PathBuf,
}

impl HuggingFaceCatalogService<UreqHubHttpClient> {
    pub(crate) fn for_cache_path(cache_path: PathBuf) -> Self {
        Self::new(UreqHubHttpClient, cache_path)
    }
}

impl<C> HuggingFaceCatalogService<C>
where
    C: HubHttpClient,
{
    pub(crate) fn new(client: C, cache_path: PathBuf) -> Self {
        Self { client, cache_path }
    }

    /// Forces a new trusted discovery pass and atomically replaces the cache
    /// only after the full response has passed validation.
    pub(crate) fn refresh(
        &self,
        inventory_revision: u64,
    ) -> Result<ModelInventorySnapshot, CatalogError> {
        self.refresh_at(inventory_revision, unix_seconds())
    }

    fn refresh_at(
        &self,
        inventory_revision: u64,
        fetched_at_unix_seconds: u64,
    ) -> Result<ModelInventorySnapshot, CatalogError> {
        let models = self.discover()?;
        validate_inventory(&models)?;
        let cache = CatalogCache {
            schema_version: CACHE_SCHEMA_VERSION,
            fetched_at_unix_seconds,
            models: models.clone(),
        };
        write_cache(&self.cache_path, &cache)?;
        ModelInventorySnapshot::validated(
            inventory_revision,
            CatalogSource::Network,
            fetched_at_unix_seconds,
            models,
        )
    }

    fn discover(&self) -> Result<Vec<RemoteModel>, CatalogError> {
        let mut next_page = Some(catalog_index_url());
        let mut requested_pages = BTreeSet::new();
        let mut seen_revisions = BTreeSet::new();
        let mut models_by_repository: BTreeMap<String, RemoteModel> = BTreeMap::new();

        for _ in 0..MAX_CATALOG_PAGES {
            let Some(page_url) = next_page.take() else {
                let mut models = models_by_repository.into_values().collect::<Vec<_>>();
                models.sort_by(|left, right| {
                    right
                        .recommended
                        .cmp(&left.recommended)
                        .then_with(|| left.display_name.cmp(&right.display_name))
                        .then_with(|| left.id.cmp(&right.id))
                });
                return Ok(models);
            };
            if !requested_pages.insert(page_url.clone()) {
                return Err(CatalogError::InvalidResponse(format!(
                    "Hugging Face catalog pagination repeated {page_url}"
                )));
            }
            let response = self.client.get_json(&page_url)?;
            let candidates: Vec<HubModelSummary> = serde_json::from_str(&response.body)
                .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
            for candidate in candidates {
                if !is_trusted_candidate(&candidate) {
                    continue;
                }
                let revision = candidate.sha.clone().ok_or_else(|| {
                    CatalogError::InvalidResponse(format!(
                        "{} did not include a revision",
                        candidate.id
                    ))
                })?;
                if !seen_revisions.insert((candidate.id.clone(), revision)) {
                    continue;
                }
                if models_by_repository.contains_key(&candidate.id) {
                    continue;
                }
                if let Some(model) = self.resolve_candidate(candidate)? {
                    models_by_repository.insert(model.id.clone(), model);
                }
            }
            next_page = response.next_page;
        }

        Err(CatalogError::InvalidResponse(format!(
            "Hugging Face catalog exceeded the {MAX_CATALOG_PAGES}-page safety limit"
        )))
    }

    fn resolve_candidate(
        &self,
        candidate: HubModelSummary,
    ) -> Result<Option<RemoteModel>, CatalogError> {
        let revision = candidate.sha.ok_or_else(|| {
            CatalogError::InvalidResponse(format!("{} did not include a revision", candidate.id))
        })?;
        if !is_full_revision(&revision) {
            return Err(CatalogError::InvalidResponse(format!(
                "{} returned a non-full revision",
                candidate.id
            )));
        }
        let tree_url = format!(
            "{API_ORIGIN}/api/models/{}/tree/{}?recursive=true&expand=true",
            candidate.id, revision
        );
        let tree_response = self.client.get_json(&tree_url)?;
        let tree: Vec<HubTreeEntry> = serde_json::from_str(&tree_response.body)
            .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
        let mut variants = tree
            .into_iter()
            .filter_map(trusted_gguf_variant)
            .collect::<Vec<_>>();
        if variants.is_empty() {
            return Ok(None);
        }
        variants.sort_by(|left, right| left.filename.cmp(&right.filename));
        let metadata = metadata_overrides()?.remove(&candidate.id);
        let display_name = metadata
            .as_ref()
            .map(|metadata| metadata.display_name.clone())
            .unwrap_or_else(|| candidate.id.clone());
        let description = metadata
            .as_ref()
            .map(|metadata| metadata.description.clone())
            .unwrap_or_else(|| "Trusted public ASR GGUF candidate.".to_owned());
        Ok(Some(RemoteModel {
            id: candidate.id,
            revision,
            display_name,
            description,
            languages: metadata
                .as_ref()
                .map(|metadata| metadata.languages.clone())
                .unwrap_or_default(),
            recommended: metadata.is_some_and(|metadata| metadata.recommended),
            trust: ModelTrust::TrustedPublisher,
            compatibility: ModelCompatibility::Experimental(
                "The complete cross-platform runtime compatibility suite has not passed."
                    .to_owned(),
            ),
            variants,
        }))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CatalogCache {
    schema_version: u16,
    fetched_at_unix_seconds: u64,
    models: Vec<RemoteModel>,
}

#[derive(Deserialize)]
struct HubModelSummary {
    id: String,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default)]
    pipeline_tag: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct HubTreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<HubLfs>,
}

#[derive(Deserialize)]
struct HubLfs {
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    oid: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Deserialize)]
struct MetadataOverrideDocument {
    schema_version: u16,
    models: BTreeMap<String, MetadataOverride>,
}

#[derive(Deserialize)]
struct MetadataOverride {
    display_name: String,
    description: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    recommended: bool,
}

fn is_trusted_candidate(candidate: &HubModelSummary) -> bool {
    candidate
        .id
        .starts_with(&format!("{TRUSTED_ORGANIZATION}/"))
        && !candidate.private
        && !is_gated(&candidate.gated)
        && candidate.pipeline_tag.as_deref() == Some(ASR_PIPELINE_TAG)
        && REQUIRED_TAGS.iter().all(|tag| {
            candidate
                .tags
                .iter()
                .any(|candidate_tag| candidate_tag == tag)
        })
        && !candidate
            .tags
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case("speaker-diarization"))
}

fn is_gated(value: &serde_json::Value) -> bool {
    value.as_bool().unwrap_or(false)
        || value
            .as_str()
            .is_some_and(|value| !value.eq_ignore_ascii_case("false"))
}

fn is_full_revision(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn catalog_index_url() -> String {
    format!(
        "{API_ORIGIN}/api/models?author={TRUSTED_ORGANIZATION}&pipeline_tag={ASR_PIPELINE_TAG}&limit={CATALOG_PAGE_SIZE}&full=true"
    )
}

fn next_catalog_page_from_link_header(link_header: &str) -> Result<Option<String>, CatalogError> {
    let mut next_page = None;
    for link in link_header.split(',') {
        let mut parts = link.split(';');
        let Some(target) = parts.next() else {
            continue;
        };
        let is_next = parts.any(|parameter| {
            parameter
                .trim()
                .strip_prefix("rel=")
                .is_some_and(|value| value.trim_matches('"').eq_ignore_ascii_case("next"))
        });
        if !is_next {
            continue;
        }
        let target = target.trim().trim_start_matches('<').trim_end_matches('>');
        if !is_trusted_catalog_page_url(target) {
            return Err(CatalogError::InvalidResponse(format!(
                "Hugging Face returned an untrusted catalog pagination URL: {target}"
            )));
        }
        if next_page.replace(target.to_owned()).is_some() {
            return Err(CatalogError::InvalidResponse(
                "Hugging Face returned multiple next catalog pages".to_owned(),
            ));
        }
    }
    Ok(next_page)
}

fn is_trusted_catalog_page_url(url: &str) -> bool {
    let Some(query) = url.strip_prefix(&format!("{API_ORIGIN}/api/models?")) else {
        return false;
    };
    let has_parameter = |name: &str, expected: &str| {
        query
            .split('&')
            .filter_map(|parameter| parameter.split_once('='))
            .any(|(candidate, value)| candidate == name && value == expected)
    };
    has_parameter("author", TRUSTED_ORGANIZATION)
        && has_parameter("pipeline_tag", ASR_PIPELINE_TAG)
        && has_parameter("full", "true")
        && query.split('&').any(|parameter| {
            parameter
                .strip_prefix("cursor=")
                .is_some_and(|cursor| !cursor.is_empty())
        })
}

fn trusted_gguf_variant(entry: HubTreeEntry) -> Option<RemoteModelVariant> {
    if entry.entry_type != "file"
        || !entry.path.to_ascii_lowercase().ends_with(".gguf")
        || !is_safe_relative_filename(&entry.path)
    {
        return None;
    }
    let lfs = entry.lfs?;
    let expected_sha256 = lfs.sha256.or(lfs.oid)?;
    let size_bytes = lfs.size.or(entry.size)?;
    if size_bytes == 0 || !is_sha256(&expected_sha256) {
        return None;
    }
    Some(RemoteModelVariant {
        id: entry.path.clone(),
        filename: entry.path,
        size_bytes,
        expected_sha256: expected_sha256.to_ascii_lowercase(),
    })
}

fn is_safe_relative_filename(filename: &str) -> bool {
    let path = Path::new(filename);
    !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_))
                && component.as_os_str() != "."
                && component.as_os_str() != ".."
        })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn metadata_overrides() -> Result<BTreeMap<String, MetadataOverride>, CatalogError> {
    let document: MetadataOverrideDocument = serde_json::from_str(METADATA_OVERRIDES_JSON)
        .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
    if document.schema_version != 1 {
        return Err(CatalogError::InvalidResponse(format!(
            "unsupported metadata override schema {}",
            document.schema_version
        )));
    }
    Ok(document.models)
}

fn write_cache(path: &Path, cache: &CatalogCache) -> Result<(), CatalogError> {
    let bytes =
        serde_json::to_vec_pretty(cache).map_err(|error| CatalogError::Cache(error.to_string()))?;
    crate::config::settings::atomic_write_bytes(path, &bytes).map_err(|error| {
        CatalogError::Cache(format!(
            "failed to atomically write catalog cache {}: {error}",
            path.display()
        ))
    })
}

fn validate_inventory(models: &[RemoteModel]) -> Result<(), CatalogError> {
    if models.is_empty() {
        return Err(CatalogError::InvalidResponse(
            "trusted model inventory was empty".to_owned(),
        ));
    }
    let mut model_ids = BTreeSet::new();
    for model in models {
        if !model.id.starts_with(&format!("{TRUSTED_ORGANIZATION}/"))
            || !is_full_revision(&model.revision)
            || model.variants.is_empty()
            || !model_ids.insert(&model.id)
        {
            return Err(CatalogError::InvalidResponse(format!(
                "{} was not a unique, revision-pinned trusted model",
                model.id
            )));
        }
        let mut variant_ids = BTreeSet::new();
        for variant in &model.variants {
            if variant.size_bytes == 0
                || !is_safe_relative_filename(&variant.filename)
                || !variant.filename.to_ascii_lowercase().ends_with(".gguf")
                || !is_sha256(&variant.expected_sha256)
                || !variant_ids.insert(&variant.id)
            {
                return Err(CatalogError::InvalidResponse(format!(
                    "{} contained an invalid or duplicate GGUF variant",
                    model.id
                )));
            }
        }
    }
    Ok(())
}

fn bundled_fallback() -> Vec<RemoteModel> {
    vec![RemoteModel {
        id: "handy-computer/whisper-tiny.en-gguf".to_owned(),
        revision: "becb8bcb804405dc97b380a523d9975888820986".to_owned(),
        display_name: "English Tiny".to_owned(),
        description: "Small English GGUF model for low-resource local dictation.".to_owned(),
        languages: vec!["en".to_owned()],
        recommended: true,
        trust: ModelTrust::TrustedPublisher,
        compatibility: ModelCompatibility::Experimental(
            "Bundled fallback metadata; refresh when online for current variants.".to_owned(),
        ),
        variants: vec![RemoteModelVariant {
            id: "whisper-tiny.en-Q4_K_M.gguf".to_owned(),
            filename: "whisper-tiny.en-Q4_K_M.gguf".to_owned(),
            size_bytes: 43_545_248,
            expected_sha256: "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b"
                .to_owned(),
        }],
    }]
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MockHub {
        responses: Mutex<BTreeMap<String, HubJsonResponse>>,
        fail: bool,
    }

    impl MockHub {
        fn with_response(self, url: String, response: &str) -> Self {
            self.with_page(url, response, None)
        }

        fn with_page(self, url: String, response: &str, next_page: Option<String>) -> Self {
            self.responses.lock().unwrap().insert(
                url,
                HubJsonResponse {
                    body: response.to_owned(),
                    next_page,
                },
            );
            self
        }
    }

    impl HubHttpClient for MockHub {
        fn get_json(&self, url: &str) -> Result<HubJsonResponse, CatalogError> {
            if self.fail {
                return Err(CatalogError::Network("offline".to_owned()));
            }
            self.responses
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .ok_or_else(|| CatalogError::Network(format!("unexpected URL {url}")))
        }
    }

    fn index_url(cursor: Option<&str>) -> String {
        let url = catalog_index_url();
        cursor.map_or(url.clone(), |cursor| format!("{url}&cursor={cursor}"))
    }

    fn tree_url(repository: &str, revision: &str) -> String {
        format!("{API_ORIGIN}/api/models/{repository}/tree/{revision}?recursive=true&expand=true")
    }

    fn index_response() -> &'static str {
        r#"[{"id":"handy-computer/whisper-tiny.en-gguf","sha":"becb8bcb804405dc97b380a523d9975888820986","private":false,"gated":false,"pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp","asr"]}]"#
    }

    fn tree_response() -> &'static str {
        r#"[{"type":"file","path":"whisper-tiny.en-Q4_K_M.gguf","size":43545248,"lfs":{"oid":"3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b","size":43545248}},{"type":"file","path":"README.md","size":4}]"#
    }

    fn temp_cache_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("scribe-hf-catalog-{name}-{}", std::process::id()))
    }

    #[test]
    fn trusted_discovery_requires_public_asr_gguf_with_full_revision_and_digest() {
        let cache = temp_cache_path("trusted.json");
        let client = MockHub::default()
            .with_response(index_url(None), index_response())
            .with_response(
                tree_url(
                    "handy-computer/whisper-tiny.en-gguf",
                    "becb8bcb804405dc97b380a523d9975888820986",
                ),
                tree_response(),
            );
        let service = HuggingFaceCatalogService::new(client, cache.clone());

        let snapshot = service.refresh_at(2, 1_000).unwrap();

        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.source, CatalogSource::Network);
        assert_eq!(snapshot.models.len(), 1);
        assert_eq!(snapshot.models[0].revision.len(), 40);
        assert_eq!(
            snapshot.models[0].variants[0].filename,
            "whisper-tiny.en-Q4_K_M.gguf"
        );
        assert_eq!(
            snapshot.models[0]
                .artifact_for("whisper-tiny.en-Q4_K_M.gguf")
                .unwrap()
                .expected_sha256,
            "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b"
        );
        let _ = fs::remove_file(cache);
    }

    #[test]
    fn paginated_discovery_filters_each_page_and_deduplicates_repository_cards() {
        let cache = temp_cache_path("paginated.json");
        let next_page = index_url(Some("opaque-next-page"));
        let second_page = r#"[
            {"id":"handy-computer/whisper-tiny.en-gguf","sha":"becb8bcb804405dc97b380a523d9975888820986","private":false,"gated":false,"pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp","asr"]},
            {"id":"handy-computer/private","sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","private":true,"gated":false,"pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]},
            {"id":"handy-computer/whisper-base.en-gguf","sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","private":false,"gated":false,"pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp","asr"]}
        ]"#;
        let client = MockHub::default()
            .with_page(index_url(None), index_response(), Some(next_page.clone()))
            .with_response(next_page, second_page)
            .with_response(
                tree_url(
                    "handy-computer/whisper-tiny.en-gguf",
                    "becb8bcb804405dc97b380a523d9975888820986",
                ),
                tree_response(),
            )
            .with_response(
                tree_url(
                    "handy-computer/whisper-base.en-gguf",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
                tree_response(),
            );
        let service = HuggingFaceCatalogService::new(client, cache.clone());

        let snapshot = service.refresh_at(2, 1_000).unwrap();

        assert_eq!(snapshot.models.len(), 2);
        assert_eq!(
            snapshot
                .models
                .iter()
                .filter(|model| model.id == "handy-computer/whisper-tiny.en-gguf")
                .count(),
            1
        );
        assert!(
            snapshot
                .models
                .iter()
                .any(|model| model.id == "handy-computer/whisper-base.en-gguf")
        );
        assert!(
            snapshot
                .models
                .iter()
                .all(|model| model.id != "handy-computer/private")
        );
        let _ = fs::remove_file(cache);
    }

    #[test]
    fn catalog_pagination_rejects_untrusted_and_repeated_continuations() {
        let trusted_next = index_url(Some("opaque-next-page"));
        assert_eq!(
            next_catalog_page_from_link_header(&format!("<{trusted_next}>; rel=\"next\"")).unwrap(),
            Some(trusted_next)
        );
        assert!(matches!(
            next_catalog_page_from_link_header(
                "<https://example.invalid/api/models?cursor=x>; rel=\"next\""
            ),
            Err(CatalogError::InvalidResponse(_))
        ));

        let cache = temp_cache_path("repeated-page.json");
        let first_page = index_url(None);
        let client = MockHub::default().with_page(first_page.clone(), "[]", Some(first_page));
        let service = HuggingFaceCatalogService::new(client, cache.clone());
        assert!(matches!(
            service.refresh_at(2, 1_000),
            Err(CatalogError::InvalidResponse(message)) if message.contains("repeated")
        ));
        let _ = fs::remove_file(cache);
    }

    #[test]
    fn private_gated_untrusted_or_non_gguf_models_do_not_enter_catalog() {
        let trusted = serde_json::from_str::<Vec<HubModelSummary>>(index_response())
            .unwrap()
            .remove(0);
        assert!(is_trusted_candidate(&trusted));

        for replacement in [
            r#"{"id":"other-org/model","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/model","sha":"becb8bcb804405dc97b380a523d9975888820986","private":true,"pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/model","sha":"becb8bcb804405dc97b380a523d9975888820986","gated":true,"pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/model","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"text-classification","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/model","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"automatic-speech-recognition","tags":["gguf"]}"#,
        ] {
            let candidate = serde_json::from_str::<HubModelSummary>(replacement).unwrap();
            assert!(!is_trusted_candidate(&candidate));
        }
        assert!(
            trusted_gguf_variant(HubTreeEntry {
                entry_type: "file".to_owned(),
                path: "../escape.gguf".to_owned(),
                size: Some(1),
                lfs: Some(HubLfs {
                    sha256: Some("a".repeat(64)),
                    oid: None,
                    size: Some(1),
                }),
            })
            .is_none()
        );
    }

    #[test]
    fn bundled_snapshot_is_versioned_and_owns_immutable_records() {
        let snapshot = ModelInventorySnapshot::bundled();
        let clone = snapshot.clone();

        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.source, CatalogSource::BundledFallback);
        assert!(!snapshot.models.is_empty());
        assert!(Arc::ptr_eq(&snapshot.models, &clone.models));
    }

    #[test]
    fn cache_refresh_replaces_an_existing_cache_file() {
        let cache = temp_cache_path("overwrite.json");
        write_cache(
            &cache,
            &CatalogCache {
                schema_version: CACHE_SCHEMA_VERSION,
                fetched_at_unix_seconds: 1,
                models: Vec::new(),
            },
        )
        .unwrap();
        write_cache(
            &cache,
            &CatalogCache {
                schema_version: CACHE_SCHEMA_VERSION,
                fetched_at_unix_seconds: 2,
                models: bundled_fallback(),
            },
        )
        .unwrap();

        let written: CatalogCache = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
        assert_eq!(written.fetched_at_unix_seconds, 2);
        assert_eq!(written.models, bundled_fallback());
        let _ = fs::remove_file(cache);
    }

    #[test]
    #[ignore = "requires public Hugging Face network access"]
    fn live_trusted_catalog_discovers_only_pinned_gguf_variants() {
        let cache = temp_cache_path("live.json");
        let service = HuggingFaceCatalogService::for_cache_path(cache.clone());

        let snapshot = service.refresh(2).unwrap();

        assert_eq!(snapshot.source, CatalogSource::Network);
        assert!(snapshot.models.iter().all(|model| {
            model.id.starts_with("handy-computer/")
                && is_full_revision(&model.revision)
                && model
                    .variants
                    .iter()
                    .all(|variant| is_sha256(&variant.expected_sha256))
        }));
        let _ = fs::remove_file(cache);
    }
}
