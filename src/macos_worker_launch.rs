//! Descriptor-bound macOS worker launch.
//!
//! Production Metal packs are executed only through the retained verified
//! executable descriptor. Catalog paths are never process-creation authority.

use std::collections::BTreeMap;
use std::ffi::OsString;

use anyhow::{Result, bail};

const SAFE_PARENT_ENVIRONMENT: &[&str] = &["HOME", "TMPDIR", "LANG", "LC_ALL"];

pub(crate) fn sanitized_environment(
    bindings: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    sanitized_environment_from(std::env::vars_os(), bindings)
}

fn sanitized_environment_from(
    source: impl IntoIterator<Item = (OsString, OsString)>,
    bindings: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    let mut environment = BTreeMap::new();
    for (name, value) in source {
        let Some(name) = name.to_str() else {
            continue;
        };
        if !SAFE_PARENT_ENVIRONMENT.contains(&name) {
            continue;
        }
        let Some(value) = value.to_str() else {
            continue;
        };
        validate_environment_field(name, value)?;
        environment.insert(name.to_owned(), value.to_owned());
    }
    for (name, value) in bindings {
        validate_environment_field(name, value)?;
        if !name.starts_with("SCRIBE_PRIVATE_") {
            bail!("verified worker binding is outside the private environment namespace");
        }
        environment.insert(name.clone(), value.clone());
    }
    Ok(environment.into_iter().collect())
}

fn validate_environment_field(name: &str, value: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.contains('=')
        || name.contains('\0')
        || value.len() > 4096
        || value.contains('\0')
    {
        bail!("worker environment contains an invalid field");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CString, c_char, c_int};
    use std::io::Write;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::sync::Mutex;

    use anyhow::{Context, Result, anyhow, bail};

    use super::*;
    use crate::gpu_worker_pack::UnixPackExecAuthority;

    const PARENT_LIVENESS_ENV: &str = "SCRIBE_PRIVATE_PARENT_LIVENESS";
    const PARENT_CONTROL_CANCEL: u8 = b'C';

    unsafe extern "C" {
        fn posix_spawn_file_actions_addchdir_np(
            actions: *mut libc::posix_spawn_file_actions_t,
            path: *const c_char,
        ) -> c_int;
    }

    pub(crate) struct MacSpawnedWorker {
        pub(crate) stdin: std::fs::File,
        pub(crate) stdout: std::fs::File,
        pub(crate) process: MacWorkerProcess,
    }

    struct ProcessState {
        pid: libc::pid_t,
        reaped: bool,
    }

    pub(crate) struct MacWorkerProcess {
        state: Mutex<ProcessState>,
        parent_control: Mutex<std::fs::File>,
    }

    impl MacWorkerProcess {
        pub(crate) fn is_running(&self) -> Result<bool> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("macOS worker process lock was poisoned"))?;
            if state.reaped {
                return Ok(false);
            }
            let mut status = 0;
            // SAFETY: state.pid is a child created by this object and status is writable.
            let result = unsafe { libc::waitpid(state.pid, &mut status, libc::WNOHANG) };
            if result == 0 {
                Ok(true)
            } else if result == state.pid {
                state.reaped = true;
                Ok(false)
            } else {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    state.reaped = true;
                    Ok(false)
                } else {
                    Err(error).context("could not poll the macOS worker process")
                }
            }
        }

        pub(crate) fn request_cooperative_cancel(&self) -> Result<bool> {
            let mut control = self
                .parent_control
                .lock()
                .map_err(|_| anyhow!("macOS worker control lock was poisoned"))?;
            match control.write(&[PARENT_CONTROL_CANCEL]) {
                Ok(1) => Ok(true),
                Ok(_) => Ok(false),
                Err(error) => Err(error).context("could not request macOS worker cancellation"),
            }
        }

        pub(crate) fn terminate(&self) -> Result<()> {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow!("macOS worker process lock was poisoned"))?;
            if state.reaped {
                return Ok(());
            }
            // posix_spawn created a process group whose id equals the child pid.
            if unsafe { libc::killpg(state.pid, libc::SIGKILL) } == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error)
                        .context("could not terminate the macOS worker process group");
                }
            }
            Ok(())
        }

        pub(crate) fn wait(&self) -> Result<()> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("macOS worker process lock was poisoned"))?;
            if state.reaped {
                return Ok(());
            }
            loop {
                let mut status = 0;
                // SAFETY: state.pid is a child created by this object and status is writable.
                let result = unsafe { libc::waitpid(state.pid, &mut status, 0) };
                if result == state.pid {
                    state.reaped = true;
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    state.reaped = true;
                    return Ok(());
                }
                return Err(error).context("could not reap the macOS worker process");
            }
        }
    }

    pub(crate) fn launch_verified_worker(
        authority: &UnixPackExecAuthority,
        worker_flag: &str,
        bindings: &[(String, String)],
    ) -> Result<MacSpawnedWorker> {
        authority
            .recheck()
            .context("verified macOS worker authority changed before spawn")?;
        let executable = InheritableFd::duplicate(authority.executable_fd().as_raw_fd())?;
        let dependency_root = InheritableFd::duplicate(authority.dependency_root_fd().as_raw_fd())?;
        let stdin_pipe = Pipe::new()?;
        let stdout_pipe = Pipe::new()?;
        let liveness_pipe = Pipe::new()?;

        let executable_path = CString::new(format!("/dev/fd/{}", executable.raw()))?;
        let dependency_root_path = CString::new(format!("/dev/fd/{}", dependency_root.raw()))?;
        let worker_flag = CString::new(worker_flag)?;
        let argv_storage = [executable_path.clone(), worker_flag];
        let mut argv = argv_storage
            .iter()
            .map(|value| value.as_ptr().cast_mut())
            .chain(std::iter::once(std::ptr::null_mut()))
            .collect::<Vec<_>>();

        let mut bindings = bindings.to_vec();
        bindings.push((
            PARENT_LIVENESS_ENV.to_owned(),
            liveness_pipe.read.raw().to_string(),
        ));
        let environment = sanitized_environment(&bindings)?;
        let environment_storage = environment
            .into_iter()
            .map(|(name, value)| CString::new(format!("{name}={value}")))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut environment_pointers = environment_storage
            .iter()
            .map(|value| value.as_ptr().cast_mut())
            .chain(std::iter::once(std::ptr::null_mut()))
            .collect::<Vec<_>>();

        let mut actions = SpawnFileActions::new()?;
        actions.dup2(executable.raw(), executable.raw())?;
        actions.dup2(dependency_root.raw(), dependency_root.raw())?;
        actions.dup2(liveness_pipe.read.raw(), liveness_pipe.read.raw())?;
        actions.dup2(stdin_pipe.read.raw(), libc::STDIN_FILENO)?;
        actions.dup2(stdout_pipe.write.raw(), libc::STDOUT_FILENO)?;
        actions.close(stdin_pipe.write.raw())?;
        actions.close(stdout_pipe.read.raw())?;
        actions.close(liveness_pipe.write.raw())?;
        actions.chdir(&dependency_root_path)?;

        let mut attributes = SpawnAttributes::new()?;
        attributes.process_group()?;
        let mut pid = 0;
        authority
            .recheck()
            .context("verified macOS worker authority changed at spawn boundary")?;
        // SAFETY: every pointer is backed by live CString storage, actions and
        // attributes were initialized, and pid is writable.
        let result = unsafe {
            libc::posix_spawn(
                &mut pid,
                executable_path.as_ptr(),
                actions.as_ptr(),
                attributes.as_ptr(),
                argv.as_mut_ptr(),
                environment_pointers.as_mut_ptr(),
            )
        };
        if result != 0 {
            return Err(std::io::Error::from_raw_os_error(result))
                .context("descriptor-bound macOS worker posix_spawn failed");
        }

        drop(stdin_pipe.read);
        drop(stdout_pipe.write);
        drop(liveness_pipe.read);
        // SAFETY: each remaining guard owns a distinct live pipe descriptor.
        let stdin = unsafe { std::fs::File::from_raw_fd(stdin_pipe.write.into_raw()) };
        let stdout = unsafe { std::fs::File::from_raw_fd(stdout_pipe.read.into_raw()) };
        let parent_control = unsafe { std::fs::File::from_raw_fd(liveness_pipe.write.into_raw()) };
        Ok(MacSpawnedWorker {
            stdin,
            stdout,
            process: MacWorkerProcess {
                state: Mutex::new(ProcessState { pid, reaped: false }),
                parent_control: Mutex::new(parent_control),
            },
        })
    }

    struct FdGuard(RawFd);

    impl FdGuard {
        fn raw(&self) -> RawFd {
            self.0
        }

        fn into_raw(mut self) -> RawFd {
            let raw = self.0;
            self.0 = -1;
            raw
        }
    }

    impl Drop for FdGuard {
        fn drop(&mut self) {
            if self.0 >= 0 {
                unsafe { libc::close(self.0) };
            }
        }
    }

    struct InheritableFd(FdGuard);

    impl InheritableFd {
        fn duplicate(source: RawFd) -> Result<Self> {
            // F_DUPFD creates a private duplicate; only that duplicate has its
            // close-on-exec bit cleared for the /dev/fd spawn bridge.
            let raw = unsafe { libc::fcntl(source, libc::F_DUPFD, 3) };
            if raw < 0 || unsafe { libc::fcntl(raw, libc::F_SETFD, 0) } < 0 {
                if raw >= 0 {
                    unsafe { libc::close(raw) };
                }
                return Err(std::io::Error::last_os_error())
                    .context("could not duplicate verified macOS launch authority");
            }
            Ok(Self(FdGuard(raw)))
        }

        fn raw(&self) -> RawFd {
            self.0.raw()
        }
    }

    struct Pipe {
        read: FdGuard,
        write: FdGuard,
    }

    impl Pipe {
        fn new() -> Result<Self> {
            let mut raw = [-1; 2];
            if unsafe { libc::pipe(raw.as_mut_ptr()) } == -1 {
                return Err(std::io::Error::last_os_error())
                    .context("could not create a macOS worker pipe");
            }
            for descriptor in raw {
                if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                    unsafe {
                        libc::close(raw[0]);
                        libc::close(raw[1]);
                    }
                    return Err(std::io::Error::last_os_error())
                        .context("could not harden a macOS worker pipe");
                }
            }
            // The liveness read end and stdio ends are explicitly preserved by
            // file actions; CLOEXEC_DEFAULT closes every unrelated descriptor.
            Ok(Self {
                read: FdGuard(raw[0]),
                write: FdGuard(raw[1]),
            })
        }
    }

    struct SpawnFileActions(libc::posix_spawn_file_actions_t);

    impl SpawnFileActions {
        fn new() -> Result<Self> {
            let mut actions = MaybeUninit::uninit();
            let result = unsafe { libc::posix_spawn_file_actions_init(actions.as_mut_ptr()) };
            if result != 0 {
                return Err(std::io::Error::from_raw_os_error(result))
                    .context("could not initialize macOS spawn file actions");
            }
            Ok(Self(unsafe { actions.assume_init() }))
        }

        fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
            &self.0
        }

        fn dup2(&mut self, source: RawFd, destination: RawFd) -> Result<()> {
            spawn_result(
                unsafe { libc::posix_spawn_file_actions_adddup2(&mut self.0, source, destination) },
                "could not configure macOS worker stdio",
            )
        }

        fn close(&mut self, descriptor: RawFd) -> Result<()> {
            spawn_result(
                unsafe { libc::posix_spawn_file_actions_addclose(&mut self.0, descriptor) },
                "could not configure macOS worker descriptor closure",
            )
        }

        fn chdir(&mut self, path: &CString) -> Result<()> {
            spawn_result(
                unsafe { posix_spawn_file_actions_addchdir_np(&mut self.0, path.as_ptr()) },
                "could not bind macOS worker dependency root",
            )
        }
    }

    impl Drop for SpawnFileActions {
        fn drop(&mut self) {
            unsafe { libc::posix_spawn_file_actions_destroy(&mut self.0) };
        }
    }

    struct SpawnAttributes(libc::posix_spawnattr_t);

    impl SpawnAttributes {
        fn new() -> Result<Self> {
            let mut attributes = MaybeUninit::uninit();
            let result = unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) };
            if result != 0 {
                return Err(std::io::Error::from_raw_os_error(result))
                    .context("could not initialize macOS spawn attributes");
            }
            Ok(Self(unsafe { attributes.assume_init() }))
        }

        fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
            &self.0
        }

        fn process_group(&mut self) -> Result<()> {
            spawn_result(
                unsafe { libc::posix_spawnattr_setpgroup(&mut self.0, 0) },
                "could not configure macOS worker process group",
            )?;
            let flags =
                (libc::POSIX_SPAWN_SETPGROUP | libc::POSIX_SPAWN_CLOEXEC_DEFAULT) as libc::c_short;
            spawn_result(
                unsafe { libc::posix_spawnattr_setflags(&mut self.0, flags) },
                "could not harden macOS worker spawn attributes",
            )
        }
    }

    impl Drop for SpawnAttributes {
        fn drop(&mut self) {
            unsafe { libc::posix_spawnattr_destroy(&mut self.0) };
        }
    }

    fn spawn_result(result: c_int, context: &'static str) -> Result<()> {
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(result)).context(context)
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use platform::{MacWorkerProcess, launch_verified_worker};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_worker_environment_drops_loader_and_backend_overrides() {
        let source = [
            (OsString::from("HOME"), OsString::from("/Users/test")),
            (
                OsString::from("DYLD_LIBRARY_PATH"),
                OsString::from("/tmp/evil"),
            ),
            (
                OsString::from("METAL_DEVICE_WRAPPER_TYPE"),
                OsString::from("1"),
            ),
            (OsString::from("MTL_CAPTURE_ENABLED"), OsString::from("1")),
            (
                OsString::from("GGML_METAL_PATH_RESOURCES"),
                OsString::from("/tmp"),
            ),
            (OsString::from("WHISPER_CUDA"), OsString::from("1")),
        ];
        let environment = sanitized_environment_from(
            source,
            &[("SCRIBE_PRIVATE_PACK_BACKEND".to_owned(), "metal".to_owned())],
        )
        .unwrap();
        assert!(environment.contains(&("HOME".to_owned(), "/Users/test".to_owned())));
        assert!(
            environment.contains(&("SCRIBE_PRIVATE_PACK_BACKEND".to_owned(), "metal".to_owned()))
        );
        assert!(environment.iter().all(|(name, _)| {
            !name.starts_with("DYLD")
                && !name.starts_with("METAL")
                && !name.starts_with("MTL")
                && !name.starts_with("GGML")
                && !name.starts_with("WHISPER")
        }));
    }

    #[test]
    fn verified_worker_bindings_must_use_private_namespace() {
        assert!(
            sanitized_environment_from(
                std::iter::empty(),
                &[("DYLD_LIBRARY_PATH".to_owned(), "/tmp".to_owned())]
            )
            .is_err()
        );
    }
}
