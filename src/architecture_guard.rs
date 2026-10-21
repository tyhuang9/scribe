//! Test-only architectural boundary guard.
//!
//! The executable is a binary crate, so integration tests cannot rely on a
//! public library surface. These tests inspect the checked-out Rust sources
//! directly and fail closed when a concrete runtime leaks above the private
//! router or when UI code starts selecting model families.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources() -> Vec<(PathBuf, String)> {
    fn visit(root: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).expect("source directory must be readable") {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    visit(&source_root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&source_root)
                .expect("source path stays beneath src")
                .to_path_buf();
            let source = fs::read_to_string(&path).expect("Rust source must be UTF-8");
            (relative, source)
        })
        .collect()
}

fn rust_code_mask(source: &str) -> Vec<bool> {
    let bytes = source.as_bytes();
    let mut code = vec![true; bytes.len()];
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            let start = index;
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            code[start..index].fill(false);
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            let mut depth = 1_u32;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    depth += 1;
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            code[start..index].fill(false);
        } else if bytes[index] == b'"'
            || (bytes[index] == b'\''
                && (bytes.get(index + 2) == Some(&b'\'')
                    || (bytes.get(index + 1) == Some(&b'\\')
                        && bytes[index + 2..bytes.len().min(index + 13)].contains(&b'\''))))
        {
            let quote = bytes[index];
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == quote {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            code[start..index].fill(false);
        } else if bytes[index] == b'r' {
            let mut cursor = index + 1;
            while bytes.get(cursor) == Some(&b'#') {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'"') {
                let hashes = cursor - index - 1;
                let start = index;
                cursor += 1;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'"'
                        && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&vec![b'#'; hashes])
                    {
                        cursor += 1 + hashes;
                        break;
                    }
                    cursor += 1;
                }
                index = cursor;
                code[start..index].fill(false);
            } else {
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    code
}

fn production_source(source: &str) -> String {
    const TEST_ATTR: &str = "#[cfg(test)]";
    // Architecture assertions inspect checked-out source rather than the Rust
    // token stream. Normalize platform checkout endings so a Windows CRLF
    // worktree has exactly the same guard semantics as an LF worktree.
    let mut retained = source.replace("\r\n", "\n").replace('\r', "\n");
    loop {
        let mask = rust_code_mask(&retained);
        let Some(start) = retained
            .match_indices(TEST_ATTR)
            .find_map(|(index, _)| mask[index].then_some(index))
        else {
            return retained;
        };
        let bytes = retained.as_bytes();
        let mut cursor = start + TEST_ATTR.len();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        while retained[cursor..].starts_with("#[") {
            let attribute_end = retained[cursor..]
                .find(']')
                .map(|offset| cursor + offset + 1)
                .expect("test-gated companion attribute must close");
            cursor = attribute_end;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
        }
        let mask = rust_code_mask(&retained);
        let mut parens = 0_i32;
        let mut brackets = 0_i32;
        let mut end = None;
        let mut index = cursor;
        while index < bytes.len() {
            if !mask[index] {
                index += 1;
                continue;
            }
            match bytes[index] {
                b'(' => parens += 1,
                b')' => parens -= 1,
                b'[' => brackets += 1,
                b']' => brackets -= 1,
                b'{' if parens == 0 && brackets == 0 => {
                    let mut depth = 1_i32;
                    index += 1;
                    while index < bytes.len() && depth > 0 {
                        if mask[index] {
                            match bytes[index] {
                                b'{' => depth += 1,
                                b'}' => depth -= 1,
                                _ => {}
                            }
                        }
                        index += 1;
                    }
                    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                        index += 1;
                    }
                    if matches!(bytes.get(index), Some(b';' | b',')) {
                        index += 1;
                    }
                    end = Some(index);
                    break;
                }
                b';' | b',' if parens == 0 && brackets == 0 => {
                    end = Some(index + 1);
                    break;
                }
                _ => {}
            }
            index += 1;
        }
        let end = end.expect("cfg(test)-gated Rust item must have a terminator");
        retained.replace_range(start..end, "");
    }
}

fn production_source_for(_path: &Path, source: &str) -> String {
    production_source(source)
}

fn named_function_bodies(source: &str, name: &str) -> Vec<String> {
    let needle = format!("fn {name}");
    let mask = rust_code_mask(source);
    let mut bodies = Vec::new();
    for (start, _) in source.match_indices(&needle) {
        if !mask[start] {
            continue;
        }
        let Some(relative_open) = source[start..].find('{') else {
            continue;
        };
        let open = start + relative_open;
        let mut depth = 1_i32;
        let mut cursor = open + 1;
        while cursor < source.len() && depth != 0 {
            if mask[cursor] {
                match source.as_bytes()[cursor] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            cursor += 1;
        }
        if depth == 0 {
            bodies.push(source[open + 1..cursor - 1].to_owned());
        }
    }
    bodies
}

fn production_pack_provisioning_allowed(
    registry_body: &str,
    worker: &str,
    trust_root_is_empty: bool,
    unix_target: bool,
) -> bool {
    let registry_is_empty = registry_body.contains("ProductionPackRegistry::empty()")
        && !registry_body.contains("from_launch_bindings");
    let registry_routes_concrete_bridge = registry_body
        .contains("crate::onnx_worker::discover_production_pack_launch_bindings")
        && registry_body.contains("ProductionPackRegistry::from_launch_bindings");
    let concrete_resolver_hello_flow = worker
        .contains("fn discover_production_pack_launch_bindings(")
        && worker.contains("impl ResolverHelloBindingBridge for")
        && worker.contains("fn resolver_verified_pack_lease(&self) -> Arc<VerifiedPackLease>")
        && worker.contains("from_verified_pack_lease")
        && worker.contains("launch_verified_worker")
        && worker.contains("VerifiedPackLaunchBinding::try_from_resolver_hello_bridge")
        && worker.contains("trait WorkerExecutableResolver")
        && worker.contains("Hello");
    let unix_launch_bodies = named_function_bodies(worker, "launch_verified_worker").join("\n");
    // Follow only the exact closed Pack delegate, not an arbitrary helper or
    // the CPU entry. Moving shared spawn mechanics must not hide Unix proof.
    let macos_launch_bodies = if unix_launch_bodies
        .contains("launch_worker(LaunchAuthority::Pack(authority), worker_flag, bindings)")
    {
        named_function_bodies(worker, "launch_worker").join("\n")
    } else {
        unix_launch_bodies.clone()
    };
    let all_launch_bodies = format!("{unix_launch_bodies}\n{macos_launch_bodies}");
    let unix_authority_constructor =
        named_function_bodies(worker, "from_verified_pack_lease").join("\n");
    let macos_protected_launch_flow = macos_launch_bodies.contains("posix_spawn")
        && macos_launch_bodies.contains("authority.protected_executable_path()")
        && !all_launch_bodies.contains("/dev/fd/")
        && unix_authority_constructor.contains("macos_protected_authority(")
        && worker.contains("ProtectedPackAuthority::open(");
    let unix_verified_launch_flow = !unix_target
        || (worker.contains("UnixPackExecAuthority")
            && worker.contains("resolver_unix_launch_authority")
            && worker.contains("executable_fd")
            && worker.contains("dependency_root_fd")
            && unix_authority_constructor.contains("open_copy_file")
            && unix_authority_constructor.contains("hash_exact_length")
            && (unix_launch_bodies.contains("execveat")
                || unix_launch_bodies.contains("fexecve")
                || macos_protected_launch_flow)
            && !all_launch_bodies.contains("Command::spawn")
            && !all_launch_bodies.contains("Command::new"));
    (registry_is_empty && trust_root_is_empty)
        || (!trust_root_is_empty
            && (registry_is_empty || registry_routes_concrete_bridge)
            && concrete_resolver_hello_flow
            && unix_verified_launch_flow)
}

#[test]
fn macos_protected_cpu_guard_traces_only_the_closed_pack_spawn_delegate_on_unix() {
    let pack = production_source(include_str!("gpu_worker_pack/mod.rs"));
    let worker = production_source(include_str!("onnx_worker.rs"));
    let launcher = production_source(include_str!("macos_worker_launch.rs"));
    let registry = "ProductionPackRegistry::empty()";
    let flow = format!("{worker}\n{pack}\n{launcher}");
    assert!(production_pack_provisioning_allowed(
        registry, &flow, false, true
    ));

    let wrong_delegate = flow.replace(
        "launch_worker(LaunchAuthority::Pack(authority), worker_flag, bindings)",
        "launch_worker(LaunchAuthority::Cpu(authority), worker_flag, bindings)",
    );
    assert_ne!(wrong_delegate, flow);
    assert!(!production_pack_provisioning_allowed(
        registry,
        &wrong_delegate,
        false,
        true
    ));
    for injected in [
        "Command::new(\"unverified\");",
        "Command::spawn();",
        "let _path = \"/dev/fd/7\";",
    ] {
        let unsafe_delegate = flow.replace(
            "let stderr = stderr_descriptor(libc::STDERR_FILENO)?;",
            &format!("{injected}\nlet stderr = stderr_descriptor(libc::STDERR_FILENO)?;"),
        );
        assert_ne!(unsafe_delegate, flow);
        assert!(!production_pack_provisioning_allowed(
            registry,
            &unsafe_delegate,
            false,
            true
        ));
    }
}

#[test]
fn cfg_test_stripping_preserves_later_production_for_lf_and_crlf() {
    let fixture = r###"fn before() { let _ = "}"; }
// } comment
#[cfg(test)]
#[allow(dead_code)]
fn hidden() { let _ = r#"{ nested }"#; /* } */ if true { let _ = '{'; } }
struct Boundary {
    before: u8,
    #[cfg(test)]
    hidden_field: String,
    after: u8,
}
fn after() { let _ = "production-after"; }
"###;
    let lf_production = production_source(fixture);
    let crlf_production = production_source(&fixture.replace('\n', "\r\n"));
    assert_eq!(crlf_production, lf_production);
    for production in [lf_production, crlf_production] {
        assert!(production.contains("fn before()"));
        assert!(production.contains("fn after()"));
        assert!(production.contains("after: u8"));
        assert!(!production.contains("fn hidden()"));
        assert!(!production.contains("hidden_field"));
    }
}

#[test]
fn stage_four_guard_rejects_dead_binding_declarations() {
    let populated_registry = r#"{
        ProductionPackRegistry::from_launch_bindings(
            crate::onnx_worker::discover_production_pack_launch_bindings()
        )
    "#;
    let dead_declarations = r#"
        struct VerifiedPackLaunchBinding;
        trait WorkerExecutableResolver {}
        struct Hello;
    "#;
    assert!(!production_pack_provisioning_allowed(
        populated_registry,
        dead_declarations,
        false,
        false,
    ));

    let concrete_flow = r#"
        trait WorkerExecutableResolver {}
        struct Hello;
        struct VerifiedPackLease;
        struct Arc<T>(T);
        impl ResolverHelloBindingBridge for ConcreteResolverHelloBridge {}
        fn resolver_verified_pack_lease(&self) -> Arc<VerifiedPackLease> {}
        fn from_verified_pack_lease(lease: Arc<VerifiedPackLease>) { open_copy_file(); hash_exact_length(); }
        fn launch_verified_worker() {}
        fn discover_production_pack_launch_bindings() {
            VerifiedPackLaunchBinding::try_from_resolver_hello_bridge(&bridge);
        }
    "#;
    assert!(production_pack_provisioning_allowed(
        populated_registry,
        concrete_flow,
        false,
        false,
    ));
    let raw_path_spawn_flow = format!(
        "{concrete_flow}\nfn launch_verified_worker(path: PathBuf, lease: Arc<VerifiedPackLease>) {{ Command::new(path).spawn(); }}"
    );
    assert!(!production_pack_provisioning_allowed(
        populated_registry,
        &raw_path_spawn_flow,
        false,
        true,
    ));
    let unix_fd_flow = format!(
        "{concrete_flow}\nstruct UnixPackExecAuthority {{ executable_fd: OwnedFd, dependency_root_fd: OwnedFd }}\nfn resolver_unix_launch_authority() -> UnixPackExecAuthority {{}}\nfn from_verified_pack_lease(lease: &VerifiedPackLease) {{ open_copy_file(); hash_exact_length(); }}\nfn launch_verified_worker() {{ execveat(executable_fd, dependency_root_fd); }}"
    );
    assert!(production_pack_provisioning_allowed(
        populated_registry,
        &unix_fd_flow,
        false,
        true,
    ));
    let descriptor_path_flow = unix_fd_flow.replace(
        "execveat(executable_fd, dependency_root_fd)",
        "posix_spawn(format!(\"/dev/fd/{}\", executable_fd))",
    );
    assert!(!production_pack_provisioning_allowed(
        populated_registry,
        &descriptor_path_flow,
        false,
        true,
    ));
    let mac_protected_flow = unix_fd_flow
        .replace("hash_exact_length();", "hash_exact_length(); macos_protected_authority(); ProtectedPackAuthority::open(lease);")
        .replace("execveat(executable_fd, dependency_root_fd)", "posix_spawn(authority.protected_executable_path())");
    assert!(production_pack_provisioning_allowed(
        populated_registry,
        &mac_protected_flow,
        false,
        true,
    ));
    assert!(!production_pack_provisioning_allowed(
        populated_registry,
        &mac_protected_flow.replace("macos_protected_authority();", ""),
        false,
        true,
    ));
    let unrelated_command = format!(
        "{unix_fd_flow}\nfn unrelated_test_or_cpu_helper() {{ Command::new(\"helper\").spawn(); }}"
    );
    assert!(production_pack_provisioning_allowed(
        populated_registry,
        &unrelated_command,
        false,
        true,
    ));
    assert!(production_pack_provisioning_allowed(
        "{ ProductionPackRegistry::empty() ",
        "",
        true,
        true,
    ));
    // Public trust enables catalog discovery independently of the legacy empty
    // constructor. It still requires the real resolver/Hello and safe launch flow.
    let empty_registry = "{ ProductionPackRegistry::empty() ";
    for registry in [empty_registry, populated_registry] {
        assert!(!production_pack_provisioning_allowed(
            registry,
            dead_declarations,
            false,
            false,
        ));
        assert!(production_pack_provisioning_allowed(
            registry,
            concrete_flow,
            false,
            false,
        ));
        assert!(!production_pack_provisioning_allowed(
            registry,
            &raw_path_spawn_flow,
            false,
            true,
        ));
        assert!(production_pack_provisioning_allowed(
            registry,
            &unix_fd_flow,
            false,
            true,
        ));
    }
    assert!(!production_pack_provisioning_allowed(
        populated_registry,
        concrete_flow,
        true,
        false,
    ));
    assert!(!production_pack_provisioning_allowed(
        "{ ProductionPackRegistry::from_launch_bindings(unverified) ",
        concrete_flow,
        false,
        false,
    ));
}

#[test]
fn macos_exec_authority_is_descriptor_paired_and_child_measured() {
    let launcher = production_source(include_str!("macos_worker_launch.rs"));
    let worker = production_source(include_str!("onnx_worker.rs"));
    let pack = production_source(include_str!("gpu_worker_pack/mod.rs"));
    let manifest = production_source(include_str!("gpu_worker_pack/manifest.rs"));

    let executable_open = named_function_bodies(&manifest, "open_executable_at").join("\n");
    for required in [
        "libc::openat(",
        "libc::O_EXEC | libc::O_NOFOLLOW | libc::O_CLOEXEC",
    ] {
        assert!(
            executable_open.contains(required),
            "executable open lost {required:?}"
        );
    }
    let pair = named_function_bodies(&pack, "verify_macos_executable_pair").join("\n");
    for required in [
        "libc::F_GETFL",
        "libc::O_EXEC",
        "libc::O_RDONLY",
        "readable_metadata.dev() != executable_metadata.dev()",
        "readable_metadata.ino() != executable_metadata.ino()",
        "entry.size_bytes",
        "readable_metadata.nlink() != 1",
        "hash_exact_file_at(readable, entry.size_bytes, &entry.path)",
        "digest != entry.sha256",
    ] {
        assert!(
            pair.contains(required),
            "macOS pair validation lost {required:?}"
        );
    }
    for required in [
        "authority.readable_executable_fd().as_raw_fd()",
        "authority.protected_executable_path().to_owned()",
        "actions.dup2(executable.raw(), executable.raw())?",
        "actions.dup2(readable_executable.raw(), readable_executable.raw())?",
    ] {
        assert!(
            launcher.contains(required),
            "macOS inherited authority lost {required:?}"
        );
    }
    let compact_launcher = launcher.split_whitespace().collect::<String>();
    for required in [
        "EXECUTABLE_FD_ENV.to_owned(),executable.raw().to_string()",
        "EXECUTABLE_READ_FD_ENV.to_owned(),readable_executable.raw().to_string()",
    ] {
        assert!(
            compact_launcher.contains(required),
            "macOS binding lost {required:?}"
        );
    }
    let take_pair = named_function_bodies(&worker, "take_macos_pack_worker_image_pair").join("\n");
    let validate = take_pair
        .find("validate_macos_pack_worker_image_pair(descriptors)?")
        .expect("child validates descriptors before ownership");
    let own = take_pair
        .find("std::fs::File::from_raw_fd(descriptors.readable)")
        .expect("child owns the validated readable descriptor");
    assert!(validate < own);
    assert!(!take_pair.contains("current_exe"));
    assert!(!take_pair.contains("remove_var"));
    let parse_pair =
        named_function_bodies(&worker, "parse_macos_pack_worker_image_descriptors").join("\n");
    assert!(
        parse_pair.contains("executable.is_some() || readable.is_some() || private_pack_present")
    );
    assert!(parse_pair.contains("descriptors.executable == descriptors.readable"));
    assert!(parse_pair.contains("forbidden.is_some_and"));
    let hash = named_function_bodies(&worker, "sha256_file_at").join("\n");
    assert!(hash.contains("file.read_at("));
    assert!(!hash.contains("File::open"));
    assert!(!hash.contains("seek("));
    let fingerprint =
        named_function_bodies(&worker, "inference_worker_capability_fingerprint").join("\n");
    assert!(fingerprint.contains("match take_macos_pack_worker_image_pair()?"));
    assert!(fingerprint.contains("sha256_file_at(&readable, length)"));
}

#[test]
fn stage_six_macos_verified_launch_is_installer_protected_and_command_free() {
    let launcher = production_source(include_str!("macos_worker_launch.rs"));
    let worker = production_source(include_str!("onnx_worker.rs"));
    let pack = production_source(include_str!("gpu_worker_pack/mod.rs"));
    let manifest = include_str!("../Cargo.toml");
    let build = include_str!("../build.rs");
    let desktop = production_source(include_str!("main.rs"));
    let worker_entry = include_str!("bin/scribe-inference-worker.rs");
    let metal_shim = include_str!("../native/scribe_macos_gpu_shim.m");
    let power_shim = include_str!("../native/scribe_macos_power_shim.c");
    let packaging = include_str!("../scripts/verify-macos-release-package.sh");
    let release_build = include_str!("../scripts/build-macos-release.sh");
    let store = production_source(include_str!("gpu_worker_pack/store.rs"));

    for required in [
        "posix_spawn(",
        "authority.protected_executable_path()",
        "posix_spawn_file_actions_addfchdir_np",
        "actions.fchdir(dependency_root.raw())?",
        "actions.dup2(dependency_root.raw(), dependency_root.raw())?",
        "POSIX_SPAWN_CLOEXEC_DEFAULT",
        "POSIX_SPAWN_SETPGROUP",
        "killpg",
        "waitpid",
        "sanitized_environment",
        "SCRIBE_PRIVATE_PARENT_LIVENESS",
    ] {
        assert!(
            launcher.contains(required),
            "macOS launch lost {required:?}"
        );
    }
    assert!(!launcher.contains("Command::new"));
    assert!(!launcher.contains("std::process::Command"));
    assert!(!launcher.contains("executable.path"));
    assert!(!launcher.contains("posix_spawn_file_actions_addchdir_np"));
    assert!(!launcher.contains("dependency_root_path"));
    let spawn = named_function_bodies(&launcher, "launch_verified_worker").join("\n");
    assert!(!spawn.contains("/dev/fd/"));
    assert!(!spawn.contains("worker_path()"));
    assert!(worker.contains("prepare_macos_protected_pack(lease)?"));
    let preparation = named_function_bodies(&pack, "prepare_macos_protected_pack").join("\n");
    assert!(preparation.contains("ProtectedPackAuthority::open(&source)"));
    assert!(preparation.contains("project_macos_protected_pack(source, &authority, &verifier)"));
    let projection = named_function_bodies(&pack, "project_macos_protected_pack").join("\n");
    assert!(projection.contains("verify_pinned(authority.pinned_root()?)"));
    assert!(projection.contains("retain_execution_source(source)"));
    for forbidden in [
        "create_dir",
        "std::fs::copy",
        "PackStore::new",
        "open_fixture",
    ] {
        assert!(!preparation.contains(forbidden));
    }
    assert!(launcher.contains("SAFE_PARENT_ENVIRONMENT"));
    assert!(launcher.contains("SCRIBE_PRIVATE_"));
    for required in [
        "UnixPackExecAuthority::from_verified_pack_lease",
        "launch_verified_worker(",
        "unix_exec_authority",
        "unix_exec_authority_arc",
        "resolver_unix_launch_authority",
    ] {
        assert!(
            worker.contains(required),
            "worker binding lost {required:?}"
        );
    }
    for required in [
        "open_copy_file",
        "hash_exact_length",
        "open_dependency_root",
        "Arc::ptr_eq",
    ] {
        assert!(pack.contains(required), "pack authority lost {required:?}");
    }
    assert!(
        manifest.contains("metal-acceleration = [\"inference-worker\", \"transcribe-cpp/metal\"]")
    );
    for framework in ["Metal", "Foundation", "IOKit"] {
        assert!(
            build.contains(&format!("framework={framework}")),
            "macOS shim lost {framework} framework link"
        );
    }
    let native_shim = named_function_bodies(build, "prepare_macos_native_shims").join("\n");
    let metal_gate = native_shim
        .find("if !metal_enabled")
        .expect("macOS native build must gate Metal linkage");
    let metal_link = native_shim
        .find("framework=Metal")
        .expect("Metal worker must link Metal.framework");
    assert!(metal_gate < metal_link);
    assert!(native_shim.contains("building_worker.as_deref() == Some(\"1\")"));
    assert!(desktop.contains("mod macos_power"));
    assert!(!desktop.contains("mod macos_gpu"));
    assert!(include_str!("main.rs").contains("metal-acceleration is worker-only"));
    assert!(worker_entry.contains("feature = \"metal-acceleration\""));
    assert!(metal_shim.contains("<Metal/Metal.h>"));
    assert!(!metal_shim.contains("IOPowerSources"));
    assert!(power_shim.contains("IOPowerSources"));
    assert!(!power_shim.contains("Metal/Metal.h"));
    assert!(pack.contains("single_executable_signed_payload"));
    assert!(pack.contains("enforce_production_discovery_epochs(discovery)"));
    assert!(pack.contains("DiscoveryEpochLedger::new"));
    assert!(pack.contains("catalog_matches_release_authority"));
    assert!(pack.contains("EMBEDDED_PACK_RELEASE_AUTHORITY"));
    assert!(
        pack.find("validated_release_authority(&catalog.bytes")
            < pack.find("PRODUCTION_DISCOVERY_CACHE.get_or_init"),
        "signed release authority must be checked before catalog cache lookup"
    );
    assert!(build.contains("SCRIBE_GPU_PACK_RELEASE_AUTHORITY"));
    assert!(build.contains("scribe_gpu_pack_release_authority.json"));
    assert!(store.contains("libc::LOCK_EX | libc::LOCK_NB"));
    assert!(store.contains("LOCKFILE_FAIL_IMMEDIATELY"));
    assert!(store.contains("PackStoreError::LockContended"));
    assert!(release_build.contains("SCRIBE_GPU_PACK_RELEASE_AUTHORITY=\"$release_authority\""));
    assert!(release_build.contains("catalog_digest"));
    assert!(packaging.contains("CPU/UI binary must not load Metal.framework"));
    assert!(packaging.contains("catalog Metal worker has no Metal load command"));
    assert!(packaging.contains("desktop does not embed the exact pack-catalog authority"));
}

#[test]
fn macos_worker_control_pipe_uses_child_only_explicit_inheritance() {
    let launcher = production_source(include_str!("macos_worker_launch.rs"));
    let entry = named_function_bodies(&launcher, "launch_verified_worker").join("\n");
    assert!(
        entry.contains("launch_worker(LaunchAuthority::Pack(authority), worker_flag, bindings)")
    );
    let body = named_function_bodies(&launcher, "launch_worker").join("\n");
    assert!(body.contains("actions.inherit(liveness_pipe.read.raw())?"));
    assert!(!body.contains("actions.dup2(liveness_pipe.read.raw(), liveness_pipe.read.raw())"));
    assert!(launcher.contains("posix_spawn_file_actions_addinherit_np(&mut self.0, descriptor)"));
    assert!(launcher.contains("POSIX_SPAWN_CLOEXEC_DEFAULT"));
}

#[test]
fn macos_protected_cpu_admission_is_compile_anchored_and_model_free() {
    let worker = production_source(include_str!("onnx_worker.rs"));
    let authority = production_source(include_str!("macos_worker_authority.rs"));
    let layout = production_source(include_str!("macos_worker_layout.rs"));
    let launcher = production_source(include_str!("macos_worker_launch.rs"));
    let contract = include_str!("../scripts/test-macos-protected-cpu-worker.sh");
    let resolver = named_function_bodies(&worker, "resolve_protected_macos_cpu_worker").join("\n");
    assert!(resolver.contains("embedded_cpu_digest()?"));
    assert!(resolver.contains("authority.recheck()?"));
    assert!(!resolver.contains("open_fixture"));
    assert!(!resolver.contains("current_exe"));
    assert!(worker.contains("ProtectedCpuAuthority::open()"));
    assert!(
        worker.contains(
            "executable.macos_cpu_authority.is_none() && executable.pack_launch.is_none()"
        )
    );
    assert!(layout.contains("option_env!(\"SCRIBE_BUNDLED_WORKER_SHA256\")"));
    assert!(layout.contains("/Library/Application Support/Scribe/CPU"));
    assert!(!layout.contains("std::env::var"));
    for required in [
        "CpuProduction",
        "validate_cpu_size(length)?",
        "cpu_image_digest(&file, length)? != digest",
        "macos_directory_names",
        "PlannedKind::Regular",
    ] {
        assert!(
            authority.contains(required),
            "CPU authority lost {required}"
        );
    }
    assert!(launcher.contains("enum LaunchAuthority"));
    let cpu_launch = named_function_bodies(&launcher, "launch_verified_cpu_worker").join("\n");
    assert!(cpu_launch.contains("LaunchAuthority::Cpu(authority)"));
    assert!(cpu_launch.contains("role.flag()"));
    assert!(cpu_launch.contains("&[]"));
    assert!(contract.contains("verify-macos-release-package.sh"));
    assert!(contract.contains("--ignored --exact"));
    assert!(contract.contains("[[ \"$count\" == 1 ]]"));
    for forbidden in [
        "sudo ",
        "installer -",
        "codesign --force",
        "cargo build",
        "--features metal-acceleration",
    ] {
        assert!(!contract.contains(forbidden));
    }
}

#[test]
fn macos_protected_cpu_vad_gate_is_binary_local_and_role_isolated() {
    let worker = production_source(include_str!("onnx_worker.rs"));
    let desktop = production_source(include_str!("main.rs"));
    let dedicated = production_source(include_str!("bin/scribe-inference-worker.rs"));
    let launcher = production_source(include_str!("macos_worker_launch.rs"));
    assert!(desktop.contains("const DEDICATED_INFERENCE_WORKER: bool = false;"));
    assert!(dedicated.contains("const DEDICATED_INFERENCE_WORKER: bool = true;"));
    let gate = named_function_bodies(&worker, "protected_macos_vad_worker_build").join("\n");
    assert!(gate.contains("crate::DEDICATED_INFERENCE_WORKER"));
    assert!(gate.contains("compiled_worker_provider(WorkerRole::Inference)"));
    assert!(!gate.contains("compiled_worker_provider(WorkerRole::Vad)"));
    let entry = named_function_bodies(&worker, "run_inference_worker_with_factory").join("\n");
    assert!(entry.contains("WorkerRole::Vad)) if protected_macos_vad_worker_build()"));
    assert!(entry.contains("macos_forbidden_cpu_bindings_present()"));
    assert!(entry.contains("return match worker_loop_for_role("));
    let vad_loop = named_function_bodies(&worker, "worker_loop_for_role").join("\n");
    assert!(vad_loop.contains("&DisabledRecognizerFactory"));
    assert!(vad_loop.contains("&NativeVadFactory"));
    let capability = named_function_bodies(
        &worker,
        "worker_capability_for_provider_with_metal_topology",
    )
    .join("\n");
    assert!(capability.contains("WorkerRole::Vad if protected_macos_vad_worker_build()"));
    assert!(capability.contains("Some(inference_worker_capability_fingerprint()?)"));
    let descriptors =
        named_function_bodies(&worker, "macos_pack_worker_image_descriptors_from_env").join("\n");
    assert!(descriptors.contains("protected_macos_vad_worker_build()"));
    assert!(descriptors.contains("VAD_WORKER_FLAG"));
    assert!(launcher.contains("enum CpuWorkerRole"));
    let role_flag = named_function_bodies(&launcher, "flag").join("\n");
    assert!(role_flag.contains("Self::Inference => crate::onnx_worker::INFERENCE_WORKER_FLAG"));
    assert!(role_flag.contains("Self::Vad => crate::onnx_worker::VAD_WORKER_FLAG"));
    let desktop_entry = named_function_bodies(&worker, "maybe_run_vad_worker").join("\n");
    let rejection = desktop_entry.find("release macOS VAD requires").unwrap();
    assert!(
        rejection
            < desktop_entry
                .find("take_parent_control_reader_from_env()")
                .unwrap()
    );
    let contract = include_str!("../scripts/test-macos-protected-cpu-worker.sh");
    assert!(
        contract.contains("macos_native_protected_cpu_vad_authenticates_health_shutdown_and_reap")
    );
    assert!(
        contract
            .contains("macos_native_protected_cpu_worker_authenticates_health_shutdown_and_reap")
    );
}

#[test]
fn macos_shared_worker_filesystem_policy_is_read_only_and_provider_independent() {
    let shared = production_source(include_str!("macos_worker_fs.rs"));
    let authority = production_source(include_str!("macos_worker_authority.rs"));
    let manifest = production_source(include_str!("gpu_worker_pack/manifest.rs"));
    assert!(authority.contains("use crate::macos_worker_fs::{"));
    assert!(authority.contains("crate::macos_worker_fs::macos_directory_names("));
    assert!(!authority.contains("crate::gpu_worker_pack::manifest::macos_directory_names("));
    assert!(
        manifest
            .contains("crate::macos_worker_fs::macos_directory_names(directory, maximum_entries)")
    );
    assert!(!shared.contains("crate::gpu_worker_pack"));
    for required in [
        "O_NOFOLLOW",
        "O_CLOEXEC",
        "O_NONBLOCK",
        "validate_child_name(name.as_bytes())?",
        "validate_node_policy(",
        "validate_acl_entry(tag, rights)?",
        "validate_filesystem_policy(",
        "libc::fdopendir",
        "libc::openat",
        "NodeIdentity",
    ] {
        assert!(shared.contains(required), "shared policy lost {required}");
    }
    for forbidden in [
        "libc::mkdir",
        "libc::rename",
        "libc::unlink",
        "libc::chown",
        "libc::fchmod",
        "Command::",
        "O_CREAT",
        "O_WRONLY",
        "O_RDWR",
    ] {
        assert!(
            !shared.contains(forbidden),
            "runtime filesystem policy gained mutation: {forbidden}"
        );
    }
    for source in [
        include_str!("main.rs"),
        include_str!("bin/scribe-inference-worker.rs"),
        include_str!("../tools/worker-pack-author/src/main.rs"),
    ] {
        assert!(source.contains("mod macos_worker_fs;"));
    }
}

#[test]
fn macos_cpu_installer_is_isolated_compiled_authority_and_ci_never_installs() {
    let entry = include_str!("../tools/macos-cpu-worker-installer/src/main.rs");
    let native = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/native.rs"
    ));
    let bootstrap_adapter = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/native/bootstrap.rs"
    ));
    let transaction_adapter = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/transaction.rs"
    ));
    let bootstrap = production_source(include_str!("macos_installer_bootstrap.rs"));
    let transaction = production_source(include_str!("macos_installer_transaction.rs"));
    let filesystem = production_source(include_str!("macos_installer_fs.rs"));
    let manifest = include_str!("../tools/macos-cpu-worker-installer/Cargo.toml");
    let check = include_str!("../scripts/verify-macos-cpu-installer-source.sh");
    let workflow = include_str!("../.github/workflows/macos-release.yml");
    for required in ["mod macos_worker_fs;", "mod macos_worker_layout;"] {
        assert!(entry.contains(required));
    }
    for forbidden in [
        "gpu_worker_pack",
        "onnx_worker",
        "transcribe-cpp",
        "sherpa",
        "std::process::Command",
        "std::env::var(",
        "std::env::var_os(",
        "remove_dir_all",
        "libc::chown",
    ] {
        assert!(
            !native.contains(forbidden),
            "installer native boundary gained {forbidden}"
        );
        assert!(
            !manifest.contains(forbidden),
            "isolated installer gained {forbidden}"
        );
        assert!(
            !bootstrap.contains(forbidden),
            "ancestor bootstrap gained {forbidden}"
        );
        assert!(!filesystem.contains(forbidden));
        assert!(!transaction.contains(forbidden));
    }
    for required in [
        "embedded_cpu_digest()?",
        "libc::geteuid()",
        "libc::LOCK_EX | libc::LOCK_NB",
        "libc::RENAME_EXCL",
        "drop(output)",
        "verify_bytes(&mut image",
        "old stage name no longer identifies",
    ] {
        assert!(native.contains(required), "CPU writer lost {required}");
    }
    assert!(
        transaction
            .find("publication = Publication::PossiblyPublished;")
            .unwrap()
            < transaction.find("match ops.publish()?").unwrap()
    );
    assert!(check.contains("--locked --offline"));
    assert!(check.contains("--all-targets -- -D warnings"));
    assert!(check.contains("Darwin) expected=42"));
    assert!(check.contains("[[ \"$count\" == \"$expected\" ]]"));
    assert!(workflow.contains("bash scripts/verify-macos-cpu-installer-source.sh"));
    let root_walk = named_function_bodies(&native, "walk_protected_root").join("\n");
    assert!(root_walk.contains("bootstrap::ensure_fixed_ancestor("));
    assert!(!root_walk.contains("libc::mkdirat"));
    assert!(!root_walk.contains("set_mode("));
    for required in [
        "impl Operations for Bootstrap",
        "run_transaction(&mut operation, component)",
        "matches!(name, \"Scribe\" | \"CPU\")",
        "self.check_empty_stage(0o700)?",
        "self.check_empty_stage(0o755)?",
        "libc::RENAME_EXCL",
        "original ancestor stage name changed",
    ] {
        assert!(
            bootstrap.contains(required),
            "ancestor transaction lost {required}"
        );
    }
    let bootstrap_confirm = named_function_bodies(&bootstrap, "confirm").join("\n");
    assert!(bootstrap_confirm.contains("checked_directory("));
    assert!(!bootstrap_confirm.contains("check_empty_stage("));
    assert!(bootstrap_adapter.contains("ensure_cpu_ancestor as ensure_fixed_ancestor"));
    assert!(transaction_adapter.contains("run_transaction(ops, \"CPU\")"));
    assert!(transaction_adapter.contains("validate_cpu_size(length)?"));
    assert!(transaction_adapter.contains("validate_cpu_digest(expected)?"));
    assert!(transaction_adapter.contains("MAX_CPU_WORKER_BYTES"));
    for source in [check, workflow] {
        for forbidden in ["sudo ", "installer -pkg", "--source-worker"] {
            assert!(
                !source.contains(forbidden),
                "source verification must not install: {forbidden}"
            );
        }
    }
    for source in [
        include_str!("main.rs"),
        include_str!("bin/scribe-inference-worker.rs"),
    ] {
        assert!(!source.contains("macos-cpu-worker-installer"));
        for module in [
            "installer_bootstrap",
            "installer_fs",
            "installer_transaction",
        ] {
            assert!(!source.contains(&format!("mod {module};")));
        }
    }
}

#[test]
fn macos_cpu_package_entry_is_compile_time_embedded_and_never_run_by_ci() {
    let main = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/main.rs"
    ));
    let entry = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/package_entry.rs"
    ));
    let native = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/native.rs"
    ));
    let build = production_source(include_str!("../tools/macos-cpu-worker-installer/build.rs"));
    let payload = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/package_payload.rs"
    ));
    let snapshot = production_source(include_str!(
        "../tools/macos-cpu-worker-installer/src/package_snapshot.rs"
    ));
    let check = include_str!("../scripts/verify-macos-cpu-installer-source.sh");
    assert!(main.contains("#[cfg(not(feature = \"installer-package-entry\"))]\nfn run()"));
    assert!(main.contains("let result = package_entry::run();"));
    let run = named_function_bodies(&entry, "run").join("\n");
    assert!(
        run.find("validate_invocation(").unwrap() < run.find("production_embedded()?").unwrap()
    );
    assert!(run.contains("libc::geteuid()"));
    let context = named_function_bodies(&entry, "validate_invocation").join("\n");
    assert!(context.find("effective_uid != 0").unwrap() < context.find("arguments.len()").unwrap());
    assert!(context.contains("arguments[1] != \"/\""));
    assert!(context.contains("arguments[2] != \"/\""));
    assert!(context.contains("arguments.get(3).is_some_and(|root| root != \"/\")"));
    assert!(entry.contains("include_bytes!(concat!(env!(\"OUT_DIR\"), \"/package-worker.bin\"))"));
    assert!(entry.contains("crate::package_payload::verify(IMAGE, digest)?"));
    let constructor = named_function_bodies(&native, "production_embedded").join("\n");
    assert!(
        constructor.find("libc::geteuid()").unwrap()
            < constructor.find("compiled_worker()?").unwrap()
    );
    assert!(
        constructor.find("compiled_worker()?").unwrap() < constructor.find("Self::new(").unwrap()
    );
    assert!(native.contains("Self::File(file) => candidate_size(file)"));
    assert!(payload.contains("validate_cpu_size(u64::try_from(bytes.len())?)?"));
    assert!(payload.contains("Sha256::digest(bytes)"));
    assert!(build.contains("release package entry requires SCRIBE_MACOS_CPU_PACKAGE_WORKER"));
    let emit = named_function_bodies(&build, "emit_package_worker").join("\n");
    assert!(
        emit.find("validate_source_path(&source)").unwrap()
            < emit.find("cargo:rerun-if-changed=").unwrap()
    );
    assert!(emit.find("package_snapshot::snapshot(").unwrap() < emit.find("fs::rename(").unwrap());
    for required in [
        "create_new(true)",
        "libc::O_NOFOLLOW | libc::O_NONBLOCK",
        "copied package image is unadmitted",
    ] {
        assert!(snapshot.contains(required));
    }
    for forbidden in [
        "std::fs",
        "std::process::Command",
        "current_exe",
        "std::env::var",
        "--source-worker",
        "--destination",
        "Keychain",
    ] {
        assert!(
            !entry.contains(forbidden),
            "package entry gained {forbidden}"
        );
    }
    for required in [
        "installer-package-entry",
        "package_expected=57",
        "native_expected=3",
        "expect_package_rejection",
        "cargo check --release",
    ] {
        assert!(check.contains(required));
    }
    for forbidden in ["cargo run", "sudo ", "installer -pkg"] {
        assert!(!check.contains(forbidden));
    }
}

#[test]
fn macos_cpu_entry_construction_is_frozen_source_only_and_not_installation_authority() {
    let source = include_str!("../build_support/macos_cpu_pkg_entry.py");
    let shared = include_str!("../build_support/macos_cpu_component.py");
    let tests = include_str!("../build_support/test_macos_cpu_pkg_entry.py");
    let workflow = include_str!("../.github/workflows/macos-release.yml");
    let build = &source[source.find("def build_entry(").unwrap()..];
    assert!(build.find("os.geteuid() != 0").unwrap() < build.find("app.is_dir()").unwrap());
    for required in [
        "component.snapshot_cpu_app_inputs(app, worker, desktop, private, digest, revision)",
        "environment[\"SCRIBE_MACOS_CPU_PACKAGE_WORKER\"] = str(worker)",
        "for target in component.TARGETS:",
        "cwd=component.REPO",
        "\"--no-default-features\"",
        "\"--features\", \"installer-package-entry\"",
        "component.hash_file(binary, MAX_SLICE)",
        "component.inspect_universal_structure(candidate,",
        "component.publish_no_replace(candidate, output)",
    ] {
        assert!(
            build.contains(required),
            "entry construction lost {required}"
        );
    }
    assert!(source.contains("MAX_SLICE = component.MAX_WORKER + SLICE_CODE_AND_ALIGNMENT"));
    assert!(source.contains("MAX_ENTRY = 2 * MAX_SLICE"));
    assert!(shared.contains("MAX_WRITER = 64 * 1024 * 1024"));
    let signed = &shared[shared.find("def inspect_universal(").unwrap()
        ..shared.find("def inspect_universal_structure(").unwrap()];
    assert!(
        signed.find("\"--verify\", \"--strict\"").unwrap()
            < signed.find("_inspect_universal_slices(").unwrap()
    );
    let structure = &shared[shared.find("def inspect_universal_structure(").unwrap()
        ..shared.find("def _inspect_universal_slices(").unwrap()];
    assert!(!structure.contains("codesign"));
    let bound = &shared[shared.find("def snapshot_cpu_app_inputs(").unwrap()
        ..shared.find("def assemble(").unwrap()];
    for required in [
        "before = cpu_app_input_snapshot(app)",
        "version = verify_cpu_app(app)",
        "copied_worker == before[CPU_APP_INPUTS[0]][:2]",
        "copied_desktop == before[CPU_APP_INPUTS[1]][:2]",
        "str(app / CPU_APP_INPUTS[1])",
        "desktop-bundle-signature-verification",
        "inspect_universal(worker,",
        "signature_operation=\"copied-worker-signature-verification\"",
        "inspect_universal_structure(desktop,",
        "hash_file(desktop, MAX_WORKER) == copied_desktop",
    ] {
        assert!(
            bound.contains(required),
            "bound input admission lost {required}"
        );
    }
    assert_eq!(
        bound
            .matches("cpu_app_input_snapshot(app) == before")
            .count(),
        2
    );
    assert!(
        bound.find("version = verify_cpu_app(app)").unwrap()
            < bound.find("inspect_universal_structure(desktop,").unwrap()
    );
    assert!(!bound.contains("--sign"));
    assert!(!bound.contains("inspect_universal(desktop,"));
    let native = include_str!("../build_support/native_macos_cpu_pkg_entry.py");
    assert!(native.contains("INPUTS = component.CPU_APP_INPUTS"));
    assert!(native.contains("return component.cpu_app_input_snapshot(app)"));
    for forbidden in [
        "__main__",
        "argparse",
        "pkgbuild",
        "--sign",
        "--expand",
        "subprocess.Popen",
        "sudo ",
        "/usr/sbin/installer",
        "\"describe\"",
        "\"install\"",
    ] {
        assert!(
            !source.contains(forbidden),
            "entry construction gained {forbidden}"
        );
    }
    assert!(tests.contains("expected = 17 + (2 if os.name == \"posix\" else 0)"));
    assert!(workflow.contains("python3 -B build_support/test_macos_cpu_pkg_entry.py"));
    assert_eq!(
        workflow
            .matches("build_support/*macos_cpu_pkg_entry.py")
            .count(),
        2
    );
}

#[test]
fn macos_native_cpu_entry_contract_constructs_without_executing_or_uploading() {
    let source =
        include_str!("../build_support/native_macos_cpu_pkg_entry.py").replace("\r\n", "\n");
    let tests = include_str!("../build_support/test_native_macos_cpu_pkg_entry.py");
    let workflow = include_str!("../.github/workflows/macos-release.yml");
    let run =
        &source[source.find("def run_contract(").unwrap()..source.find("def main()").unwrap()];
    assert!(run.find("os.geteuid() != 0").unwrap() < run.find("app.is_dir()").unwrap());
    assert_eq!(run.matches("entry.build_entry(").count(), 2);
    assert!(run.contains("component.hash_file(output, entry.MAX_ENTRY)"));
    assert!(run.contains("component.fingerprint(output.lstat()) == identity"));
    assert!(run.contains("input_snapshot(app) == before"));
    assert!(run.contains("except component.CleanupUncertain:\n            raise"));
    assert_eq!(run.matches("component.capture(").count(), 1);
    assert!(run.contains("[\"/usr/bin/lipo\", str(output), \"-archs\"]"));
    for forbidden in [
        "subprocess",
        "os.system",
        "--sign",
        "pkgbuild",
        "/usr/sbin/installer",
        "--destination",
        "--expand",
        "upload",
        "Keychain",
    ] {
        // The module docstring states that no upload occurs; inspect code only.
        assert!(
            !run.contains(forbidden),
            "native contract gained {forbidden}"
        );
    }
    assert!(tests.contains("expected = 13 + (1 if os.name == \"posix\" else 0)"));
    assert!(workflow.contains("python3 -B build_support/test_native_macos_cpu_pkg_entry.py"));
    let native_step = workflow
        .find("- name: Construct real universal CPU entry")
        .unwrap();
    assert!(
        workflow
            .find("- name: Build ad hoc signed universal structural package")
            .unwrap()
            < native_step
    );
    let step = &workflow[native_step
        ..workflow[native_step..]
            .find("- name: Run protected CPU")
            .unwrap()
            + native_step];
    assert!(step.contains("timeout-minutes: 50"));
    assert!(step.contains("shasum -a 256"));
    assert!(step.contains("python3 -B build_support/native_macos_cpu_pkg_entry.py --app"));
    assert!(step.contains(
        "--worker-sha256 \"$worker_sha256\" --build-revision \"$SCRIBE_BUILD_REVISION\""
    ));
    assert!(!step.contains("upload-artifact"));
}

#[test]
fn macos_authoring_tool_failures_report_closed_phase_without_raw_diagnostics() {
    let source = include_str!("../build_support/macos_cpu_component.py");
    let entry = include_str!("../build_support/macos_cpu_pkg_entry.py");
    let tests = include_str!("../build_support/test_macos_cpu_component.py");
    let capture =
        &source[source.find("def capture(").unwrap()..source.find("def has_markers(").unwrap()];
    assert!(
        capture.find("validate_tool_operation(operation)").unwrap()
            < capture.find("subprocess.Popen(").unwrap()
    );
    assert!(
        capture.find("process.wait(").unwrap()
            < capture
                .find("raise ToolExitError(operation, status)")
                .unwrap()
    );
    assert!(
        capture.contains("raise CleanupUncertain(\"process group remains after leader exit\")")
    );
    let error = &source[source.find("class ToolExitError(").unwrap()
        ..source.find("def validate_identity(").unwrap()];
    assert!(error.contains("validate_tool_operation(operation)"));
    assert!(error.contains("type(status) is int"));
    assert!(error.contains("self.operation, self.status = operation, status"));
    for forbidden in [
        "self.command",
        "self.arguments",
        "self.stdout",
        "self.stderr",
        "self.output",
        "CalledProcessError",
    ] {
        assert!(
            !error.contains(forbidden),
            "authoring tool error gained {forbidden}"
        );
    }
    for phase in [
        "application-verification",
        "artifact-architectures",
        "artifact-signature-verification",
        "artifact-slice",
        "artifact-load-commands",
        "artifact-dependencies",
        "writer-build-arm64",
        "writer-build-x86_64",
        "writer-combine",
        "development-writer-signing",
        "writer-description",
    ] {
        assert!(
            source.matches(phase).count() >= 2,
            "unlabelled component operation {phase}"
        );
    }
    for phase in ["entry-build-arm64", "entry-build-x86_64", "entry-combine"] {
        assert!(entry.contains(phase));
    }
    assert!(tests.contains("expected = 33 + (18 if os.name == \"posix\" else 0)"));
    for required in [
        "test_actual_bundle_context_preserves_signed_input_binding_without_execution",
        "test_every_source_context_change_or_same_bytes_replacement_rejects",
        "test_copied_byte_mismatch_and_changes_during_inspection_reject",
        "test_app_or_worker_signature_failure_cannot_reach_desktop_inspection",
        "test_both_callers_stop_before_build_or_publication_after_admission_rejection",
    ] {
        assert!(tests.contains(required));
    }
    assert!(tests.contains(
        "test_tool_exit_diagnostic_redacts_real_child_arguments_environment_and_streams"
    ));
}

#[test]
fn macos_package_profile_only_observes_a_fixed_unprivileged_fixture() {
    let source = include_str!("../build_support/macos_cpu_pkg_profile.py");
    let tests = include_str!("../build_support/test_macos_cpu_pkg_profile.py");
    let workflow = include_str!("../.github/workflows/macos-release.yml").replace("\r\n", "\n");
    let probe = &source[source.find("def probe()").unwrap()..source.find("def main()").unwrap()];
    assert!(probe.find("os.geteuid() != 0").unwrap() < probe.find("private_workspace(").unwrap());
    assert!(probe.contains("fixture.open(\"xb\")"));
    assert_eq!(probe.matches("component.capture(").count(), 2);
    assert!(probe.contains("[\"/usr/bin/pkgbuild\", \"--nopayload\", \"--scripts\""));
    assert!(probe.contains("[\"/usr/bin/xar\", \"-tf\", str(package)]"));
    assert!(source.contains("len(sys.argv) == 1"));
    assert!(source.contains("decoder.decompress(data, maximum + 1)"));
    assert!(source.contains("not decoder.unused_data"));
    assert!(source.contains("\"observation_only\": True"));
    for forbidden in [
        "/usr/sbin/installer",
        "/usr/bin/codesign",
        "subprocess.Popen",
        "os.system(",
        "--expand",
        "extractall",
        "--sign",
    ] {
        assert!(
            !source.contains(forbidden),
            "format probe gained {forbidden}"
        );
    }
    assert!(tests.contains("expected = 28 + (1 if sys.platform == \"darwin\" else 0)"));
    let decoder = &source
        [source.find("def _decode_odc(").unwrap()..source.find("def observe_generated(").unwrap()];
    for required in [
        "ODC_HEADER_BYTES = 76",
        "ODC_MAX_RECORDS = 16",
        "ODC_MAX_NAME_BYTES = 256",
        "\"schema_version\": 2",
        "scripts_inventory = _observe_scripts_fixture(scripts)",
        "\"scripts_inventory\": scripts_inventory",
    ] {
        assert!(
            source.contains(required),
            "fixed ODC observation lost {required}"
        );
    }
    for required in [
        "48 <= byte <= 55",
        "end <= len(data)",
        "len(records) < ODC_MAX_RECORDS",
        "not any(data[end:])",
        "record[\"data\"] == FIXTURE",
        "identity not in seen",
    ] {
        assert!(decoder.contains(required));
    }
    for forbidden in ["component.capture(", "Path(", "open(", "os."] {
        assert!(!decoder.contains(forbidden));
    }
    for forbidden in [
        "prefix_hex",
        "scripts.count(FIXTURE)",
        "fixture_occurrences",
    ] {
        assert!(!source.contains(forbidden));
    }
    for required in [
        "test_odc_literal_header_and_unaligned_records",
        "test_odc_every_numeric_field_rejects_non_octal",
        "test_odc_all_truncations_name_terminators_and_entry_padding_reject",
        "test_fixed_inventory_rejects_aliases_extra_records_types_and_partial_payloads",
        "test_link_counts_and_permissions_are_observations_not_admission",
        "test_full_report_omits_payload_prefixes_and_private_numeric_metadata",
    ] {
        assert!(tests.contains(required));
    }
    assert!(source.contains("\"mode\": observed_mode(file)"));
    assert!(tests.contains("test_absent_mode_is_reported_as_unknown_without_inferred_permissions"));
    assert!(tests.contains("test_present_mode_keeps_strict_scalar_validation"));
    assert!(tests.contains("test_actual_unsigned_nopayload_format_without_installation"));
    let profile_job = workflow
        .split("\n  package-profile:\n")
        .nth(1)
        .unwrap()
        .split("\n  structural:\n")
        .next()
        .unwrap();
    let structural_job = workflow
        .split("\n  structural:\n")
        .nth(1)
        .unwrap()
        .split("\n  official-sign-notarize:\n")
        .next()
        .unwrap();
    let official_job = workflow
        .split("\n  official-sign-notarize:\n")
        .nth(1)
        .unwrap();
    let profile_command = "run: python3 -B build_support/test_macos_cpu_pkg_profile.py";
    assert_eq!(workflow.matches(profile_command).count(), 1);
    assert!(profile_job.contains(profile_command));
    assert_eq!(profile_job.matches("- uses:").count(), 2);
    assert_eq!(profile_job.matches("run:").count(), 1);
    for required in [
        "timeout-minutes: 10",
        "timeout-minutes: 2",
        "fail-fast: false",
        "runner: macos-15-intel\n            arch: x86_64",
        "runner: macos-14\n            arch: aarch64",
        "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09",
        "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1",
        "python-version: '3.13'",
    ] {
        assert!(
            profile_job.contains(required),
            "profile preflight lost {required}"
        );
    }
    for forbidden in ["secrets.", "continue-on-error:", "always()"] {
        assert!(!profile_job.contains(forbidden));
        assert!(!structural_job.contains(forbidden));
    }
    for forbidden in [
        "cargo ",
        "codesign",
        "notarytool",
        "sudo ",
        "upload-artifact",
        "environment:",
        "permissions:",
    ] {
        assert!(
            !profile_job.contains(forbidden),
            "profile preflight gained {forbidden}"
        );
    }
    assert!(structural_job.contains("\n    needs: package-profile\n"));
    assert!(official_job.contains("\n    needs: structural\n"));
    let packaging_check = include_str!("../scripts/test-macos-release-packaging.sh");
    assert!(
        packaging_check.contains("sed -n '/^  package-profile:/,/^  official-sign-notarize:/p'")
    );
    assert!(packaging_check.contains("<<<\"$unprivileged_jobs\""));
    assert_eq!(
        workflow
            .matches("build_support/*macos_cpu_pkg_profile.py")
            .count(),
        2
    );
}

#[test]
fn macos_metal_installer_copies_only_admitted_content_and_ci_never_installs() {
    let entry = production_source(include_str!(
        "../tools/macos-metal-pack-installer/src/main.rs"
    ));
    let native = production_source(include_str!(
        "../tools/macos-metal-pack-installer/src/native.rs"
    ));
    let admission = production_source(include_str!(
        "../tools/macos-metal-pack-installer/src/admission.rs"
    ));
    let check = include_str!("../scripts/verify-macos-metal-installer-source.sh");
    let workflow = include_str!("../.github/workflows/macos-release.yml");
    let constructor = named_function_bodies(&native, "production").join("\n");
    assert!(
        constructor.find("require_installer_root(").unwrap()
            < constructor.find("admit_source(candidate)?").unwrap()
    );
    let prepare = named_function_bodies(&native, "prepare").join("\n");
    assert!(
        prepare.find("self.source.reverify()?").unwrap()
            < prepare.find("self.prepare_store()?").unwrap()
    );
    assert!(
        prepare.find("libc::flock(").unwrap()
            < prepare
                .find("self.source.destination_components()")
                .unwrap()
    );
    let root = named_function_bodies(&native, "prepare_store").join("\n");
    for name in ["Library", "Application Support", "Scribe", "WorkerPacks"] {
        assert!(root.contains(&format!("\"{name}\"")));
    }
    for required in [
        "libc::LOCK_EX | libc::LOCK_NB",
        "libc::RENAME_EXCL",
        "self.source.open_for_copy(slot)?",
        "slot.maximum()",
        "drop(output)",
        "self.source.verify_materialized(pinned)?",
        "self.inspect_nodes(root)?",
        "self.published = true;",
        "self.prove_cleanup_tree(stage)?",
    ] {
        assert!(native.contains(required), "Metal writer lost {required}");
    }
    assert!(admission.contains("copy.copy_entries() != self.lease.copy_entries()"));
    let cleanup = named_function_bodies(&native, "cleanup").join("\n");
    assert!(
        cleanup.find("self.prove_cleanup_tree(stage)?").unwrap() < cleanup.find("unlink(").unwrap()
    );
    for forbidden in [
        "fixture_uid",
        "fixture_root",
        "copy_hook",
        "std::process::Command",
        "std::env::var(",
        "std::env::var_os(",
        "remove_dir_all",
        "libc::chown",
        "PackStore",
        "activate(",
        "Keychain",
        "SecItem",
    ] {
        assert!(
            !native.contains(forbidden),
            "Metal writer gained {forbidden}"
        );
    }
    for forbidden in [
        "--destination",
        "--trust-root",
        "--release-authority",
        "--epoch",
    ] {
        assert!(!entry.contains(forbidden));
    }
    assert!(check.contains("Darwin) expected=90; admission_expected=19"));
    assert!(check.contains("native_expected=16"));
    assert!(check.contains("--locked --offline"));
    assert!(check.contains("--all-targets -- -D warnings"));
    assert!(workflow.contains("bash scripts/verify-macos-metal-installer-source.sh"));
    for source in [check, workflow] {
        for forbidden in ["sudo ", "installer -pkg", "--source-pack"] {
            assert!(
                !source.contains(forbidden),
                "CI must not install: {forbidden}"
            );
        }
    }
    for source in [
        include_str!("main.rs"),
        include_str!("bin/scribe-inference-worker.rs"),
    ] {
        assert!(!source.contains("macos-metal-pack-installer"));
    }
}

#[test]
fn macos_device_release_epoch_authority_is_bounded_append_only_and_pre_launch() {
    let pack = production_source(include_str!("gpu_worker_pack/mod.rs"));
    let release_policy = production_source(include_str!("gpu_worker_pack/release_authority.rs"));
    let authority = production_source(include_str!("gpu_worker_pack/device_release_epoch.rs"));
    let native = include_str!("../native/scribe_macos_keychain_epoch.c");
    let header = include_str!("../native/scribe_macos_keychain_epoch.h");
    let build = include_str!("../build.rs");
    let routes = production_source(include_str!("onnx_worker.rs"));
    let reviewed_namespace =
        include_str!("../runtime-manifests/gpu-keychain-namespace-macos-release.json");

    for required in [
        "release_security_epoch",
        "keychain_access_group",
        "DeviceRollbackAuthorityRejected",
        "SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP",
        "SCRIBE_REVIEWED_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP",
        "device_release_epoch::admit",
    ] {
        assert!(
            pack.contains(required),
            "release authority lost {required:?}"
        );
    }
    let release_wrapper = named_function_bodies(&pack, "validated_release_authority").join("\n");
    for required in [
        "release_authority::validated_release_authority(",
        "app_version: env!(\"CARGO_PKG_VERSION\")",
        "build_revision: env!(\"SCRIBE_BUILD_REVISION\")",
        "app_protocol_version: manifest::APP_PROTOCOL_VERSION",
    ] {
        assert!(
            release_wrapper.contains(required),
            "runtime release delegation lost {required:?}"
        );
    }
    let document_validation =
        named_function_bodies(&release_policy, "validated_release_authority_document").join("\n");
    for required in [
        "authority.schema_version != 2",
        "authority_bytes.len() > MAX_RELEASE_AUTHORITY_BYTES",
        "authority.entries.len() > MAX_PRODUCTION_PACKS",
        "serde_json::to_vec(&authority).ok().as_deref() != Some(authority_bytes)",
        "entry.security_epoch == authority.release_security_epoch",
        "canonical_keychain_access_group(&authority.keychain_access_group)",
    ] {
        assert!(
            document_validation.contains(required),
            "shared release policy lost {required:?}"
        );
    }
    let authority_validation = pack
        .find("validated_release_authority(&catalog.bytes")
        .expect("exact release authority validation must exist");
    let cache_lookup = pack
        .find("PRODUCTION_DISCOVERY_CACHE.get_or_init")
        .expect("bounded discovery cache must exist");
    let cached_admission = pack
        .find("enforce_production_discovery_epochs(discovery, &release_authority)")
        .expect("cached device admission must exist");
    let fresh_verification = pack
        .find("verify_catalog_entries(install_root")
        .expect("fresh pack verification must exist");
    let fresh_admission = pack
        .rfind("enforce_production_discovery_epochs(discovery, &release_authority)")
        .expect("fresh device admission must exist");
    assert!(
        authority_validation < cache_lookup
            && cache_lookup < cached_admission
            && fresh_verification < fresh_admission,
        "device authority must follow exact signed authority/pack verification on cached and fresh paths"
    );

    for required in [
        "scan_markers(store)",
        "store.append(&candidate_marker)",
        "marker_floor(&scan_markers(store)?)",
        "CapacityExceeded",
        "MAX_MARKERS",
        "OnceLock<Client>",
        "sync_channel(1)",
        "try_send",
        "recv_timeout",
    ] {
        assert!(
            authority.contains(required),
            "bounded authority lost {required:?}"
        );
    }
    for required in [
        "kSecUseDataProtectionKeychain",
        "kSecAttrSynchronizable",
        "kCFBooleanFalse",
        "kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly",
        "kSecAttrAccessGroup",
        "kSecAttrService",
        "kSecMatchLimitAll",
        "kSecReturnAttributes",
        "kSecReturnData",
        "errSecItemNotFound",
        "errSecDuplicateItem",
        "SecItemCopyMatching",
        "SecItemAdd",
    ] {
        assert!(native.contains(required), "Keychain shim lost {required:?}");
    }
    for forbidden in ["SecItemUpdate", "SecItemDelete"] {
        assert!(
            !native.contains(forbidden),
            "Keychain shim regained {forbidden}"
        );
        assert!(
            !header.contains(forbidden),
            "Keychain API regained {forbidden}"
        );
    }
    for required in [
        "native/scribe_macos_keychain_epoch.c",
        "framework=Security",
        "SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP",
        "SCRIBE_REVIEWED_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP",
        "gpu-keychain-namespace-macos-release.json",
        "scribe_macos_keychain_authority",
    ] {
        assert!(
            build.contains(required),
            "macOS build contract lost {required:?}"
        );
    }
    assert_eq!(
        reviewed_namespace.trim_end(),
        r#"{"schema_version":1,"keychain_access_group":""}"#,
        "production Keychain namespace must remain explicitly unprovisioned until reviewed"
    );
    let final_recheck = routes
        .find("revalidate_production_device_epoch")
        .expect("request-bound device release revalidation must exist");
    let route_activation = routes[final_recheck..]
        .find("let identity = Self::route_identity(route)")
        .map(|offset| final_recheck + offset)
        .expect("route activation must follow request-bound revalidation");
    assert!(
        final_recheck < route_activation,
        "device release revalidation must precede active GPU route publication"
    );
}

#[test]
fn runtime_artifact_module_is_a_leaf_value_boundary() {
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("runtime_artifact.rs"),
    )
    .expect("runtime artifact source must be readable");
    for forbidden in ["crate::onnx_worker", "crate::runtime_router"] {
        assert!(
            !source.contains(forbidden),
            "artifact leaf imported execution module {forbidden:?}"
        );
    }
}

#[test]
fn static_gguf_and_native_onnx_are_the_only_inference_architectures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let artifacts = fs::read_to_string(root.join("src/runtime_artifact.rs"))
        .expect("runtime artifact source must be readable");
    assert!(artifacts.contains("enum RuntimeArtifact"));
    assert!(artifacts.contains("Gguf(RuntimeModel)"));
    assert!(artifacts.contains("OnnxBundle(OnnxModelSpec)"));

    let catalog = fs::read_to_string(root.join("src/model_catalog.rs"))
        .expect("model catalog source must be readable");
    assert!(catalog.contains("enum ArtifactFormat"));
    assert!(catalog.contains("Gguf,"));

    let worker = fs::read_to_string(root.join("src/onnx_worker.rs"))
        .expect("inference worker source must be readable");
    assert!(worker.contains("enum WireRuntimeArtifact"));
    assert!(worker.contains("Gguf(WireRuntimeModel)"));
    assert!(worker.contains("OnnxBundle(OnnxModelSpec)"));
    assert!(worker.contains("ASR recognizers are unavailable in the desktop executable"));
    assert!(worker.contains("resolve_adjacent_inference_worker"));
    assert!(worker.contains("scribe-inference-worker{}"));
    assert!(worker.contains("INFERENCE_WORKER_FLAG"));

    let inference_server = fs::read_to_string(root.join("src/inference_server.rs"))
        .expect("worker-only inference server source must be readable");
    assert!(inference_server.contains("OfflineRecognizer::create("));
    assert!(inference_server.contains("OnlineRecognizer::create("));
    assert!(inference_server.contains("use sherpa_onnx"));
    assert!(!worker.contains("OfflineRecognizer::create("));
    assert!(!worker.contains("OnlineRecognizer::create("));

    let router = fs::read_to_string(root.join("src/runtime_router.rs"))
        .expect("runtime router source must be readable");
    assert!(router.contains("enum RuntimeKind"));
    assert!(router.contains("TranscribeCpp,"));
    assert!(router.contains("EmbeddedRuntime::new("));
    assert!(!router.contains("Command::new"));

    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("Cargo manifest must be readable");
    assert!(manifest.contains("inference-worker = [\"dep:transcribe-cpp\"]"));
    assert!(manifest.contains(
        "transcribe-cpp = { version = \"=0.1.3\", default-features = false, optional = true }"
    ));
    assert!(manifest.contains("name = \"scribe-inference-worker\""));
    assert!(manifest.contains("required-features = [\"inference-worker\"]"));
}

#[test]
fn dynamic_whisper_and_standalone_runtime_paths_stay_removed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let obsolete_paths = [
        "src/compatibility_bridge.rs",
        "src/runtime_catalog.rs",
        "src/stt/whisper_cpp.rs",
        "native/whisper_shim.c",
    ];
    for relative in obsolete_paths {
        assert!(
            !root.join(relative).exists(),
            "retired runtime path was restored: {relative}"
        );
    }

    let forbidden = [
        "LegacyGgml",
        "LegacyCompatibility",
        "LegacyBatchAdapter",
        "transcribe_legacy",
        "whisper_cli",
        "whisper-cli",
        "whisper.dll",
        "ggml.dll",
        "SCRIBE_WHISPER_",
        "SCRIBE_RUNTIME_DEST",
        "RepairModelRuntime",
        "MaintainModelRuntime",
        "libloading::",
        "scribe_whisper_",
    ];
    for (path, source) in rust_sources() {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source_for(&path, &source);
        for retired in forbidden {
            assert!(
                !production.contains(retired),
                "retired runtime token {retired:?} remains in production source {}",
                path.display()
            );
        }
    }

    let build = fs::read_to_string(root.join("build.rs")).expect("build script must be readable");
    for retired in [
        "whisper_shim",
        "scribe_whisper",
        "LoadLibrary",
        "GetProcAddress",
    ] {
        assert!(
            !build.contains(retired),
            "dynamic Whisper build token {retired:?} was restored"
        );
    }

    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("Cargo manifest must be readable");
    assert!(
        !manifest
            .lines()
            .any(|line| line.trim_start().starts_with("libloading =")),
        "libloading must not be a direct production dependency"
    );

    let tray = fs::read_to_string(root.join("src/tray.rs")).expect("tray source must be readable");
    for required in [
        "CString::new(name)",
        "libc::dlopen",
        "libc::RTLD_LAZY | libc::RTLD_LOCAL",
        "libc::dlclose(handle)",
    ] {
        assert!(
            tray.contains(required),
            "Linux tray availability probe must retain {required:?}"
        );
    }
}

const WORKER_RUNTIME_MARKER: &str = "worker-only native runtime";

#[test]
fn windows_gpu_capture_stays_opt_in_and_out_of_release_builds() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(repository.join("Cargo.toml")).unwrap();
    assert!(
        manifest
            .lines()
            .any(|line| line == "windows-gpu-capture-observation = []"),
        "capture feature must not enable a native inference provider"
    );
    let main = production_source(include_str!("main.rs"));
    assert!(main.contains(
        "#[cfg(all(windows, feature = \"windows-gpu-capture-observation\"))]\nmod windows_gpu_capture;"
    ));
    let observer = main
        .find("windows_gpu_capture::maybe_run_local_command()")
        .expect("collector command remains an explicit optional entry point");
    let startup = main
        .find("support_assets::materialize_bundled_support_assets()")
        .expect("normal startup remains separate");
    assert!(observer < startup, "observer must not initialize the UI");

    let builder = fs::read_to_string(repository.join("scripts/build-windows-release.ps1")).unwrap();
    // Local frozen test installers can opt in; ordinary release builds cannot.
    // The packaging suite also executes both paths and rejects unsafe feature mutations.
    let build_commands = builder
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for required in [
        "[switch]$LocalFrozenGpuObservation\n)",
        "if ($frozenCpuWorkerRequested) {\nAssert-WindowsFrozenCpuWorkerLocalOnlyEnvironment\n$resolvedFrozenWorkerSourceRoot = if ($frozenCpuWorkerSourceRootWasExplicit) {",
        "if ($LocalFrozenGpuObservation -and -not $frozenCpuWorkerRequested) {\nthrow 'LocalFrozenGpuObservation is available only with a local frozen CPU worker record.'\n}",
        "$desktopFeatures = @('ui-harness')\nif ($LocalFrozenGpuObservation) {\n$desktopFeatures += 'windows-gpu-capture-observation'\n}\n$gpuAutoPolicyDesktopBuildId = Get-WindowsGpuAutoPolicyDesktopBuildId `\n-RepositoryRoot $repositoryRoot -BuildRevisionOverride $env:SCRIBE_BUILD_REVISION\n& cargo build --locked --offline --release --bin local-transcriber --features ($desktopFeatures -join ',') --target $targetTriple --manifest-path (Join-Path $repositoryRoot \"Cargo.toml\")",
    ] {
        assert!(
            build_commands.contains(required),
            "local observer opt-in must preserve release boundary {required:?}"
        );
    }
    assert_eq!(
        builder.matches("windows-gpu-capture-observation").count(),
        1
    );
    assert_eq!(builder.matches("$desktopFeatures").count(), 3);
    let workflow = fs::read_to_string(repository.join(".github/workflows/release.yml")).unwrap();
    assert!(
        !workflow.contains("LocalFrozenGpuObservation")
            && !workflow.contains("FrozenCpuWorkerRecordPath"),
        "ordinary release CI must not opt into local frozen observer builds"
    );
    assert!(workflow.contains(
        "run: pwsh -NoProfile -File .\\scripts\\test-windows-gpu-capture-observation.ps1"
    ));
}

#[test]
fn windows_gpu_probe_stays_early_opt_in_and_probe_only() {
    let main = production_source(include_str!("main.rs"));
    assert!(main.contains(
        "#[cfg(all(windows, feature = \"windows-gpu-capture-observation\"))]\nmod windows_gpu_probe;"
    ));
    let probe = main
        .find("windows_gpu_probe::maybe_run_local_command()")
        .expect("GPU pack probe remains an explicit optional entry point");
    for later in [
        "gpu_worker_pack::maybe_run_pack_verifier()",
        "onnx_worker::maybe_run_vad_worker()",
        "transcription::maybe_run_installation_smoke_helper()",
        "windows_gpu_capture::maybe_run_local_command()",
        "benchmark::maybe_run_local_command()",
        "support_assets::materialize_bundled_support_assets()",
    ] {
        assert!(
            probe
                < main
                    .find(later)
                    .expect("expected later private/startup mode"),
            "GPU pack probe must reject combined modes before {later} can run"
        );
    }

    let command = production_source(include_str!("windows_gpu_probe.rs"));
    for required in [
        "--scribe-windows-gpu-pack-probe",
        "--ack-local-security-records",
        "--ack-local-execution-copies",
        "run_production_gpu_pack_probe(allow_execution_copy)",
        "execution_copy_acknowledgement(args)",
        "MAX_REPORT_BYTES",
    ] {
        assert!(
            command.contains(required),
            "GPU pack probe must retain boundary {required:?}"
        );
    }
    for forbidden in [
        "--model",
        "--wav",
        "--output",
        "--gpu-device",
        "--pack-root",
        "--trust",
        "--timeout",
        "RuntimeWorker",
        "TranscriptionService",
        "PreparedAudio",
        "HealthCache",
    ] {
        assert!(
            !command.contains(forbidden),
            "GPU pack probe must not accept or construct {forbidden:?}"
        );
    }

    let worker = production_source(include_str!("onnx_worker.rs"));
    for required in [
        "GPU_PROVIDER_DISCOVERY_BUDGET",
        "GPU_PROVIDER_PROBE_CLEANUP_GRACE",
        "for_diagnostic_pack_probe",
        "stderr(Stdio::null())",
        "wait_before(diagnostic_probe.cleanup_deadline)",
        "driver_identity_sha256",
        "authenticated_devices_truncated",
        "diagnostics_truncated",
    ] {
        assert!(
            worker.contains(required),
            "GPU pack probe must retain bounded authenticated behavior {required:?}"
        );
    }
    let spawned = worker
        .find("let mut child = command.spawn()?")
        .expect("diagnostic worker launch must remain explicit");
    let started = worker[spawned..]
        .find("diagnostic_probe.process_started()")
        .map(|offset| spawned + offset)
        .expect("diagnostic probe must mark a started child");
    let bound = worker
        .find("bind_worker_process_tree_or_terminate(&mut child, bind_worker_process_tree)")
        .expect("worker process-tree binding must remain explicit");
    assert!(
        spawned < started && started < bound,
        "diagnostic cleanup must become unconfirmed immediately after process creation"
    );
}

#[test]
fn windows_gpu_idle_diagnostic_claims_its_acknowledgements_before_probe_dispatch() {
    let main = production_source(include_str!("main.rs"));
    let idle = main
        .find("transcription::maybe_run_windows_gpu_idle_diagnostic()")
        .expect("idle diagnostic remains an explicit collector entry point");
    let probe = main
        .find("windows_gpu_probe::maybe_run_local_command()")
        .expect("GPU pack probe remains an explicit collector entry point");
    let startup = main
        .find("support_assets::materialize_bundled_support_assets()")
        .expect("normal desktop startup remains distinct");
    assert!(
        idle < probe,
        "idle diagnostic must parse its acknowledgement flags before broad probe dispatch"
    );
    assert!(idle < startup, "idle diagnostic must not initialize the UI");

    let idle_source = production_source(include_str!("transcription/gpu_idle_diagnostic.rs"));
    for required in [
        "--scribe-windows-gpu-idle-diagnostic",
        "--ack-local-security-records",
        "--ack-local-execution-copies",
        "parse_command(args)",
    ] {
        assert!(
            idle_source.contains(required),
            "idle diagnostic must retain strict dispatch boundary {required:?}"
        );
    }
}

const NATIVE_RUNTIME_OWNER_PATHS: [&str; 4] = [
    "embedded_runtime.rs",
    "inference_server.rs",
    "onnx_worker.rs",
    "runtime_router.rs",
];

fn is_native_runtime_owner(path: &Path) -> bool {
    NATIVE_RUNTIME_OWNER_PATHS
        .iter()
        .any(|allowed| path == Path::new(allowed))
}

#[test]
fn native_runtime_marker_set_matches_exact_owner_allowlist() {
    let sources = rust_sources();
    let mut documented = sources
        .iter()
        .filter(|(path, source)| {
            path != Path::new("architecture_guard.rs") && source.contains(WORKER_RUNTIME_MARKER)
        })
        .map(|(path, _)| path.as_path())
        .collect::<Vec<_>>();
    documented.sort();
    let mut expected = NATIVE_RUNTIME_OWNER_PATHS
        .iter()
        .map(Path::new)
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(documented, expected);

    let copied_marker = "//! worker-only native runtime\nfn escaped() {}";
    assert!(copied_marker.contains(WORKER_RUNTIME_MARKER));
    assert!(!is_native_runtime_owner(Path::new("copied_marker.rs")));
}

#[test]
fn generic_process_worker_transport_has_neutral_diagnostics_and_thread_names() {
    let worker = rust_sources()
        .into_iter()
        .find(|(path, _)| path == Path::new("onnx_worker.rs"))
        .map(|(_, source)| production_source(&source))
        .expect("process worker source exists");
    let transport = worker
        .split("pub(crate) struct ProcessWorkerSupervisor")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) struct InferenceWorkerSupervisor")
                .next()
        })
        .expect("generic process worker transport remains delimited");
    for stale in [
        "ONNX pending map",
        "ONNX transcription request",
        "ONNX request",
        "ONNX spawn lock",
        "ONNX writer lock",
        "an ONNX stream",
        "no ONNX stream",
        "another ONNX request",
    ] {
        assert!(
            !transport.contains(stale),
            "generic process worker transport restored ONNX-specific diagnostic {stale:?}"
        );
    }
    for stale in [
        "scribe-onnx-launch",
        "scribe-onnx-reader-",
        "scribe-onnx-reaper-",
    ] {
        assert!(
            !worker.contains(stale),
            "generic process worker thread restored ONNX-specific label {stale:?}"
        );
    }
    for required in [
        "scribe-process-worker-launch",
        "scribe-process-worker-reader-",
        "scribe-process-worker-reaper-",
    ] {
        assert!(
            worker.contains(required),
            "generic process worker thread label {required:?} is missing"
        );
    }
}

#[test]
fn native_runtime_ownership_is_confined_to_exact_owner_paths() {
    let sources = rust_sources();
    let worker = sources
        .iter()
        .find(|(path, _)| path == Path::new("onnx_worker.rs"))
        .map(|(_, source)| source.as_str())
        .expect("worker entrypoint exists");

    for required in [
        "INFERENCE_WORKER_FLAG",
        "VAD_WORKER_FLAG",
        "--scribe-inference-worker",
        "--scribe-vad-worker",
        "WorkerRole::Inference",
        "WorkerRole::Vad",
        "pub(crate) fn maybe_run_vad_worker()",
        "pub(crate) fn run_inference_worker_with_factory",
        "fn worker_loop_for_role",
        "RuntimeRouter::new()",
        "fn load_worker_runtime",
        "fn execute_worker_batch",
        "WireRuntimeArtifact::OnnxBundle",
    ] {
        assert!(
            worker.contains(required),
            "unified child runtime must retain {required:?}"
        );
    }
    for obsolete in [
        "LEGACY_ONNX_WORKER_FLAG",
        "OnnxSpeechRuntime",
        "OnnxWorkerSupervisor",
        "Control::Load {",
        "Control::Transcribe",
    ] {
        assert!(
            !worker.contains(obsolete),
            "obsolete nested ONNX topology {obsolete:?} must stay removed"
        );
    }
    let role_parser = worker
        .split("fn worker_role_from_args")
        .nth(1)
        .and_then(|tail| tail.split("/// Generic parent-side facade").next())
        .expect("worker role parser remains delimited before the parent facade");
    for required_rejection in [
        "value == \"--onnx-worker\"",
        "value.starts_with(\"--scribe-\")",
        "value.ends_with(\"-worker\")",
        "bail!(\"unknown private Scribe worker role\")",
    ] {
        assert!(
            role_parser.contains(required_rejection),
            "private worker-shaped arguments must fail closed via {required_rejection:?}"
        );
    }
    let router = sources
        .iter()
        .find(|(path, _)| path == Path::new("runtime_router.rs"))
        .map(|(path, source)| production_source_for(path, source))
        .expect("runtime router source exists");
    for obsolete in [
        "OnnxSupervisorControl",
        "OnnxSupervisorFactory",
        "production_onnx_supervisor",
        "ProcessWorkerSupervisor",
        "HeavyRuntimeOwner::OnnxSpeech",
    ] {
        assert!(
            !router.contains(obsolete),
            "RuntimeRouter restored obsolete ONNX machinery {obsolete:?}"
        );
    }

    let native_owner_sources = sources
        .iter()
        .filter(|(path, _)| is_native_runtime_owner(path))
        // An owner module may contain focused `#[cfg(test)]` adapters between
        // its production sections. The exact path allowlist is authoritative,
        // so inspect the complete owner module here.
        .map(|(_, source)| source.as_str())
        .collect::<Vec<_>>();
    assert!(
        native_owner_sources.iter().any(|source| {
            source.contains("OfflineRecognizer::create(")
                && source.contains("OnlineRecognizer::create(")
        }),
        "the marked child runtime must directly own the sherpa recognizers"
    );
    assert!(
        native_owner_sources
            .iter()
            .any(|source| source.contains("SileroVadModel::load_bundled(")),
        "the marked child runtime must directly own the VAD recognizer"
    );
    assert!(
        native_owner_sources
            .iter()
            .any(|source| source.contains("Model::load_with(")),
        "the marked child runtime must directly own the embedded GGUF model"
    );

    let native_constructors = [
        "Model::load_with(",
        "EmbeddedRuntime::new(",
        "TranscribeCppRuntime::new(",
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
        "SileroVadModel::load_bundled(",
        "NativeRuntimeOpaque",
        "NativeWhisperHandle",
    ];
    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source_for(path, source);
        if native_constructors
            .iter()
            .any(|constructor| production.contains(constructor))
        {
            assert!(
                is_native_runtime_owner(path),
                "native model/session/recognizer/FFI construction escaped the exact owner allowlist: {}",
                path.display()
            );
        }
    }

    let service = sources
        .iter()
        .find(|(path, _)| path == Path::new("transcription.rs"))
        .map(|(_, source)| production_source(source))
        .expect("transcription service source exists");
    assert!(
        service.contains("let worker = RuntimeWorker::new_process();"),
        "production TranscriptionService must dispatch through the process supervisor"
    );

    let main = sources
        .iter()
        .find(|(path, _)| path == Path::new("main.rs"))
        .map(|(_, source)| production_source(source))
        .expect("desktop entrypoint exists");
    assert!(main.contains("onnx_worker::maybe_run_vad_worker()"));
    assert!(!main.contains("inference_server"));
    assert!(!main.contains("run_inference_worker_with_factory"));

    let inference_server = sources
        .iter()
        .find(|(path, _)| path == Path::new("inference_server.rs"))
        .map(|(_, source)| production_source(source))
        .expect("worker-only inference server exists");
    for required in [
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
        "run_inference_worker_with_factory(&NativeRecognizerFactory)",
    ] {
        assert!(
            inference_server.contains(required),
            "worker-only inference server must retain {required:?}"
        );
    }
    for forbidden in [
        "OfflineRecognizer",
        "OnlineRecognizer",
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
        "NativeRecognizerFactory",
        "offline_recognizer_config",
        "online_recognizer_config",
    ] {
        assert!(
            !worker.contains(forbidden),
            "desktop/VAD worker substrate must not compile ASR server token {forbidden:?}"
        );
    }

    let dedicated = sources
        .iter()
        .find(|(path, _)| path == Path::new("bin/scribe-inference-worker.rs"))
        .map(|(_, source)| production_source(source))
        .expect("dedicated inference entrypoint exists");
    assert!(dedicated.contains("mod inference_server;"));
    assert!(dedicated.contains("inference_server::run()"));
    assert!(!dedicated.contains("maybe_run_vad_worker"));
}

#[test]
fn frozen_worker_compatibility_is_compiled_exact_and_inference_only() {
    use crate::worker_compatibility::{
        FrozenCpuWorkerCandidate, FrozenGpuBackend, FrozenGpuWorkerCandidate,
        approve_test_cpu_worker, approve_test_gpu_worker,
    };

    const ORIGIN_APP_BUILD: &str =
        "local-transcriber@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078";
    const WORKER_BUILD: &str =
        "scribe-inference-worker@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078";
    const CPU_WORKER_SHA256: &str =
        "2f8c80b0f74d8c52b88569ce851964da91f36c76a537e9a447995a0aa00ee0f0";
    const CUDA_WORKER_SHA256: &str =
        "4059b48dcdc94e79c2bd278f886549b85e61f319e3518ff72d5d163b987f2e43";
    const CUDA_PACK_VERSION: &str = "r209-49e9d68";
    const CUDA_PACK_DIGEST: &str =
        "7fe5bb0e0137b8d8a9c9c08c5fa21e66e9f0ea6e40c0dd2b0464b4351d9559bb";
    const VULKAN_WORKER_SHA256: &str =
        "46d946c8aed1b830317b038f97ca1ed5fdce1483c859d471c2f8d4ecae202463";
    const VULKAN_PACK_VERSION: &str = "r209-49e9d68-v1";
    const VULKAN_PACK_DIGEST: &str =
        "6a68d3b00b7afea39eed52f03121ccbfadfd080333963101770e6a717f1db763";
    const LONG_PATH_ORIGIN: &str =
        "local-transcriber@0.1.0#e4238ea997b1bc5f39929d3da6291ce6cd3d28ea";
    const LONG_PATH_WORKER: &str =
        "scribe-inference-worker@0.1.0#e4238ea997b1bc5f39929d3da6291ce6cd3d28ea";
    const LONG_PATH_CPU_SHA256: &str =
        "126edab6a8b1e88b93fa44f1246a7d27b30b4ea207ec74bb6609cef6e9d76a9a";
    const OLD_ORIGIN_APP_BUILD: &str =
        "local-transcriber@0.1.0#c03e125ced185e74a3215bd582a5f5f621569f20";
    const OLD_WORKER_BUILD: &str =
        "scribe-inference-worker@0.1.0#c03e125ced185e74a3215bd582a5f5f621569f20";
    const OLD_CPU_WORKER_SHA256: &str =
        "e7085925086fde3a25aa410610c4c89324bbc3b4ed6b857c28a08d31ca33b54d";
    const OLD_CUDA_WORKER_SHA256: &str =
        "d490a858c86876773606ef5f8c1dc50109a17bdd72a9a248dee83e3a6282186e";
    const OLD_CUDA_PACK_VERSION: &str = "frozen-r-c03e125";
    const OLD_CUDA_PACK_DIGEST: &str =
        "fc1f003b290209ebdd285820befcc74ddaa9d4582e1ae8ac86e983779f72db70";
    const OLD_VULKAN_WORKER_SHA256: &str =
        "b5b10d6cc67791644f3cd2d0db53b610b112fedaef004918f3651b9d7e335b62";
    const OLD_VULKAN_PACK_VERSION: &str = "frozen-r-c03e125";
    const OLD_VULKAN_PACK_DIGEST: &str =
        "9d3f2b18c9a20206afa2ac831d92ee080c04114b5dfa1607bc5fa4bb679283a5";
    const EXPECTED_POLICY: &str = concat!(
        "{\"schema_version\":1,\"target_os\":\"windows\",\"target_arch\":\"x86_64\",\"entries\":[",
        "{\"kind\":\"cpu\",\"origin_app_build\":\"local-transcriber@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078\",\"worker_build\":\"scribe-inference-worker@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078\",\"worker_sha256\":\"2f8c80b0f74d8c52b88569ce851964da91f36c76a537e9a447995a0aa00ee0f0\",\"protocol_version\":5,\"runtime_abi_version\":1},",
        "{\"kind\":\"gpu\",\"origin_app_build\":\"local-transcriber@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078\",\"worker_build\":\"scribe-inference-worker@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078\",\"worker_sha256\":\"4059b48dcdc94e79c2bd278f886549b85e61f319e3518ff72d5d163b987f2e43\",\"protocol_version\":5,\"runtime_abi_version\":1,\"backend\":\"cuda\",\"provider\":\"transcribe-cpp-ggml-cuda\",\"pack_id\":\"scribe-cuda-windows-x64\",\"pack_version\":\"r209-49e9d68\",\"pack_digest\":\"7fe5bb0e0137b8d8a9c9c08c5fa21e66e9f0ea6e40c0dd2b0464b4351d9559bb\",\"security_epoch\":1},",
        "{\"kind\":\"gpu\",\"origin_app_build\":\"local-transcriber@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078\",\"worker_build\":\"scribe-inference-worker@0.1.0#49e9d684c40798d913949c88fd7cd1dd363e7078\",\"worker_sha256\":\"46d946c8aed1b830317b038f97ca1ed5fdce1483c859d471c2f8d4ecae202463\",\"protocol_version\":5,\"runtime_abi_version\":1,\"backend\":\"vulkan\",\"provider\":\"transcribe-cpp-ggml-vulkan\",\"pack_id\":\"scribe-vulkan-windows-x64\",\"pack_version\":\"r209-49e9d68-v1\",\"pack_digest\":\"6a68d3b00b7afea39eed52f03121ccbfadfd080333963101770e6a717f1db763\",\"security_epoch\":1}",
        ",",
        "{\"kind\":\"gpu\",\"origin_app_build\":\"local-transcriber@0.1.0#0321d97a2d068e6a99f083e3504c5c0e2ec6f67b\",\"worker_build\":\"scribe-inference-worker@0.1.0#0321d97a2d068e6a99f083e3504c5c0e2ec6f67b\",\"worker_sha256\":\"eff7e6ba4bf42c2ca3b26c0249a0cd99580fa62cd025c254db182d54623b5e91\",\"protocol_version\":5,\"runtime_abi_version\":1,\"backend\":\"vulkan\",\"provider\":\"transcribe-cpp-ggml-vulkan\",\"pack_id\":\"scribe-vulkan-windows-x64\",\"pack_version\":\"r220-0321d97\",\"pack_digest\":\"e4abadbb950b7c3c5c6b3aa0bfb6dc8db9d5d967b45d2733a3d2b881fe9a96b8\",\"security_epoch\":1}",
        ",",
        "{\"kind\":\"cpu\",\"origin_app_build\":\"local-transcriber@0.1.0#e4238ea997b1bc5f39929d3da6291ce6cd3d28ea\",\"worker_build\":\"scribe-inference-worker@0.1.0#e4238ea997b1bc5f39929d3da6291ce6cd3d28ea\",\"worker_sha256\":\"126edab6a8b1e88b93fa44f1246a7d27b30b4ea207ec74bb6609cef6e9d76a9a\",\"protocol_version\":5,\"runtime_abi_version\":1},",
        "{\"kind\":\"gpu\",\"origin_app_build\":\"local-transcriber@0.1.0#e4238ea997b1bc5f39929d3da6291ce6cd3d28ea\",\"worker_build\":\"scribe-inference-worker@0.1.0#e4238ea997b1bc5f39929d3da6291ce6cd3d28ea\",\"worker_sha256\":\"45c41b12a22c880b87fced2a5de93449777dc496c1323a27ba60f0f23a916fc3\",\"protocol_version\":5,\"runtime_abi_version\":1,\"backend\":\"vulkan\",\"provider\":\"transcribe-cpp-ggml-vulkan\",\"pack_id\":\"scribe-vulkan-windows-x64\",\"pack_version\":\"lp-e4238ea-r1\",\"pack_digest\":\"a9832a0914fc1cd6e1a24bdd3b7249991c2ed8d9841e1c87912440c0af20766d\",\"security_epoch\":1}",
        "]}"
    );

    let desktop = production_source(include_str!("main.rs"));
    let worker_entry = production_source(include_str!("bin/scribe-inference-worker.rs"));
    let author = production_source(include_str!("../tools/worker-pack-author/src/main.rs"));
    let compatibility = production_source(include_str!("worker_compatibility.rs"));
    let worker = production_source(include_str!("onnx_worker.rs"));
    let documentation = include_str!("../docs/GPU_WORKER_PACKS.md");
    let documentation_words = documentation
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let manifest_bytes: &[u8] =
        include_bytes!("../runtime-manifests/frozen-worker-compatibility-windows-x64.json");
    let manifest_without_line_ending = manifest_bytes
        .strip_suffix(b"\r\n")
        .or_else(|| manifest_bytes.strip_suffix(b"\n"))
        .unwrap_or(manifest_bytes);

    assert_eq!(manifest_without_line_ending, EXPECTED_POLICY.as_bytes());

    let cpu_candidate = FrozenCpuWorkerCandidate {
        worker_sha256: CPU_WORKER_SHA256,
        protocol_version: 5,
        runtime_abi_version: 1,
    };
    let cpu_approval = approve_test_cpu_worker(manifest_bytes, &cpu_candidate)
        .expect("the exact frozen CPU worker must be approved");
    assert!(cpu_approval.is_cpu());
    assert_eq!(cpu_approval.context().origin_app_build, ORIGIN_APP_BUILD);
    assert_eq!(cpu_approval.worker_build(), WORKER_BUILD);
    assert_eq!(cpu_approval.worker_sha256(), CPU_WORKER_SHA256);
    assert_eq!(cpu_approval.protocol_version(), 5);
    assert_eq!(cpu_approval.runtime_abi_version(), 1);

    let long_path_cpu = FrozenCpuWorkerCandidate {
        worker_sha256: LONG_PATH_CPU_SHA256,
        ..cpu_candidate
    };
    let long_path_cpu_approval = approve_test_cpu_worker(manifest_bytes, &long_path_cpu)
        .expect("the exact manifest-bearing frozen CPU worker must be approved");
    assert!(long_path_cpu_approval.is_cpu());
    assert_eq!(
        long_path_cpu_approval.context().origin_app_build,
        LONG_PATH_ORIGIN
    );
    assert_eq!(long_path_cpu_approval.worker_build(), LONG_PATH_WORKER);
    assert_eq!(long_path_cpu_approval.worker_sha256(), LONG_PATH_CPU_SHA256);
    assert_eq!(long_path_cpu_approval.protocol_version(), 5);
    assert_eq!(long_path_cpu_approval.runtime_abi_version(), 1);
    for changed in [
        FrozenCpuWorkerCandidate {
            worker_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            ..long_path_cpu
        },
        FrozenCpuWorkerCandidate {
            protocol_version: 6,
            ..long_path_cpu
        },
        FrozenCpuWorkerCandidate {
            runtime_abi_version: 2,
            ..long_path_cpu
        },
    ] {
        assert!(approve_test_cpu_worker(manifest_bytes, &changed).is_none());
    }

    let exact_cuda_candidate = FrozenGpuWorkerCandidate {
        origin_app_build: ORIGIN_APP_BUILD,
        worker_build: WORKER_BUILD,
        worker_sha256: CUDA_WORKER_SHA256,
        protocol_version: 5,
        runtime_abi_version: 1,
        backend: FrozenGpuBackend::Cuda,
        provider: "transcribe-cpp-ggml-cuda",
        pack_id: "scribe-cuda-windows-x64",
        pack_version: CUDA_PACK_VERSION,
        pack_digest: CUDA_PACK_DIGEST,
        security_epoch: 1,
    };
    let cuda_approval = approve_test_gpu_worker(manifest_bytes, &exact_cuda_candidate)
        .expect("the exact frozen CUDA worker pack must be approved");
    assert!(cuda_approval.matches_gpu(&exact_cuda_candidate));

    let exact_vulkan_candidate = FrozenGpuWorkerCandidate {
        origin_app_build: ORIGIN_APP_BUILD,
        worker_build: WORKER_BUILD,
        worker_sha256: VULKAN_WORKER_SHA256,
        protocol_version: 5,
        runtime_abi_version: 1,
        backend: FrozenGpuBackend::Vulkan,
        provider: "transcribe-cpp-ggml-vulkan",
        pack_id: "scribe-vulkan-windows-x64",
        pack_version: VULKAN_PACK_VERSION,
        pack_digest: VULKAN_PACK_DIGEST,
        security_epoch: 1,
    };
    let vulkan_approval = approve_test_gpu_worker(manifest_bytes, &exact_vulkan_candidate)
        .expect("the exact frozen Vulkan worker pack must be approved");
    assert!(vulkan_approval.matches_gpu(&exact_vulkan_candidate));

    let rebuilt_vulkan = FrozenGpuWorkerCandidate {
        origin_app_build: "local-transcriber@0.1.0#0321d97a2d068e6a99f083e3504c5c0e2ec6f67b",
        worker_build: "scribe-inference-worker@0.1.0#0321d97a2d068e6a99f083e3504c5c0e2ec6f67b",
        worker_sha256: "eff7e6ba4bf42c2ca3b26c0249a0cd99580fa62cd025c254db182d54623b5e91",
        protocol_version: 5,
        runtime_abi_version: 1,
        backend: FrozenGpuBackend::Vulkan,
        provider: "transcribe-cpp-ggml-vulkan",
        pack_id: "scribe-vulkan-windows-x64",
        pack_version: "r220-0321d97",
        pack_digest: "e4abadbb950b7c3c5c6b3aa0bfb6dc8db9d5d967b45d2733a3d2b881fe9a96b8",
        security_epoch: 1,
    };
    let long_path_vulkan = FrozenGpuWorkerCandidate {
        origin_app_build: LONG_PATH_ORIGIN,
        worker_build: LONG_PATH_WORKER,
        worker_sha256: "45c41b12a22c880b87fced2a5de93449777dc496c1323a27ba60f0f23a916fc3",
        pack_version: "lp-e4238ea-r1",
        pack_digest: "a9832a0914fc1cd6e1a24bdd3b7249991c2ed8d9841e1c87912440c0af20766d",
        ..rebuilt_vulkan
    };
    for rebuilt_vulkan in [rebuilt_vulkan, long_path_vulkan] {
        let rebuilt_approval = approve_test_gpu_worker(manifest_bytes, &rebuilt_vulkan)
            .expect("the exact rebuilt Vulkan worker pack must be approved");
        assert!(rebuilt_approval.matches_gpu(&rebuilt_vulkan));
        for changed in [
            FrozenGpuWorkerCandidate {
                origin_app_build: ORIGIN_APP_BUILD,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                worker_build: WORKER_BUILD,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                worker_sha256: VULKAN_WORKER_SHA256,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                protocol_version: 6,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                runtime_abi_version: 2,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                backend: FrozenGpuBackend::Cuda,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                provider: "transcribe-cpp-ggml-cuda",
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                pack_id: "scribe-cuda-windows-x64",
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                pack_version: VULKAN_PACK_VERSION,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                pack_digest: VULKAN_PACK_DIGEST,
                ..rebuilt_vulkan
            },
            FrozenGpuWorkerCandidate {
                security_epoch: 2,
                ..rebuilt_vulkan
            },
        ] {
            assert!(approve_test_gpu_worker(manifest_bytes, &changed).is_none());
        }
    }

    assert!(
        approve_test_cpu_worker(
            manifest_bytes,
            &FrozenCpuWorkerCandidate {
                worker_sha256: OLD_CPU_WORKER_SHA256,
                protocol_version: 5,
                runtime_abi_version: 1,
            },
        )
        .is_none()
    );
    for (current, old_worker_sha256, old_pack_version, old_pack_digest) in [
        (
            &exact_cuda_candidate,
            OLD_CUDA_WORKER_SHA256,
            OLD_CUDA_PACK_VERSION,
            OLD_CUDA_PACK_DIGEST,
        ),
        (
            &exact_vulkan_candidate,
            OLD_VULKAN_WORKER_SHA256,
            OLD_VULKAN_PACK_VERSION,
            OLD_VULKAN_PACK_DIGEST,
        ),
    ] {
        for (case, candidate) in [
            (
                "previous complete cohort",
                FrozenGpuWorkerCandidate {
                    origin_app_build: OLD_ORIGIN_APP_BUILD,
                    worker_build: OLD_WORKER_BUILD,
                    worker_sha256: old_worker_sha256,
                    pack_version: old_pack_version,
                    pack_digest: old_pack_digest,
                    ..*current
                },
            ),
            (
                "new worker with previous pack identity",
                FrozenGpuWorkerCandidate {
                    pack_version: old_pack_version,
                    pack_digest: old_pack_digest,
                    ..*current
                },
            ),
            (
                "previous worker with new pack identity",
                FrozenGpuWorkerCandidate {
                    worker_sha256: old_worker_sha256,
                    ..*current
                },
            ),
            (
                "previous origin build",
                FrozenGpuWorkerCandidate {
                    origin_app_build: OLD_ORIGIN_APP_BUILD,
                    ..*current
                },
            ),
            (
                "previous worker build",
                FrozenGpuWorkerCandidate {
                    worker_build: OLD_WORKER_BUILD,
                    ..*current
                },
            ),
        ] {
            assert!(
                approve_test_gpu_worker(manifest_bytes, &candidate).is_none(),
                "{case} unexpectedly approved for {:?}",
                current.backend,
            );
        }
    }

    let unknown_cpu_sha256 = "00".repeat(32);
    assert!(
        approve_test_cpu_worker(
            manifest_bytes,
            &FrozenCpuWorkerCandidate {
                worker_sha256: &unknown_cpu_sha256,
                ..cpu_candidate
            },
        )
        .is_none()
    );
    let unknown_cuda_sha256 = "22".repeat(32);
    let unknown_cuda_candidate = FrozenGpuWorkerCandidate {
        worker_sha256: &unknown_cuda_sha256,
        ..exact_cuda_candidate
    };
    assert!(approve_test_gpu_worker(manifest_bytes, &unknown_cuda_candidate).is_none());
    let unknown_cuda_pack_candidate = FrozenGpuWorkerCandidate {
        worker_sha256: CUDA_WORKER_SHA256,
        pack_id: "scribe-cuda-windows-x64-unknown",
        ..unknown_cuda_candidate
    };
    assert!(approve_test_gpu_worker(manifest_bytes, &unknown_cuda_pack_candidate).is_none());
    let unknown_vulkan_sha256 = "11".repeat(32);
    let unknown_vulkan_candidate = FrozenGpuWorkerCandidate {
        worker_sha256: &unknown_vulkan_sha256,
        ..exact_vulkan_candidate
    };
    assert!(approve_test_gpu_worker(manifest_bytes, &unknown_vulkan_candidate).is_none());
    let unknown_vulkan_pack_candidate = FrozenGpuWorkerCandidate {
        worker_sha256: VULKAN_WORKER_SHA256,
        pack_id: "scribe-vulkan-windows-x64-unknown",
        ..unknown_vulkan_candidate
    };
    assert!(approve_test_gpu_worker(manifest_bytes, &unknown_vulkan_pack_candidate).is_none());
    for source in [desktop.as_str(), worker_entry.as_str(), author.as_str()] {
        assert!(source.contains("mod worker_compatibility;"));
    }
    for required in [
        "struct FrozenWorkerApproval",
        "approve_compiled_cpu_worker",
        "approve_compiled_gpu_worker",
        "include_bytes!(\"../runtime-manifests/frozen-worker-compatibility-windows-x64.json\")",
    ] {
        assert!(
            compatibility.contains(required),
            "frozen-worker compile-time authority lost {required:?}"
        );
    }
    assert!(!compatibility.contains("std::env::var"));
    assert!(!compatibility.contains("std::env::args"));
    assert!(worker.contains("CompatibilityHello"));
    assert!(worker.contains("worker compatibility Hello is inference-only"));
    assert!(worker.contains("worker compatibility sessions are unsupported on this platform"));
    assert!(documentation.contains("bounded LOCAL diagnostic artifact mapping"));
    assert!(documentation.contains("exactly six independently verified entries"));
    assert!(documentation_words.contains(
        "LOCAL describes intended use and publication status, not a runtime enforcement boundary"
    ));
    assert!(documentation_words.contains(
        "every Windows x64 consumer compiled from this source contains these exact approvals"
    ));
    assert!(documentation.contains("does not authenticate the parent executable to the child"));
    assert!(documentation.contains("does not qualify `Auto`"));
    assert!(documentation.contains("inference-only SCIF v5 session boundary"));
    assert!(documentation_words.contains("cannot acquire the new Hello behavior retroactively"));
}

#[test]
fn verified_worker_pack_stage_five_keeps_auto_evidence_bound_and_trust_closed() {
    let desktop = include_str!("main.rs");
    let module = include_str!("gpu_worker_pack/mod.rs");
    let health = include_str!("gpu_worker_pack/health.rs");
    let manifest = include_str!("gpu_worker_pack/manifest.rs");
    let store = include_str!("gpu_worker_pack/store.rs");
    let worker = include_str!("onnx_worker.rs");
    let qualification = include_str!("gpu_auto_qualification.rs");
    let mac_launcher = include_str!("macos_worker_launch.rs");
    let documentation = include_str!("../docs/GPU_WORKER_PACKS.md");
    let qualification_manifest =
        include_str!("../runtime-manifests/gpu-auto-qualification-windows-x64.json");
    let production_manifest = production_source(manifest);
    let registry_body = module
        .split("pub(crate) fn production_registry() -> ProductionPackRegistry")
        .nth(1)
        .and_then(|source| source.split('}').next())
        .expect("production registry function remains structurally visible");
    assert!(production_manifest.contains("struct ProductionTrustRoot"));
    assert!(production_manifest.contains("fn public_key(&self, _key_id: &str) -> Option<&[u8]>"));
    assert!(
        production_manifest.contains("../../runtime-manifests/worker-pack-production-trust.json")
    );
    let production_trust: serde_json::Value = serde_json::from_str(include_str!(
        "../runtime-manifests/worker-pack-production-trust.json"
    ))
    .expect("compiled production trust manifest must be valid JSON");
    assert_eq!(production_trust["schema_version"], 1);
    let trust_root_is_empty = production_trust["keys"] == serde_json::json!([]);
    for module_lint_reason in [
        "the desktop retains the target-aware Stage 5 Auto policy types used by its private worker protocol",
        "the desktop embeds Stage 5 qualification evidence for private worker routing without exposing a public settings surface",
        "Stage 4 retains verified-pack activation and rollback seams beyond bundled catalog discovery",
    ] {
        assert!(desktop.contains(module_lint_reason));
    }
    assert!(registry_body.contains("ProductionPackRegistry::empty()"));
    assert_eq!(production_trust["keys"].as_array().unwrap().len(), 1);
    assert_eq!(
        production_trust["keys"][0]["key_id"],
        "scribe-pack-ed25519-20260929-v1"
    );
    for source in [desktop, module, health, manifest, store] {
        assert!(
            !source.contains("#![allow"),
            "Stage 3 lint exceptions must remain module-scoped, never crate-wide"
        );
    }
    for required in [
        "trait ResolverHelloBindingBridge",
        "struct VerifiedPackLaunchBinding",
        "resolver_verified_pack_lease(&self) -> Arc<VerifiedPackLease>",
        "verified_pack_lease: Arc<VerifiedPackLease>",
        "pub(crate) fn verified_pack_lease(&self) -> &VerifiedPackLease",
        "bindings: Vec<VerifiedPackLaunchBinding>",
        "from_launch_bindings(bindings: Vec<VerifiedPackLaunchBinding>)",
        "try_from_resolver_hello_bridge",
        "bridge.hello_pack_id() == pack.pack_id.as_str()",
        "bridge.hello_pack_version() == pack.pack_version.as_str()",
        "bridge.hello_pack_digest() == pack.pack_digest",
        "bridge.hello_runtime_abi() == pack.runtime_abi_version",
        "bridge.hello_backend() == pack.backend",
        "bridge.hello_provider() == pack.provider",
        "hello_stable_device_identity",
        "struct UnixPackExecAuthority",
        "resolver_unix_launch_authority",
        "verified_pack_lease: Arc<VerifiedPackLease>",
        "Arc::ptr_eq",
        "executable_fd",
        "dependency_root_fd",
    ] {
        assert!(
            module.contains(required),
            "typed Stage 4 production provisioning gate lost {required:?}"
        );
    }
    let production_worker = production_source(worker);
    let production_launch_flow = format!("{worker}\n{module}\n{mac_launcher}");
    assert!(
        production_worker.contains(
            "fn resolver_unix_launch_authority(\n        &self,\n    ) -> Option<Arc<crate::gpu_worker_pack::UnixPackExecAuthority>>"
        ),
        "Stage 4 Unix pack binding must expose a fallible authority boundary"
    );
    assert!(
        !production_worker.contains(
            "unreachable!(\"production verified-pack launch is Windows-only in Stage 4\")"
        ),
        "unsupported Unix production pack binding must fail closed without a panic"
    );
    for required in [
        "struct LaunchableWorker<'lease>",
        "_lease: &'lease VerifiedPackLease",
    ] {
        assert!(
            production_manifest.contains(required),
            "verified-pack lease launch gate lost {required:?}"
        );
    }
    assert!(
        production_pack_provisioning_allowed(
            registry_body,
            &production_launch_flow,
            trust_root_is_empty,
            cfg!(unix),
        ),
        "production pack trust/catalog cannot be provisioned before production discovery consumes concrete typed bindings created by WorkerExecutableResolver and Hello validation"
    );
    for required in [
        "Stage 4",
        "WorkerExecutableResolver",
        "Hello",
        "ID/version/digest",
        "backend/provider",
        "stable device",
        "ProductionTrustRoot",
        "reviewed public key",
        "Stage 5 Windows Auto qualification",
        "default_deny",
        "five cold and twenty warm",
    ] {
        assert!(
            documentation.contains(required),
            "Stage 4 pack-launch binding documentation lost {required}"
        );
    }
    for forbidden in ["Ed25519KeyPair", "private_key", "signing_seed"] {
        assert!(
            !production_manifest.contains(forbidden),
            "production pack verifier contains signing material/API marker {forbidden}"
        );
    }
    assert!(module.contains("--scribe-verify-worker-pack"));
    assert!(module.contains("PackBackend::Cuda"));
    assert!(module.contains("PackBackend::Vulkan"));
    assert!(module.contains("PackBackend::Metal"));
    for required in [
        "fn discover_production_pack_launch_bindings(",
        "InferenceWorkerSupervisor::for_pack_probe,",
        "verified_pack_bindings()",
        "ProductionPackRegistry::from_launch_bindings(bindings)",
        "preference == AccelerationPreference::Auto",
        "auto_qualified_pack_discovery",
        "auto_gpu_discovery_fingerprint",
        "auto_qualified_gpu_route_catalog",
        "AutoQualificationPolicy::embedded_current_platform",
        "auto_gpu_routes",
        "Auto selected the guaranteed CPU fallback",
        "worker_preference_for_route",
        "routes: Arc::clone(&self.routes)",
    ] {
        assert!(
            worker.contains(required),
            "Stage 5 verified discovery/evidence-bound Auto contract lost {required:?}"
        );
    }
    let discovery_route = production_worker
        .split("fn discover_production_pack_launch_bindings(")
        .nth(1)
        .and_then(|source| source.split("fn prepare_pack_probes(").next())
        .expect("production pack discovery remains structurally visible");
    for required in [
        "crate::gpu_worker_pack::prepare_pack_execution,",
        "InferenceWorkerSupervisor::for_pack_probe,",
    ] {
        assert!(discovery_route.contains(required));
    }
    let prepared = discovery_route.find("prepare_pack_probes(").unwrap();
    let probed = discovery_route
        .find("discover_pack_launch_bindings_with_budget(")
        .unwrap();
    assert!(
        prepared < probed,
        "execution preparation must precede the provider budget"
    );
    assert!(qualification.contains("serde(deny_unknown_fields)"));
    assert!(qualification.contains("cold_runs < 5"));
    assert!(qualification.contains("warm_runs < 20"));
    assert!(qualification.contains("* 100 > u128::from(evidence.cpu_p95_ms) * 110"));
    assert!(qualification.contains("correctness_verified"));
    assert!(qualification.contains("reliability_verified"));
    assert!(qualification.contains("fn qualify_pack"));
    assert!(qualification.contains("fn qualify_target"));
    let qualification_value: serde_json::Value = serde_json::from_str(qualification_manifest)
        .expect("Auto qualification manifest is valid JSON");
    assert_eq!(qualification_value["schema_version"], 2);
    assert_eq!(qualification_value["mode"], "default_deny");
    assert_eq!(qualification_value["target_os"], "windows");
    assert_eq!(qualification_value["target_arch"], "x86_64");
    assert_eq!(qualification_value["entries"], serde_json::json!([]));
    for required in [
        "crate::windows_vulkan_loader::ash_entry(require_policy_loader)",
        "PhysicalDeviceIDProperties",
        "PhysicalDeviceDriverProperties",
        "native:luid:",
        "native:uuid:",
        "VulkanDeviceCatalog::discover",
        "GPU provider device has no matching Vulkan LUID/UUID identity",
    ] {
        assert!(
            worker.contains(required),
            "worker-only Vulkan stable-identity contract lost {required:?}"
        );
    }
    let loader = include_str!("windows_vulkan_loader.rs");
    assert!(loader.contains("ash::Entry::from_static_fn(static_fn)"));
    assert!(loader.contains("GetProcAddress("));
    assert!(worker.contains("VulkanDeviceCatalog::discover(true)"));
}

#[test]
fn worker_pack_authoring_is_isolated_pinned_and_production_closed() {
    let root_manifest = include_str!("../Cargo.toml");
    let desktop = include_str!("main.rs");
    let production_manifest = include_str!("gpu_worker_pack/manifest.rs");
    let author_manifest = include_str!("../tools/worker-pack-author/Cargo.toml");
    let author_lock = include_str!("../tools/worker-pack-author/Cargo.lock");
    let author_entrypoint = include_str!("../tools/worker-pack-author/src/main.rs");
    let authoring = include_str!("worker_pack_authoring.rs");
    let build = include_str!("../scripts/build-windows-gpu-worker-pack.ps1");
    let cpu_baseline = include_str!("../scripts/windows-cpu-worker-native-baseline.ps1");
    let contract = include_str!("../runtime-manifests/gpu-worker-toolchain-windows-x64.json");

    assert!(!root_manifest.contains("scribe-worker-pack-tool"));
    assert!(!root_manifest.contains("pack-authoring"));
    assert!(!desktop.contains("worker_pack_authoring"));
    assert!(author_manifest.contains("publish = false"));
    assert!(author_manifest.contains("path = \"src/main.rs\""));
    assert!(author_lock.contains("name = \"scribe-worker-pack-tool\""));
    for required in [
        "check-production-key",
        "--fixture-signing",
        "SigningMode::Production",
        "production_key_pair",
        "TrustRoot::public_key(&ProductionTrustRoot, key_id)",
        "Ed25519KeyPair::from_pkcs8",
        "external production signing key does not match",
        "no separately reviewed public key embedded",
    ] {
        assert!(
            author_entrypoint.contains(required) || authoring.contains(required),
            "isolated pack authoring lost {required:?}"
        );
    }
    for forbidden in [
        "BEGIN PRIVATE KEY",
        "production_signing_seed",
        "PRODUCTION_SEED",
    ] {
        assert!(!authoring.contains(forbidden));
        assert!(!production_manifest.contains(forbidden));
        assert!(!contract.contains(forbidden));
    }
    for required in [
        "1.96.0",
        "ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96",
        "transcribe_cpp_checksum",
        "transcribe_cpp_sys_checksum",
        "ash_checksum",
        "39e9c3835d686b0a6084ab4234fcd1b07dbf6e4767dce60874b12356a25ecd4a",
        "a94e021ef658dc7c788837341a13f6acea3baf3c",
        "b7080b6f470bac96ef0afe56b25ae9b2f9f0ca82d10dad19bf3a2fc5ffd6cffc",
        "1.4.357.0",
        "f8c97ee2c8bfcd31da87b602622c6e742389f98a83693b504cf538de4c75d3fa",
        "12.8.93",
        "14.44.35207",
        "MultiThreaded",
        "/Brepro",
    ] {
        assert!(
            contract.contains(required),
            "toolchain contract lost {required:?}"
        );
    }
    for required in [
        "--locked",
        "--offline",
        "SCRIBE_BUILD_REVISION",
        "SCRIBE_BUILDING_WORKER = '1'",
        "SOURCE_DATE_EPOCH",
        "check-production-key",
        "windows-pe-imports.ps1",
        "Copy-ReviewedGpuWorkerDependencyClosure",
        "GPU pack contains an undeclared native dependency",
        "Resolve-ShortCargoTargetDirectory",
        "BuildEnvironment",
        "Enable-ValidatedCmakeBuildJunction",
        "one exact NTFS junction",
        "windows-cpu-worker-native-baseline.ps1",
        "Get-ValidatedGpuWorkerNativeBuildEvidenceRoot",
        "Assert-WindowsCpuWorkerBaselineNativeBuildEvidence $nativeBuildEvidenceRoot",
        "Assert-WindowsCpuWorkerBaselineAmbientEnvironment",
        "Get-ProcessEnvironmentState",
        "target-feature=+crt-static",
    ] {
        assert!(
            build.contains(required),
            "pack build gate lost {required:?}"
        );
    }
    for required in [
        "TRANSCRIBE_X86_CONSERVATIVE=ON",
        "GGML_NATIVE=OFF",
        "Assert-WindowsCpuWorkerBaselineNativeBuildEvidence",
    ] {
        assert!(
            cpu_baseline.contains(required),
            "shared CPU baseline gate lost {required:?}"
        );
    }
    assert!(root_manifest.contains(
        "vulkan-acceleration = [\"inference-worker\", \"transcribe-cpp/vulkan\", \"dep:ash\"]"
    ));
    assert!(root_manifest.contains("ash = { version = \"=0.37.3\", optional = true }"));
}

#[test]
fn worker_pack_health_persistence_stays_bounded_and_content_free() {
    let source = include_str!("gpu_worker_pack/health.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("health cache has a production section");
    for required in [
        "pack_digest",
        "runtime_abi",
        "os_arch",
        "driver_version",
        "stable_device_identity",
        "model_digest",
        "app_build",
        "device_set_digest",
        "FIRST_QUARANTINE_SECONDS: u64 = 15 * 60",
        "SECOND_QUARANTINE_SECONDS: u64 = 6 * 60 * 60",
        "THIRD_QUARANTINE_SECONDS: u64 = 7 * 24 * 60 * 60",
    ] {
        assert!(
            production.contains(required),
            "health contract lost {required}"
        );
    }
    for forbidden in [
        "audio_path",
        "transcript",
        "raw_error",
        "diagnostic_text",
        "error_message",
    ] {
        assert!(
            !production.contains(forbidden),
            "health cache production schema regained forbidden content: {forbidden}"
        );
    }
}

#[test]
fn release_packaging_accepts_only_compiled_verified_declared_pack_roots() {
    let build = include_str!("../scripts/build-windows-release.ps1");
    let stage = include_str!("../scripts/stage-verified-worker-packs.ps1");
    let release_policy = include_str!("../scripts/resolve-windows-gpu-release-policy.ps1");
    let installer = include_str!("../installer/scribe.iss");
    let workflow = include_str!("../.github/workflows/release.yml");
    for required in [
        "WorkerPackRoot",
        "stage-verified-worker-packs.ps1",
        "worker-pack-catalog.json",
        "PackFiles",
    ] {
        assert!(
            build.contains(required),
            "release pack integration lost {required}"
        );
    }
    assert!(stage.matches("Invoke-PackVerifier $verifier").count() >= 2);
    assert!(stage.contains("workers/packs/"));
    assert!(stage.contains("PackRoot.Count -gt 8"));
    assert!(stage.contains("allPackFiles.Count -gt 1024"));
    assert!(!stage.contains("Ed25519KeyPair"));
    assert!(!stage.contains("SIGNING_KEY"));
    assert!(installer.contains("#include WorkerPackAllowlist"));
    assert!(installer.contains("IsGeneratedWorkerPackFile(RelativePath)"));
    assert!(workflow.contains("/DWorkerPackAllowlist=..\\dist\\worker-pack-allowlist.iss"));
    for required in [
        "default: false",
        "SCRIBE_GPU_PACK_RELEASE_POLICY",
        "resolve-windows-gpu-release-policy.ps1",
        "temporary_cpu_only_stage4",
        "gpu_packs_required",
        "needs.build.outputs.gpu_worker_packs_included",
        "report-windows-worker-pack-sizes.ps1",
    ] {
        assert!(
            workflow.contains(required),
            "Stage 4 release workflow lost {required:?}"
        );
    }
    for forbidden in [
        "include_gpu_worker_packs:",
        "Build production-signed CUDA and Vulkan worker packs",
        "SCRIBE_GPU_PACK_SIGNING_KEY_PKCS8_BASE64",
        "SCRIBE_GPU_PACK_SIGNING_KEY_ID",
        "GPU_PACK_PRIVATE_KEY_BASE64",
        "artifacts\\gpu-worker-packs\\production",
        "-WorkerPackRoot",
    ] {
        assert!(
            !workflow.contains(forbidden),
            "candidate-ref release workflow regained signing authority or production GPU-pack input {forbidden:?}"
        );
    }
    for required in [
        "Official Windows releases require SCRIBE_GPU_PACK_RELEASE_POLICY",
        "temporary_cpu_only_stage4",
        "gpu_packs_required",
        "include_gpu_worker_packs",
        "candidate-ref workflow never receives GPU pack signing authority",
        "separately protected trusted signing workflow",
    ] {
        assert!(
            release_policy.contains(required),
            "GPU release policy gate lost {required:?}"
        );
    }
    assert!(!build.contains("--features vulkan-acceleration"));
}

#[test]
fn windows_gpu_pack_promotion_keeps_candidate_and_signing_authority_separate() {
    let authoring = include_str!("worker_pack_authoring.rs");
    let tool = include_str!("../tools/worker-pack-author/src/main.rs");
    let build = include_str!("../scripts/build-windows-gpu-worker-pack.ps1");
    let promote = include_str!("../scripts/promote-windows-gpu-worker-packs.ps1");
    let contract_test = include_str!("../scripts/test-windows-gpu-pack-promotion.ps1");
    let workflow = include_str!("../.github/workflows/windows-gpu-pack-promotion.yml");
    let broker_manifest = include_str!("../tools/windows-gpu-promotion-broker/Cargo.toml");
    let broker_client = include_str!("../tools/windows-gpu-promotion-broker/src/main.rs");
    let broker_contract = include_str!("../tools/windows-gpu-promotion-broker/src/lib.rs");
    let broker_protocol = include_str!("../tools/windows-gpu-promotion-broker/src/protocol.rs");
    let (broker_protocol_production, _) = broker_protocol
        .split_once("#[cfg(test)]")
        .expect("broker protocol must keep tests behind a production boundary");
    let broker_native = include_str!("../tools/windows-gpu-promotion-broker/src/windows_native.rs");
    let (broker_native_production, _) = broker_native
        .split_once("#[cfg(test)]")
        .expect("broker native transport must keep tests behind a production boundary");
    let broker_service = include_str!(
        "../tools/windows-gpu-promotion-broker/src/bin/scribe-windows-gpu-promotion-service.rs"
    );
    let broker_fixture = include_str!("../tools/windows-gpu-promotion-broker/src/fixture.rs");
    let transport_test = include_str!("../scripts/test-windows-gpu-broker-transport.ps1");
    let client_policy_provisioner =
        include_str!("../scripts/provision-windows-gpu-broker-client-policy.ps1");
    let protected = workflow
        .split("  protected-promote:")
        .nth(1)
        .expect("protected promotion job exists");

    for required in [
        "prepare-pack",
        "inspect-prepared-pack",
        "sign-prepared-pack",
        "--expected-manifest-sha256",
        "--expected-pack-digest",
    ] {
        assert!(
            tool.contains(required) || authoring.contains(required),
            "prepared-pack authoring contract lost {required:?}"
        );
    }
    for required in [
        "'Prepared'",
        "ManifestSha256",
        "ToolchainManifestSha256",
        "SigningKeyId = if ($SigningMode -eq 'Prepared') { $null }",
    ] {
        assert!(build.contains(required), "unsigned build lost {required:?}");
    }
    for required in [
        "ExpectedRepository",
        "ExpectedSourceRevision",
        "ExpectedRunAttempt",
        "ExpectedArtifactDigest",
        "ExpectedHandoffSha256",
        "ExpectedReleaseSetDigest",
        "ExpectedToolchainManifestSha256",
        "ExpectedPackVersion",
        "MinimumSecurityEpoch",
        "fixture-only",
        "repository script never receives production signing authority",
    ] {
        assert!(
            promote.contains(required),
            "fixture promotion boundary lost {required:?}"
        );
    }
    for required in [
        "environment: windows-gpu-pack-signing",
        "runs-on: windows-2022",
        "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c",
        "digest-mismatch: error",
        "cargo fetch --locked --manifest-path tools/worker-pack-author/Cargo.toml",
        "cargo fetch --locked --manifest-path tools/windows-gpu-promotion-broker/Cargo.toml",
        "cargo test --locked --offline --manifest-path tools/windows-gpu-promotion-broker/Cargo.toml",
        "test-windows-gpu-broker-transport.ps1 -RequireScmIntegration",
        "test-windows-gpu-approved-signing.ps1",
        "test-windows-gpu-signing-policy.ps1",
        "scripts/provision-windows-gpu-broker-client-policy.ps1",
        "steps.upload.outputs.artifact-id",
        "steps.upload.outputs.artifact-digest",
        "SCRIBE_GPU_SIGNER_SOURCE_SHA",
        "SCRIBE_GPU_SIGNER_BINARY_SHA256",
        "SCRIBE_GPU_SIGNER_WRAPPER_SHA256",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_SOURCE_SHA",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_RUN_ID",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_RUN_ATTEMPT",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_ARTIFACT_ID",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_ARTIFACT_SHA256",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_BINARY_SHA256",
        "vars.SCRIBE_GPU_APPROVED_SIGNER_WRAPPER_SHA256",
        "needs.preflight-signing.outputs.approval_sha256",
        "needs.preflight-signing.outputs.signer_pins_sha256",
        "-Mode PublicationCheck",
        "secrets.SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64",
    ] {
        assert!(
            workflow.contains(required),
            "protected workflow lost {required:?}"
        );
    }
    for forbidden in [
        "actions/checkout@",
        "cargo ",
        "self-hosted",
        "promote-windows-gpu-worker-packs.ps1",
        "--private-key",
        "--ledger-root",
        "--broker-endpoint",
        "--require-unused-release-set",
        "PRODUCTION_BROKER_PROVISIONED",
        "vars.SCRIBE_GPU_SIGNER_",
        "id-token: write",
    ] {
        assert!(
            !protected.contains(forbidden),
            "protected job regained candidate code or raw signing authority {forbidden:?}"
        );
    }
    assert_eq!(
        workflow
            .matches("secrets.SCRIBE_GPU_PACK_PRIVATE_KEY_BASE64")
            .count(),
        1,
        "raw signing authority must be scoped to one protected step"
    );
    let approved_wrapper = include_str!("../scripts/invoke-windows-gpu-approved-signing.ps1");
    for required in [
        "inspect-approved-windows-set",
        "sign-approved-windows-set",
        "Assert-SigningPinsUnchanged",
        "Get-CurrentSigningPolicy",
        "$process.StandardInput.BaseStream.Write",
        "CryptographicOperations]::ZeroMemory",
    ] {
        assert!(
            approved_wrapper.contains(required),
            "approved wrapper lost {required:?}"
        );
    }
    assert!(broker_manifest.contains("[workspace]"));
    assert!(broker_manifest.contains("rust-version = \"1.96\""));
    assert!(
        broker_contract
            .replace("\r\n", "\n")
            .contains("#[cfg(test)]\nmod fixture;")
    );
    for forbidden in [
        "fixture-ed25519-v1",
        "FIXTURE_SEED",
        "private-key",
        "ledger-root",
        "broker-endpoint",
    ] {
        assert!(
            !broker_client.contains(forbidden),
            "release broker client contains fixture or authority material {forbidden:?}"
        );
    }
    for required in [
        "#[serde(deny_unknown_fields)]",
        "promote-windows-pack-set",
        "--require-unused-release-set",
        "pub struct PromotionIntent",
        "#[derive(Clone, Eq, PartialEq)]",
        "pub struct ClientInvocation",
        "PROMOTION_POLICY_NAMESPACE",
        "scribe-windows-gpu-promotion-intent-v1",
        "self.intent.validate()?",
        "self.workflow_source_sha != self.source_revision",
        "validate_positive_decimal(&minimum_security_epoch_text, 20)",
        "minimum_security_epoch_requires_canonical_positive_u64_decimal",
        "canonical_intent_bytes_and_domain_digest_match_the_powershell_golden_vector",
    ] {
        assert!(
            broker_contract.contains(required),
            "broker request contract lost {required:?}"
        );
    }
    for required in [
        r"\\.\pipe\ScribeGpuPromotionBroker.v1",
        "ScribeGpuPromotionBroker",
        "S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137",
        "SGPBIPC1",
        "MAX_REQUEST_PAYLOAD",
        "MAX_RESPONSE_PAYLOAD",
        "BrokerRequestV1",
        "BrokerResponseV1",
        "BrokerAckV1",
        "ProductionAuthorityNotProvisioned",
        "scribe-windows-gpu-promotion-request-v1",
        "scribe-windows-gpu-promotion-response-v1",
        "MAX_ACK_PAYLOAD",
        "from_canonical_json",
        "#[serde(deny_unknown_fields)]",
    ] {
        assert!(
            broker_protocol.contains(required),
            "fixed broker wire contract lost {required:?}"
        );
    }
    for forbidden in [
        "handoff_root",
        "output_root",
        "broker_endpoint",
        "private_key",
        "ledger_root",
    ] {
        assert!(
            !broker_protocol_production.contains(forbidden),
            "broker wire contract gained local path or authority field {forbidden:?}"
        );
    }
    for required in [
        "SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)",
        "SetDllDirectoryW",
        "SECURITY_SQOS_PRESENT",
        "SECURITY_IDENTIFICATION",
        "SECURITY_EFFECTIVE_ONLY",
        "GetNamedPipeServerProcessId",
        "LookupAccountSidW",
        "SidTypeUser",
        "GetKernelObjectSecurity",
        "SetKernelObjectSecurity",
        "AddAccessAllowedAceEx",
        "TokenSessionId",
        "PROCESS_CLIENT_QUERY_ACCESS",
        "TOKEN_CLIENT_QUERY_ACCESS",
        "require_exact_query_acl_delta",
        "IsTokenRestricted",
        "TokenRestrictedSids",
        "CreateNamedPipeW",
        "FILE_FLAG_FIRST_PIPE_INSTANCE",
        "FILE_FLAG_OVERLAPPED",
        "PIPE_TYPE_MESSAGE",
        "PIPE_REJECT_REMOTE_CLIENTS",
        "ImpersonateNamedPipeClient",
        "RevertToSelf",
        "revert_or_abort",
        "SecurityIdentification",
        "TokenUser",
        "require_user_sid(token.raw(), authorized_client_sid)",
        r"SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization",
        "KEY_READ | KEY_WOW64_64KEY",
        "RegQueryInfoKeyW",
        "RegEnumValueW",
        "RegQueryValueExW",
        "SchemaVersion",
        "AuthorizedClientSid",
        "GetSecurityInfo",
        "SE_DACL_PROTECTED",
        "KEY_ALL_ACCESS_MASK",
        "KEY_READ_MASK",
        "load_authorization_policy()?",
        "create_server_pipe(&policy)?",
        "0x00100183",
        "D:P",
        "CancelIoEx",
        "encode_ack_frame",
        "decode_ack_frame",
        "StartServiceCtrlDispatcherW",
        "ERROR_CALL_NOT_IMPLEMENTED",
        "SERVICE_WIN32_OWN_PROCESS",
    ] {
        assert!(
            broker_native_production.contains(required),
            "authenticated Windows broker transport lost {required:?}"
        );
    }
    for forbidden in [
        ";;;AU)",
        ";;;BU)",
        ";;;LS)",
        ";;;BA)",
        ";;;WD)",
        ";;;AN)",
        "--broker-endpoint",
        "--console",
        "--install",
        "ProcessIdToSessionId",
        "SetTokenInformation",
        "TokenDefaultDacl",
        "AdjustTokenPrivileges",
    ] {
        assert!(
            !broker_native_production.contains(forbidden),
            "Windows broker transport gained forbidden authority or ACL input {forbidden:?}"
        );
    }
    assert!(broker_service.contains("run_service_dispatcher"));
    assert!(!broker_service.contains("std::env::args"));
    assert!(
        broker_client.contains("broker authenticated; production authority is not provisioned")
    );
    assert!(
        broker_client.contains("broker is unavailable and production authority is not provisioned")
    );
    for required in [
        "Refusing to modify the pre-existing fixed-name service",
        "sidtype",
        "restricted",
        "$sidTypeMatches.Count -eq 1",
        "Groups['value'].Value -ceq 'RESTRICTED'",
        "$primaryFailure",
        "$cleanupFailures",
        "$safeToRemoveMachineTarget",
        "NT AUTHORITY\\LocalService",
        "SetAccessRuleProtection",
        "S-1-5-32-544",
        "ReadAndExecute",
        "function Test-FileSystemAccessRuleConstructorNormalization",
        "0x000200a9",
        "0x001200a9",
        "$persistedRights",
        "FileSystemAccessRule constructor did not normalize",
        "[Security.AccessControl.FileSystemRights]::ReadAndExecute",
        "[Security.AccessControl.FileSystemRights]::Synchronize",
        "[Security.AccessControl.FileSystemRights]::Write",
        "[Security.AccessControl.FileSystemRights]::Delete",
        "[Security.AccessControl.FileSystemRights]::ChangePermissions",
        "[Security.AccessControl.FileSystemRights]::TakeOwnership",
        "$persistedReadAndExecuteRights",
        "refusing destructive cleanup",
        "FromSeconds(4)",
        "$stopProof = [Diagnostics.Stopwatch]::StartNew()",
        "$stopProof.Elapsed.TotalMilliseconds -lt 4500",
        "$exactRights -eq 0x00100183",
        "[IO.Pipes.PipeAccessRights]::Synchronize",
        "WaitForConnectionAsync",
        "The client sent request bytes before authenticating the service",
        "same-name user-process pipe server as rejected authentication",
        "fixed authenticated NotProvisioned diagnostic",
        "did not remain running after the authenticated round trip",
        "3925971f64ffaf94450d30373183cf912a01a8948a1a8d892831627329568083",
        "7d4774c4ad2c0f59d57079e33d3729863a2a679739845f21b4a023207b580143",
        "RequireScmIntegration",
        "WaitForStatus",
        "Refusing to modify a pre-existing fixed Windows GPU broker client policy",
        "Missing policy",
        "Weak broad-read policy DACL",
        "Extra policy value",
        "Malformed policy schema",
        "Broad malformed policy SID",
        "Assert-RejectedServiceStartup -Label 'Unmapped policy SID'",
        "orphan-SID",
        "Set-PolicyValue -Name 'AuthorizedClientSid' -Value $orphanSid",
        "Rejected unmapped-policy restart did not keep the broker unavailable.",
        "Rejected unmapped-policy restart exposed a broker pipe.",
        "PipeAccessRights]::FullControl",
        "Assert-ExactPolicyAcl",
        "Assert-OwnedPolicyState -State $state",
        "SecurityFingerprint",
        "CleanupTamper",
        "if ($result.ExitCode -eq 0)",
        "New-ProvisioningInvocationNonce",
        "Read-ProvisioningSuccessRecord",
        "$script:ownedPolicyAncestors = @($ownedPolicyAncestors) + @($createdByInvocation)",
        "NtDeleteKey(SafeRegistryHandle keyHandle)",
        "RegRenameKey(",
        "KEY_WRITE | DELETE | KEY_QUERY_VALUE | KEY_WOW64_64KEY",
        "DELETE | READ_CONTROL | KEY_QUERY_VALUE | KEY_ENUMERATE_SUB_KEYS | KEY_WOW64_64KEY",
        "Noncanonical policy value name",
        "boundary-swap policy",
        "Policy cleanup retained authority after its exact NtDeleteKey succeeded.",
        "$script:ownedPolicyAncestors = @($ownedPolicyAncestors | Where-Object { $_ -cne $path })",
        "Remove-OwnedPolicy",
        "$ephemeralAccount = New-EphemeralStandardAccount",
        "$script:ownedEphemeralAccount = [pscustomobject]@{",
        "-Sid $ephemeralSid",
        "@($ephemeralSid, [Security.AccessControl.FileSystemRights]::ReadAndExecute)",
        "$clientForCredential = Join-Path $machineTarget",
        "$harnessForCredential = Join-Path $machineTarget",
        "Get-FileHash -Algorithm SHA256 -LiteralPath $clientForCredential",
        "Get-FileHash -Algorithm SHA256 -LiteralPath $harnessForCredential",
        "Invoke-EphemeralProcess -FilePath $clientForCredential",
        "$start.UserName = $script:ownedEphemeralAccount.Name",
        "$start.Domain = $env:COMPUTERNAME",
        "$start.Password = $script:ephemeralPassword",
        "$start.LoadUserProfile = $false",
        "$start.WorkingDirectory = [IO.Path]::GetFullPath($machineTarget)",
        "$start.Environment.Clear()",
        "$candidateToken = New-CryptographicHex -ByteCount 16",
        "$pathToken = $fixturePaths.Token",
        "$machineTarget = $fixturePaths.MachineTarget",
        "$handoff = $fixturePaths.Handoff",
        "$output = $fixturePaths.Output",
        "-cmatch '^[0-9a-f]{32}$'",
        "-cmatch '^s[0-9a-f]{32}$'",
        r#"$leaf -cmatch "^$Prefix[0-9a-f]{32}$""#,
        "$serviceForScm = Join-Path $machineTarget 's.exe'",
        "$clientForCredential = Join-Path $machineTarget 'c.exe'",
        "$harnessForCredential = Join-Path $machineTarget 'p.ps1'",
        "Get-ValidatedNoTouchPath -DriveRoot $DriveRoot -Prefix 'h' -Token $candidateToken",
        "Get-ValidatedNoTouchPath -DriveRoot $DriveRoot -Prefix 'o' -Token $candidateToken",
        "Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output",
        "$DriveRoot -cmatch '^[A-Z]:\\\\$'",
        "[IO.Path]::IsPathFullyQualified($candidate)",
        "[IO.Path]::GetFullPath($candidate) -ceq $candidate",
        "[IO.Path]::GetPathRoot($candidate) -ceq $DriveRoot",
        "[IO.Path]::GetDirectoryName($candidate) -ceq $DriveRoot",
        "[IO.Path]::GetFileName($candidate) -ceq $leaf",
        "[IO.Path]::IsPathFullyQualified($env:SystemRoot)",
        "Windows system directory is noncanonical.",
        "[IO.Path]::IsPathFullyQualified($CommonAppData)",
        "Machine-wide application-data root is noncanonical.",
        "$HandoffRoot -cne $OutputRoot",
        "Could not select an absent three-path credentialed fixture set after $maximumAttempts attempts.",
        "The no-touch handoff path appeared and will be left untouched.",
        "The no-touch output path appeared and will be left untouched.",
        "$safeToRemoveMachineTarget = $null -ne $ownedMachineTarget",
        "Refusing protected staging cleanup while a credentialed process may still be active.",
        "Refusing protected staging cleanup while the exact broker service still exists.",
        "Refusing protected staging cleanup outside its exact CommonApplicationData parent.",
        "Refusing protected staging cleanup through a reparse point.",
        "Refusing protected staging cleanup containing an unexpected entry.",
        "public static class CredentialCommandLine",
        "CreateProcessWithLogonMaximumUtf16Units = 1024",
        "ReservedUtf16Units = 64",
        "MaximumUtf16UnitsIncludingNull = 960",
        "Credentialed command-line bound lost its fixed 64-unit reserve below the native 1024-unit ceiling.",
        "Render(fileName, arguments).Length + 1",
        "Char.IsWhiteSpace(value)",
        "commandLine.Append('\\\\', checked(backslashes * 2 + 1))",
        "commandLine.Append('\\\\', checked(backslashes * 2))",
        "Executable path is not canonical for credentialed launch.",
        "Credentialed launch arguments cannot contain null.",
        "Credentialed launch arguments cannot contain NUL.",
        "function Test-CredentialCommandLineContract",
        "surrogate pair",
        "non-ASCII whitespace",
        "backslashes before quote",
        "quoted trailing backslash",
        "requires 961 UTF-16 units including NUL; limit is 960.",
        "$acceptedLength -eq 960",
        "$maximumShapeLength -le 960",
        "$maximumArguments = New-ValidClientArguments",
        "Credentialed command-line preflight failure started or adopted a process.",
        "RunEphemeralIdentityProbe",
        "RunEphemeralFullControlProbe",
        "RunEphemeralStalledProbe",
        "$current.User.Value -ceq $ExpectedSid",
        "$expectedRid -ge 1000",
        "Add-LocalGroupMember -SID $standardUsersSid -Member $verified",
        "Assert-True (-not $stalledProcess.HasExited)",
        "Disable-LocalUser -SID $state.Sid",
        "Remove-LocalUser -SID $state.Sid",
        "$script:ownedEphemeralAccount = $null",
        "Assert-NoEphemeralProfileRegistration",
        "foreign-cleanup-marker",
        "expected-post-create-pre-enable-failure",
        "Test-EphemeralProcessOwnershipBoundary",
        "Release-ExitedEphemeralProcess",
        "Refusing to release credentialed process ownership before exit is positively confirmed.",
        "Failed credential-process release discarded exact ownership.",
        "Credentialed process termination remained uncertain after kill.",
        "Refusing account cleanup while its exact credentialed process may still be active.",
    ] {
        assert!(
            transport_test.contains(required),
            "SCM transport harness lost {required:?}"
        );
    }
    for forbidden in [
        "-Sid $identity.User.Value",
        "New-ProtectedPolicy -Sid $runnerSid",
        "Remove-LocalUser -Name",
        "ConvertFrom-SecureString",
        "SecureStringToBSTR",
        "PasswordInClearText",
        "RunImpersonated",
        "ArgumentList.Add($script:ephemeralPassword",
        "Environment['PASSWORD']",
        "Write-Output $script:ephemeralPassword",
        "WriteAllText($script:ephemeralPassword",
        ".Arguments =",
        "UseShellExecute = $true",
        "Start-Process",
        "CreateProcessAsUser",
        "CreateProcessWithToken",
        " -EncodedCommand",
        "cmd.exe /c",
        "Environment['ARGUMENTS']",
        "Environment['ARGS']",
        "Environment['COMMAND_LINE']",
        "ConvertTo-ResponseFile",
        "Write-ResponseFile",
        "Remove-Item -LiteralPath $handoff",
        "Remove-Item -LiteralPath $output",
    ] {
        assert!(
            !transport_test.contains(forbidden),
            "SCM transport harness gained forbidden credential behavior {forbidden:?}"
        );
    }
    let non_elevated_return = transport_test
        .find("if (-not $isElevated)")
        .expect("SCM transport harness keeps its non-elevated return");
    let constructor_normalization_start = transport_test
        .find("function Test-FileSystemAccessRuleConstructorNormalization")
        .expect("SCM transport harness defines its pure FileSystemAccessRule constructor-normalization test");
    let constructor_normalization_call = transport_test
        .rfind("Test-FileSystemAccessRuleConstructorNormalization")
        .expect("SCM transport harness invokes its pure FileSystemAccessRule constructor-normalization test");
    assert!(
        constructor_normalization_start < constructor_normalization_call
            && constructor_normalization_call < non_elevated_return,
        "SCM transport harness must run constructor normalization before its non-elevated return"
    );
    let command_line_contract_start = transport_test
        .find("function Test-CredentialCommandLineContract")
        .expect("SCM transport harness defines its credentialed command-line contract");
    let command_line_contract_call = transport_test
        .rfind("Test-CredentialCommandLineContract")
        .expect("SCM transport harness invokes its credentialed command-line contract");
    assert!(
        command_line_contract_start < command_line_contract_call
            && command_line_contract_call < non_elevated_return,
        "SCM transport harness must run credentialed command-line checks before its non-elevated return"
    );
    let credential_start_begin = transport_test
        .find("function Start-EphemeralProcess")
        .expect("SCM transport harness defines its bounded credentialed process start helper");
    let credential_start_end = transport_test[credential_start_begin..]
        .find("function Test-CredentialCommandLineContract")
        .map(|offset| credential_start_begin + offset)
        .expect("SCM transport harness bounds its credentialed process start helper");
    let credential_start = &transport_test[credential_start_begin..credential_start_end];
    let immutable_arguments = credential_start
        .find("[string[]]$Arguments.Clone()")
        .expect("credentialed launch clones its argument array");
    let length_preflight = credential_start
        .find("CredentialCommandLine]::ValidateLength($FilePath, $immutableArguments)")
        .expect("credentialed launch preflights its immutable arguments");
    let structured_arguments = credential_start
        .find("New-EphemeralProcessStartInfo -FilePath $FilePath -Arguments $immutableArguments")
        .expect(
            "credentialed launch passes the preflighted arguments to ArgumentList construction",
        );
    let process_start = credential_start
        .find("[Diagnostics.Process]::Start($start)")
        .expect("credentialed launch starts through ProcessStartInfo");
    let process_ownership = credential_start
        .find("$script:activeCredentialProcess = $process")
        .expect("credentialed launch establishes exact process ownership");
    assert!(
        immutable_arguments < length_preflight
            && length_preflight < structured_arguments
            && structured_arguments < process_start
            && process_start < process_ownership,
        "credentialed launch must clone, preflight, populate ArgumentList, start, and then establish ownership in order"
    );
    let credential_start_info_begin = transport_test
        .find("function New-EphemeralProcessStartInfo")
        .expect("SCM transport harness defines its credentialed ProcessStartInfo builder");
    let credential_start_info_end = transport_test[credential_start_info_begin..]
        .find("function Start-EphemeralProcess")
        .map(|offset| credential_start_info_begin + offset)
        .expect("SCM transport harness bounds its credentialed ProcessStartInfo builder");
    let credential_start_info =
        &transport_test[credential_start_info_begin..credential_start_info_end];
    assert!(
        credential_start_info
            .contains("foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }"),
        "credentialed ProcessStartInfo must populate ArgumentList from the preflighted immutable argument copy"
    );
    assert_eq!(
        transport_test
            .matches("$candidateToken = New-CryptographicHex -ByteCount 16")
            .count(),
        1,
        "SCM transport harness must generate one 128-bit token per attempted three-path fixture set"
    );
    let no_touch_builder_start = transport_test
        .find("function Get-ValidatedNoTouchPath")
        .expect("SCM transport harness defines its bounded no-touch path validator");
    let no_touch_builder_end = transport_test[no_touch_builder_start..]
        .find("function Assert-NoTouchPathsRemainAbsent")
        .map(|offset| no_touch_builder_start + offset)
        .expect("SCM transport harness bounds its no-touch path validator");
    let no_touch_builder = &transport_test[no_touch_builder_start..no_touch_builder_end];
    assert!(
        !no_touch_builder.contains("New-Item")
            && !no_touch_builder.contains("Remove-Item")
            && !no_touch_builder.contains("Set-Acl"),
        "no-touch path validation must not create, reserve, adopt, delete, or change ACLs on a drive-root path"
    );
    assert!(
        !transport_test.contains("New-Item -ItemType Directory -Path $handoff")
            && !transport_test.contains("New-Item -ItemType Directory -Path $output"),
        "SCM transport harness must not reserve a no-touch client path"
    );
    assert!(
        !transport_test.contains("Set-Acl -LiteralPath $handoff")
            && !transport_test.contains("Set-Acl -LiteralPath $output")
            && !transport_test.contains("Set-Acl -LiteralPath $systemVolumeRoot"),
        "SCM transport harness must not mutate a no-touch path or drive-root ACL"
    );
    let path_set_selector_start = transport_test
        .find("function Select-AvailableFixturePathSet")
        .expect("SCM transport harness defines its bounded three-path collision selector");
    let path_set_selector_end = transport_test[path_set_selector_start..]
        .find("function Test-FixturePathSetAvailabilityContract")
        .map(|offset| path_set_selector_start + offset)
        .expect("SCM transport harness bounds its three-path collision selector");
    let path_set_selector = &transport_test[path_set_selector_start..path_set_selector_end];
    for required in [
        "$maximumAttempts = 8",
        "$attempt -lt $maximumAttempts",
        "$candidateToken = New-CryptographicHex -ByteCount 16",
        "-Token $candidateToken",
        "Test-Path -LiteralPath $candidateMachineTarget",
        "Test-Path -LiteralPath $candidateHandoff",
        "Test-Path -LiteralPath $candidateOutput",
        "$candidateSetAvailable = Test-FixturePathSetAvailable",
        "if (-not $candidateSetAvailable) { continue }",
        "Token = $candidateToken",
        "MachineTarget = $candidateMachineTarget",
        "Handoff = $candidateHandoff",
        "Output = $candidateOutput",
        "after $maximumAttempts attempts.",
    ] {
        assert!(
            path_set_selector.contains(required),
            "SCM transport harness collision selector lost {required:?}"
        );
    }
    assert_eq!(
        path_set_selector.matches("-Token $candidateToken").count(),
        3,
        "every attempted staging/handoff/output path must derive from the same candidate token"
    );
    assert!(
        !path_set_selector.contains("New-Item")
            && !path_set_selector.contains("Remove-Item")
            && !path_set_selector.contains("Set-Acl"),
        "three-path collision selection must not mutate, reserve, adopt, or delete a candidate path"
    );
    let availability_test_start = transport_test
        .find("function Test-FixturePathSetAvailabilityContract")
        .expect("SCM transport harness defines deterministic three-path collision coverage");
    let availability_test_call = transport_test
        .rfind("Test-FixturePathSetAvailabilityContract")
        .expect("SCM transport harness invokes deterministic three-path collision coverage");
    assert!(
        availability_test_start < availability_test_call
            && availability_test_call < non_elevated_return,
        "SCM transport harness must run deterministic collision coverage before its non-elevated return"
    );
    let direct_credential_call = "Invoke-EphemeralProcess -FilePath $clientForCredential";
    assert_eq!(
        transport_test.matches(direct_credential_call).count(),
        2,
        "SCM transport harness must retain exactly two direct authenticated real-client calls"
    );
    let first_credential_call = transport_test
        .find(direct_credential_call)
        .expect("SCM transport harness performs its first authenticated real-client call");
    let second_credential_call = transport_test
        [first_credential_call + direct_credential_call.len()..]
        .find(direct_credential_call)
        .map(|offset| first_credential_call + direct_credential_call.len() + offset)
        .expect("SCM transport harness performs its second authenticated real-client call");
    let no_touch_assertion =
        "Assert-NoTouchPathsRemainAbsent -HandoffRoot $handoff -OutputRoot $output";
    let first_credential_status = transport_test[first_credential_call..]
        .find("Broker service did not remain running after the authenticated round trip.")
        .map(|offset| first_credential_call + offset)
        .expect("first authenticated call retains its service-status postcondition");
    let first_credential_absent = transport_test[first_credential_status..]
        .find(no_touch_assertion)
        .map(|offset| first_credential_status + offset)
        .expect("first authenticated call retains its post-call no-touch assertion");
    let first_credential_stop = transport_test[first_credential_absent..]
        .find("[void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)")
        .map(|offset| first_credential_absent + offset)
        .expect("first authenticated call retains its service-stop boundary");
    assert!(
        first_credential_call < first_credential_status
            && first_credential_status < first_credential_absent
            && first_credential_absent < first_credential_stop
            && first_credential_stop < second_credential_call,
        "first authenticated call must assert no-touch paths after result/status postconditions and before service-stop work"
    );
    assert_eq!(
        transport_test[first_credential_call..second_credential_call]
            .matches(no_touch_assertion)
            .count(),
        1,
        "first authenticated call region must contain exactly one post-call no-touch assertion"
    );
    let second_credential_status = transport_test[second_credential_call..]
        .find("Rejected unmapped-policy restart exposed a broker pipe.")
        .map(|offset| second_credential_call + offset)
        .expect("second credentialed call retains its broker-unavailable postcondition");
    let second_credential_absent = transport_test[second_credential_status..]
        .find(no_touch_assertion)
        .map(|offset| second_credential_status + offset)
        .expect("second authenticated call retains its post-call no-touch assertion");
    let second_credential_stop = transport_test[second_credential_absent..]
        .find("[void](Assert-OwnedBrokerService -ExpectedPath $serviceForScm)")
        .map(|offset| second_credential_absent + offset)
        .expect("second authenticated call retains its service-stop boundary");
    assert!(
        second_credential_call < second_credential_status
            && second_credential_status < second_credential_absent
            && second_credential_absent < second_credential_stop,
        "second authenticated call must assert no-touch paths after result/status postconditions and before service-stop work"
    );
    assert_eq!(
        transport_test[second_credential_call..second_credential_stop]
            .matches(no_touch_assertion)
            .count(),
        1,
        "second authenticated call region must contain exactly one post-call no-touch assertion"
    );
    assert_eq!(
        transport_test.matches(no_touch_assertion).count(),
        4,
        "SCM transport harness must retain initial, two authenticated post-call, and final cleanup no-touch assertions"
    );
    let ephemeral_account_creation = transport_test
        .find("$ephemeralAccount = New-EphemeralStandardAccount")
        .expect("SCM transport harness creates its ephemeral account");
    assert!(
        non_elevated_return < ephemeral_account_creation,
        "SCM transport harness creates persistent account state before its non-elevated return"
    );
    for required in [
        "[string]$AuthorizedClientSid",
        "[string]$InvocationNonce",
        r"SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization",
        "RegistryView]::Registry64",
        "RegCreateKeyExW",
        "CreateProtectedKey",
        "ref SecurityAttributes securityAttributes",
        "SecurityDescriptor = descriptor",
        "O:SYD:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;",
        "OpenExistingKeyNoFollow",
        "REG_OPTION_OPEN_LINK",
        "SymbolicLinkValue",
        "$mutationMask = [uint32]0x500d0026",
        "Refusing to modify a pre-existing Windows GPU broker client policy",
        "RegistryRights]::FullControl",
        "RegistryRights]::ReadKey",
        "ProvisioningState",
        "REG_CREATED_NEW_KEY",
        "created_ancestors = @($createdAncestorPaths)",
        "scribe-windows-gpu-broker-client-policy-provisioning-success-v1",
        "RestorePrivilegeScope : IDisposable",
        "AdjustTokenPrivilegesAndCapturePrevious",
        "RestoreTokenPrivileges",
        "private bool restorationComplete;",
        "public static void RestoreOrFailFast(RestorePrivilegeScope scope)",
        "Environment.FailFast(",
        "Removing the marker is the only commit point",
        "Assert-Policy -Key $key -ExpectProvisioningMarker $false",
        "S-1-5-20",
        "$serviceSid",
    ] {
        assert!(
            client_policy_provisioner.contains(required),
            "broker client-policy provisioner lost {required:?}"
        );
    }
    let creator_owner_predicate_name = "Test-StandardSoftwareCreatorOwnerInheritanceTemplate";
    assert_eq!(
        client_policy_provisioner
            .matches(creator_owner_predicate_name)
            .count(),
        2,
        "broker client-policy provisioner must define and call its CREATOR OWNER predicate exactly once"
    );
    let creator_owner_predicate_start = client_policy_provisioner
        .find("function Test-StandardSoftwareCreatorOwnerInheritanceTemplate(")
        .expect("provisioner defines its exact CREATOR OWNER predicate");
    let raw_acl_classifier_name = "Test-SafePolicyAncestorAcl";
    assert_eq!(
        client_policy_provisioner
            .matches(raw_acl_classifier_name)
            .count(),
        2,
        "broker client-policy provisioner must define and call its raw DACL classifier exactly once"
    );
    let raw_acl_classifier_start = client_policy_provisioner[creator_owner_predicate_start..]
        .find("function Test-SafePolicyAncestorAcl(")
        .map(|offset| creator_owner_predicate_start + offset)
        .expect("raw CREATOR OWNER predicate has a bounded source region");
    let creator_owner_predicate =
        &client_policy_provisioner[creator_owner_predicate_start..raw_acl_classifier_start];
    for required in [
        "$Path -ceq 'SOFTWARE'",
        "$Ace -is [Security.AccessControl.CommonAce]",
        "-not $Ace.IsCallback",
        "$Ace.AceType -eq [Security.AccessControl.AceType]::AccessAllowed",
        "$Ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessAllowed",
        "$Ace.AceFlags -eq [Security.AccessControl.AceFlags]::ContainerInherit",
        "[uint32]$Ace.AccessMask -eq [uint32]0x000f003f",
        "$Ace.SecurityIdentifier.Value -ceq 'S-1-3-0'",
        "$Ace.OpaqueLength -eq 0",
    ] {
        assert!(
            creator_owner_predicate.contains(required),
            "CREATOR OWNER predicate lost exact comparison {required:?}"
        );
    }
    let safe_ancestor_start = client_policy_provisioner[raw_acl_classifier_start..]
        .find("function Assert-SafePolicyAncestor(")
        .map(|offset| raw_acl_classifier_start + offset)
        .expect("raw ancestor-DACL classifier has a bounded source region");
    let raw_acl_classifier =
        &client_policy_provisioner[raw_acl_classifier_start..safe_ancestor_start];
    for required in [
        "$null -eq $Acl",
        "$creatorOwnerTemplateCount -gt 1",
        "Test-StandardSoftwareCreatorOwnerInheritanceTemplate -Ace $ace -Path $Path",
        "$ace -isnot [Security.AccessControl.QualifiedAce]",
        "$ace.AceQualifier -eq [Security.AccessControl.AceQualifier]::AccessDenied",
        "$ace.AceQualifier -ne [Security.AccessControl.AceQualifier]::AccessAllowed",
        "([uint32]$ace.AccessMask -band $MutationMask) -eq 0",
        "$TrustedOwners -cnotcontains $ace.SecurityIdentifier.Value",
    ] {
        assert!(
            raw_acl_classifier.contains(required),
            "raw ancestor-DACL classifier lost guard {required:?}"
        );
    }
    for forbidden in [
        "SetAccessControl",
        "SetAccessRule",
        "AddAccessRule",
        "RemoveAccessRule",
        "SetValue",
        "DeleteValue",
        "CreateSubKey",
        "RegCreateKey",
        "RegDeleteKey",
        "RegSetValue",
        "NtDeleteKey",
    ] {
        assert!(
            !creator_owner_predicate.contains(forbidden),
            "CREATOR OWNER predicate gained forbidden mutation API {forbidden:?}"
        );
        assert!(
            !raw_acl_classifier.contains(forbidden),
            "raw ancestor-DACL classifier gained forbidden mutation API {forbidden:?}"
        );
    }
    let safe_ancestor_end = client_policy_provisioner[safe_ancestor_start..]
        .find("function Assert-Policy(")
        .map(|offset| safe_ancestor_start + offset)
        .expect("ancestor validator has a bounded source region");
    let safe_ancestor = &client_policy_provisioner[safe_ancestor_start..safe_ancestor_end];
    assert!(
        safe_ancestor.contains("[Security.AccessControl.RawSecurityDescriptor]::new($security.GetSecurityDescriptorBinaryForm(), 0)"),
        "ancestor validator no longer obtains the complete raw DACL"
    );
    assert!(
        safe_ancestor.contains("Test-SafePolicyAncestorAcl -Acl $raw.DiscretionaryAcl -Path $Path -TrustedOwners $trustedOwners -MutationMask $mutationMask"),
        "ancestor validator no longer classifies the complete raw DACL at its sole trust decision"
    );
    assert!(
        !safe_ancestor.contains("GetAccessRules("),
        "ancestor validator regressed to projected RegistryAccessRules that can omit raw ACE types"
    );
    let born_protected = client_policy_provisioner
        .rfind("Assert-PolicySecurity -Key $key")
        .expect("provisioner verifies create-time policy security");
    let first_value_write = client_policy_provisioner
        .find("$key.SetValue($provisioningValue")
        .expect("provisioner writes its incomplete marker");
    assert!(
        born_protected < first_value_write,
        "broker policy must be born protected before its first value write"
    );
    let create_method_start = client_policy_provisioner
        .find("public static int CreateProtectedKey(")
        .expect("provisioner defines its native create helper");
    let create_method_end = client_policy_provisioner[create_method_start..]
        .find("public static int OpenExistingKeyNoFollow(")
        .map(|offset| create_method_start + offset)
        .expect("native create helper has a bounded source region");
    assert!(
        client_policy_provisioner[create_method_start..create_method_end]
            .contains("ref securityAttributes"),
        "native policy creation must pass non-null security attributes"
    );
    let ancestor_validation = client_policy_provisioner
        .find("foreach ($ancestorPath in $policyAncestors)")
        .expect("provisioner validates the fixed ancestor chain");
    let leaf_creation = client_policy_provisioner
        .find("$status = [Scribe.GpuBroker.RegistryNative]::CreateProtectedKey(")
        .expect("provisioner creates the authorization leaf");
    assert!(
        ancestor_validation < leaf_creation,
        "broker policy ancestor chain must be secured before leaf creation"
    );
    let privilege_enable = client_policy_provisioner
        .find(
            "$restorePrivilegeScope = [Scribe.GpuBroker.RegistryNative]::EnableRestorePrivilege()",
        )
        .expect("provisioner enables restore privilege through a scoped token");
    let privilege_restore_before_commit = client_policy_provisioner[privilege_enable..]
        .find("$restorePrivilegeScope.Dispose()")
        .map(|offset| privilege_enable + offset)
        .expect("provisioner restores privilege before committing policy");
    let policy_commit = client_policy_provisioner
        .find("$key.DeleteValue($provisioningValue")
        .expect("provisioner removes its incomplete marker at commit");
    let privilege_restore = client_policy_provisioner
        .rfind("[Scribe.GpuBroker.RegistryNative]::RestoreOrFailFast($restorePrivilegeScope)")
        .expect("provisioner retries restoration or terminates at its outer boundary");
    let success_emission = client_policy_provisioner
        .rfind("$successRecord | ConvertTo-Json")
        .expect("provisioner emits its success record");
    assert!(
        privilege_enable < ancestor_validation
            && ancestor_validation < privilege_restore_before_commit
            && privilege_restore_before_commit < policy_commit
            && policy_commit < privilege_restore
            && privilege_restore < success_emission,
        "broker policy provisioning must restore privileges before policy commit and success output"
    );
    let restore_scope_start = client_policy_provisioner
        .find("public sealed class RestorePrivilegeScope : IDisposable")
        .expect("provisioner defines its restoration scope");
    let restore_scope_end = client_policy_provisioner[restore_scope_start..]
        .find("public static void RestoreOrFailFast(RestorePrivilegeScope scope)")
        .map(|offset| restore_scope_start + offset)
        .expect("provisioner bounds its restoration scope implementation");
    let restore_scope = &client_policy_provisioner[restore_scope_start..restore_scope_end];
    let native_restore = restore_scope
        .find("if (!RestoreTokenPrivileges(")
        .expect("restoration scope invokes the native exact-state restore");
    let restored_state = restore_scope
        .find("restorationComplete = true;")
        .expect("restoration scope records exact restoration");
    let close_token = restore_scope
        .find("if (!CloseHandle(token))")
        .expect("restoration scope closes its owned token");
    let release_token = restore_scope
        .find("token = IntPtr.Zero;")
        .expect("restoration scope eventually relinquishes token ownership");
    let release_previous = restore_scope
        .find("previousState = default(TokenPrivileges);")
        .expect("restoration scope eventually clears captured state");
    assert!(
        native_restore < restored_state
            && restored_state < close_token
            && close_token < release_token
            && release_token < release_previous,
        "broker policy provisioning must retain token and prior state until restore and close succeed"
    );
    for forbidden in [
        "PrivilegeRestoreFailureEvidencePath",
        "restoreFailuresRemaining",
    ] {
        assert!(
            !client_policy_provisioner.contains(forbidden),
            "production policy provisioner exposes test failure hook {forbidden:?}"
        );
    }
    for required in [
        "PrivilegeRestoreRetryModel",
        "marker=incomplete;restore_attempts=3;token_owned=true;previous_state=37",
        "Object.ReferenceEquals(provisioningOriginal, provisioningPropagated)",
        "Console.Out.Flush();",
        "-MaximumCapturedOutputCharacters 16384",
    ] {
        assert!(
            transport_test.contains(required),
            "SCM harness lost privilege retry proof {required:?}"
        );
    }
    assert!(
        !transport_test.contains("PrivilegeRestoreFailureEvidencePath"),
        "SCM harness persistent-failure fixture exposes an arbitrary evidence path"
    );
    assert!(
        !transport_test.contains(
            "if (Test-Path -LiteralPath $policyRegistryPath) { $script:createdPolicy = $true }"
        ),
        "SCM harness must not infer destructive policy ownership from path existence"
    );
    assert!(
        !transport_test.contains("initiallyMissingPolicyAncestors"),
        "SCM harness must not infer ancestor ownership from a missing-path snapshot"
    );
    assert!(
        !transport_test.contains(".DeleteSubKey("),
        "SCM harness cleanup must not use path-based registry deletion"
    );
    let bound_cleanup_start = transport_test
        .find("function Remove-RegistryKeyByValidatedHandle")
        .expect("SCM harness defines exact-handle registry cleanup");
    let bound_cleanup_end = transport_test[bound_cleanup_start..]
        .find("function Remove-OwnedPolicy")
        .map(|offset| bound_cleanup_start + offset)
        .expect("SCM harness exact-handle cleanup has a bounded source region");
    let bound_cleanup = &transport_test[bound_cleanup_start..bound_cleanup_end];
    let bound_validation = bound_cleanup
        .find("& $Validate $key")
        .expect("SCM harness validates the opened registry object");
    let boundary_hook = bound_cleanup
        .find("& $BeforeDelete")
        .expect("SCM harness supports a deterministic pre-delete boundary test");
    let bound_deletion = bound_cleanup
        .find("DeleteExactKey($handle)")
        .expect("SCM harness deletes through the opened registry handle");
    assert!(
        bound_validation < boundary_hook && boundary_hook < bound_deletion,
        "SCM harness must validate, exercise the boundary, and delete through the same live handle"
    );
    let remove_policy_start = transport_test
        .find("function Remove-OwnedPolicy(")
        .expect("SCM harness defines policy cleanup");
    let remove_policy_end = transport_test[remove_policy_start..]
        .find("function Remove-OwnedPolicyAncestors")
        .map(|offset| remove_policy_start + offset)
        .expect("SCM harness policy cleanup has a bounded source region");
    let remove_policy = &transport_test[remove_policy_start..remove_policy_end];
    let exact_delete_call = remove_policy
        .find("Remove-RegistryKeyByValidatedHandle")
        .expect("SCM harness invokes exact-handle policy deletion");
    let drop_ownership = remove_policy
        .find("$script:ownedPolicyState = $null")
        .expect("SCM harness drops deleted policy ownership");
    let post_delete_observation = remove_policy
        .find("Test-Path -LiteralPath $policyRegistryPath")
        .expect("SCM harness observes the fixed path after deletion");
    assert!(
        exact_delete_call < drop_ownership && drop_ownership < post_delete_observation,
        "SCM harness must drop ownership before observing a post-delete replacement"
    );
    for forbidden in [
        "[string]$AccountName",
        "LookupAccountName",
        "Convert-NameToSid",
        "HKEY_CURRENT_USER",
    ] {
        assert!(
            !client_policy_provisioner.contains(forbidden),
            "broker client-policy provisioner gained override or account-name input {forbidden:?}"
        );
    }
    assert!(
        !client_policy_provisioner
            .to_ascii_lowercase()
            .contains("env:"),
        "broker client-policy provisioner gained a case-insensitive environment override"
    );
    for required in [
        "RECEIPT_DOMAIN",
        "LEDGER_DOMAIN",
        "scribe-windows-gpu-promotion-receipt-v2",
        "scribe-windows-gpu-promotion-ledger-record-v2",
        "promotion_intent_sha256",
        "LedgerKind::Reserved",
        "LedgerKind::Ready",
        "LedgerKind::Published",
        "MoveFileExW",
        "FILE_FLAG_OPEN_REPARSE_POINT",
        "FILE_SHARE_READ",
        "reject_named_streams",
        "reject_hardlink",
        "copy_retained_file",
        "signature_envelope_sha256",
        "pack_version: pack.manifest.pack_version.clone()",
        "expected_receipt_statement",
        "bounded_directory_names",
        "concurrent_duplicate_requests_have_one_winner",
        "fault_after_first_pack_never_publishes_a_partial_pair_and_burns_replay",
        "recovery_rejects_a_valid_but_cross_release_output_substitution",
        "mismatched_handoff_and_intent_fail_before_reservation",
        "self_consistent_but_unauthorized_intent_fails_before_reservation",
        "post_reservation_failure_burns_replay_and_advances_epoch_high_water",
        "receipt_ledger_and_publication_names_are_path_free_and_intent_bound",
        "receipt_rejects_a_valid_signature_over_an_incorrect_intent_digest",
        "genuine_legacy_v1_flattened_receipt_is_rejected_by_v2_verification",
        "genuine_legacy_v1_named_path_ledger_is_rejected_by_v2_loader",
        "consumes_canonical_handoff_generated_by_powershell_and_worker_pack_author",
    ] {
        assert!(
            broker_fixture.contains(required),
            "test-only privileged broker proof lost {required:?}"
        );
    }
    for required in [
        "InteropFixtureDirectory",
        "promotion-intent.json",
        "policy_namespace = 'scribe-windows-gpu-production-v1'",
        "workflow_source_sha = $revision",
    ] {
        assert!(
            contract_test.contains(required),
            "PowerShell interoperability producer lost {required:?}"
        );
    }
    assert!(workflow.contains("provide no-follow open semantics or pin path ancestors"));
    assert!(contract_test.contains("Windows GPU pack promotion contract tests passed."));
}

#[test]
fn worker_roles_use_private_pipes_and_protocol_only_stdout() {
    let sources = rust_sources();
    let identity = include_str!("worker_identity.rs");
    let worker = sources
        .iter()
        .find(|(path, _)| path == Path::new("onnx_worker.rs"))
        .map(|(_, source)| production_source(source))
        .expect("worker entrypoint exists");

    assert!(worker.contains("PROTOCOL_MAGIC: [u8; 4] = *b\"SCIF\""));
    assert!(worker.contains("crate::worker_identity"));
    assert!(identity.contains("PROTOCOL_VERSION: u8 = 5"));
    for bound_capability in [
        "challenge",
        "app_build",
        "worker_build",
        "bundled_worker_sha256",
        "abi",
        "role",
        "provider",
        "artifacts",
    ] {
        assert!(
            worker.contains(bound_capability),
            "SCIF v5 capability must bind {bound_capability}"
        );
    }
    assert!(worker.contains("Stdio::piped()"));
    assert!(worker.contains("std::io::stdout().lock()"));
    assert!(worker.contains("stderr(Stdio::inherit())"));
    for launch_hardening in [
        "configure_worker_environment(&mut command)",
        "command.env_clear()",
        "harden_windows_dll_search",
        "SetDefaultDllDirectories",
        "FILE_FLAG_OPEN_REPARSE_POINT",
        "worker executable must not be a hardlink",
        "executable.revalidate()",
    ] {
        assert!(
            worker.contains(launch_hardening),
            "worker launch hardening must retain {launch_hardening}"
        );
    }
    assert!(
        !worker
            .lines()
            .any(|line| line.trim_start().starts_with("print!("))
    );
    assert!(
        !worker
            .lines()
            .any(|line| line.trim_start().starts_with("println!("))
    );

    for forbidden in [
        "TcpListener",
        "TcpStream",
        "UdpSocket",
        "localhost",
        "127.0.0.1",
        "http://",
        "https://",
        "reqwest",
        "ureq",
    ] {
        assert!(
            !worker.contains(forbidden),
            "worker transport must remain private pipe-based; found {forbidden:?}"
        );
    }

    assert!(
        worker.contains("WorkerRole::Inference => INFERENCE_WORKER_FLAG")
            && worker.contains("WorkerRole::Vad => VAD_WORKER_FLAG"),
        "STT and VAD must launch as distinct worker roles"
    );
}

#[test]
fn retired_download_helpers_and_private_descriptor_fields_stay_removed() {
    let sources = rust_sources();
    let downloads = sources
        .iter()
        .find(|(path, _)| path == Path::new("managed_downloads.rs"))
        .map(|(_, source)| production_source(source))
        .expect("managed download source exists");
    for retired in [
        "download_faster_whisper_model",
        "download_vosk_model",
        "download_sherpa_model",
        "download_runner_model",
    ] {
        assert!(
            !downloads.contains(retired),
            "retired download helper {retired:?} was restored"
        );
    }

    let catalog = sources
        .iter()
        .find(|(path, _)| path == Path::new("model_catalog.rs"))
        .map(|(_, source)| production_source(source))
        .expect("model catalog source exists");
    let declaration = catalog
        .find("pub struct ModelDescriptor")
        .expect("ModelDescriptor declaration exists");
    let open = catalog[declaration..]
        .find('{')
        .map(|offset| declaration + offset)
        .expect("ModelDescriptor body opens");
    let mask = rust_code_mask(&catalog);
    let mut depth = 1_i32;
    let mut close = open + 1;
    while close < catalog.len() && depth > 0 {
        if mask[close] {
            match catalog.as_bytes()[close] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
        close += 1;
    }
    assert_eq!(depth, 0, "ModelDescriptor body closes");
    let body = &catalog[open + 1..close - 1];
    for private in [
        "backend",
        "runtime",
        "architecture",
        "artifact",
        "revision",
        "sha256",
        "filename",
    ] {
        assert!(
            !body.lines().any(|line| {
                line.trim_start()
                    .strip_prefix("pub ")
                    .is_some_and(|field| field.starts_with(&format!("{private}:")))
            }),
            "ModelDescriptor leaks private field {private:?}"
        );
    }
}

#[test]
fn retired_python_provider_stack_stays_absent_without_crossing_native_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for retired_path in [
        "src/stt/faster_whisper.rs",
        "src/stt/vosk.rs",
        "src/stt/sherpa_onnx.rs",
        "scripts/faster_whisper_runner.py",
        "scripts/vosk_runner.py",
        "scripts/sherpa_onnx_runner.py",
        "scripts/bundle-faster-whisper-runtime.sh",
        "scripts/bundle-vosk-runtime.sh",
        "scripts/bundle-sherpa-onnx-runtime.sh",
        "scripts/bundle-moonshine-runtime.sh",
        "scripts/bundle-parakeet-runtime.sh",
    ] {
        assert!(
            !root.join(retired_path).exists(),
            "retired provider file was restored: {retired_path}"
        );
    }

    let retired_ids_and_aliases = [
        "vosk_small_en",
        "faster_whisper_tiny_en",
        "faster_whisper_base_en",
        "faster_whisper_small_en_gpu",
        "faster_whisper_medium_en_gpu",
        "faster_whisper_large_v3",
        "faster_whisper_turbo",
        "faster_whisper_distil_large_v3",
        "sherpa_onnx_zipformer_small",
        "parakeet_0_6b",
        "faster_whisper",
        "faster_whisper_small_en",
        "faster_whisper_medium_en",
        "sherpa_onnx_streaming",
    ];
    let retired_runner_invocations = [
        "faster_whisper_runner.py",
        "vosk_runner.py",
        "sherpa_onnx_runner.py",
        "scribe-faster-whisper",
        "scribe-vosk",
        "scribe-sherpa-onnx",
        "SCRIBE_FAST_WHISPER_RUNTIME_DEST",
        "SCRIBE_VOSK_RUNTIME_DEST",
        "SCRIBE_SHERPA_ONNX_RUNTIME_DEST",
    ];
    for (path, source) in rust_sources() {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source_for(&path, &source);
        for retired in retired_ids_and_aliases {
            assert!(
                !production.contains(&format!("\"{retired}\"")),
                "retired provider ID/alias {retired:?} remains recognized by {}",
                path.display()
            );
        }
        for invocation in retired_runner_invocations {
            assert!(
                !production.contains(invocation),
                "retired runner invocation {invocation:?} remains in {}",
                path.display()
            );
        }
        if ![
            Path::new("model_catalog.rs"),
            Path::new("onnx_model_bundles.rs"),
            Path::new("onnx_worker.rs"),
            Path::new("runtime_artifact.rs"),
            Path::new("runtime_router.rs"),
        ]
        .contains(&path.as_path())
        {
            assert!(
                !production.contains("\"moonshine\""),
                "retired bare Moonshine provider ID escaped the native allowlist into {}",
                path.display()
            );
        }
    }

    let scripts = fs::read_dir(root.join("scripts")).expect("scripts directory must be readable");
    for entry in scripts {
        let path = entry.expect("script entry must be readable").path();
        if !path.is_file() {
            continue;
        }
        let source = fs::read_to_string(&path).expect("maintainer scripts must be UTF-8");
        for invocation in retired_runner_invocations {
            assert!(
                !source.contains(invocation),
                "retired runner invocation {invocation:?} remains in {}",
                path.display()
            );
        }
    }
    for removed_runtime_maintenance_path in [
        "scripts/runtime-dependencies.env",
        "scripts/check-runtime-dependency-updates.py",
    ] {
        assert!(
            !root.join(removed_runtime_maintenance_path).exists(),
            "retired dynamic runtime maintenance tool was restored: {removed_runtime_maintenance_path}"
        );
    }

    let stt = fs::read_to_string(root.join("src/stt/mod.rs"))
        .expect("STT cancellation module must be readable");
    let direct_dispatch = production_source(&stt);
    assert!(
        !direct_dispatch.contains("provider_for_backend"),
        "STT cancellation boundary must not perform provider lookup"
    );
    assert!(!direct_dispatch.contains("Command::new"));

    for retained_path in [
        "vendor/sherpa-onnx-sys/LICENSE",
        "native/sherpa-onnx-v1.13.5/PROVENANCE.md",
        "native/transcribe-cpp-v0.1.3/LICENSE",
        "native/transcribe-cpp-v0.1.3/PROVENANCE.md",
        "native/whisper-f049fff/LICENSE",
        "native/whisper-f049fff/PROVENANCE.md",
        "resources/licenses/Moonshine-MIT.txt",
    ] {
        assert!(
            root.join(retained_path).is_file(),
            "native transcription/Sherpa/Moonshine evidence was removed: {retained_path}"
        );
    }
}

#[test]
fn private_onnx_runtime_contract_does_not_leak_into_product_surfaces() {
    let sources = rust_sources();
    let protected = [
        Path::new("app.rs"),
        Path::new("config.rs"),
        Path::new("model_catalog.rs"),
    ];
    let forbidden = [
        "OnnxModelSpec",
        "OnnxModelFamily",
        "OnnxFileRole",
        "OnnxSpeech",
        "native-onnx",
    ];

    for (path, source) in &sources {
        if !protected.iter().any(|protected| path == protected) && !path.starts_with("ui") {
            continue;
        }
        let production = production_source(source).replace("moonshine-tiny-en-int8-onnx", "");
        for identifier in forbidden {
            assert!(
                !production.contains(identifier),
                "private ONNX runtime identifier {identifier:?} leaked into {}",
                path.display()
            );
        }
    }
}

#[test]
fn application_and_ui_sources_are_runtime_neutral() {
    let sources = rust_sources();
    let family_terms = [
        "whisper.cpp",
        "faster-whisper",
        "vosk",
        "sherpa",
        "zipformer",
        "moonshine",
        "parakeet",
        "qwen",
        "voxtral",
        "nemotron",
        "sensevoice",
        "canary",
    ];
    let semantic_escapes = [
        "use crate::stt",
        "runtime_catalog::",
        "provider_for_backend",
        ".backend",
        "RuntimeRouter",
        "transcribe_with_config",
        "stt::whisper_cpp",
        "stt::faster_whisper",
        "stt::vosk",
        "stt::sherpa_onnx",
    ];

    for (path, source) in &sources {
        let is_application = path == Path::new("app.rs");
        let is_ui = path.starts_with("ui") && path != Path::new("ui/harness.rs");
        if !is_application && !is_ui {
            continue;
        }
        let production = production_source(source).replace("moonshine-tiny-en-int8-onnx", "");
        let lowered = production.to_ascii_lowercase();
        for term in family_terms {
            assert!(
                !lowered.contains(term),
                "application/UI source {} contains model-family term {term:?}",
                path.display()
            );
        }
        for escape in semantic_escapes {
            assert!(
                !production.contains(escape),
                "application/UI source {} bypasses TranscriptionService via {escape:?}",
                path.display()
            );
        }
    }
}

#[test]
fn model_family_logic_is_confined_to_private_adapters_and_catalog_validation() {
    let sources = rust_sources();
    let allowed_files = [
        "config.rs",
        "installations.rs",
        "managed_downloads.rs",
        "model_catalog.rs",
        "models.rs",
        "onnx_model_bundles.rs",
        "runtime_artifact.rs",
        "runtime_router.rs",
        "settings/schema.rs",
        "silero_vad_native.rs",
        "transcription.rs",
        "ui/harness.rs",
    ];
    let family_terms = [
        "whisper.cpp",
        "faster-whisper",
        "vosk",
        "sherpa",
        "zipformer",
        "moonshine",
        "parakeet",
        "qwen",
        "voxtral",
        "nemotron",
        "sensevoice",
        "canary",
    ];

    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs")
            || path.starts_with("stt")
            || is_native_runtime_owner(path)
            || allowed_files
                .iter()
                .any(|allowed| path == Path::new(allowed))
        {
            continue;
        }
        let production = production_source(source)
            .replace("moonshine-tiny-en-int8-onnx", "")
            .to_ascii_lowercase();
        for term in family_terms {
            assert!(
                !production.contains(term),
                "model-family logic {term:?} escaped private adapters/catalog validation into {}",
                path.display()
            );
        }
    }
}

#[test]
fn onnx_bundle_http_and_typed_receipts_stay_below_the_service_boundary() {
    let sources = rust_sources();
    let bundles = sources
        .iter()
        .find(|(path, _)| path == Path::new("onnx_model_bundles.rs"))
        .map(|(_, source)| production_source(source))
        .expect("private ONNX bundle module exists");
    assert_eq!(
        bundles
            .matches("download_pinned_artifact_for_target(")
            .count(),
        1,
        "only the explicit bundle installation path may invoke the HTTP downloader"
    );
    assert!(bundles.contains("fn stage_onnx_bundle_install("));
    assert!(bundles.contains("fn verified_receipt_at("));
    assert!(bundles.contains("OnnxModelSpec"));

    for protected in [Path::new("model_catalog.rs"), Path::new("models.rs")] {
        let production = sources
            .iter()
            .find(|(path, _)| path == protected)
            .map(|(_, source)| production_source(source))
            .expect("protected source exists");
        for forbidden in [
            "onnx_model_bundles",
            "OnnxBundleReceipt",
            "OnnxBundleManifest",
            "stage_onnx_bundle_install",
        ] {
            assert!(
                !production.contains(forbidden),
                "private ONNX bundle contract {forbidden:?} leaked into {}",
                protected.display()
            );
        }
    }
}

#[test]
fn tentative_transcripts_have_no_output_or_history_module_path() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let protected = [
        source_root.join("text_output.rs"),
        source_root.join("history").join("mod.rs"),
        source_root.join("history").join("database.rs"),
    ];

    for path in protected {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        let production = production_source(&source).to_ascii_lowercase();
        assert!(
            !production.contains("tentative"),
            "{} must only receive finalized text; tentative text belongs in the overlay",
            path.display()
        );
    }
}

#[test]
fn route_shell_has_no_synthetic_models_scroll_surface() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let screens = fs::read_to_string(source_root.join("ui").join("screens.rs"))
        .expect("shared screens source must be readable");
    let app = fs::read_to_string(source_root.join("app.rs"))
        .expect("application source must be readable");
    let harness = fs::read_to_string(source_root.join("ui").join("harness.rs"))
        .expect("harness source must be readable");

    for forbidden in [
        "models_footer_spacer",
        "MODEL_COMPARISON_BODY_BLEED",
        "comparison_top + 10_000.0",
        "comparison_top + 10000.0",
    ] {
        assert!(
            !screens.contains(forbidden),
            "shared models UI must not recreate synthetic scroll artifact {forbidden}"
        );
    }
    assert!(screens.contains("fn show_route_scroll"));
    assert!(screens.contains("const ROUTE_TOP_INSET: f32 = 28.0"));
    assert!(screens.contains("const ROUTE_HORIZONTAL_INSET: f32 = 28.0"));
    assert!(screens.contains("let route_width = ui.available_width()"));
    assert!(screens.contains("ui.set_width(route_width)"));
    assert!(
        screens.contains("ui.set_width((route_width - ROUTE_HORIZONTAL_INSET * 2.0).max(0.0))")
    );
    assert!(screens.contains("comparison_viewport.width() - ROUTE_HORIZONTAL_INSET * 2.0"));
    assert!(app.contains("show_route_scroll(ui, UiRoute::Models"));
    assert!(app.contains("show_route_scroll(ui, UiRoute::History"));
    assert!(app.contains("SettingsTab::About"));
    assert!(
        !app.contains("sync_passive_microphone_monitor"),
        "route rendering must not acquire the microphone while idle"
    );
    assert!(
        !app.contains("page-scroll"),
        "legacy pages must not reintroduce an inner route scroll area"
    );
    assert!(harness.contains("show_route_scroll(ui, view.route"));
}

#[test]
fn no_web_runtime_or_ui_pcm_transport_is_present() {
    let sources = rust_sources();
    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source(source).to_ascii_lowercase();
        for forbidden in ["tauri::", "webview", "ipc::", "javascript"] {
            assert!(
                !production.contains(forbidden),
                "{} introduces forbidden web/UI transport {forbidden:?}",
                path.display()
            );
        }
    }

    for (path, source) in &sources {
        if !path.starts_with("ui") {
            continue;
        }
        let production = production_source(source);
        for pcm_shape in ["Vec<f32>", "&[f32]", "PreparedAudio"] {
            assert!(
                !production.contains(pcm_shape),
                "UI module {} receives native PCM via {pcm_shape}",
                path.display()
            );
        }
    }
}

#[test]
fn production_native_path_does_not_force_harness_light_visuals() {
    let sources = rust_sources();
    let main = sources
        .iter()
        .find(|(path, _)| path == Path::new("main.rs"))
        .map(|(_, source)| source)
        .expect("main source exists");
    let app = sources
        .iter()
        .find(|(path, _)| path == Path::new("app.rs"))
        .map(|(_, source)| source)
        .expect("app source exists");

    assert!(
        main.contains("follow_system_theme: true"),
        "native options must continue following the system theme"
    );
    assert!(
        main.contains("Box::new(app::LocalTranscriberApp::new(cc))"),
        "normal startup must retain the production app path"
    );
    assert!(
        !main.contains("set_visuals(egui::Visuals::light())"),
        "native options/startup must not force light visuals"
    );
    assert!(
        app.contains("cc.egui_ctx.set_visuals(stitch_visuals(resolve_theme_mode("),
        "production app startup must keep its theme selection path"
    );
    assert!(
        app.contains("frame.info().system_theme"),
        "production app updates must retain system-theme resolution"
    );
    assert!(
        !app.contains("configure_harness_style"),
        "harness-only theme initialization must not leak into production"
    );
}

#[test]
fn windows_release_bundles_the_exact_offline_base_model_with_attribution() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = repository
        .join("runtime-manifests")
        .join("whisper-base-en-q8_0-windows-x64.json");
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&manifest_path).expect("bundled model manifest must be readable"),
    )
    .expect("bundled model manifest must be valid JSON");
    let catalog = crate::model_catalog::runtime_model_manifest(
        &crate::transcription::ModelId::new(crate::model_catalog::BUNDLED_BASE_MODEL_ID),
    )
    .expect("bundled base model remains in the normalized catalog");

    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["model_id"], catalog.id);
    assert_eq!(manifest["repository"], catalog.artifact_repository);
    assert_eq!(manifest["revision"], catalog.artifact_revision);
    assert_eq!(manifest["artifact_filename"], catalog.artifact_filename);
    assert_eq!(manifest["size_bytes"], catalog.artifact_size_bytes);
    assert_eq!(manifest["sha256"], catalog.artifact_sha256);
    assert_eq!(manifest["platform_triple"], "x86_64-pc-windows-msvc");
    assert_eq!(
        manifest["attribution_files"],
        serde_json::json!([
            "resources/licenses/Apache-2.0.txt",
            "resources/licenses/OpenAI-Whisper-MIT.txt",
            "resources/licenses/Whisper-Base-En-NOTICE.txt"
        ])
    );

    let bundler = fs::read_to_string(repository.join("scripts").join("bundle-base-model.ps1"))
        .expect("bundled model packaging script must be readable");
    for required in [
        "Get-FileHash",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "--scribe-install-smoke-parent",
        "HF_HUB_OFFLINE",
        "TRANSFORMERS_OFFLINE",
        "cancellation_verified",
        "capabilities.cancellation",
        "exact executable name local-transcriber.exe",
        "canonical executable parent must equal",
        "Assert-NoReparseAncestors",
        "Assert-TreeHasNoReparsePoints",
    ] {
        assert!(
            bundler.contains(required),
            "bundled model packaging must retain {required}"
        );
    }
    for forbidden in ["Invoke-WebRequest", "Start-BitsTransfer", "curl.exe"] {
        assert!(
            !bundler.contains(forbidden),
            "release packaging must not download the bundled model via {forbidden}"
        );
    }
    assert!(
        !bundler.contains("\"-\","),
        "bundled model smoke must use the current self-contained helper protocol"
    );

    let release = fs::read_to_string(repository.join("scripts").join("build-windows-release.ps1"))
        .expect("Windows release script must be readable");
    let frozen_cpu_worker = fs::read_to_string(
        repository
            .join("scripts")
            .join("new-windows-frozen-cpu-worker.ps1"),
    )
    .expect("frozen CPU worker script must be readable");
    let cpu_worker_baseline = fs::read_to_string(
        repository
            .join("scripts")
            .join("windows-cpu-worker-native-baseline.ps1"),
    )
    .expect("CPU worker native baseline script must be readable");
    for required in [
        "cargo build --locked --offline --release --bin local-transcriber --features ($desktopFeatures -join ',') --target $targetTriple",
        "$desktopFeatures = @('ui-harness')",
        "if ($LocalFrozenGpuObservation) {",
        "$desktopFeatures += 'windows-gpu-capture-observation'",
        "if ($LocalFrozenGpuObservation -and -not $frozenCpuWorkerRequested)",
        "cargo build --locked --offline --release --bin scribe-inference-worker --features inference-worker --target $targetTriple",
        "Open-WindowsFrozenCpuWorkerReadHandle $sourceInferenceWorker",
        "Assert-WindowsLongPathAwareApplicationManifest $sourceInferenceWorker",
        "Get-WindowsFrozenCpuWorkerOpenStreamSha256 $freshCpuWorkerStream",
        "SCRIBE_BUNDLED_WORKER_SHA256",
        "SCRIBE_BUILDING_WORKER",
        "scribe-inference-worker.exe",
        "x86_64-pc-windows-msvc",
        "CARGO_TARGET_DIR",
        "[System.IO.Path]::IsPathFullyQualified($env:CARGO_TARGET_DIR)",
        "Join-Path $repositoryRoot $env:CARGO_TARGET_DIR",
        r#"$cargoTargetRoot "$targetTriple\release""#,
        "Assert-Amd64Pe",
        "0x8664",
        "Assert-SafeStagingPath",
        "Remove-ValidatedStaging",
        "Assert-ExactAllowlist",
        "Assert-AllowedPayloadFile",
        "bundle-inventory.json",
        "README.txt",
        "Assert-WindowsGuiSubsystem",
        "Assert-ReviewedWindowsPe",
        "Windows GUI (2)",
        "Windows console PE",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "Move-Item -LiteralPath $stagingBundle -Destination $finalBundle",
        r#"artifacts\Scribe-windows-x64"#,
        "Final release bundle already exists",
        "A stale release staging sibling exists",
        "licenses/THIRD-PARTY-NOTICES.txt",
        "licenses/transcribe.cpp-MIT.txt",
        "licenses/transcribe.cpp-PROVENANCE.md",
        "licenses/whisper.cpp-MIT.txt",
        "licenses/whisper.cpp-PROVENANCE.md",
        "licenses/sherpa-onnx-PROVENANCE.md",
        "licenses/Silero-VAD-MIT.txt",
        "licenses/Silero-VAD-PROVENANCE.md",
        "This release workflow does not claim Authenticode signing",
        "setup refuses safely and does not delete or change that content",
        "Do not delete per-user app data or external/imported models as part of rollback",
        "Assert-ReleaseSmokeDiagnostics",
        r#"detected architecture 'whisper'"#,
    ] {
        assert!(
            release.contains(required),
            "Windows release packaging must retain {required}"
        );
    }
    for forbidden in ["--all-features", "vulkan-acceleration"] {
        assert!(
            !release.contains(forbidden),
            "Windows release packaging must not enable {forbidden}"
        );
    }
    for source in [&release, &frozen_cpu_worker] {
        for required in [
            "windows-cpu-worker-native-baseline.ps1",
            "New-WindowsCpuWorkerBaselineBuild $targetTriple",
            "Assert-WindowsCpuWorkerBaselineEvidence $cpuWorkerBaseline",
            "Restore-WindowsCpuWorkerBaselineEnvironment $cpuWorkerBaseline",
            "CPU worker native baseline evidence retained:",
        ] {
            assert!(
                source.contains(required),
                "Windows CPU worker builders must retain {required:?}"
            );
        }
    }
    for required in [
        "TRANSCRIBE_X86_CONSERVATIVE=ON",
        "GGML_NATIVE=OFF",
        "TRANSCRIBE_GGML_BACKEND_DL=OFF",
        "GGML_OPENMP=OFF",
        "target-feature=+crt-static",
        "CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded",
        "Fresh CPU worker baseline target unexpectedly already exists",
        "CMAKE_ARGS",
        "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS",
        "CPU worker baseline requires exactly one transcribe-cpp-sys Cargo build directory",
        "CPU worker baseline does not recognize the generated CMake generator",
        "CPU worker baseline generated ggml-cpu C/C++ flags contain a native or higher ISA requirement",
        "CPU worker baseline Visual Studio ggml-cpu flags contain an unrecognized or higher ISA requirement",
        "GGML_AVX512_BF16",
        "/(?:arch)",
        "(?:HOST|TARGET)_",
        "AVX[A-Z0-9_]*",
        "/FI",
        "Assert-WindowsCpuWorkerBaselineNativeBuildEvidence",
    ] {
        assert!(
            cpu_worker_baseline.contains(required),
            "Windows CPU worker native baseline lost {required:?}"
        );
    }
    let frozen_integrity = fs::read_to_string(
        repository
            .join("scripts")
            .join("windows-frozen-cpu-worker-integrity.ps1"),
    )
    .expect("frozen CPU worker integrity helper must be readable");
    for required in [
        "scripts/windows-cpu-worker-native-baseline.ps1",
        "scripts/new-windows-frozen-cpu-worker.ps1",
        "scripts/build-windows-release.ps1",
        ".cargo/config.toml",
        "windows-frozen-cpu-worker-contract-v1",
    ] {
        assert!(
            frozen_integrity.contains(required),
            "frozen CPU worker build contract must bind {required:?}"
        );
    }

    let vulkan_developer_build = fs::read_to_string(
        repository
            .join("scripts")
            .join("build-vulkan-worker-dev.ps1"),
    )
    .expect("Vulkan developer worker build script must be readable");
    for required in [
        "--bin scribe-inference-worker --features vulkan-acceleration",
        "--bin local-transcriber --features ui-harness,vulkan-acceleration",
        "$env:SCRIBE_BUILDING_WORKER = '1'",
        "$env:SCRIBE_BUNDLED_WORKER_SHA256 = $null",
        "vulkan-dev-bundle-",
        "Copy-Item -LiteralPath $cargoDesktop -Destination $desktop",
        "Copy-Item -LiteralPath $cargoWorker -Destination $worker",
    ] {
        assert!(
            vulkan_developer_build.contains(required),
            "Vulkan developer build must retain {required:?}"
        );
    }
    assert!(
        !vulkan_developer_build.contains("--release"),
        "the opt-in Vulkan helper must not claim or create a release build"
    );
    assert!(
        !release.contains(r#"target\release"#),
        "the unqualified Cargo release directory must not be used as a bundle"
    );

    let worker_build = release
        .find("$env:SCRIBE_BUILDING_WORKER = '1'")
        .expect("release build marks the worker-only compilation");
    let worker_cargo = release
        .find("cargo build --locked --offline --release --bin scribe-inference-worker")
        .expect("release build compiles the worker first");
    let worker_marker_clear = release[worker_cargo..]
        .find("$env:SCRIBE_BUILDING_WORKER = $null")
        .map(|offset| worker_cargo + offset)
        .expect("release build clears the worker marker before desktop compilation");
    let worker_lease = release
        .find("Open-WindowsFrozenCpuWorkerReadHandle $sourceInferenceWorker")
        .expect("release build holds the completed worker image");
    let worker_manifest = release
        .find("Assert-WindowsLongPathAwareApplicationManifest $sourceInferenceWorker")
        .expect("release build checks the held worker manifest");
    let worker_hash = release
        .find("Get-WindowsFrozenCpuWorkerOpenStreamSha256 $freshCpuWorkerStream")
        .expect("release build hashes the held worker image");
    let desktop_cargo = release
        .find("cargo build --locked --offline --release --bin local-transcriber")
        .expect("release build compiles the anchored desktop second");
    assert!(
        worker_build < worker_cargo
            && worker_cargo < worker_lease
            && worker_lease < worker_manifest
            && worker_manifest < worker_marker_clear
            && worker_marker_clear < worker_hash
            && worker_hash < desktop_cargo,
        "release builds must mark/build the worker, hold/check its manifest, clear the marker, hash the held image, then build the desktop"
    );

    let build_script = fs::read_to_string(repository.join("build.rs"))
        .expect("Cargo build script must be readable");
    for required in [
        "SCRIBE_BUILDING_WORKER",
        "release desktop build requires SCRIBE_BUNDLED_WORKER_SHA256",
        "release worker build must clear SCRIBE_BUNDLED_WORKER_SHA256",
        "cargo:rustc-env=SCRIBE_BUNDLED_WORKER_SHA256={digest}",
    ] {
        assert!(
            build_script.contains(required),
            "release trust-anchor build script must retain {required:?}"
        );
    }

    let release_inputs = fs::read_to_string(
        repository
            .join("scripts")
            .join("prepare-windows-release-inputs.ps1"),
    )
    .expect("release input preparation script must be readable");
    for required in [
        "whisper-base-en-q8_0-windows-x64.json",
        "Get-FileHash",
        "huggingface.co/$modelRepository/resolve/$modelRevision/$modelFilename",
        "Release input SHA-256 mismatch",
    ] {
        assert!(
            release_inputs.contains(required),
            "release input preparation must retain {required}"
        );
    }
    for forbidden in ["RuntimeSource", "runtime-manifest.json", "Expand-Archive"] {
        assert!(
            !release_inputs.contains(forbidden),
            "release input preparation must not retain dynamic runtime contract {forbidden}"
        );
    }

    for removed_runtime_path in [
        "runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json",
        "scripts/build-release-bundle.sh",
        "scripts/build-whisper-cuda.sh",
        "scripts/build-whisper-ollama-cuda-backend.sh",
        "scripts/bundle-whisper-runtime.ps1",
        "scripts/bundle-whisper-runtime.sh",
        "scripts/check-runtime-dependency-updates.py",
        "scripts/runtime-dependencies.env",
    ] {
        assert!(
            !repository.join(removed_runtime_path).exists(),
            "obsolete dynamic runtime release artifact must stay removed: {removed_runtime_path}"
        );
    }
    for forbidden in [
        "RuntimeSource",
        "runtimes/whisper_cpp",
        "runtime-manifest.json",
        "\"-\",",
    ] {
        assert!(
            !release.contains(forbidden),
            "self-contained release build must not stage dynamic runtime artifact {forbidden}"
        );
    }

    let workflow = fs::read_to_string(
        repository
            .join(".github")
            .join("workflows")
            .join("release.yml"),
    )
    .expect("Windows release workflow must be readable");
    for required in [
        "prepare-windows-release-inputs.ps1",
        "build-windows-release.ps1",
        "verify-windows-release-package.ps1",
        "-BundlePath dist\\portable",
        "-PortableZipPath dist\\Scribe-windows-x64.zip",
        "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09",
        "actions/upload-artifact@b7c566a772e6b6bfb58ed0dc250532a479d7789f",
        "dtolnay/rust-toolchain@01ba1edad32c6f80dbcce879d3e0fa5a00b2a84e",
        "INNO_INSTALLER_URL: https://github.com/jrsoftware/issrc/releases/download/is-7_1_0/innosetup-7.1.0-x64.exe",
        "INNO_INSTALLER_SHA256: 0362a383ed217d4c4239b5933866dd96d3eb2102737da92f80f6057a4b40df2f",
        "INNO_COMPILER_SHA256: d06ebd38f38e3cee60a3c50cc45bd449d77e0bc6a5cabc607ea9886808e4de1a",
        "Get-AuthenticodeSignature -LiteralPath $installerPath",
        "X509NameType]::SimpleName",
        "Pyrsys B.V.",
        "'/PORTABLE=1'",
        "'/TASKS=\"\"'",
        "Pinned Inno Setup portable root must be absent before acquisition",
        "-ExerciseStableUpgrade",
        "-EvidenceDirectory dist\\installer-verification-logs",
        "name: windows-installer-verification-logs",
    ] {
        assert!(
            workflow.contains(required),
            "Windows release workflow must retain {required}"
        );
    }
    for uses_line in workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("uses:"))
    {
        let reference = uses_line
            .strip_prefix("uses:")
            .expect("uses line prefix checked")
            .split('#')
            .next()
            .expect("action reference must exist")
            .trim();
        let (_, revision) = reference
            .rsplit_once('@')
            .expect("GitHub Action reference must contain @");
        assert!(
            revision.len() == 40
                && revision
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "GitHub Action reference must use a lowercase immutable full SHA: {uses_line}"
        );
    }
    assert!(
        !workflow.contains("choco install innosetup"),
        "Inno Setup acquisition must not trust a mutable network-only Chocolatey install"
    );
    let inno_acquire_start = workflow
        .find("      - name: Acquire digest-pinned Inno Setup")
        .expect("Inno Setup acquisition step must exist");
    let inno_acquire_end = workflow[inno_acquire_start..]
        .find("      - name: Build and normalize Windows installer")
        .map(|offset| inno_acquire_start + offset)
        .expect("Inno Setup compiler build step must follow acquisition");
    let inno_acquire = &workflow[inno_acquire_start..inno_acquire_end];
    assert!(
        !inno_acquire.contains("INNO_NUPKG_")
            && !inno_acquire.contains("community.chocolatey.org")
            && !inno_acquire.contains("Expand-Archive"),
        "Inno Setup acquisition must not retain Chocolatey package-wrapper provenance"
    );
    assert!(
        !workflow.contains("Copy-Item target\\release\\local-transcriber.exe"),
        "Windows release workflow must not publish a bare executable"
    );

    let inno_provenance = fs::read_to_string(
        repository
            .join("installer")
            .join("inno-setup-7.1.0-provenance.json"),
    )
    .expect("Inno Setup provenance must be readable");
    for required in [
        "\"schema_version\": 2",
        "\"product_version\": \"7.1.0\"",
        "https://github.com/jrsoftware/issrc/releases/download/is-7_1_0/innosetup-7.1.0-x64.exe",
        "\"installer_size_bytes\": 14304168",
        "0362a383ed217d4c4239b5933866dd96d3eb2102737da92f80f6057a4b40df2f",
        "\"compiler_size_bytes\": 2135968",
        "d06ebd38f38e3cee60a3c50cc45bd449d77e0bc6a5cabc607ea9886808e4de1a",
        "Pyrsys B.V.",
        "SetupArchitecture=x86",
        "do not independently reproduce upstream review",
    ] {
        assert!(
            inno_provenance.contains(required),
            "Inno Setup provenance must retain {required}"
        );
    }
    assert!(
        !repository
            .join("installer")
            .join("inno-setup-6.7.1-provenance.json")
            .exists(),
        "obsolete Inno Setup provenance must not remain beside the direct-official record"
    );
    for forbidden in [
        "\"package_url\"",
        "\"package_size_bytes\"",
        "\"package_sha256\"",
        "\"embedded_installer_path\"",
        "\"embedded_installer_size_bytes\"",
        "\"embedded_installer_sha256\"",
        "\"embedded_package_verification_path\"",
    ] {
        assert!(
            !inno_provenance.contains(forbidden),
            "Inno Setup provenance must not retain obsolete package-wrapper field {forbidden}"
        );
    }

    let installer = fs::read_to_string(repository.join("installer").join("scribe.iss"))
        .expect("Windows installer script must be readable");
    assert!(
        installer.contains("Source: \"..\\dist\\portable\\*\"")
            && installer.contains("recursesubdirs")
            && installer.contains("createallsubdirs")
            && installer.contains("BeforeInstall: ReleasePayloadHandleForCurrentFile")
            && installer.contains("StableAppIdGuid \"8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A\"")
            && installer.contains("DefaultDirName={code:ResolveDefaultDir}")
            && installer.contains("{localappdata}\\Programs\\Scribe")
            && installer.contains("SetupArchitecture=x86")
            && installer.contains("AppId={code:ResolveAppId}")
            && installer.contains("ReadBoundedToken('SCRIBEVERIFY')")
            && installer.contains("function PrepareToInstall")
            && installer.contains("function ValidateAndBindInstallTree")
            && installer.contains("function QueryExistingAttributes")
            && installer.contains("function BindDirectory")
            && installer.contains("function BindFileForUpdate")
            && installer.contains("function IsInnoUninstallerArtifact")
            && installer.contains("FindFirstFileW")
            && installer.contains("FindNextFileW")
            && installer.contains("FindFirstStreamW")
            && installer.contains("FindNextStreamW")
            && installer.contains("FileShareRead or FileShareWrite")
            && installer.contains("GenericRead or GenericWrite")
            && installer.contains("FileFlagBackupSemantics or FileFlagOpenReparsePoint")
            && installer.contains("DLLGetLastError")
            && installer.contains("ErrorFileNotFound")
            && installer.contains("ErrorPathNotFound")
            && installer.contains("ErrorNoMoreFiles")
            && installer.contains("ErrorHandleEof")
            && installer.contains("FILE_ATTRIBUTE_REPARSE_POINT")
            && installer.contains("case-insensitive path collision")
            && installer.contains("alternate NTFS data stream")
            && installer.contains("SizeOf(FindDataLayoutProbe) <> 592")
            && installer.contains("SizeOf(StreamDataLayoutProbe) <> 600")
            && installer.contains("CreateUninstallRegKey=IsNormalInstall")
            && installer.contains("Check: IsNormalInstall")
            && installer.contains("UsePreviousAppDir=yes")
            && installer.contains("UsePreviousTasks=yes")
            && installer.contains("UsePreviousLanguage=no")
            && installer.contains("Setup did not delete or change any existing content")
            && installer.contains("VerificationInstallDir(Token)")
            && installer.contains("WizardDirValue"),
        "Windows installer must preflight and recursively copy only the validated portable payload"
    );
    assert_eq!(
        installer.matches("GetFileAttributesW(").count(),
        2,
        "every installer attribute query must use the fail-closed error-classifying helper"
    );
    let local_frozen_installer =
        fs::read_to_string(repository.join("installer").join("scribe-local-frozen.iss"))
            .expect("local frozen installer script must be readable");
    assert!(
        local_frozen_installer.contains("SetupArchitecture=x86")
            && local_frozen_installer.contains("ArchitecturesInstallIn64BitMode=x64compatible")
            && local_frozen_installer.contains("ArchitecturesAllowed=x64compatible"),
        "both installer templates must preserve the x64 payload contract with an explicit x86 Setup output"
    );
    let directory_probe_start = installer
        .find("function BindDirectory")
        .expect("installer directory identity binding must exist");
    let directory_probe_end = installer[directory_probe_start..]
        .find("function BindFileForUpdate")
        .map(|offset| directory_probe_start + offset)
        .expect("installer file identity binding must follow directory binding");
    assert!(
        !installer[directory_probe_start..directory_probe_end].contains("FileShareDelete"),
        "installer must keep enumerated directories from being renamed after preflight"
    );
    let file_probe_end = installer[directory_probe_end..]
        .find("function ValidateNoReparseAncestors")
        .map(|offset| directory_probe_end + offset)
        .expect("installer ancestor validator must follow file identity binding");
    let file_probe_source = &installer[directory_probe_end..file_probe_end];
    let uninstaller_artifact_start = installer
        .find("function IsInnoUninstallerArtifact")
        .expect("installer uninstaller artifact predicate must exist");
    let uninstaller_artifact_end = installer[uninstaller_artifact_start..]
        .find("function QueryExistingAttributes")
        .map(|offset| uninstaller_artifact_start + offset)
        .expect("installer attribute helper must follow uninstaller artifact predicate");
    let uninstaller_artifact_source =
        &installer[uninstaller_artifact_start..uninstaller_artifact_end];
    assert!(
        !installer.contains("FileShareDelete")
            && uninstaller_artifact_source.contains("SameStr(RelativePath, 'unins000.exe')")
            && uninstaller_artifact_source.contains("SameStr(RelativePath, 'unins000.dat')")
            && uninstaller_artifact_source
                .matches("SameStr(RelativePath,")
                .count()
                == 2
            && file_probe_source.contains("ReleaseBeforeInnoReplacement: Boolean")
            && file_probe_source.contains("IdentityAccess := 0")
            && file_probe_source.contains("if not ReleaseBeforeInnoReplacement then")
            && file_probe_source.contains("IdentityAccess := GenericRead")
            && file_probe_source
                .contains("IdentityHandle, Path, ReleaseBeforeInnoReplacement, ErrorText",)
            && file_probe_source
                .contains("Path, IdentityAccess, FileShareRead or FileShareWrite, 0, OpenExisting")
            && file_probe_source
                .contains("Path, GenericRead or GenericWrite, FileShareRead or FileShareWrite"),
        "installer must keep normal file bindings delete-denying and tag only the exact Inno uninstaller pair for release"
    );
    let inspect_start = installer
        .find("function InspectExistingTree")
        .expect("installer tree inspector must exist");
    let inspect_end = installer[inspect_start..]
        .find("function ValidateAndBindInstallTree")
        .map(|offset| inspect_start + offset)
        .expect("stable tree validator must follow tree inspector");
    let inspect_source = &installer[inspect_start..inspect_end];
    let bind_file_call_start = inspect_source
        .find("BindFileForUpdate(")
        .expect("installer tree inspection must bind existing files");
    let bind_file_call_end = inspect_source[bind_file_call_start..]
        .find(") then")
        .map(|offset| bind_file_call_start + offset)
        .expect("installer file binding call must close before its failure branch");
    let bind_file_call_source = &inspect_source[bind_file_call_start..bind_file_call_end];
    let child_path_argument = bind_file_call_source
        .find("ChildPath")
        .expect("installer file binding must use the enumerated child path");
    let uninstaller_argument = bind_file_call_source
        .find("IsInnoUninstallerArtifact(RelativePath)")
        .expect("installer file binding must classify the validated relative path");
    let error_argument = bind_file_call_source
        .find("ErrorText")
        .expect("installer file binding must preserve its fail-closed error result");
    let uninstaller_release_start = installer
        .find("procedure ReleaseInnoUninstallerHandles")
        .expect("installer must release Inno uninstaller handles before replacement");
    let uninstaller_release_end = installer[uninstaller_release_start..]
        .find("function RetainBoundHandle")
        .map(|offset| uninstaller_release_start + offset)
        .expect("installer retained-handle helper must follow uninstaller release helper");
    let uninstaller_release_source = &installer[uninstaller_release_start..uninstaller_release_end];
    let payload_release_start = installer
        .find("procedure ReleasePayloadHandleForCurrentFile")
        .expect("installer must release each payload handle at its BeforeInstall boundary");
    let payload_release_end = installer[payload_release_start..]
        .find("function RetainBoundHandle")
        .map(|offset| payload_release_start + offset)
        .expect("installer retained-handle helper must follow payload release helper");
    let payload_release_source = &installer[payload_release_start..payload_release_end];
    let lifecycle_start = installer
        .find("function PrepareToInstall")
        .expect("installer preflight lifecycle must exist");
    let lifecycle_source = &installer[lifecycle_start..];
    assert!(
        child_path_argument < uninstaller_argument
            && uninstaller_argument < error_argument
            && file_probe_source.contains("RetainBoundHandle(")
            && file_probe_source.contains("IdentityHandle, Path, ReleaseBeforeInnoReplacement")
            && file_probe_source.contains("RejectAlternateStreams(Path, False")
            && file_probe_source.contains("GenericRead or GenericWrite")
            && uninstaller_release_source
                .contains("if BoundHandleReleaseBeforeInnoReplacement[I] then")
            && uninstaller_release_source.contains("CloseHandle(BoundHandles[I])")
            && payload_release_source.contains(
                "CurrentPath := RemoveBackslashUnlessRoot(ExpandFileName(ExpandConstant(CurrentFilename)))"
            )
            && payload_release_source.contains("SameStr(BoundHandlePaths[I], CurrentPath)")
            && payload_release_source
                .contains("if BoundHandleReleaseBeforeInnoReplacement[I] then")
            && payload_release_source.contains("if MatchingHandleIndex <> -1 then")
            && payload_release_source.contains("if FileExists(CurrentPath) then")
            && payload_release_source.contains("if MatchingHandleIndex = -1 then")
            && payload_release_source
                .contains("if not CloseHandle(BoundHandles[MatchingHandleIndex]) then")
            && payload_release_source
                .contains("BoundHandles[MatchingHandleIndex] := InvalidHandleValue")
            && payload_release_source.contains("else if MatchingHandleIndex <> -1 then")
            && lifecycle_source.contains("ReleaseInnoUninstallerHandles();")
            && matches!(
                (
                    lifecycle_source.find("ReleaseInnoUninstallerHandles();"),
                    lifecycle_source.find("WaitAtTestBoundary();")
                ),
                (Some(release), Some(pause)) if release < pause
            ),
        "installer must retain delete-denying payload identity handles while releasing only the validated Inno metadata handles before file replacement"
    );
    let first_probe = inspect_source
        .find("BindDirectory(")
        .expect("installer enumeration must first bind directory identity");
    let enumeration_start = inspect_source
        .find("FindFirstFileW(")
        .expect("installer tree inspection must use native enumeration");
    let enumeration_end = inspect_source
        .rfind("FindNextFileW(")
        .expect("installer tree inspection must continue native enumeration");
    assert!(
        first_probe < enumeration_start && enumeration_start < enumeration_end,
        "installer enumeration must retain a reparse-aware directory handle while reading entries"
    );
    assert!(
        inspect_source.contains("if ErrorCode <> ErrorNoMoreFiles")
            && inspect_source.contains("if ErrorCode = ErrorFileNotFound")
            && inspect_source
                .matches("ErrorCode := DLLGetLastError;")
                .count()
                == 2,
        "installer enumeration must fail closed on start and continuation errors"
    );
    assert!(
        installer.contains("BoundHandles: array[0..2047] of THandle")
            && installer.contains("procedure ReleaseBoundHandles()")
            && lifecycle_source.contains("if CurStep = ssPostInstall then")
            && lifecycle_source.contains("procedure DeinitializeSetup();")
            && lifecycle_source.matches("ReleaseBoundHandles();").count() >= 4,
        "installer must retain identity handles through installation and release them on every exit"
    );
    assert!(
        !installer.contains("[InstallDelete]")
            && !installer.contains("[UninstallDelete]")
            && !installer.contains("[Registry]")
            && !installer.contains("[INI]"),
        "Windows installer must not broadly delete an existing program directory"
    );

    let main = fs::read_to_string(repository.join("src").join("main.rs"))
        .expect("application main source must be readable");
    assert!(
        main.contains("windows_subsystem = \"windows\"")
            && main.contains("report_startup_failure")
            && main.contains("MessageBoxW"),
        "non-debug Windows startup failures must stay visible without a console"
    );

    let packaging_tests = fs::read_to_string(
        repository
            .join("scripts")
            .join("test-windows-release-packaging.ps1"),
    )
    .expect("Windows release fail-closed tests must be readable");
    for required in [
        "PE Machine mismatch",
        "Validated staging cleanup",
        "Out-of-bounds cleanup",
        "outside the explicit allowlist",
        "Cargo-target bundle path",
        "repository-relative Cargo target",
        "expected detected architecture 'whisper'",
        "Existing final bundle",
        "Stale staging refusal",
        "exact executable name",
        "canonical executable parent",
        "PE subsystem mismatch",
        "Windows release workflow",
        "Windows installer",
        "duplicate case-insensitive",
        "Assert-SafePortableZip",
        "Assert-PayloadParity",
        "RUNTIMES/whisper/whisper.dll",
        "nested/model.ONNX",
        "python/runner.py",
        "unreviewed normal import DLL: whisper.dll",
        "unreviewed delay import DLL: onnxruntime.dll",
        "setCaseSensitiveInfo",
    ] {
        assert!(
            packaging_tests.contains(required),
            "Windows release fail-closed tests must cover {required}"
        );
    }

    let package_verifier = fs::read_to_string(
        repository
            .join("scripts")
            .join("verify-windows-release-package.ps1"),
    )
    .expect("release payload verifier must be readable");
    for required in [
        "Release payload differs from its explicit inventory",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "/VERYSILENT",
        "Assert-SafePortableZip",
        "Assert-PayloadParity $bundle $zipRoot \"Portable ZIP\"",
        "Assert-PayloadParity $bundle $installedRoot \"Installed\"",
        "AllowedAdditionalFiles $InnoSetupUninstallerArtifacts",
        "/SCRIBEVERIFY=$verificationToken",
        "Assert-IsolatedInstallerLog",
        "Assert-ExactTreeSnapshot",
        "Invoke-ProtectedRenameRace",
        "Invoke-ReparseRefusalFixture",
        "Assert-Amd64GuiPe",
        "Assert-ReviewedWindowsPe",
        "[switch]$ExerciseStableUpgrade",
        "[string]$EvidenceDirectory",
        "accepted an override outside its derived temporary destination",
        "Stable case-insensitive path collision",
        "Stable unexpected legacy runtime tree",
        "Stable payload file with alternate data stream",
        "Stable payload directory with alternate data stream",
        "Stable root rename race",
        "Stable child-directory rename race",
        "Stable file rename race",
        "Bundle inventory paths differ from the canonical self-contained payload allowlist",
    ] {
        assert!(
            package_verifier.contains(required),
            "release payload verifier must retain {required}"
        );
    }

    let pe_imports = fs::read_to_string(repository.join("scripts").join("windows-pe-imports.ps1"))
        .expect("self-contained Windows PE import parser must be readable");
    for required in [
        "Read-PeImportDirectory",
        r#"[ValidateSet("normal", "delay")]"#,
        "normalDirectoryOffset",
        "delayDirectoryOffset",
        "Convert-PeRvaToFileOffset",
        "Assert-ReviewedWindowsPe",
        "unreviewed normal import DLL",
        "unreviewed delay import DLL",
        "api-ms-win-core-path-l1-1-0.dll",
        "kernel32.dll",
        "psapi.dll",
        "user32.dll",
    ] {
        assert!(
            pe_imports.contains(required),
            "self-contained Windows PE import parser must retain {required}"
        );
    }

    let notice = fs::read_to_string(
        repository
            .join("resources")
            .join("licenses")
            .join("Whisper-Base-En-NOTICE.txt"),
    )
    .expect("bundled model notice must be readable");
    assert!(notice.contains(catalog.artifact_repository));
    assert!(notice.contains(catalog.artifact_revision));
    assert!(notice.contains(catalog.artifact_sha256));
    assert!(notice.contains("Apache-2.0.txt"));
    assert!(notice.contains("OpenAI-Whisper-MIT.txt"));
    assert!(notice.contains("official OpenAI Whisper model distribution metadata"));
    assert!(notice.contains("artifacts as Apache-2.0"));
    assert!(notice.contains("source-code repository includes an MIT License"));
    assert!(notice.contains("not presented here as the license for the"));
    assert!(notice.contains("does not state a legal conclusion"));
    let upstream_mit = fs::read_to_string(
        repository
            .join("resources")
            .join("licenses")
            .join("OpenAI-Whisper-MIT.txt"),
    )
    .expect("OpenAI Whisper MIT notice must be readable");
    assert!(upstream_mit.contains("Copyright (c) 2022 OpenAI"));
}
