//! Test-only parent verification profiling. No worker execution, policy changes,
//! persistent health data, or changes to the transcription evidence schema.

use super::*;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
#[cfg(windows)]
use std::time::Duration;
use std::time::Instant;

#[derive(Debug, Serialize)]
pub(crate) struct PayloadObservation {
    pub(crate) ordinal: usize,
    pub(crate) relative_path: String,
    pub(crate) verified_bytes: u64,
    pub(crate) read_hash_ns: u128,
}

thread_local! {
    static OBSERVATIONS: RefCell<Option<Vec<PayloadObservation>>> = const { RefCell::new(None) };
}

// The guard must be dropped on the thread whose collector it installed.
struct CaptureGuard(PhantomData<Rc<()>>);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        OBSERVATIONS.with(|slot| *slot.borrow_mut() = None);
    }
}

fn capture<T, E>(
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<(T, Vec<PayloadObservation>), E> {
    OBSERVATIONS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(
            slot.is_none(),
            "nested verification profiling is unsupported"
        );
        *slot = Some(Vec::new());
    });
    let _guard = CaptureGuard(PhantomData);
    let result = operation()?;
    let observations = OBSERVATIONS.with(|slot| slot.borrow_mut().take().unwrap());
    Ok((result, observations))
}

pub(super) fn payload_start() -> Option<Instant> {
    OBSERVATIONS.with(|slot| slot.borrow().as_ref().map(|_| Instant::now()))
}

pub(super) fn payload_verified(entry: &PayloadEntry, started: Option<Instant>) {
    if let Some(started) = started {
        let elapsed = started.elapsed();
        OBSERVATIONS.with(|slot| {
            if let Some(observations) = slot.borrow_mut().as_mut() {
                observations.push(PayloadObservation {
                    ordinal: observations.len(),
                    relative_path: entry.path.clone(),
                    verified_bytes: entry.size_bytes,
                    read_hash_ns: elapsed.as_nanos(),
                });
            }
        });
    }
}

fn collector_is_clear() -> bool {
    OBSERVATIONS.with(|slot| slot.borrow().is_none())
}

// The application-only executable verifier uses this narrow test hook to
// prove that a failed executable check cannot publish partial payload samples.
// Keep the collector implementation independent of the application runtime so
// the standalone worker-pack author tool can test the shared verifier.
pub(crate) fn capture_for_test<T, E>(
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<(T, Vec<PayloadObservation>), E> {
    capture(operation)
}

pub(crate) fn collector_is_clear_for_test() -> bool {
    collector_is_clear()
}

#[test]
fn payload_profile_records_exact_successful_inventory() {
    let root = test_support::temp_root("profile-success");
    let (verifier, lease) = test_support::leased_fixture(&root);
    let ((full_elapsed, worker_path), observations) = capture(|| -> anyhow::Result<_> {
        let start = Instant::now();
        let launchable = verifier.launchable_worker(&lease)?;
        let full_elapsed = start.elapsed();
        Ok((full_elapsed, launchable.path().to_owned()))
    })
    .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].ordinal, 0);
    assert_eq!(observations[0].relative_path, "bin/worker.exe");
    assert_eq!(observations[0].verified_bytes, 13);
    assert!(observations[0].read_hash_ns <= full_elapsed.as_nanos());
    assert_eq!(worker_path, lease.worker_path());
    assert!(collector_is_clear());
    drop(lease);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn payload_profile_discards_records_after_later_payload_failure() {
    let root = test_support::temp_root("profile-bad-payload");
    let mut manifest = test_support::base_manifest();
    manifest.payload.push(PayloadEntry {
        path: "bin/z.dll".to_owned(),
        size_bytes: 3,
        sha256: format!("{:x}", Sha256::digest(b"yes")),
    });
    let trust = test_support::write_signed(&root, manifest);
    fs::write(root.join("bin/z.dll"), b"bad").unwrap();
    let verifier = PackVerifier::new(trust, Compatibility::current(&[PackBackend::Vulkan]));
    assert!(matches!(
        capture(|| verifier.verify(&root)),
        Err(PackVerificationError::PayloadDigestMismatch(path)) if path == "bin/z.dll"
    ));
    assert!(collector_is_clear());
    fs::write(root.join("bin/z.dll"), b"yes").unwrap();
    let (_, observations) = capture(|| verifier.verify(&root)).unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[1].ordinal, 1);
    assert_eq!(observations[1].relative_path, "bin/z.dll");
    assert_eq!(observations[1].verified_bytes, 3);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn payload_profile_clears_after_panic() {
    assert!(
        std::panic::catch_unwind(|| {
            let _: Result<((), _), ()> = capture(|| {
                payload_verified(&test_support::base_manifest().payload[0], payload_start());
                panic!("injected profiling failure");
            });
        })
        .is_err()
    );
    assert!(collector_is_clear());
    let (_, observations) = capture(|| Ok::<_, ()>(())).unwrap();
    assert!(observations.is_empty());
}

#[test]
fn payload_profile_rejects_nested_capture_without_clobbering() {
    let (_, observations) = capture(|| -> Result<(), ()> {
        payload_verified(&test_support::base_manifest().payload[0], payload_start());
        assert!(std::panic::catch_unwind(|| capture(|| Ok::<_, ()>(()))).is_err());
        payload_verified(&test_support::base_manifest().payload[0], payload_start());
        Ok(())
    })
    .unwrap();
    assert_eq!(observations.len(), 2);
    assert!(collector_is_clear());
}

#[test]
fn payload_profile_is_thread_local() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads = (1..=2)
        .map(|count| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let (_, observations) = capture(|| -> Result<(), ()> {
                    barrier.wait();
                    for _ in 0..count {
                        payload_verified(
                            &test_support::base_manifest().payload[0],
                            payload_start(),
                        );
                    }
                    barrier.wait();
                    Ok(())
                })
                .unwrap();
                assert_eq!(observations.len(), count);
                assert!(collector_is_clear());
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(collector_is_clear());
}

const ARTIFACT_REVISION: &str = "26df7731cd9c8e74758fe1e621cc4b4d73fad19c";
const ARTIFACT_APP_BUILD: &str = "local-transcriber@0.1.0#26df7731cd9c8e74758fe1e621cc4b4d73fad19c";
const ARTIFACT_WORKER_BUILD: &str =
    "scribe-inference-worker@0.1.0#26df7731cd9c8e74758fe1e621cc4b4d73fad19c";

fn artifact_compatibility() -> Compatibility<'static> {
    Compatibility {
        app_build: ARTIFACT_APP_BUILD,
        worker_build: ARTIFACT_WORKER_BUILD,
        target_os: "windows",
        target_arch: "x86_64",
        allowed_backends: &[PackBackend::Cuda],
    }
}

fn assert_profile_evaluator(debug_assertions: bool, evaluator_revision: &str) {
    assert!(!debug_assertions, "profile with a release evaluator");
    assert_ne!(evaluator_revision, ARTIFACT_REVISION);
}

#[test]
fn profile_compatibility_distinguishes_evaluator_and_artifact_revisions() {
    let current = Compatibility::current(&[PackBackend::Cuda]);
    assert_eq!(current.app_build, crate::onnx_worker::DESKTOP_BUILD_ID);
    assert!(
        artifact_compatibility()
            .app_build
            .ends_with(ARTIFACT_REVISION)
    );
    let trust = test_support::fixture_trust_root();
    let verifier = PackVerifier::new(&trust, artifact_compatibility());
    let mut manifest = test_support::base_manifest();
    manifest.app_build = "local-transcriber@0.1.0#different-evaluator".to_owned();
    assert!(matches!(
        verifier.validate_manifest(&manifest),
        Err(PackVerificationError::BuildMismatch)
    ));
    assert_profile_evaluator(false, "different-evaluator");
    assert!(
        std::panic::catch_unwind(|| assert_profile_evaluator(true, "different-evaluator")).is_err()
    );
    assert!(
        std::panic::catch_unwind(|| assert_profile_evaluator(false, ARTIFACT_REVISION)).is_err()
    );
}

#[cfg(windows)]
pub(crate) fn windows_cuda_retained_fixture_parent_verification_profile<T>(
    evaluator_revision: &str,
    evaluator_debug_assertions: bool,
    mut measure_executable: impl FnMut(&Path, &str) -> anyhow::Result<(Duration, T)>,
) -> anyhow::Result<serde_json::Value> {
    const MANIFEST_SHA: &str = "682cc1bf43f170d5fecd03a981b9e114c550ab46488022f33b8779d43c2a70dc";
    const SIGNATURE_SHA: &str = "671de157521e73af656cd8185eed8811ad929b663ceefc866e3b5e0515680f0e";
    const PACK_DIGEST: &str = "da02d2065768ff9b166d8d27fa35e5d7b0db58217a7af68874b7e05804b27821";
    const WORKER_SHA: &str = "ef61230f28f3ba332d0afa9f6d6dd5de9f7fa154b95d6634cde7e8202c11b39a";

    assert_profile_evaluator(evaluator_debug_assertions, evaluator_revision);
    let source = PathBuf::from(std::env::var_os("SCRIBE_PROFILE_RETAINED_CUDA_PACK").expect(
        "set SCRIBE_PROFILE_RETAINED_CUDA_PACK to the retained fixture-26df7731cd9c-4edf826e5bf3 pack",
    ));
    let trust = test_support::fixture_trust_root();
    let verifier = PackVerifier::new(&trust, artifact_compatibility());
    // The independently pinned envelope hashes bind the complete inventory,
    // including all payload sizes/digests. Never derive trust from an input file.
    let (manifest_bytes, manifest_handle) =
        read_bounded_regular_from(&source, None, Path::new(MANIFEST_NAME), MAX_MANIFEST_BYTES)
            .unwrap();
    let (signature_bytes, signature_handle) = read_bounded_regular_from(
        &source,
        None,
        Path::new(SIGNATURE_NAME),
        MAX_SIGNATURE_BYTES,
    )
    .unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&manifest_bytes)),
        MANIFEST_SHA
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(&signature_bytes)),
        SIGNATURE_SHA
    );
    let (descriptor, entries, source_handles) = verifier.verify_inner(&source, None).unwrap();
    assert_eq!(descriptor.pack_digest, PACK_DIGEST);
    assert_eq!(descriptor.pack_id.as_str(), "scribe-cuda-windows-x64");
    assert_eq!(
        descriptor.pack_version.as_str(),
        "fixture-26df7731cd9c-4edf826e5bf3"
    );
    assert_eq!(
        descriptor.worker_relative_path,
        "bin/scribe-inference-worker.exe"
    );

    let owner = test_support::temp_root("parent-verification-profile");
    // Catch panics solely to drop all leased handles and remove this owned
    // scratch tree before propagating failure. No partial timing output.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<_> {
        let store = owner.join("workers/packs");
        let destination = store
            .join(descriptor.pack_id.as_str())
            .join(descriptor.pack_version.as_str())
            .join(PACK_DIGEST);
        fs::create_dir_all(&destination).unwrap();
        for entry in &entries {
            let target = destination.join(&entry.path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(source.join(&entry.path), target).unwrap();
        }
        let pinned = PinnedPackRoot::open(
            &fs::canonicalize(&store).unwrap(),
            [&descriptor.pack_id, &descriptor.pack_version],
            PACK_DIGEST,
        )
        .unwrap();
        let lease = verifier.verify_pinned(pinned).unwrap();
        assert_eq!(lease.verified_pack().pack_digest, PACK_DIGEST);
        // Preparation above is deliberately unmeasured and warms file caches.
        let mut samples = Vec::new();
        // Retain every verified image through the complete measurement series.
        // The callback owns the app-specific image guard, while this generic
        // collector remains usable by the standalone pack-author tool.
        let mut verified_images = Vec::new();
        for ordinal in 0..5 {
            let ((launchable_ns, (executable_elapsed, verified_image)), payloads) =
                capture(|| -> anyhow::Result<_> {
                    let started = Instant::now();
                    let launchable = verifier.launchable_worker(&lease)?;
                    let elapsed = started.elapsed();
                    let executable = measure_executable(launchable.path(), WORKER_SHA)?;
                    Ok((elapsed.as_nanos(), executable))
                })?;
            assert_eq!(payloads.len(), 3);
            assert_eq!(
                payloads.iter().map(|p| p.verified_bytes).sum::<u64>(),
                1_002_442_240
            );
            assert!(payloads.iter().map(|p| p.read_hash_ns).sum::<u128>() <= launchable_ns);
            samples.push(serde_json::json!({
                "ordinal": ordinal,
                "launchable_worker_ns": launchable_ns,
                "initial_executable_verification_ns": executable_elapsed.as_nanos(),
                "payload_read_hash_nested_in_launchable_worker": payloads,
            }));
            verified_images.push(verified_image);
        }
        Ok(samples)
    }));
    drop((manifest_handle, signature_handle, source_handles));
    let cleanup = fs::remove_dir_all(&owner);
    let samples = match result {
        Ok(Ok(samples)) => {
            cleanup.map_err(|error| {
                anyhow::anyhow!(
                    "owned profiling scratch cleanup failed; no result published: {error}"
                )
            })?;
            samples
        }
        Ok(Err(error)) => {
            if let Err(cleanup_error) = cleanup {
                return Err(anyhow::anyhow!(
                    "parent verification diagnostic failed: {error:#}; additionally failed to remove owned profiling scratch: {cleanup_error}"
                ));
            }
            return Err(error);
        }
        Err(panic) => {
            cleanup.expect("owned profiling scratch cleanup failed; no result published");
            std::panic::resume_unwind(panic);
        }
    };
    Ok(serde_json::json!({
        "diagnostic": "parent-verification-profile-v1",
        "fixture_only": true,
        "auto_eligible": false,
        "worker_executed": false,
        "evaluator_revision": evaluator_revision,
        "artifact_revision": ARTIFACT_REVISION,
        "artifact_manifest_sha256": MANIFEST_SHA,
        "artifact_signature_sha256": SIGNATURE_SHA,
        "artifact_pack_digest": PACK_DIGEST,
        "preparation_warms_filesystem_cache": true,
        "samples": samples,
    }))
}
