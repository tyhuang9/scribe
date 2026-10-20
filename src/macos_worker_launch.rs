//! Installer-protected macOS worker launch.
//!
//! Production Metal packs execute only through a retained root-protected path
//! capability. Catalog paths and `/dev/fd` are never spawn authority.

use std::collections::BTreeMap;
use std::ffi::OsString;

use anyhow::{Result, bail};

const SAFE_PARENT_ENVIRONMENT: &[&str] = &["HOME", "TMPDIR", "LANG", "LC_ALL"];

#[cfg(target_os = "macos")]
pub(crate) const EXECUTABLE_FD_ENV: &str = "SCRIBE_PRIVATE_MACOS_EXECUTABLE_FD";
#[cfg(target_os = "macos")]
pub(crate) const EXECUTABLE_READ_FD_ENV: &str = "SCRIBE_PRIVATE_MACOS_EXECUTABLE_READ_FD";

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
    use std::ffi::{CString, c_int};
    use std::io::Write;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::sync::Mutex;

    use anyhow::{Context, Result, anyhow, bail};

    use super::*;
    use crate::gpu_worker_pack::UnixPackExecAuthority;

    const PARENT_LIVENESS_ENV: &str = "SCRIBE_PRIVATE_PARENT_LIVENESS";
    const PARENT_CONTROL_CANCEL: u8 = b'C';
    const DROP_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

    unsafe extern "C" {
        fn posix_spawn_file_actions_addfchdir_np(
            actions: *mut libc::posix_spawn_file_actions_t,
            descriptor: c_int,
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
        group_retired: bool,
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
            owned_child_has_exited(&mut state).map(|exited| !exited)
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
            self.terminate_with_group_signal(signal_group)
        }

        fn terminate_with_group_signal(
            &self,
            signal: impl Fn(libc::pid_t, c_int) -> std::io::Result<()>,
        ) -> Result<()> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("macOS worker process lock was poisoned"))?;
            if state.group_retired {
                return Ok(());
            }
            if state.reaped {
                // Reaping releases the PID reservation. Only a non-delivering
                // absence probe is safe now; a reused group may conservatively
                // keep cleanup fenced, but must never receive our SIGKILL.
                require_group_absent(state.pid, &signal)?;
                state.group_retired = true;
                return Ok(());
            }
            owned_child_has_exited(&mut state)?;
            // posix_spawn created a process group whose id equals the child pid.
            match signal(state.pid, libc::SIGKILL) {
                Ok(()) => {}
                Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                    state.group_retired = true;
                }
                Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
                    // Darwin excludes zombies from killpg's targets and can
                    // return EPERM for a zombie-only group. A terminal leader
                    // alone does NOT prove its descendants are gone. Reserve
                    // its PID with WNOWAIT, reap only that owned zombie, then
                    // require ESRCH from a non-delivering group probe.
                    if !owned_child_has_exited(&mut state)? {
                        return Err(error)
                            .context("could not terminate the macOS worker process group");
                    }
                    reap_child(&mut state, libc::WNOHANG)?;
                    require_group_absent(state.pid, &signal)?;
                    state.group_retired = true;
                }
                Err(error) => {
                    return Err(error)
                        .context("could not terminate the macOS worker process group");
                }
            }
            Ok(())
        }

        pub(crate) fn wait(&self) -> Result<()> {
            self.wait_with_group_signal(signal_group)
        }

        fn wait_with_group_signal(
            &self,
            signal: impl Fn(libc::pid_t, c_int) -> std::io::Result<()>,
        ) -> Result<()> {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("macOS worker process lock was poisoned"))?;
            if !state.reaped {
                reap_child(&mut state, 0)?;
            }
            if !state.group_retired {
                require_group_absent(state.pid, &signal)?;
                state.group_retired = true;
            }
            Ok(())
        }
    }

    fn owned_child_has_exited(state: &mut ProcessState) -> Result<bool> {
        match child_has_exited(state.pid) {
            Ok(exited) => Ok(exited),
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    // Ownership was lost; no later path may signal a possibly
                    // reused PID/process group with a nonzero signal.
                    state.reaped = true;
                }
                Err(error).context("could not poll the owned macOS worker process")
            }
        }
    }

    fn child_has_exited(pid: libc::pid_t) -> std::io::Result<bool> {
        loop {
            let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
            // WNOWAIT preserves the child's PID until group-directed cleanup
            // finishes. Polling must not silently release that reservation.
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    information.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                let information = unsafe { information.assume_init() };
                return Ok(unsafe { information.si_pid() } == pid);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn signal_group(pid: libc::pid_t, signal: c_int) -> std::io::Result<()> {
        if unsafe { libc::killpg(pid, signal) } == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn require_group_absent(
        pid: libc::pid_t,
        signal: &impl Fn(libc::pid_t, c_int) -> std::io::Result<()>,
    ) -> Result<()> {
        match signal(pid, 0) {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            Err(error) => Err(error).context("macOS worker group cleanup remains unconfirmed"),
            Ok(()) => bail!("macOS worker group remains present after its leader exited"),
        }
    }

    fn reap_child(state: &mut ProcessState, options: c_int) -> Result<()> {
        loop {
            let mut status = 0;
            // SAFETY: state.pid is a child created by this object and status is writable.
            let result = unsafe { libc::waitpid(state.pid, &mut status, options) };
            if result == state.pid {
                state.reaped = true;
                return Ok(());
            }
            if result == 0 {
                bail!("macOS worker was not terminal at its reap boundary");
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                state.reaped = true;
            }
            return Err(error).context("could not reap the macOS worker process");
        }
    }

    impl Drop for MacWorkerProcess {
        fn drop(&mut self) {
            let state = self
                .state
                .get_mut()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.reaped {
                return;
            }
            if child_has_exited(state.pid).is_err() {
                // A lost/unverifiable child is not authority to signal a PID
                // that may already belong to an unrelated process.
                return;
            }

            // The worker is launched in a private process group. Killing the
            // group also stops backend subprocesses, while the direct signal
            // ensures the child itself is terminated if group setup was lost.
            // Drop cannot report cleanup errors, so every operation is best
            // effort and reap polling is bounded to avoid wedging shutdown.
            unsafe {
                libc::killpg(state.pid, libc::SIGKILL);
                libc::kill(state.pid, libc::SIGKILL);
            }
            let deadline = std::time::Instant::now() + DROP_REAP_TIMEOUT;
            loop {
                let mut status = 0;
                // SAFETY: state.pid is the child owned by this process object.
                let result = unsafe { libc::waitpid(state.pid, &mut status, libc::WNOHANG) };
                if result == state.pid {
                    state.reaped = true;
                    return;
                }
                if result == -1 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        state.reaped = true;
                    }
                    return;
                }
                if std::time::Instant::now() >= deadline {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }

    pub(crate) fn launch_verified_worker(
        authority: &UnixPackExecAuthority,
        worker_flag: &str,
        bindings: &[(String, String)],
    ) -> Result<MacSpawnedWorker> {
        // Snapshot stderr before rechecks/pipes can reuse an initially closed
        // fd 2. The retained copy and every private pipe are allocated above
        // stdio, so spawn actions cannot alias their own sources/destinations.
        let stderr = stderr_descriptor(libc::STDERR_FILENO)?;
        authority
            .recheck()
            .context("verified macOS worker authority changed before spawn")?;
        let executable = InheritableFd::duplicate(authority.executable_fd().as_raw_fd())?;
        let readable_executable =
            InheritableFd::duplicate(authority.readable_executable_fd().as_raw_fd())?;
        let dependency_root = InheritableFd::duplicate(authority.dependency_root_fd().as_raw_fd())?;
        let stdin_pipe = Pipe::new()?;
        let stdout_pipe = Pipe::new()?;
        let liveness_pipe = Pipe::new()?;

        let executable_path = authority.protected_executable_path().to_owned();
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
        // These launcher-authored bindings intentionally follow caller
        // bindings. A caller cannot redirect either retained image authority.
        bindings.push((
            EXECUTABLE_READ_FD_ENV.to_owned(),
            readable_executable.raw().to_string(),
        ));
        bindings.push((EXECUTABLE_FD_ENV.to_owned(), executable.raw().to_string()));
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
        actions.dup2(readable_executable.raw(), readable_executable.raw())?;
        actions.dup2(dependency_root.raw(), dependency_root.raw())?;
        actions.dup2(liveness_pipe.read.raw(), liveness_pipe.read.raw())?;
        actions.dup2(stdin_pipe.read.raw(), libc::STDIN_FILENO)?;
        actions.dup2(stdout_pipe.write.raw(), libc::STDOUT_FILENO)?;
        // CLOEXEC_DEFAULT would otherwise discard startup diagnostics. Match
        // the ordinary worker launcher's inherited stderr, never its IPC pipe.
        actions.dup2(stderr.raw(), libc::STDERR_FILENO)?;
        actions.close(stdin_pipe.write.raw())?;
        actions.close(stdout_pipe.read.raw())?;
        actions.close(liveness_pipe.write.raw())?;
        actions.fchdir(dependency_root.raw())?;

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
                .context("installer-protected macOS worker posix_spawn failed");
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
                state: Mutex::new(ProcessState {
                    pid,
                    reaped: false,
                    group_retired: false,
                }),
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

    fn stderr_descriptor(source: RawFd) -> Result<FdGuard> {
        let duplicate = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate >= 0 {
            return Ok(FdGuard(duplicate));
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EBADF) {
            return Err(error).context("could not retain macOS worker stderr");
        }
        // GUI launch need not provide stderr. Never let that prevent worker
        // startup, and never route native diagnostics into the protocol pipe.
        let null = c"/dev/null";
        let descriptor = unsafe {
            libc::open(
                null.as_ptr(),
                libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not open macOS worker null stderr");
        }
        let null = FdGuard(descriptor);
        duplicate_private_fd(null.raw())
    }

    fn duplicate_private_fd(source: RawFd) -> Result<FdGuard> {
        let descriptor = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 3) };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not retain private macOS worker descriptor");
        }
        Ok(FdGuard(descriptor))
    }

    struct InheritableFd(FdGuard);

    impl InheritableFd {
        fn duplicate(source: RawFd) -> Result<Self> {
            // F_DUPFD creates a private duplicate; only that duplicate has its
            // close-on-exec bit cleared for inherited image evidence or the
            // descriptor-bound working-directory spawn action.
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
            let read = FdGuard(raw[0]);
            let write = FdGuard(raw[1]);
            // The liveness read end and stdio ends are explicitly preserved by
            // file actions; CLOEXEC_DEFAULT closes every unrelated descriptor.
            Ok(Self {
                read: duplicate_private_fd(read.raw())?,
                write: duplicate_private_fd(write.raw())?,
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

        fn fchdir(&mut self, descriptor: RawFd) -> Result<()> {
            spawn_result(
                unsafe { posix_spawn_file_actions_addfchdir_np(&mut self.0, descriptor) },
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

    #[cfg(test)]
    mod tests {
        use std::ffi::c_char;
        use std::fs;
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        use std::panic::AssertUnwindSafe;
        use std::path::PathBuf;
        use std::process::Command;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{Duration, Instant};

        use super::*;

        static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);

        struct TestRoot(PathBuf);

        impl TestRoot {
            fn new(label: &str) -> Self {
                let sequence = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
                let root = std::env::temp_dir()
                    .join(format!("scribe-{label}-{}-{sequence}", std::process::id()));
                fs::create_dir(&root).expect("create private macOS spawn fixture root");
                Self(root)
            }
        }

        impl Drop for TestRoot {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        struct TestChild {
            pid: libc::pid_t,
            reaped: bool,
        }

        impl TestChild {
            fn wait_before(&mut self, deadline: Instant) -> Result<c_int> {
                loop {
                    let mut status = 0;
                    let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
                    if result == self.pid {
                        self.reaped = true;
                        return Ok(status);
                    }
                    if result == -1 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() == std::io::ErrorKind::Interrupted {
                            if Instant::now() >= deadline {
                                bail!(
                                    "macOS spawn-action fixture did not exit before its deadline"
                                );
                            }
                            continue;
                        }
                        if error.raw_os_error() == Some(libc::ECHILD) {
                            self.reaped = true;
                        }
                        return Err(error).context("could not reap macOS spawn-action fixture");
                    }
                    if Instant::now() >= deadline {
                        bail!("macOS spawn-action fixture did not exit before its deadline");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }

        impl Drop for TestChild {
            fn drop(&mut self) {
                if self.reaped {
                    return;
                }
                unsafe {
                    libc::killpg(self.pid, libc::SIGKILL);
                    libc::kill(self.pid, libc::SIGKILL);
                }
                let deadline = Instant::now() + Duration::from_millis(500);
                loop {
                    let mut status = 0;
                    let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
                    if result == self.pid {
                        self.reaped = true;
                        return;
                    }
                    if result == -1 {
                        let error = std::io::Error::last_os_error();
                        if error.raw_os_error() == Some(libc::ECHILD) {
                            self.reaped = true;
                            return;
                        }
                        if error.kind() == std::io::ErrorKind::Interrupted
                            && Instant::now() < deadline
                        {
                            continue;
                        }
                        return;
                    }
                    if Instant::now() >= deadline {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }

        fn spawn_pwd(
            actions: &SpawnFileActions,
            attributes: &SpawnAttributes,
        ) -> (c_int, libc::pid_t) {
            let executable = CString::new("/bin/pwd").unwrap();
            let argument = CString::new("pwd").unwrap();
            let mut argv = [argument.as_ptr().cast_mut(), std::ptr::null_mut()];
            let mut environment: [*mut c_char; 1] = [std::ptr::null_mut()];
            let mut pid = 0;
            let result = unsafe {
                libc::posix_spawn(
                    &mut pid,
                    executable.as_ptr(),
                    actions.as_ptr(),
                    attributes.as_ptr(),
                    argv.as_mut_ptr(),
                    environment.as_mut_ptr(),
                )
            };
            (result, pid)
        }

        fn assert_cwd_descriptor_rejected(descriptor: RawFd, retain: bool) {
            let mut actions = SpawnFileActions::new().unwrap();
            if retain {
                actions.dup2(descriptor, descriptor).unwrap();
            }
            if actions.fchdir(descriptor).is_err() {
                return;
            }
            let mut attributes = SpawnAttributes::new().unwrap();
            attributes.process_group().unwrap();
            let (result, pid) = spawn_pwd(&actions, &attributes);
            if result == 0 {
                let mut child = TestChild { pid, reaped: false };
                let _ = child.wait_before(Instant::now() + Duration::from_secs(2));
                panic!("invalid working-directory descriptor launched a child");
            }
            assert!(
                result == libc::EBADF || result == libc::ENOTDIR,
                "unexpected invalid working-directory descriptor error: {result}"
            );
        }

        #[test]
        fn macos_exec_authority_retains_image_evidence_without_descriptor_spawn() {
            use std::os::unix::fs::PermissionsExt;

            let path = CString::new("/usr/bin/true").unwrap();
            let metadata = fs::metadata("/usr/bin/true").unwrap();
            assert_ne!(metadata.permissions().mode() & 0o111, 0);
            assert_eq!(
                unsafe {
                    libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS)
                },
                0,
                "fixture is not executable by the effective test identity"
            );
            let readable_raw = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            assert!(readable_raw >= 0, "open readable executable fixture");
            let readable = unsafe { std::fs::File::from_raw_fd(readable_raw) };
            let executable_raw = unsafe {
                libc::open(
                    path.as_ptr(),
                    libc::O_EXEC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            assert!(executable_raw >= 0, "open O_EXEC executable fixture");
            let executable = unsafe { std::fs::File::from_raw_fd(executable_raw) };
            let readable_flags = unsafe { libc::fcntl(readable.as_raw_fd(), libc::F_GETFL) };
            let executable_flags = unsafe { libc::fcntl(executable.as_raw_fd(), libc::F_GETFL) };
            assert_eq!(readable_flags & libc::O_EXEC, 0);
            assert_eq!(readable_flags & libc::O_ACCMODE, libc::O_RDONLY);
            assert_eq!(executable_flags & libc::O_EXEC, libc::O_EXEC);

            // These are measurement evidence only. The separate native Metal
            // helper contract exercises actual protected-path spawn and IPC;
            // Darwin's /dev/fd vnode is not an executable image authority.
            let readable_metadata = readable.metadata().unwrap();
            let executable_metadata = executable.metadata().unwrap();
            use std::os::unix::fs::MetadataExt;
            assert_eq!(readable_metadata.dev(), executable_metadata.dev());
            assert_eq!(readable_metadata.ino(), executable_metadata.ino());
        }

        #[test]
        fn descriptor_directory_spawn_retained_fd_survives_path_rename() {
            let root = TestRoot::new("retained-worker-directory");
            let original = root.0.join("original");
            let renamed = root.0.join("renamed");
            fs::create_dir(&original).unwrap();
            let directory = std::fs::File::open(&original).unwrap();
            let retained = InheritableFd::duplicate(directory.as_raw_fd()).unwrap();
            fs::rename(&original, &renamed).unwrap();
            fs::create_dir(&original).unwrap();

            let output = Pipe::new().unwrap();
            let mut actions = SpawnFileActions::new().unwrap();
            actions.dup2(retained.raw(), retained.raw()).unwrap();
            actions
                .dup2(output.write.raw(), libc::STDOUT_FILENO)
                .unwrap();
            actions.close(output.read.raw()).unwrap();
            actions.fchdir(retained.raw()).unwrap();
            let mut attributes = SpawnAttributes::new().unwrap();
            attributes.process_group().unwrap();
            let (result, pid) = spawn_pwd(&actions, &attributes);
            assert_eq!(result, 0, "retained-directory posix_spawn failed");
            let mut child = TestChild { pid, reaped: false };
            drop(output.write);
            let status = child
                .wait_before(Instant::now() + Duration::from_secs(2))
                .unwrap();
            assert_eq!(status, 0);

            let mut observed = String::new();
            let mut reader = unsafe { std::fs::File::from_raw_fd(output.read.into_raw()) };
            reader.read_to_string(&mut observed).unwrap();
            assert_eq!(
                PathBuf::from(observed.trim()),
                fs::canonicalize(&renamed).unwrap()
            );
            assert_ne!(
                PathBuf::from(observed.trim()),
                fs::canonicalize(&original).unwrap(),
                "spawn followed the replacement pathname instead of the retained directory"
            );
        }

        #[test]
        fn descriptor_directory_spawn_rejects_non_directory_and_invalid_descriptors_without_child()
        {
            let non_directory = std::fs::File::open("/dev/null").unwrap();
            let retained = InheritableFd::duplicate(non_directory.as_raw_fd()).unwrap();
            assert_cwd_descriptor_rejected(retained.raw(), true);
            assert_cwd_descriptor_rejected(-1, false);
        }

        fn lifecycle_process(program: &str, args: &[&str]) -> MacWorkerProcess {
            let mut command = Command::new(program);
            command.args(args);
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let child = command.spawn().expect("spawn lifecycle fixture");
            let pid = child.id() as libc::pid_t;
            std::mem::forget(child);
            MacWorkerProcess {
                state: Mutex::new(ProcessState {
                    pid,
                    reaped: false,
                    group_retired: false,
                }),
                parent_control: Mutex::new(std::fs::File::open("/dev/null").unwrap()),
            }
        }

        fn await_terminal(process: &MacWorkerProcess) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while process.is_running().unwrap() {
                assert!(Instant::now() < deadline, "lifecycle fixture did not exit");
                std::thread::yield_now();
            }
        }

        #[test]
        fn macos_worker_lifecycle_poll_reserves_terminal_child_until_group_cleanup() {
            let process = lifecycle_process("/usr/bin/true", &[]);
            await_terminal(&process);
            let state = process.state.lock().unwrap();
            assert!(!state.reaped, "polling must retain PID ownership");
            assert!(child_has_exited(state.pid).unwrap());
            drop(state);
            process.terminate().unwrap();
            process.wait().unwrap();
            assert!(!process.is_running().unwrap());
            assert!(process.state.lock().unwrap().group_retired);
        }

        #[test]
        fn macos_worker_lifecycle_live_permission_failure_remains_fenced() {
            let process = lifecycle_process("/bin/sleep", &["30"]);
            assert!(process.is_running().unwrap());
            assert!(
                process
                    .terminate_with_group_signal(|_, signal| {
                        assert_eq!(signal, libc::SIGKILL);
                        Err(std::io::Error::from_raw_os_error(libc::EPERM))
                    })
                    .is_err()
            );
            let state = process.state.lock().unwrap();
            assert!(!state.reaped);
            assert!(!state.group_retired);
            drop(state);
            process.terminate().unwrap();
            process.wait().unwrap();
        }

        #[test]
        fn macos_worker_lifecycle_surviving_group_never_receives_stale_signals() {
            let process = lifecycle_process("/usr/bin/true", &[]);
            await_terminal(&process);
            assert!(
                process
                    .terminate_with_group_signal(|_, signal| {
                        if signal == libc::SIGKILL {
                            Err(std::io::Error::from_raw_os_error(libc::EPERM))
                        } else {
                            assert_eq!(signal, 0);
                            Ok(()) // Inject a surviving group after the real leader is reaped.
                        }
                    })
                    .is_err()
            );
            assert!(process.state.lock().unwrap().reaped);
            assert!(!process.state.lock().unwrap().group_retired);
            assert!(
                process
                    .terminate_with_group_signal(|_, signal| {
                        assert_eq!(signal, 0, "never signal a group after releasing its PID");
                        Err(std::io::Error::from_raw_os_error(libc::EPERM))
                    })
                    .is_err()
            );
            assert!(!process.state.lock().unwrap().group_retired);
            // The actual disposable group is absent; only a zero probe is used.
            process.terminate().unwrap();
            process.wait().unwrap();
        }

        #[test]
        fn macos_worker_lifecycle_lost_child_ownership_never_authorizes_signals() {
            let process = lifecycle_process("/usr/bin/true", &[]);
            await_terminal(&process);
            let pid = process.state.lock().unwrap().pid;
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert!(
                process
                    .terminate_with_group_signal(|_, _| {
                        panic!("ownership loss must be detected before signalling")
                    })
                    .is_err()
            );
            assert!(process.state.lock().unwrap().reaped);
            assert!(
                process
                    .terminate_with_group_signal(|_, signal| {
                        assert_eq!(signal, 0);
                        Ok(())
                    })
                    .is_err()
            );
            assert!(!process.state.lock().unwrap().group_retired);
            process.terminate().unwrap();
            process.wait().unwrap();
        }

        #[test]
        fn macos_worker_lifecycle_wait_also_confirms_group_absence() {
            let process = lifecycle_process("/usr/bin/true", &[]);
            process.wait().unwrap();
            let state = process.state.lock().unwrap();
            assert!(state.reaped);
            assert!(state.group_retired);
        }

        #[test]
        fn macos_worker_lifecycle_signal_success_is_not_group_cleanup_confirmation() {
            let process = lifecycle_process("/usr/bin/true", &[]);
            await_terminal(&process);
            process
                .terminate_with_group_signal(|_, signal| {
                    assert_eq!(signal, libc::SIGKILL);
                    Ok(())
                })
                .unwrap();
            assert!(!process.state.lock().unwrap().group_retired);
            assert!(
                process
                    .wait_with_group_signal(|_, signal| {
                        assert_eq!(signal, 0);
                        Ok(()) // A successful signal must not hide a surviving group.
                    })
                    .is_err()
            );
            assert!(process.state.lock().unwrap().reaped);
            assert!(!process.state.lock().unwrap().group_retired);
            process.wait().unwrap();
        }

        #[test]
        fn macos_worker_lifecycle_terminal_permission_failure_requires_absent_group() {
            let process = lifecycle_process("/usr/bin/true", &[]);
            await_terminal(&process);
            let signals = Mutex::new(Vec::new());
            process
                .terminate_with_group_signal(|_, signal| {
                    signals.lock().unwrap().push(signal);
                    Err(std::io::Error::from_raw_os_error(
                        if signal == libc::SIGKILL {
                            libc::EPERM
                        } else {
                            assert_eq!(signal, 0);
                            libc::ESRCH
                        },
                    ))
                })
                .unwrap();
            assert_eq!(*signals.lock().unwrap(), [libc::SIGKILL, 0]);
            let state = process.state.lock().unwrap();
            assert!(state.reaped);
            assert!(state.group_retired);
        }

        #[test]
        fn macos_worker_lifecycle_stderr_retains_only_the_selected_descriptor() {
            let pipe = Pipe::new().unwrap();
            let stderr = stderr_descriptor(pipe.write.raw()).unwrap();
            assert_ne!(
                unsafe { libc::fcntl(stderr.raw(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            let message = b"startup failure";
            assert_eq!(
                unsafe { libc::write(stderr.raw(), message.as_ptr().cast(), message.len()) },
                message.len() as isize
            );
            drop(stderr);
            drop(pipe.write);
            let mut reader = unsafe { std::fs::File::from_raw_fd(pipe.read.into_raw()) };
            let mut observed = Vec::new();
            reader.read_to_end(&mut observed).unwrap();
            assert_eq!(observed, message);
        }

        #[test]
        fn macos_worker_lifecycle_closed_stderr_uses_null_without_global_fd_changes() {
            const CHILD_ENV: &str = "SCRIBE_PRIVATE_TEST_CLOSED_STDIO";
            if std::env::var(CHILD_ENV).as_deref() != Ok("1") {
                let mut child = Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "macos_worker_launch::platform::tests::macos_worker_lifecycle_closed_stderr_uses_null_without_global_fd_changes",
                        "--test-threads=1",
                    ])
                    .env(CHILD_ENV, "1")
                    .spawn().unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        assert!(
                            status.success(),
                            "isolated closed-stdio child failed: {status}"
                        );
                        return;
                    }
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("isolated closed-stdio child exceeded its deadline");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            // Only this disposable, exact-test subprocess loses its stdio.
            // The parallel parent test runner's descriptors are untouched.
            struct RestoreStdio([FdGuard; 3]);
            impl Drop for RestoreStdio {
                fn drop(&mut self) {
                    for (destination, source) in self.0.iter().enumerate() {
                        unsafe { libc::dup2(source.raw(), destination as c_int) };
                    }
                }
            }
            // Restore even on unwind so the child libtest harness can report
            // its real result; the closed-fd window below remains genuine.
            let _restore = RestoreStdio([
                duplicate_private_fd(libc::STDIN_FILENO).unwrap(),
                duplicate_private_fd(libc::STDOUT_FILENO).unwrap(),
                duplicate_private_fd(libc::STDERR_FILENO).unwrap(),
            ]);
            unsafe {
                libc::close(libc::STDIN_FILENO);
                libc::close(libc::STDOUT_FILENO);
                libc::close(libc::STDERR_FILENO);
            }
            let stderr = stderr_descriptor(libc::STDERR_FILENO).unwrap();
            assert!(stderr.raw() >= 3);
            assert_ne!(
                unsafe { libc::fcntl(stderr.raw(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            let message = b"startup failure";
            assert_eq!(
                unsafe { libc::write(stderr.raw(), message.as_ptr().cast(), message.len()) },
                message.len() as isize
            );
            let pipe = Pipe::new().unwrap();
            assert!(pipe.read.raw() >= 3 && pipe.write.raw() >= 3);
            let mut actions = SpawnFileActions::new().unwrap();
            actions.dup2(pipe.write.raw(), libc::STDOUT_FILENO).unwrap();
            actions.dup2(stderr.raw(), libc::STDERR_FILENO).unwrap();
            actions.close(pipe.read.raw()).unwrap();
            let mut attributes = SpawnAttributes::new().unwrap();
            attributes.process_group().unwrap();
            let (result, pid) = spawn_pwd(&actions, &attributes);
            assert_eq!(result, 0, "closed-stdio spawn failed");
            let mut child = TestChild { pid, reaped: false };
            drop(pipe.write);
            assert_eq!(
                child
                    .wait_before(Instant::now() + Duration::from_secs(2))
                    .unwrap(),
                0
            );
            let mut reader = unsafe { std::fs::File::from_raw_fd(pipe.read.into_raw()) };
            let mut output = String::new();
            reader.read_to_string(&mut output).unwrap();
            assert!(
                !output.trim().is_empty(),
                "stdout was accidentally closed by aliased pipe actions"
            );
        }

        #[test]
        fn drop_terminates_and_reaps_worker_even_after_state_lock_poisoning() {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "sleep 30"]);
            // SAFETY: this async-signal-safe call only creates a private
            // process group in the child immediately before exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let child = command.spawn().expect("spawn disposable worker");
            let pid = child.id() as libc::pid_t;
            std::mem::forget(child);
            let process = MacWorkerProcess {
                state: Mutex::new(ProcessState {
                    pid,
                    reaped: false,
                    group_retired: false,
                }),
                parent_control: Mutex::new(
                    std::fs::File::open("/dev/null").expect("open worker control fixture"),
                ),
            };

            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _guard = process.state.lock().expect("lock worker state");
                panic!("poison worker state lock");
            }));
            drop(process);

            let mut status = 0;
            // SAFETY: pid names the child that Drop must already have reaped.
            let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            assert_eq!(result, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
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
