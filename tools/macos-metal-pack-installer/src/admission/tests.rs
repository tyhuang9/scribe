use super::*;
use crate::manifest::{PackManifest, PayloadEntry, StoreComponent, TrustRoot};
use crate::release_authority::{MAX_RELEASE_AUTHORITY_BYTES, PackCatalog, PackCatalogEntry};
use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

struct FixtureTrust(Vec<u8>);
impl TrustRoot for FixtureTrust {
    fn public_key(&self, id: &str) -> Option<&[u8]> {
        (id == "fixture-ed25519-v1").then_some(self.0.as_slice())
    }
}
fn fixture_key() -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap()
}
fn fixture_trust() -> &'static FixtureTrust {
    static TRUST: OnceLock<FixtureTrust> = OnceLock::new();
    TRUST.get_or_init(|| FixtureTrust(fixture_key().public_key().as_ref().to_vec()))
}
fn fixture_verifier(trust: &dyn TrustRoot) -> PackVerifier<'_> {
    PackVerifier::new(
        trust,
        Compatibility {
            target_os: "macos",
            ..Compatibility::current(&[PackBackend::Metal])
        },
    )
}

struct Fixture {
    owner: PathBuf,
    root: PathBuf,
    manifest: PackManifest,
    catalog: PackCatalog,
    authority: PackReleaseAuthority,
}
impl Fixture {
    fn new(change: impl FnOnce(&mut PackManifest)) -> Self {
        let owner = manifest::test_support::temp_root("metal-installer-admission");
        let mut manifest = manifest::test_support::base_manifest();
        manifest.pack_id = StoreComponent::new("scribe-metal").unwrap();
        manifest.backend = PackBackend::Metal;
        manifest.provider = PROVIDER.to_owned();
        manifest.target_os = "macos".to_owned();
        manifest.worker_path = WORKER.to_owned();
        manifest.payload[0].path = WORKER.to_owned();
        change(&mut manifest);
        manifest
            .payload
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.pack_digest = manifest::compute_pack_digest(&manifest).unwrap();
        let relative = format!(
            "workers/packs/{}/{}/{}",
            manifest.pack_id.as_str(),
            manifest.pack_version.as_str(),
            manifest.pack_digest
        );
        let root = owner.join(&relative);
        fs::create_dir_all(&root).unwrap();
        for payload in &manifest.payload {
            let path = root.join(&payload.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"signed worker").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(root.join(manifest::MANIFEST_NAME), &bytes).unwrap();
        let signature = fixture_key()
            .sign(&bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        fs::write(root.join(manifest::SIGNATURE_NAME), format!(
            "{{\"schema_version\":1,\"key_id\":\"fixture-ed25519-v1\",\"signature_hex\":\"{signature}\"}}"
        )).unwrap();
        let mut files = vec![
            manifest::MANIFEST_NAME.to_owned(),
            manifest::SIGNATURE_NAME.to_owned(),
        ];
        files.extend(manifest.payload.iter().map(|file| file.path.clone()));
        let installed = files
            .iter()
            .map(|file| fs::metadata(root.join(file)).unwrap().len())
            .sum();
        let entry = PackCatalogEntry {
            pack_id: manifest.pack_id.clone(),
            pack_version: manifest.pack_version.clone(),
            pack_digest: manifest.pack_digest.clone(),
            security_epoch: manifest.security_epoch,
            runtime_abi_version: manifest.runtime_abi_version,
            backend: manifest.backend,
            provider: manifest.provider.clone(),
            target_os: manifest.target_os.clone(),
            target_arch: manifest.target_arch.clone(),
            worker_relative_path: manifest.worker_path.clone(),
            root: relative.clone(),
            installed_size_bytes: installed,
            compressed_size_bytes: 1,
            files: files
                .iter()
                .map(|file| format!("{relative}/{file}"))
                .collect(),
        };
        let authority_entry = PackReleaseAuthorityEntry {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_revision: env!("SCRIBE_BUILD_REVISION").to_owned(),
            app_protocol_version: manifest::APP_PROTOCOL_VERSION,
            pack_id: entry.pack_id.clone(),
            pack_version: entry.pack_version.clone(),
            pack_digest: entry.pack_digest.clone(),
            security_epoch: entry.security_epoch,
            runtime_abi_version: entry.runtime_abi_version,
            backend: entry.backend,
            provider: entry.provider.clone(),
            target_os: entry.target_os.clone(),
            target_arch: entry.target_arch.clone(),
            worker_relative_path: entry.worker_relative_path.clone(),
            root: entry.root.clone(),
            installed_size_bytes: installed,
            compressed_size_bytes: 1,
            files: entry.files.clone(),
        };
        let catalog = PackCatalog {
            schema_version: 1,
            packs: vec![entry],
        };
        let authority = PackReleaseAuthority {
            schema_version: 2,
            catalog_sha256: format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&catalog).unwrap())
            ),
            release_security_epoch: manifest.security_epoch,
            keychain_access_group: "ABCDE12345.com.scribe.local-transcriber".to_owned(),
            entries: vec![authority_entry],
        };
        Self {
            owner,
            root,
            manifest,
            catalog,
            authority,
        }
    }
    fn release(&self) -> Result<PackReleaseAuthority> {
        validated_release_authority(
            &serde_json::to_vec(&self.catalog).unwrap(),
            &serde_json::to_vec(&self.authority).unwrap(),
            build_identity(),
        )
        .context("invalid fixture release")
    }
    fn pinned(&self) -> PinnedPackRoot {
        PinnedPackRoot::open(
            &self.owner.join("workers/packs"),
            [&self.manifest.pack_id, &self.manifest.pack_version],
            &self.manifest.pack_digest,
        )
        .unwrap()
    }
    fn admit(&self) -> Result<AdmittedSource<'static>> {
        admit_pinned(
            MetalReleasePermit::for_native_release(self.release()?)?,
            self.pinned(),
            fixture_verifier(fixture_trust()),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.owner).expect("remove exclusively created private fixture");
    }
}

#[test]
fn metal_admission_exact_fixture_reverifies_without_installing() {
    let fixture = Fixture::new(|_| {});
    let admitted = fixture.admit().unwrap();
    assert_eq!(admitted.lease.copy_entries().len(), 3);
    admitted.reverify().unwrap();
    assert_eq!(fs::read_dir(&fixture.owner).unwrap().count(), 1);
}

#[test]
fn metal_admission_signature_requires_release_inclusion_and_production_trust() {
    let fixture = Fixture::new(|_| {});
    fixture_verifier(fixture_trust())
        .verify(&fixture.root)
        .unwrap();
    assert!(MetalReleasePermit::for_native_release(compiled_release().unwrap()).is_err());
    assert!(
        admit_pinned(
            MetalReleasePermit::for_native_release(fixture.release().unwrap()).unwrap(),
            fixture.pinned(),
            fixture_verifier(&ProductionTrustRoot)
        )
        .is_err(),
        "fixture trust is never production trust"
    );
}

#[test]
fn metal_admission_rejects_every_release_entry_field_mismatch() {
    let fixture = Fixture::new(|_| {});
    let original = serde_json::to_value(&fixture.authority).unwrap();
    for field in original["entries"][0].as_object().unwrap().keys() {
        let mut changed = original.clone();
        let value = &mut changed["entries"][0][field];
        if field == "backend" {
            *value = serde_json::json!("cuda");
        } else if value.is_string() {
            *value = serde_json::json!("wrong");
        } else if value.is_array() {
            *value = serde_json::json!([]);
        } else {
            *value = serde_json::json!(999);
        }
        let parsed = serde_json::from_value::<PackReleaseAuthority>(changed);
        if let Ok(authority) = parsed {
            assert!(
                validated_release_authority(
                    &serde_json::to_vec(&fixture.catalog).unwrap(),
                    &serde_json::to_vec(&authority).unwrap(),
                    build_identity()
                )
                .is_none(),
                "{field}"
            );
        }
    }
}

#[test]
fn metal_admission_release_parser_preserves_canonical_bounds_and_build_binding() {
    let fixture = Fixture::new(|_| {});
    let catalog = serde_json::to_vec(&fixture.catalog).unwrap();
    let authority = serde_json::to_vec(&fixture.authority).unwrap();
    let mut wrong_build = build_identity();
    wrong_build.app_version = "9.9.9";
    assert!(validated_release_authority(&catalog, &authority, wrong_build).is_none());
    let mut newline = authority.clone();
    newline.push(b'\n');
    assert!(validated_release_authority(&catalog, &newline, build_identity()).is_none());
    let oversized = vec![b' '; MAX_RELEASE_AUTHORITY_BYTES + 1];
    assert!(validated_release_authority(&oversized, &authority, build_identity()).is_none());
    assert!(validated_release_authority(&catalog, &oversized, build_identity()).is_none());
    let mut changed_catalog = catalog.clone();
    changed_catalog.push(b' ');
    assert!(validated_release_authority(&changed_catalog, &authority, build_identity()).is_none());
}

#[test]
fn metal_admission_rejects_valid_signed_but_incompatible_provider_backend_arch_and_abi() {
    for case in 0..7 {
        let fixture = Fixture::new(|manifest| match case {
            0 => manifest.provider = "other-metal".to_owned(),
            1 => manifest.backend = PackBackend::Vulkan,
            2 => manifest.target_arch = "other-architecture".to_owned(),
            3 => manifest.target_os = "linux".to_owned(),
            4 => manifest.runtime_abi_version += 1,
            5 => manifest.app_build.push_str("-foreign"),
            _ => manifest.worker_build.push_str("-foreign"),
        });
        assert!(fixture.admit().is_err(), "case {case}");
    }
}

#[test]
fn metal_admission_rejects_ambiguous_or_auxiliary_signed_inventory() {
    let mut fixture = Fixture::new(|_| {});
    fixture
        .authority
        .entries
        .push(fixture.authority.entries[0].clone());
    assert!(MetalReleasePermit::for_native_release(fixture.authority.clone()).is_err());
    let auxiliary = Fixture::new(|manifest| {
        manifest.payload.push(PayloadEntry {
            path: "worker/auxiliary.dylib".to_owned(),
            size_bytes: 13,
            sha256: format!("{:x}", Sha256::digest(b"signed worker")),
        })
    });
    fixture_verifier(fixture_trust())
        .verify(&auxiliary.root)
        .unwrap();
    assert!(auxiliary.admit().is_err());
}

#[test]
fn metal_admission_rejects_signed_source_different_from_exact_permit() {
    let source = Fixture::new(|_| {});
    let other =
        Fixture::new(|manifest| manifest.pack_version = StoreComponent::new("other").unwrap());
    assert!(
        admit_pinned(
            MetalReleasePermit::for_native_release(other.release().unwrap()).unwrap(),
            source.pinned(),
            fixture_verifier(fixture_trust())
        )
        .is_err()
    );
}

#[test]
fn metal_admission_rejects_size_root_inventory_and_epoch_in_release() {
    let fixture = Fixture::new(|_| {});
    for case in 0..7 {
        let mut release = fixture.authority.clone();
        match case {
            0 => release.entries[0].installed_size_bytes += 1,
            1 => release.entries[0].root.push_str("/extra"),
            2 => release.entries[0].files.reverse(),
            3 => release.entries[0].files.push("extra".to_owned()),
            4 => release.entries[0].security_epoch += 1,
            5 => release.entries[0].compressed_size_bytes = 0,
            _ => release.release_security_epoch = 0,
        }
        let result = MetalReleasePermit::for_native_release(release).and_then(|permit| {
            admit_pinned(permit, fixture.pinned(), fixture_verifier(fixture_trust()))
        });
        assert!(result.is_err(), "case {case}");
    }
}

#[test]
fn metal_admission_rejects_tampering_and_unexpected_files() {
    for extra in [false, true] {
        let fixture = Fixture::new(|_| {});
        if extra {
            fs::write(fixture.root.join("extra"), b"unexpected").unwrap();
        } else {
            fs::write(fixture.root.join(WORKER), b"forged worker").unwrap();
        }
        assert!(fixture.admit().is_err());
    }
}

#[test]
fn metal_admission_rejects_protocol_epoch_and_signature_failures() {
    for case in 0..4 {
        let fixture = Fixture::new(|manifest| match case {
            0 => manifest.app_protocol_version += 1,
            1 => manifest.worker_protocol_version += 1,
            2 => manifest.security_epoch = 0,
            _ => {}
        });
        if case == 3 {
            fs::write(fixture.root.join(manifest::SIGNATURE_NAME), b"{}").unwrap();
        }
        assert!(fixture.admit().is_err(), "case {case}");
    }
}

#[cfg(unix)]
#[test]
fn metal_admission_reverification_checks_bytes_not_only_directory_identity() {
    let fixture = Fixture::new(|_| {});
    let admitted = fixture.admit().unwrap();
    fs::write(fixture.root.join(WORKER), b"forged worker").unwrap();
    admitted.lease.recheck().unwrap(); // Identity alone is deliberately insufficient.
    assert!(admitted.reverify().is_err());
}

#[cfg(unix)]
#[test]
fn metal_admission_rejects_source_replacement_links_and_hardlinks() {
    use std::os::unix::fs::symlink;
    for case in 0..3 {
        let fixture = Fixture::new(|_| {});
        let admitted = fixture.admit().unwrap();
        match case {
            0 => {
                fs::rename(&fixture.root, fixture.root.with_extension("moved")).unwrap();
                fs::create_dir(&fixture.root).unwrap();
            }
            1 => {
                fs::rename(fixture.root.join(WORKER), fixture.owner.join("image")).unwrap();
                symlink(fixture.owner.join("image"), fixture.root.join(WORKER)).unwrap();
            }
            _ => fs::hard_link(fixture.root.join(WORKER), fixture.owner.join("alias")).unwrap(),
        }
        assert!(admitted.reverify().is_err(), "case {case}");
    }
}

#[cfg(unix)]
#[test]
fn metal_admission_fifo_probe() {
    // Private subprocess entry, absent outside unit-test binaries. The parent
    // bounds this test so an accidental return to blocking opens fails cleanly.
    // Ambient test variables alone must never make the ordinary suite open a
    // caller-selected path or take the subprocess-only completion exit.
    let arguments = std::env::args_os().skip(1).take(2).collect::<Vec<_>>();
    if arguments.as_slice() != ["--exact", "admission::tests::metal_admission_fifo_probe"] {
        return;
    }
    let Some(root) = std::env::var_os("SCRIBE_TEST_METAL_FIFO_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(manifest::open_regular_no_follow(&root.join(manifest::MANIFEST_NAME)).is_err());
    let handle = std::fs::File::open(&root).unwrap();
    let pinned = PinnedPackRoot::from_anchored_handles(root, vec![handle]).unwrap();
    assert!(
        fixture_verifier(fixture_trust())
            .verify_pinned(pinned)
            .is_err()
    );
    // A renamed/missing --exact test exits zero with no tests. The parent's
    // unique completion code proves this body reached both rejection checks.
    std::process::exit(73);
}

#[cfg(unix)]
#[test]
fn metal_admission_fifo_inputs_are_rejected_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let fixture = Fixture::new(|_| {});
    let path = fixture.root.join(manifest::MANIFEST_NAME);
    fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "admission::tests::metal_admission_fifo_probe"])
        .env("SCRIBE_TEST_METAL_FIFO_ROOT", &fixture.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert_eq!(
                    status.code(),
                    Some(73),
                    "FIFO probe did not complete its assertions"
                );
                break;
            }
            Ok(None) => {}
            Err(error) => {
                let killed = child.kill();
                let reaped = child.wait();
                panic!("FIFO probe wait failed: {error}; kill={killed:?}; reap={reaped:?}");
            }
        }
        if Instant::now() >= deadline {
            let killed = child.kill();
            let reaped = child.wait();
            panic!("candidate verification blocked on a FIFO; kill={killed:?}; reap={reaped:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn metal_admission_native_pin_rejects_alias_spelling_and_symlink_ancestors() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new(|_| {});
    let pinned = pin_candidate(&fixture.root).unwrap();
    admit_pinned(
        MetalReleasePermit::for_native_release(fixture.release().unwrap()).unwrap(),
        pinned,
        fixture_verifier(fixture_trust()),
    )
    .unwrap()
    .reverify()
    .unwrap();
    for path in [
        PathBuf::from("relative"),
        fixture.root.join(".."),
        PathBuf::from(format!("{}/.", fixture.root.display())),
        PathBuf::from(format!("{}/", fixture.root.display())),
    ] {
        assert!(pin_candidate(&path).is_err());
    }
    symlink(fixture.owner.join("workers"), fixture.owner.join("alias")).unwrap();
    let via_link = fixture
        .owner
        .join("alias/packs")
        .join(fixture.manifest.pack_id.as_str())
        .join(fixture.manifest.pack_version.as_str())
        .join(&fixture.manifest.pack_digest);
    assert!(pin_candidate(&via_link).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn metal_admission_native_empty_release_rejects_before_candidate_access() {
    assert!(
        admit_source(std::path::Path::new("/nonexistent-candidate"))
            .err()
            .unwrap()
            .to_string()
            .contains("no native Metal pack")
    );
}
