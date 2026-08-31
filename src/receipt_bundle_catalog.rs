//! Catalog-facing facts about embedded receipt-backed bundles.
//!
//! This deliberately exposes only an Available bundle's checked aggregate
//! size. Private manifest, receipt, download, and installation details stay
//! inside the ONNX bundle service.

/// Returns the checked pinned-file aggregate for an embedded Available bundle.
pub(crate) fn available_bundle_aggregate_size_bytes(bundle_id: &str) -> Option<u64> {
    let bundle = crate::onnx_model_bundles::bundle_manifest(bundle_id)?;
    if bundle.availability != crate::onnx_model_bundles::BundleAvailability::Available {
        return None;
    }
    bundle
        .files
        .iter()
        .try_fold(0_u64, |total, file| total.checked_add(file.size_bytes))
}
