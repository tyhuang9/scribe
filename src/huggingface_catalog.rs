//! Trusted, backend-owned Hugging Face catalog discovery.
//!
//! The UI never queries Hugging Face or constructs artifact URLs. This module
//! discovers only a constrained public publisher namespace, resolves a full
//! commit before accepting a GGUF, and publishes a complete validated
//! in-memory snapshot. It intentionally returns experimental candidates until
//! the runtime compatibility gate has enough release evidence to promote one.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Component, Path};
use std::sync::Arc;

use serde::Deserialize;

const API_ORIGIN: &str = "https://huggingface.co";
const TRUSTED_ORGANIZATION: &str = "handy-computer";
const CATALOG_PAGE_SIZE: usize = 100;
const MAX_CATALOG_PAGES: usize = 100;
const MAX_HTTP_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_CANDIDATES: usize = 2_000;
const MAX_INVENTORY_MODELS: usize = 1_000;
const MAX_TREE_ENTRIES: usize = 10_000;
const MAX_VARIANTS_PER_MODEL: usize = 32;
const MAX_TOTAL_VARIANTS: usize = 8_000;
const MAX_METADATA_BYTES: usize = 256 * 1024;
const MAX_REPOSITORY_BYTES: usize = 128;
const MAX_FILENAME_BYTES: usize = 512;
const MAX_DISPLAY_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_LANGUAGES_PER_MODEL: usize = 64;
const MAX_LANGUAGE_BYTES: usize = 64;
const MAX_TAGS_PER_CANDIDATE: usize = 64;
const MAX_TAG_BYTES: usize = 128;
const MAX_AGGREGATE_INVENTORY_BYTES: usize = 8 * 1024 * 1024;
const REQUIRED_TAGS: [&str; 2] = ["gguf", "transcribe.cpp"];
const ASR_PIPELINE_TAG: &str = "automatic-speech-recognition";
const METADATA_OVERRIDES_JSON: &str = include_str!("../resources/model_metadata_overrides.json");

/// The publisher gate that allowed a model to appear in this catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteModelVariant {
    pub(crate) id: String,
    pub(crate) filename: String,
    pub(crate) size_bytes: u64,
    pub(crate) expected_sha256: String,
}

/// A revision-pinned model discovered through the trusted backend service.
#[derive(Clone, Debug, Eq, PartialEq)]
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
            Self::BundledFallback => "Bundled trusted catalog",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelInventorySnapshot {
    revision: u64,
    source: CatalogSource,
    models: Arc<[RemoteModel]>,
}

impl ModelInventorySnapshot {
    pub(crate) fn bundled() -> Self {
        Self::validated(1, CatalogSource::BundledFallback, bundled_fallback())
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
        models: Vec<RemoteModel>,
    ) -> Result<Self, CatalogError> {
        Self::validated(revision, source, models)
    }

    #[cfg(test)]
    pub(crate) fn shares_records_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.models, &other.models)
    }

    #[cfg(test)]
    pub(crate) fn from_records_unchecked_for_projection(
        revision: u64,
        source: CatalogSource,
        models: Vec<RemoteModel>,
    ) -> Self {
        Self {
            revision,
            source,
            models: models.into(),
        }
    }

    fn validated(
        revision: u64,
        source: CatalogSource,
        models: Vec<RemoteModel>,
    ) -> Result<Self, CatalogError> {
        validate_inventory(&models)?;
        Ok(Self {
            revision,
            source,
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
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| CatalogError::Network(error.to_string()))?;
        if bytes.len() > MAX_HTTP_RESPONSE_BYTES {
            return Err(CatalogError::InvalidResponse(format!(
                "Hugging Face response exceeded {MAX_HTTP_RESPONSE_BYTES} bytes"
            )));
        }
        let body = String::from_utf8(bytes)
            .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
        Ok(HubJsonResponse { body, next_page })
    }
}

/// Strict discovery service. All network endpoints are constructed internally
/// from the fixed Hugging Face origin and trusted organization.
pub(crate) struct HuggingFaceCatalogService<C = UreqHubHttpClient> {
    client: C,
}

impl HuggingFaceCatalogService<UreqHubHttpClient> {
    pub(crate) fn online() -> Self {
        Self::new(UreqHubHttpClient)
    }
}

impl<C> HuggingFaceCatalogService<C>
where
    C: HubHttpClient,
{
    pub(crate) fn new(client: C) -> Self {
        Self { client }
    }

    /// Produces a complete immutable snapshot only after the full response has
    /// passed validation.
    pub(crate) fn refresh(
        &self,
        inventory_revision: u64,
    ) -> Result<ModelInventorySnapshot, CatalogError> {
        let models = self.discover()?;
        ModelInventorySnapshot::validated(inventory_revision, CatalogSource::Network, models)
    }

    fn discover(&self) -> Result<Vec<RemoteModel>, CatalogError> {
        let mut next_page = Some(catalog_index_url());
        let mut requested_pages = BTreeSet::new();
        let mut seen_revisions = BTreeSet::new();
        let mut models_by_repository: BTreeMap<String, RemoteModel> = BTreeMap::new();
        let metadata = metadata_overrides()?;
        let mut total_candidates = 0_usize;

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
            ensure_response_size(&response.body)?;
            let candidates: Vec<HubModelSummary> = serde_json::from_str(&response.body)
                .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
            if candidates.len() > CATALOG_PAGE_SIZE {
                return Err(CatalogError::InvalidResponse(format!(
                    "Hugging Face returned more than {CATALOG_PAGE_SIZE} candidates in one page"
                )));
            }
            total_candidates = total_candidates
                .checked_add(candidates.len())
                .filter(|count| *count <= MAX_TOTAL_CANDIDATES)
                .ok_or_else(|| {
                    CatalogError::InvalidResponse(format!(
                        "Hugging Face catalog exceeded {MAX_TOTAL_CANDIDATES} candidates"
                    ))
                })?;
            for candidate in candidates {
                validate_candidate_bounds(&candidate)?;
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
                if let Some(model) = self.resolve_candidate(candidate, &metadata)? {
                    if models_by_repository.len() >= MAX_INVENTORY_MODELS {
                        return Err(CatalogError::InvalidResponse(format!(
                            "Hugging Face catalog exceeded {MAX_INVENTORY_MODELS} models"
                        )));
                    }
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
        metadata: &BTreeMap<String, MetadataOverride>,
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
        ensure_response_size(&tree_response.body)?;
        let tree: Vec<HubTreeEntry> = serde_json::from_str(&tree_response.body)
            .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
        if tree.len() > MAX_TREE_ENTRIES {
            return Err(CatalogError::InvalidResponse(format!(
                "{} exceeded {MAX_TREE_ENTRIES} tree entries",
                candidate.id
            )));
        }
        let mut variants = Vec::new();
        for entry in tree {
            if entry.path.len() > MAX_FILENAME_BYTES {
                return Err(CatalogError::InvalidResponse(format!(
                    "{} contained an oversized tree path",
                    candidate.id
                )));
            }
            if let Some(variant) = trusted_gguf_variant(entry) {
                if variants.len() >= MAX_VARIANTS_PER_MODEL {
                    return Err(CatalogError::InvalidResponse(format!(
                        "{} exceeded {MAX_VARIANTS_PER_MODEL} GGUF variants",
                        candidate.id
                    )));
                }
                variants.push(variant);
            }
        }
        if variants.is_empty() {
            return Ok(None);
        }
        variants.sort_by(|left, right| left.filename.cmp(&right.filename));
        let metadata = metadata.get(&candidate.id);
        let display_name = metadata
            .map(|metadata| metadata.display_name.clone())
            .unwrap_or_else(|| candidate.id.clone());
        let description = metadata
            .map(|metadata| metadata.description.clone())
            .unwrap_or_else(|| "Trusted public ASR GGUF candidate.".to_owned());
        Ok(Some(RemoteModel {
            id: candidate.id,
            revision,
            display_name,
            description,
            languages: metadata
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
    is_safe_repository_id(&candidate.id)
        && !candidate.private
        && !is_gated(&candidate.gated)
        && candidate.pipeline_tag.as_deref() == Some(ASR_PIPELINE_TAG)
        && candidate.tags.len() <= MAX_TAGS_PER_CANDIDATE
        && candidate.tags.iter().all(|tag| tag.len() <= MAX_TAG_BYTES)
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

fn validate_candidate_bounds(candidate: &HubModelSummary) -> Result<(), CatalogError> {
    if candidate.id.len() > MAX_REPOSITORY_BYTES
        || candidate.sha.as_ref().is_some_and(|sha| sha.len() > 40)
        || candidate
            .pipeline_tag
            .as_ref()
            .is_some_and(|tag| tag.len() > MAX_TAG_BYTES)
        || candidate.tags.len() > MAX_TAGS_PER_CANDIDATE
        || candidate.tags.iter().any(|tag| tag.len() > MAX_TAG_BYTES)
        || (candidate
            .id
            .starts_with(&format!("{TRUSTED_ORGANIZATION}/"))
            && !is_safe_repository_id(&candidate.id))
    {
        Err(CatalogError::InvalidResponse(
            "Hugging Face candidate exceeded trusted identifier or string bounds".to_owned(),
        ))
    } else {
        Ok(())
    }
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

fn is_safe_identifier(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_safe_repository_id(value: &str) -> bool {
    value.len() <= MAX_REPOSITORY_BYTES
        && value
            .split_once('/')
            .is_some_and(|(organization, repository)| {
                organization == TRUSTED_ORGANIZATION
                    && !repository.is_empty()
                    && is_safe_identifier(repository)
            })
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
    filename.len() <= MAX_FILENAME_BYTES
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_))
                && component.as_os_str() != "."
                && component.as_os_str() != ".."
                && component
                    .as_os_str()
                    .to_str()
                    .is_some_and(is_safe_identifier)
        })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn metadata_overrides() -> Result<BTreeMap<String, MetadataOverride>, CatalogError> {
    if METADATA_OVERRIDES_JSON.len() > MAX_METADATA_BYTES {
        return Err(CatalogError::InvalidResponse(format!(
            "metadata overrides exceeded {MAX_METADATA_BYTES} bytes"
        )));
    }
    let document: MetadataOverrideDocument = serde_json::from_str(METADATA_OVERRIDES_JSON)
        .map_err(|error| CatalogError::InvalidResponse(error.to_string()))?;
    if document.schema_version != 1 {
        return Err(CatalogError::InvalidResponse(format!(
            "unsupported metadata override schema {}",
            document.schema_version
        )));
    }
    if document.models.len() > MAX_INVENTORY_MODELS
        || document.models.iter().any(|(repository, metadata)| {
            !is_safe_repository_id(repository)
                || metadata.display_name.len() > MAX_DISPLAY_NAME_BYTES
                || metadata.description.len() > MAX_DESCRIPTION_BYTES
                || metadata.languages.len() > MAX_LANGUAGES_PER_MODEL
                || metadata
                    .languages
                    .iter()
                    .any(|language| language.is_empty() || language.len() > MAX_LANGUAGE_BYTES)
        })
    {
        return Err(CatalogError::InvalidResponse(
            "metadata overrides exceeded trusted inventory bounds".to_owned(),
        ));
    }
    Ok(document.models)
}

fn validate_inventory(models: &[RemoteModel]) -> Result<(), CatalogError> {
    if models.is_empty() || models.len() > MAX_INVENTORY_MODELS {
        return Err(CatalogError::InvalidResponse(
            "trusted model inventory had an invalid model count".to_owned(),
        ));
    }
    let mut model_ids = BTreeSet::new();
    let mut total_variants = 0_usize;
    let mut aggregate_bytes = 0_usize;
    for model in models {
        if !is_safe_repository_id(&model.id)
            || !is_full_revision(&model.revision)
            || model.variants.is_empty()
            || model.variants.len() > MAX_VARIANTS_PER_MODEL
            || model.display_name.is_empty()
            || model.display_name.len() > MAX_DISPLAY_NAME_BYTES
            || model.description.len() > MAX_DESCRIPTION_BYTES
            || model.compatibility.detail().len() > MAX_DESCRIPTION_BYTES
            || model.languages.len() > MAX_LANGUAGES_PER_MODEL
            || model
                .languages
                .iter()
                .any(|language| language.is_empty() || language.len() > MAX_LANGUAGE_BYTES)
            || !model_ids.insert(&model.id)
        {
            return Err(CatalogError::InvalidResponse(format!(
                "{} was not a unique, revision-pinned trusted model",
                model.id
            )));
        }
        total_variants = total_variants
            .checked_add(model.variants.len())
            .filter(|count| *count <= MAX_TOTAL_VARIANTS)
            .ok_or_else(|| {
                CatalogError::InvalidResponse(
                    "trusted model inventory exceeded the aggregate variant limit".to_owned(),
                )
            })?;
        for length in [
            model.id.len(),
            model.revision.len(),
            model.display_name.len(),
            model.description.len(),
            model.compatibility.detail().len(),
        ]
        .into_iter()
        .chain(model.languages.iter().map(String::len))
        {
            aggregate_bytes = aggregate_bytes
                .checked_add(length)
                .filter(|bytes| *bytes <= MAX_AGGREGATE_INVENTORY_BYTES)
                .ok_or_else(|| {
                    CatalogError::InvalidResponse(
                        "trusted model inventory exceeded the aggregate byte limit".to_owned(),
                    )
                })?;
        }
        let mut variant_ids = BTreeSet::new();
        for variant in &model.variants {
            if variant.size_bytes == 0
                || variant.id.is_empty()
                || variant.id.len() > MAX_FILENAME_BYTES
                || !is_safe_relative_filename(&variant.id)
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
            aggregate_bytes = aggregate_bytes
                .checked_add(
                    variant
                        .id
                        .len()
                        .saturating_add(variant.filename.len())
                        .saturating_add(variant.expected_sha256.len()),
                )
                .filter(|bytes| *bytes <= MAX_AGGREGATE_INVENTORY_BYTES)
                .ok_or_else(|| {
                    CatalogError::InvalidResponse(
                        "trusted model inventory exceeded the aggregate byte limit".to_owned(),
                    )
                })?;
        }
    }
    Ok(())
}

fn ensure_response_size(body: &str) -> Result<(), CatalogError> {
    if body.len() > MAX_HTTP_RESPONSE_BYTES {
        Err(CatalogError::InvalidResponse(format!(
            "Hugging Face response exceeded {MAX_HTTP_RESPONSE_BYTES} bytes"
        )))
    } else {
        Ok(())
    }
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

    #[test]
    fn trusted_discovery_requires_public_asr_gguf_with_full_revision_and_digest() {
        let client = MockHub::default()
            .with_response(index_url(None), index_response())
            .with_response(
                tree_url(
                    "handy-computer/whisper-tiny.en-gguf",
                    "becb8bcb804405dc97b380a523d9975888820986",
                ),
                tree_response(),
            );
        let service = HuggingFaceCatalogService::new(client);

        let snapshot = service.refresh(2).unwrap();

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
    }

    #[test]
    fn paginated_discovery_filters_each_page_and_deduplicates_repository_cards() {
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
        let service = HuggingFaceCatalogService::new(client);

        let snapshot = service.refresh(2).unwrap();

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

        let first_page = index_url(None);
        let client = MockHub::default().with_page(first_page.clone(), "[]", Some(first_page));
        let service = HuggingFaceCatalogService::new(client);
        assert!(matches!(
            service.refresh(2),
            Err(CatalogError::InvalidResponse(message)) if message.contains("repeated")
        ));
    }

    #[test]
    fn private_gated_untrusted_or_non_gguf_models_do_not_enter_catalog() {
        let trusted = serde_json::from_str::<Vec<HubModelSummary>>(index_response())
            .unwrap()
            .remove(0);
        assert!(is_trusted_candidate(&trusted));

        for replacement in [
            r#"{"id":"other-org/model","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/../model","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/model/extra","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
            r#"{"id":"handy-computer/model?private=true","sha":"becb8bcb804405dc97b380a523d9975888820986","pipeline_tag":"automatic-speech-recognition","tags":["gguf","transcribe.cpp"]}"#,
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
    fn catalog_bounds_fail_closed_before_publishing_a_snapshot() {
        assert!(matches!(
            ensure_response_size(&"x".repeat(MAX_HTTP_RESPONSE_BYTES + 1)),
            Err(CatalogError::InvalidResponse(_))
        ));

        let candidate: serde_json::Value = serde_json::from_str(index_response()).unwrap();
        let candidate = candidate.as_array().unwrap()[0].clone();
        let oversized_page =
            serde_json::to_string(&vec![candidate; CATALOG_PAGE_SIZE + 1]).unwrap();
        let service = HuggingFaceCatalogService::new(
            MockHub::default().with_response(index_url(None), &oversized_page),
        );
        assert!(matches!(
            service.refresh(2),
            Err(CatalogError::InvalidResponse(message)) if message.contains("one page")
        ));

        let mut oversized_model = bundled_fallback().remove(0);
        oversized_model.description = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
        assert!(matches!(
            ModelInventorySnapshot::from_trusted_records(
                2,
                CatalogSource::Network,
                vec![oversized_model]
            ),
            Err(CatalogError::InvalidResponse(_))
        ));

        let mut variant_heavy_model = bundled_fallback().remove(0);
        variant_heavy_model.variants =
            vec![variant_heavy_model.variants[0].clone(); MAX_VARIANTS_PER_MODEL + 1];
        assert!(matches!(
            ModelInventorySnapshot::from_trusted_records(
                2,
                CatalogSource::Network,
                vec![variant_heavy_model]
            ),
            Err(CatalogError::InvalidResponse(_))
        ));
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
    #[ignore = "requires public Hugging Face network access"]
    fn live_trusted_catalog_discovers_only_pinned_gguf_variants() {
        let service = HuggingFaceCatalogService::online();

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
    }
}
