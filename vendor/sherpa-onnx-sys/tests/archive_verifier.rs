#[path = "../src/archive_verifier.rs"]
mod archive_verifier;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use archive_verifier::{
    activate_verified_archive, debug_escape_allowed, reviewed_static_archive_integrity,
    static_archive_name, unpack_archive_safely, validate_static_library_layout, verify_archive,
    ArchiveIntegrity, STATIC_ARCHIVES,
};
use bzip2::write::BzEncoder;
use bzip2::Compression;
use sha2::{Digest, Sha256};
use tar::Builder;

fn temp_dir(label: &str) -> PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "scribe-sherpa-{label}-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_archive(path: &Path, entry: &Path, bytes: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let encoder = BzEncoder::new(file, Compression::best());
    let mut builder = Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, entry, bytes).unwrap();
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn write_raw_path_archive(path: &Path, raw_path: &[u8], bytes: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let encoder = BzEncoder::new(file, Compression::best());
    let mut builder = Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.as_mut_bytes()[..raw_path.len()].copy_from_slice(raw_path);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, bytes).unwrap();
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn integrity_for(path: &Path) -> ArchiveIntegrity {
    let bytes = fs::read(path).unwrap();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    ArchiveIntegrity {
        name: "fixture.tar.bz2",
        size: bytes.len() as u64,
        sha256: Box::leak(digest.into_boxed_str()),
    }
}

#[test]
fn all_five_release_static_archives_are_pinned() {
    let mappings = [
        ("windows", "x86_64", "win-x64-static-MT-Release-lib.tar.bz2"),
        ("linux", "x86_64", "linux-x64-static-lib.tar.bz2"),
        ("linux", "aarch64", "linux-aarch64-static-lib.tar.bz2"),
        ("macos", "x86_64", "osx-x64-static-lib.tar.bz2"),
        ("macos", "aarch64", "osx-arm64-static-lib.tar.bz2"),
    ];
    assert_eq!(STATIC_ARCHIVES.len(), mappings.len());
    for (os, arch, suffix) in mappings {
        let name = static_archive_name("1.13.5", os, arch).unwrap();
        assert_eq!(name, format!("sherpa-onnx-v1.13.5-{suffix}"));
        let archive = reviewed_static_archive_integrity(&name).unwrap();
        assert_eq!(archive.name, name);
        assert_eq!(archive.sha256.len(), 64);
        assert!(archive.size > 1_000_000);
    }
}

#[test]
fn debug_escape_hatches_are_impossible_in_release_profiles() {
    assert!(debug_escape_allowed("debug", true, true));
    assert!(!debug_escape_allowed("debug", true, false));
    assert!(!debug_escape_allowed("release", true, true));
    assert!(!debug_escape_allowed("release", false, true));
    assert!(!debug_escape_allowed("debug", false, true));
}

#[test]
fn correct_archive_is_verified_then_extracted() {
    let root = temp_dir("correct");
    let archive = root.join("fixture.tar.bz2");
    write_archive(&archive, Path::new("expected/lib/libfixture.a"), b"fixture");
    let integrity = integrity_for(&archive);
    let destination = root.join("extract");

    verify_archive(&archive, &integrity).unwrap();
    fs::create_dir_all(&destination).unwrap();
    unpack_archive_safely(&archive, &destination, "expected").unwrap();
    assert_eq!(
        fs::read(destination.join("expected/lib/libfixture.a")).unwrap(),
        b"fixture"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn wrong_size_or_tampering_is_rejected_before_extraction() {
    let root = temp_dir("tampered");
    let archive = root.join("fixture.tar.bz2");
    write_archive(&archive, Path::new("expected/lib/libfixture.a"), b"fixture");
    let integrity = integrity_for(&archive);
    let destination = root.join("extract");

    fs::OpenOptions::new()
        .append(true)
        .open(&archive)
        .unwrap()
        .write_all(b"tampered")
        .unwrap();
    assert!(verify_archive(&archive, &integrity)
        .unwrap_err()
        .to_string()
        .contains("size mismatch"));
    assert!(!destination.exists());

    fs::write(&archive, vec![0_u8; integrity.size as usize]).unwrap();
    assert!(verify_archive(&archive, &integrity)
        .unwrap_err()
        .to_string()
        .contains("SHA-256 mismatch"));
    assert!(!destination.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn traversal_or_wrong_root_is_rejected_before_unpacking() {
    let root = temp_dir("traversal");
    let archive = root.join("fixture.tar.bz2");
    write_raw_path_archive(&archive, b"expected/../../escape", b"fixture");
    let destination = root.join("extract");

    assert!(unpack_archive_safely(&archive, &destination, "expected").is_err());
    assert!(!root.join("escape").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn extracted_cache_without_its_verified_archive_is_rejected() {
    let root = temp_dir("missing-archive");
    let cache = root.join("cache");
    let lib = cache.join("expected/lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("libfixture.a"), b"fixture").unwrap();
    let integrity = ArchiveIntegrity {
        name: "missing.tar.bz2",
        size: 1,
        sha256: "00",
    };
    assert!(activate_verified_archive(
        &root.join("missing.tar.bz2"),
        &integrity,
        &cache,
        "expected",
        "linux",
        &["fixture"],
    )
    .is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tampered_archive_is_rejected_even_when_extracted_cache_looks_complete() {
    let root = temp_dir("poisoned-cache");
    let archive = root.join("fixture.tar.bz2");
    write_archive(&archive, Path::new("expected/lib/libfixture.a"), b"fixture");
    let integrity = integrity_for(&archive);
    let cache = root.join("cache");
    let lib = cache.join("expected/lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("libfixture.a"), b"fixture").unwrap();
    fs::write(&archive, vec![0_u8; integrity.size as usize]).unwrap();

    let error = activate_verified_archive(
        &archive,
        &integrity,
        &cache,
        "expected",
        "linux",
        &["fixture"],
    )
    .unwrap_err();
    assert!(error.to_string().contains("SHA-256 mismatch"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verified_archive_replaces_poisoned_extracted_library_bytes() {
    let root = temp_dir("poisoned-library");
    let archive = root.join("fixture.tar.bz2");
    write_archive(&archive, Path::new("expected/lib/libfixture.a"), b"reviewed");
    let integrity = integrity_for(&archive);
    let cache = root.join("cache");
    let lib = cache.join("expected/lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(lib.join("libfixture.a"), b"poisoned").unwrap();

    let activated = activate_verified_archive(
        &archive,
        &integrity,
        &cache,
        "expected",
        "linux",
        &["fixture"],
    )
    .unwrap();
    assert_eq!(fs::read(activated.join("libfixture.a")).unwrap(), b"reviewed");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_staging_directory_is_ignored_during_atomic_activation() {
    let root = temp_dir("partial-staging");
    let archive = root.join("fixture.tar.bz2");
    write_archive(&archive, Path::new("expected/lib/libfixture.a"), b"fixture");
    let integrity = integrity_for(&archive);
    let cache = root.join("cache");
    let partial = cache.join(".expected.staging-interrupted/expected/lib");
    fs::create_dir_all(&partial).unwrap();
    fs::write(partial.join("libfixture.a"), b"partial").unwrap();

    let activated = activate_verified_archive(
        &archive,
        &integrity,
        &cache,
        "expected",
        "linux",
        &["fixture"],
    )
    .unwrap();
    assert_eq!(
        fs::read(activated.join("libfixture.a")).unwrap(),
        b"fixture"
    );
    assert!(
        partial.exists(),
        "unrelated partial staging is not trusted or activated"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_activation_fails_safe_and_leaves_only_reviewed_bytes() {
    let root = temp_dir("concurrent-activation");
    let archive = root.join("fixture.tar.bz2");
    write_archive(&archive, Path::new("expected/lib/libfixture.a"), b"reviewed");
    let integrity = integrity_for(&archive);
    let cache = root.join("cache");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let archive = archive.clone();
        let cache = cache.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            activate_verified_archive(
                &archive,
                &integrity,
                &cache,
                "expected",
                "linux",
                &["fixture"],
            )
            .is_ok()
        }));
    }
    barrier.wait();
    let successes = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(|succeeded| *succeeded)
        .count();
    assert!(successes >= 1);
    assert_eq!(
        fs::read(cache.join("expected/lib/libfixture.a")).unwrap(),
        b"reviewed"
    );
    for entry in fs::read_dir(&cache).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(!name.contains(".staging-") && !name.contains(".replaced-"));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn static_library_layout_requires_every_platform_named_archive() {
    let root = temp_dir("library-layout");
    fs::write(root.join("libfirst.a"), b"one").unwrap();
    assert!(validate_static_library_layout(&root, "linux", &["first", "second"]).is_err());
    fs::write(root.join("libsecond.a"), b"two").unwrap();
    validate_static_library_layout(&root, "linux", &["first", "second"]).unwrap();

    fs::write(root.join("first.lib"), b"one").unwrap();
    assert!(validate_static_library_layout(&root, "windows", &["first", "second"]).is_err());
    fs::write(root.join("second.lib"), b"two").unwrap();
    validate_static_library_layout(&root, "windows", &["first", "second"]).unwrap();
    fs::remove_dir_all(root).unwrap();
}
