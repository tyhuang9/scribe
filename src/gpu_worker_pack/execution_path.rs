//! Bounded Windows Vulkan execution projection, never a discovery source.

use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::Arc;

use super::manifest::{
    Compatibility, PackBackend, PackVerifier, ProductionTrustRoot, VerifiedPackLease,
};
use super::store::PackStore;
use super::{PackDiscoveryDiagnostic, PackDiscoveryIssue, PackLeaseDiscovery};

// Conservative third-party compatibility budget, including the NUL and the
// canonical verbatim prefix. This is not a proven vendor failure threshold.
const EXECUTION_PATH_UNITS: usize = 260;

fn path_fits(path: &Path) -> bool {
    path.as_os_str().encode_wide().count() < EXECUTION_PATH_UNITS
}

fn inventory_fits(root: &Path, lease: &VerifiedPackLease) -> bool {
    path_fits(&root.join(&lease.verified_pack().worker_relative_path))
        && lease
            .copy_entries()
            .iter()
            .all(|entry| path_fits(&root.join(&entry.path)))
}

fn needs_copy(lease: &VerifiedPackLease) -> bool {
    lease.verified_pack().backend == PackBackend::Vulkan
        && std::fs::canonicalize(&lease.verified_pack().root)
            .map_or(true, |root| !inventory_fits(&root, lease))
}

pub(super) fn needs_preparation(discovery: &PackLeaseDiscovery) -> bool {
    discovery.leases.iter().any(|lease| needs_copy(lease))
}

pub(super) fn prepare_with_permission(
    discovery: PackLeaseDiscovery,
    allow_execution_copy: bool,
    prepare: impl FnOnce(PackLeaseDiscovery) -> PackLeaseDiscovery,
) -> PackLeaseDiscovery {
    if allow_execution_copy {
        prepare(discovery)
    } else {
        reject_unacknowledged(discovery)
    }
}

fn reject_unacknowledged(mut discovery: PackLeaseDiscovery) -> PackLeaseDiscovery {
    discovery.leases.retain(|lease| {
        if !needs_copy(lease) {
            return true;
        }
        let pack = lease.verified_pack();
        discovery.diagnostics.push(PackDiscoveryDiagnostic::pack(
            PackDiscoveryIssue::ExecutionCopyNotAcknowledged,
            &pack.pack_id,
            pack.backend,
        ));
        false
    });
    discovery
}

pub(super) fn prepare(discovery: PackLeaseDiscovery) -> PackLeaseDiscovery {
    let private_root = crate::config::project_dirs()
        .ok()
        .map(|directories| directories.data_local_dir().join("gpu-exec"));
    prepare_with_root(discovery, private_root.as_deref())
}

fn prepare_with_root(
    mut discovery: PackLeaseDiscovery,
    private_root: Option<&Path>,
) -> PackLeaseDiscovery {
    discovery.leases = discovery
        .leases
        .into_iter()
        .filter_map(|lease| {
            if !needs_copy(&lease) {
                return Some(lease);
            }
            #[cfg(test)]
            let trust = lease
                .test_reverification_trust()
                .unwrap_or(&ProductionTrustRoot);
            #[cfg(not(test))]
            let trust = &ProductionTrustRoot;
            let verifier = PackVerifier::new(
                trust,
                Compatibility {
                    app_build: crate::onnx_worker::DESKTOP_BUILD_ID,
                    worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID,
                    target_os: std::env::consts::OS,
                    target_arch: std::env::consts::ARCH,
                    allowed_backends: &[PackBackend::Vulkan],
                },
            );
            let prepared = private_root
                .ok_or(PackDiscoveryIssue::ExecutionPathRejected)
                .and_then(|root| prepare_at(&lease, root, &verifier));
            match prepared {
                Ok(prepared) => Some(prepared),
                Err(issue) => {
                    let pack = lease.verified_pack();
                    discovery.diagnostics.push(PackDiscoveryDiagnostic::pack(
                        issue,
                        &pack.pack_id,
                        pack.backend,
                    ));
                    None
                }
            }
        })
        .collect();
    // Do not replace catalog_generation with any cache path or metadata.
    discovery
}

fn verbatim_local_root(path: &Path) -> Result<PathBuf, PackDiscoveryIssue> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(PackDiscoveryIssue::ExecutionPathRejected);
    };
    if !matches!(components.next(), Some(Component::RootDir))
        || !components.all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(PackDiscoveryIssue::ExecutionPathRejected);
    }
    match prefix.kind() {
        Prefix::Disk(_) => {
            let mut extended = std::ffi::OsString::from(r"\\?\");
            extended.push(path.as_os_str());
            Ok(PathBuf::from(extended))
        }
        Prefix::VerbatimDisk(_) => Ok(path.to_path_buf()),
        _ => Err(PackDiscoveryIssue::ExecutionPathRejected),
    }
}

fn prepare_at(
    source: &Arc<VerifiedPackLease>,
    private_root: &Path,
    verifier: &PackVerifier<'_>,
) -> Result<Arc<VerifiedPackLease>, PackDiscoveryIssue> {
    if !needs_copy(source) {
        return Ok(Arc::clone(source));
    }
    let root = verbatim_local_root(private_root)?;
    let pack = source.verified_pack();
    let destination = root
        .join("workers")
        .join("packs")
        .join(pack.pack_id.as_str())
        .join(pack.pack_version.as_str())
        .join(&pack.pack_digest);
    // Reject before creating directories/copying if even this fixed destination
    // cannot fit. Anchored store operations reject links, redirects and streams.
    if !inventory_fits(&destination, source) {
        return Err(PackDiscoveryIssue::ExecutionPathRejected);
    }
    let store = PackStore::new(root.join("workers"), root.join("state"), verifier);
    let prepared = store
        .stage_and_open_verified(source)
        .map_err(|_| PackDiscoveryIssue::ExecutionStoreRejected)?;
    let canonical = std::fs::canonicalize(&prepared.verified_pack().root)
        .map_err(|_| PackDiscoveryIssue::ExecutionStoreRejected)?;
    if !inventory_fits(&canonical, &prepared) {
        return Err(PackDiscoveryIssue::ExecutionPathRejected);
    }
    prepared
        .recheck()
        .map_err(|_| PackDiscoveryIssue::ExecutionStoreRejected)?;
    Ok(Arc::new(prepared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_worker_pack::manifest::test_support::{leased_fixture, temp_root};

    #[test]
    fn path_budget_counts_utf16_prefix_and_nul_exactly() {
        let prefix = r"\\?\C:\";
        let base = PathBuf::from(format!(
            "{prefix}{}",
            "x".repeat(EXECUTION_PATH_UNITS - prefix.len() - 1)
        ));
        assert!(path_fits(&base));
        let mut over = base.into_os_string();
        over.push("x");
        assert!(!path_fits(Path::new(&over)));
        let unicode = PathBuf::from(format!("{prefix}{}", "😀".repeat(126)));
        assert_eq!(unicode.as_os_str().encode_wide().count(), 259);
        assert!(path_fits(&unicode));
        let mut over = unicode.into_os_string();
        over.push("é");
        assert!(!path_fits(Path::new(&over)));
    }

    #[test]
    fn private_root_rejects_relative_network_device_and_parent_paths() {
        for path in [
            r"relative",
            r"C:relative",
            r"\\server\share\cache",
            r"\\.\C:\cache",
            r"C:\cache\..\escape",
        ] {
            assert!(verbatim_local_root(Path::new(path)).is_err(), "{path}");
        }
        assert_eq!(
            verbatim_local_root(Path::new(r"C:\cache")).unwrap(),
            PathBuf::from(r"\\?\C:\cache")
        );
    }

    #[test]
    fn short_paths_skip_all_cache_io_and_keep_original_lease() {
        let root = temp_root("ex");
        let (verifier, source) = leased_fixture(&root);
        let source = Arc::new(source);
        assert!(!needs_copy(&source));
        let unused = root.join("unused");
        let prepared = prepare_at(&source, &unused, &verifier).unwrap();
        assert!(Arc::ptr_eq(&source, &prepared));
        assert!(!unused.exists());
        drop(prepared);
        drop(source);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unacknowledged_projection_keeps_short_candidates_and_original_generation() {
        let root = temp_root("ex");
        let (_, short) = leased_fixture(&root.join("short"));
        let long = root.join("x".repeat(100)).join("y".repeat(100));
        let (_, long) = leased_fixture(&long);
        let short = Arc::new(short);
        let long = Arc::new(long);
        let source_inventory = std::fs::read_dir(&long.verified_pack().root)
            .unwrap()
            .count();
        let discovery = PackLeaseDiscovery {
            leases: vec![Arc::clone(&short), Arc::clone(&long)],
            diagnostics: Vec::new(),
            catalog_generation: Some("bundled-generation".into()),
        };
        let unused_cache = root.join("unused-execution-cache");
        let rejected = prepare_with_permission(discovery, false, |_| {
            std::fs::create_dir(&unused_cache).unwrap();
            panic!("unacknowledged diagnostic must never enter execution preparation")
        });
        assert!(!unused_cache.exists());
        assert_eq!(
            rejected.catalog_generation.as_deref(),
            Some("bundled-generation")
        );
        assert_eq!(rejected.leases.len(), 1);
        assert!(Arc::ptr_eq(&rejected.leases[0], &short));
        assert_eq!(rejected.diagnostics.len(), 1);
        assert_eq!(
            rejected.diagnostics[0].issue,
            PackDiscoveryIssue::ExecutionCopyNotAcknowledged
        );
        assert!(!short.has_execution_source());
        assert!(!long.has_execution_source());
        assert_eq!(
            std::fs::read_dir(&long.verified_pack().root)
                .unwrap()
                .count(),
            source_inventory
        );
        drop(rejected);
        drop(short);
        drop(long);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn long_paths_copy_exact_bytes_and_reuse_verified_destination() {
        let root = temp_root("ex");
        let long = root.join("x".repeat(100)).join("y".repeat(100));
        let (verifier, source) = leased_fixture(&long);
        let source = Arc::new(source);
        assert!(needs_copy(&source));
        let cache = root.join("e");
        let prepared = prepare_at(&source, &cache, &verifier).unwrap();
        assert!(prepared.has_execution_source());
        assert_eq!(prepared.copy_entries(), source.copy_entries());
        assert!(inventory_fits(
            &std::fs::canonicalize(&prepared.verified_pack().root).unwrap(),
            &prepared
        ));
        assert_ne!(prepared.verified_pack().root, source.verified_pack().root);
        verifier.launchable_worker(&prepared).unwrap();
        let cached = prepare_at(&source, &cache, &verifier).unwrap();
        assert_eq!(cached.verified_pack(), prepared.verified_pack());
        drop(cached);
        drop(prepared);
        drop(source);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preparation_keeps_original_generation_and_empty_discovery_creates_nothing() {
        let root = temp_root("ex");
        let cache = root.join("e");
        let empty = PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: Vec::new(),
            catalog_generation: Some("bundled-generation".into()),
        };
        let empty = prepare_with_root(empty, Some(&cache));
        assert!(empty.leases.is_empty());
        assert_eq!(
            empty.catalog_generation.as_deref(),
            Some("bundled-generation")
        );
        assert!(!cache.exists());
        let long = root.join("x".repeat(100)).join("y".repeat(100));
        let (_, source) = leased_fixture(&long);
        let discovery = PackLeaseDiscovery {
            leases: vec![Arc::new(source)],
            diagnostics: Vec::new(),
            catalog_generation: Some("bundled-generation".into()),
        };
        let prepared = prepare_with_root(discovery, Some(&cache));
        assert_eq!(
            prepared.catalog_generation.as_deref(),
            Some("bundled-generation")
        );
        assert!(prepared.diagnostics.is_empty());
        assert_eq!(prepared.leases.len(), 1);
        assert!(prepared.leases[0].has_execution_source());
        drop(prepared);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn still_overlong_or_unavailable_destination_fails_without_source_fallback() {
        let root = temp_root("ex");
        let long = root.join("x".repeat(100)).join("y".repeat(100));
        let (verifier, source) = leased_fixture(&long);
        let source = Arc::new(source);
        let too_long = root.join("z".repeat(160));
        assert_eq!(
            prepare_at(&source, &too_long, &verifier).unwrap_err(),
            PackDiscoveryIssue::ExecutionPathRejected
        );
        assert!(!too_long.exists());
        let not_directory = root.join("file");
        std::fs::write(&not_directory, b"not a cache").unwrap();
        assert_eq!(
            prepare_at(&source, &not_directory, &verifier).unwrap_err(),
            PackDiscoveryIssue::ExecutionStoreRejected
        );
        drop(source);
        std::fs::remove_dir_all(root).unwrap();
    }
}
