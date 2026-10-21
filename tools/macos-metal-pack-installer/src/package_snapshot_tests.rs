use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    owner: PathBuf,
    pack: PathBuf,
}
impl Fixture {
    fn new(arch: &str) -> Self {
        let owner = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "scribe-metal-snapshot-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&owner).unwrap();
        let pack = owner.join("pack");
        fs::create_dir(&pack).unwrap();
        fs::create_dir(pack.join("worker")).unwrap();
        let worker = b"public synthetic Metal worker\n";
        fs::write(pack.join("worker/scribe-inference-worker"), worker).unwrap();
        let manifest = serde_json::json!({"backend":"metal","target_os":"macos","target_arch":arch,"worker_path":"worker/scribe-inference-worker", "payload":[{"path":"worker/scribe-inference-worker","size_bytes":worker.len(),"sha256":format!("{:x}",Sha256::digest(worker))}]});
        fs::write(
            pack.join("pack-manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            pack.join("pack-manifest.sig"),
            b"public unsigned source fixture\n",
        )
        .unwrap();
        Self { owner, pack }
    }
    fn digest(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(fs::read(self.pack.join("pack-manifest.json")).unwrap())
        )
    }
    fn output(&self) -> PathBuf {
        self.owner.join("snapshot")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        AFTER_HASH.with(|hook| *hook.borrow_mut() = None);
        fs::remove_dir_all(&self.owner).unwrap();
    }
}

#[test]
fn metal_snapshot_freezes_exact_slots_for_each_explicit_target_not_host() {
    assert_eq!(MANIFEST_MAX, crate::manifest::MAX_MANIFEST_BYTES);
    assert_eq!(SIGNATURE_MAX, crate::manifest::MAX_SIGNATURE_BYTES);
    assert_eq!(WORKER_MAX, crate::manifest::MAX_FILE_BYTES);
    for arch in ["aarch64", "x86_64"] {
        let f = Fixture::new(arch);
        snapshot(&f.pack, &f.output(), &f.digest(), arch).unwrap();
        for (input, output) in [
            "pack-manifest.json",
            "pack-manifest.sig",
            "worker/scribe-inference-worker",
        ]
        .into_iter()
        .zip(OUTPUTS)
        {
            assert_eq!(
                fs::read(f.pack.join(input)).unwrap(),
                fs::read(f.output().join(output)).unwrap()
            );
        }
        assert_eq!(fs::read_dir(f.output()).unwrap().count(), 3);
    }
}
#[test]
fn metal_snapshot_rejects_wrong_frozen_metadata_worker_and_lengths() {
    for wrong_manifest in [false, true] {
        let f = Fixture::new("aarch64");
        let digest = f.digest();
        if wrong_manifest {
            fs::write(f.pack.join("pack-manifest.json"), b"changed").unwrap();
        } else {
            fs::write(
                f.pack.join("worker/scribe-inference-worker"),
                b"tampered synthetic worker",
            )
            .unwrap();
        }
        assert!(snapshot(&f.pack, &f.output(), &digest, "aarch64").is_err());
    }
    for maximum in [MANIFEST_MAX, SIGNATURE_MAX, WORKER_MAX] {
        length(1, maximum).unwrap();
        length(maximum, maximum).unwrap();
        for size in [0, maximum + 1, u64::MAX] {
            assert!(length(size, maximum).is_err());
        }
    }
    let changes: [fn(&mut serde_json::Value); 14] = [
        |m| m["target_os"] = serde_json::json!("linux"),
        |m| m["backend"] = serde_json::json!("cuda"),
        |m| m["worker_path"] = serde_json::json!("worker/other"),
        |m| m["payload"] = serde_json::json!([]),
        |m| {
            let entry = m["payload"][0].clone();
            m["payload"].as_array_mut().unwrap().push(entry);
        },
        |m| m["payload"][0]["path"] = serde_json::json!("worker/other"),
        |m| {
            m["payload"][0]
                .as_object_mut()
                .unwrap()
                .remove("size_bytes");
        },
        |m| m["payload"][0]["size_bytes"] = serde_json::json!("30"),
        |m| m["payload"][0]["size_bytes"] = serde_json::json!(1.5),
        |m| m["payload"][0]["size_bytes"] = serde_json::json!(0),
        |m| m["payload"][0]["size_bytes"] = serde_json::json!(WORKER_MAX + 1),
        |m| m["payload"][0]["sha256"] = serde_json::json!(null),
        |m| m["payload"][0]["sha256"] = serde_json::json!("AB".repeat(32)),
        |m| m["payload"][0]["sha256"] = serde_json::json!("0".repeat(64)),
    ];
    for change in changes {
        let f = Fixture::new("aarch64");
        let file = f.pack.join("pack-manifest.json");
        let mut value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        change(&mut value);
        fs::write(file, serde_json::to_vec(&value).unwrap()).unwrap();
        // Recomputed anchor proves shape/worker rejection, not stale SHA failure.
        assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    }
}
#[test]
fn metal_snapshot_requires_frozen_authority_safe_paths_and_target_before_io() {
    let f = Fixture::new("aarch64");
    assert!(
        snapshot(&f.owner.join("missing"), &f.output(), "invalid", "aarch64")
            .unwrap_err()
            .to_string()
            .contains("frozen manifest")
    );
    for arch in ["", "arm64", "riscv64", "x86_64"] {
        assert!(snapshot(&f.pack, &f.output(), &f.digest(), arch).is_err());
        // Incompatible shape may create only this private partial snapshot.
        if f.output().exists() {
            fs::remove_dir_all(f.output()).unwrap();
        }
    }
    assert!(validate_path(Path::new("relative")).is_err());
    assert!(validate_path(&f.owner.join("bad\ncargo:rustc-env=bad")).is_err());
    for (catalog, authority) in [
        (b"{}".as_slice(), b"{}".as_slice()),
        (
            b"{\"packs\":[]}",
            b"{\"entries\":[{}],\"release_security_epoch\":1}",
        ),
        (
            b"{\"packs\":[{}]}",
            b"{\"entries\":[{}],\"release_security_epoch\":0}",
        ),
    ] {
        assert!(require_release_inputs(catalog, authority).is_err());
    }
    require_release_inputs(
        b"{\"packs\":[{}]}",
        b"{\"entries\":[{}],\"release_security_epoch\":1}",
    )
    .unwrap(); // Shape ONLY, not admission.
}
#[test]
fn metal_snapshot_never_overwrites_output_or_adopts_partial_work() {
    let f = Fixture::new("aarch64");
    fs::create_dir(f.output()).unwrap();
    fs::write(f.output().join("sentinel"), b"preserve").unwrap();
    assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    assert_eq!(fs::read(f.output().join("sentinel")).unwrap(), b"preserve");
    assert_eq!(fs::read_dir(f.output()).unwrap().count(), 1);
}
#[test]
fn metal_snapshot_rejects_extra_missing_empty_and_partial_inputs() {
    for input in [
        "extra",
        "worker/extra",
        "pack-manifest.sig",
        "worker/scribe-inference-worker",
    ] {
        let f = Fixture::new("aarch64");
        if input.ends_with("extra") {
            fs::write(f.pack.join(input), b"unexpected").unwrap();
        } else {
            fs::write(f.pack.join(input), []).unwrap();
        }
        assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    }
    let f = Fixture::new("aarch64");
    fs::remove_file(f.pack.join("worker/scribe-inference-worker")).unwrap();
    assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    assert!(!f.output().exists());
    for index in 0..100 {
        fs::write(f.pack.join(format!("extra-{index}")), b"extra").unwrap();
    }
    assert!(
        names(
            &f.pack,
            &["pack-manifest.json", "pack-manifest.sig", "worker"]
        )
        .is_err()
    );
}
#[test]
fn metal_snapshot_independent_readback_detects_mutation_after_source_hash() {
    let f = Fixture::new("aarch64");
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let witness = fired.clone();
    AFTER_HASH.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |source| {
            if source.ends_with("scribe-inference-worker") {
                witness.set(true);
                let mut bytes = fs::read(source).unwrap();
                bytes[0] ^= 1;
                fs::write(source, bytes).unwrap();
            }
        }))
    });
    assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    assert!(fired.get());
}
#[cfg(windows)]
#[test]
fn metal_snapshot_windows_hardlinks_reparse_and_stream_paths_reject() {
    let f = Fixture::new("x86_64");
    fs::hard_link(
        f.pack.join("pack-manifest.sig"),
        f.owner.join("signature-link"),
    )
    .unwrap();
    assert!(snapshot(&f.pack, &f.output(), &f.digest(), "x86_64").is_err());
    assert!(validate_path(&f.owner.join("stream:payload")).is_err());
    // PathBuf::join normalizes parents on verbatim Windows paths. Supply the
    // raw argument spelling so this actually exercises lexical rejection.
    let parent_alias = format!("{}\\parent\\..\\alias", f.owner.display());
    assert!(validate_path(Path::new(&parent_alias)).is_err());
    assert!(validate_path(&f.owner.join("trailing.")).is_err());
    let native = f.pack.to_str().unwrap();
    let native = native.strip_prefix("\\\\?\\").unwrap_or(native);
    validate_path(Path::new(&native.replace('\\', "/"))).unwrap();
    fs::remove_file(f.owner.join("signature-link")).unwrap();
    let alias = f.owner.join("alias");
    for path in [&alias, &f.pack] {
        assert!(
            !path
                .to_str()
                .unwrap()
                .contains(['&', '|', '%', '!', '^', '<', '>', '"', '(', ')', '\r', '\n']),
            "private cmd fixture path contains shell metacharacters"
        );
    }
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/v:off", "/c", "mklink", "/J"])
        .arg(&alias)
        .arg(&f.pack)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "private fixture junction creation failed"
    );
    assert!(
        snapshot(
            &alias,
            &f.owner.join("alias-snapshot"),
            &f.digest(),
            "x86_64"
        )
        .is_err()
    );
    assert!(!f.owner.join("alias-snapshot").exists());
}
#[cfg(unix)]
#[test]
fn metal_snapshot_native_links_and_fifo_are_rejected_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let f = Fixture::new("aarch64");
    let signature = f.pack.join("pack-manifest.sig");
    fs::rename(&signature, f.owner.join("signature")).unwrap();
    symlink(f.owner.join("signature"), &signature).unwrap();
    assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    fs::remove_file(&signature).unwrap();
    fs::hard_link(f.owner.join("signature"), &signature).unwrap();
    if f.output().exists() {
        fs::remove_dir_all(f.output()).unwrap();
    }
    assert!(snapshot(&f.pack, &f.output(), &f.digest(), "aarch64").is_err());
    fs::remove_file(&signature).unwrap();
    let name = std::ffi::CString::new(signature.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(open_regular(&signature, SIGNATURE_MAX).is_err());
}
