//! Catalog-facing facts about embedded receipt-backed bundles.
//!
//! This deliberately exposes only an Available bundle's checked aggregate
//! size. Private manifest, receipt, download, and installation details stay
//! inside the ONNX bundle service.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::Deserialize;

const CATALOG_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/onnx-model-bundles-v1.json"
));
const CATALOG_SCHEMA_VERSION: u16 = 1;

#[derive(Deserialize)]
struct AggregateCatalog {
    schema_version: u16,
    bundles: Vec<AggregateBundle>,
}

#[derive(Deserialize)]
struct AggregateBundle {
    id: String,
    availability: BundleAvailability,
    files: Vec<AggregateFile>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum BundleAvailability {
    Available,
    Unavailable,
}

#[derive(Deserialize)]
struct AggregateFile {
    size_bytes: u64,
}

fn parse_available_aggregates(bytes: &[u8]) -> Option<Vec<(String, u64)>> {
    let catalog: AggregateCatalog = serde_json::from_slice(bytes).ok()?;
    if catalog.schema_version != CATALOG_SCHEMA_VERSION {
        return None;
    }

    let mut ids = BTreeSet::new();
    let mut available = Vec::new();
    for bundle in catalog.bundles {
        if bundle.id.is_empty() || !ids.insert(bundle.id.clone()) {
            return None;
        }
        if bundle.availability != BundleAvailability::Available {
            continue;
        }
        let aggregate = bundle.files.iter().try_fold(0_u64, |total, file| {
            if file.size_bytes == 0 {
                None
            } else {
                total.checked_add(file.size_bytes)
            }
        })?;
        if aggregate == 0 {
            return None;
        }
        available.push((bundle.id, aggregate));
    }
    Some(available)
}

/// Returns the checked pinned-file aggregate for an embedded Available bundle.
pub(crate) fn available_bundle_aggregate_size_bytes(bundle_id: &str) -> Option<u64> {
    static AVAILABLE: OnceLock<Option<Vec<(String, u64)>>> = OnceLock::new();
    AVAILABLE
        .get_or_init(|| parse_available_aggregates(CATALOG_BYTES))
        .as_ref()?
        .iter()
        .find_map(|(id, aggregate)| (id == bundle_id).then_some(*aggregate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_projection_rejects_bad_schema_duplicates_and_empty_available_bundles() {
        assert!(parse_available_aggregates(br#"{"schema_version":2,"bundles":[]}"#).is_none());
        assert!(
            parse_available_aggregates(
                br#"{"schema_version":1,"bundles":[{"id":"same","availability":"available","files":[{"size_bytes":1}]},{"id":"same","availability":"unavailable","files":[]}]}"#
            )
            .is_none()
        );
        assert!(
            parse_available_aggregates(
                br#"{"schema_version":1,"bundles":[{"id":"empty","availability":"available","files":[]}]}"#
            )
            .is_none()
        );
    }

    #[test]
    fn aggregate_projection_is_fail_closed_and_checked() {
        let parsed = parse_available_aggregates(
            br#"{"schema_version":1,"bundles":[{"id":"ready","availability":"available","files":[{"size_bytes":2},{"size_bytes":3}]},{"id":"blocked","availability":"unavailable","files":[]}]}"#,
        )
        .unwrap();
        assert_eq!(parsed, vec![("ready".to_owned(), 5)]);
        assert!(
            parse_available_aggregates(
                br#"{"schema_version":1,"bundles":[{"id":"overflow","availability":"available","files":[{"size_bytes":18446744073709551615},{"size_bytes":1}]}]}"#
            )
            .is_none()
        );
    }
}
