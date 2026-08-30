//! Descriptor-bound Linux x86_64 GNU inference-worker admission and launch.
//!
//! Production admission starts at the compile-time `/usr/lib/scribe` contract.
//! The child side of `fork` performs no allocation and calls only raw,
//! async-signal-safe syscalls before `execveat(AT_EMPTY_PATH)`.

#![cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]

use std::ffi::{CStr, CString, c_char, c_int, c_long, c_uint, c_void};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex};

pub(crate) const INSTALL_ROOT: &str = "/usr/lib/scribe";
pub(crate) const WORKER_NAME: &str = "scribe-inference-worker";

const INSTALL_COMPONENTS: &[&CStr] = &[c"usr", c"lib", c"scribe"];
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;
const OPEN_RESOLVE: u64 = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS;
const O_RDONLY: u64 = 0;
const O_CLOEXEC: u64 = 0o2000000;
const O_DIRECTORY: u64 = 0o200000;
const O_NOFOLLOW: u64 = 0o400000;
const O_NONBLOCK: c_int = 0o4000;
const F_DUPFD_CLOEXEC: c_int = 1030;
const F_GETFL: c_int = 3;
const F_SETFL: c_int = 4;
const AT_EMPTY_PATH: c_int = 0x1000;
const PR_SET_PDEATHSIG: c_int = 1;
const PR_SET_NO_NEW_PRIVS: c_int = 38;
const SIGKILL: c_int = 9;
const WNOHANG: c_int = 1;
const EINTR: c_int = 4;
const ESRCH: c_int = 3;
const ECHILD: c_int = 10;
const POLLIN: i16 = 0x001;
const POLLHUP: i16 = 0x010;
const POLLERR: i16 = 0x008;
const SYS_EXECVEAT: c_long = 322;
const SYS_CLOSE_RANGE: c_long = 436;
const SYS_OPENAT2: c_long = 437;
const CHILD_FD_BASE: c_int = 64;
const EXEC_FD: c_int = 3;
const ROOT_FD: c_int = 4;
const LIVENESS_FD: c_int = 5;
const ERROR_FD: c_int = 6;
const ERROR_RECORD_LEN: usize = 8;
const EXEC_HANDSHAKE_TIMEOUT_MS: c_int = 5_000;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
    fn pipe2(pipefd: *mut c_int, flags: c_int) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn fork() -> c_int;
    fn getpid() -> c_int;
    fn getppid() -> c_int;
    fn setpgid(pid: c_int, pgid: c_int) -> c_int;
    fn prctl(option: c_int, ...) -> c_int;
    fn fchdir(fd: c_int) -> c_int;
    fn dup3(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize;
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn _exit(status: c_int) -> !;
    fn __errno_location() -> *mut c_int;
    #[cfg(test)]
    fn fchown(fd: c_int, owner: c_uint, group: c_uint) -> c_int;
}

pub(crate) trait LinuxExecAuthority: Send + Sync {
    fn executable_fd(&self) -> RawFd;
    fn dependency_root_fd(&self) -> RawFd;
    fn recheck(&self) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
}

impl FileIdentity {
    fn from_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
        })
    }
}

struct RootBinding {
    anchor: File,
    components: Vec<CString>,
    require_root_owner: bool,
}

pub(crate) struct InstalledWorkerAuthority {
    binding: RootBinding,
    root: File,
    executable: File,
    root_identity: FileIdentity,
    executable_identity: FileIdentity,
    expected_sha256: [u8; 32],
}

pub(crate) struct InstalledWorkerIdentity {
    pub(crate) length: u64,
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) sha256: String,
}

impl InstalledWorkerAuthority {
    pub(crate) fn open_production(expected_sha256: &str) -> io::Result<Arc<Self>> {
        if expected_sha256.is_empty() {
            return Err(invalid(
                "Linux packaged worker requires a compile-time release SHA-256",
            ));
        }
        let anchor = File::open("/")?;
        let components = INSTALL_COMPONENTS
            .iter()
            .map(|component| CString::new(component.to_bytes()).expect("static component"))
            .collect();
        Self::open_from_binding(
            RootBinding {
                anchor,
                components,
                require_root_owner: true,
            },
            expected_sha256,
        )
    }

    #[cfg(test)]
    pub(crate) fn open_test_fixture(
        parent: &std::path::Path,
        root_name: &str,
        expected_sha256: &str,
    ) -> io::Result<Arc<Self>> {
        let anchor = File::open(parent)?;
        Self::open_from_binding(
            RootBinding {
                anchor,
                components: vec![
                    CString::new(root_name).map_err(|_| invalid("invalid root name"))?,
                ],
                require_root_owner: false,
            },
            expected_sha256,
        )
    }

    fn open_from_binding(binding: RootBinding, expected_sha256: &str) -> io::Result<Arc<Self>> {
        let expected_sha256 = parse_sha256(expected_sha256)?;
        let root = open_bound_directory(&binding)?;
        validate_directory(&root, binding.require_root_owner, "worker authority root")?;
        let executable = openat2_file(root.as_raw_fd(), c"scribe-inference-worker")?;
        validate_executable(
            &executable,
            binding.require_root_owner,
            "packaged inference worker",
        )?;
        let actual_sha256 = sha256_file(&executable)?;
        if actual_sha256 != expected_sha256 {
            return Err(invalid("packaged inference worker SHA-256 mismatch"));
        }
        let authority = Arc::new(Self {
            root_identity: FileIdentity::from_file(&root)?,
            executable_identity: FileIdentity::from_file(&executable)?,
            binding,
            root,
            executable,
            expected_sha256,
        });
        authority.recheck()?;
        Ok(authority)
    }

    pub(crate) fn identity(&self) -> InstalledWorkerIdentity {
        InstalledWorkerIdentity {
            length: self.executable_identity.length,
            device: self.executable_identity.device,
            inode: self.executable_identity.inode,
            sha256: self
                .expected_sha256
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        }
    }

    pub(crate) fn duplicate_executable(&self) -> io::Result<File> {
        self.executable.try_clone()
    }
}

impl LinuxExecAuthority for InstalledWorkerAuthority {
    fn executable_fd(&self) -> RawFd {
        self.executable.as_raw_fd()
    }

    fn dependency_root_fd(&self) -> RawFd {
        self.root.as_raw_fd()
    }

    fn recheck(&self) -> io::Result<()> {
        validate_directory(
            &self.root,
            self.binding.require_root_owner,
            "retained worker authority root",
        )?;
        validate_executable(
            &self.executable,
            self.binding.require_root_owner,
            "retained packaged inference worker",
        )?;
        if FileIdentity::from_file(&self.root)? != self.root_identity
            || FileIdentity::from_file(&self.executable)? != self.executable_identity
        {
            return Err(invalid("retained Linux worker authority identity changed"));
        }
        if sha256_file(&self.executable)? != self.expected_sha256 {
            return Err(invalid("retained Linux worker bytes changed"));
        }

        let rebound_root = open_bound_directory(&self.binding)?;
        validate_directory(
            &rebound_root,
            self.binding.require_root_owner,
            "reopened worker authority root",
        )?;
        if FileIdentity::from_file(&rebound_root)? != self.root_identity {
            return Err(invalid(
                "Linux worker authority root was renamed or replaced",
            ));
        }
        let rebound_executable =
            openat2_file(rebound_root.as_raw_fd(), c"scribe-inference-worker")?;
        validate_executable(
            &rebound_executable,
            self.binding.require_root_owner,
            "reopened packaged inference worker",
        )?;
        if FileIdentity::from_file(&rebound_executable)? != self.executable_identity
            || sha256_file(&rebound_executable)? != self.expected_sha256
        {
            return Err(invalid("Linux worker executable was renamed or replaced"));
        }
        Ok(())
    }
}

fn open_bound_directory(binding: &RootBinding) -> io::Result<File> {
    let mut current = binding.anchor.try_clone()?;
    validate_directory(
        &current,
        binding.require_root_owner,
        "worker authority ancestor",
    )?;
    for component in &binding.components {
        let next = openat2_directory(current.as_raw_fd(), component)?;
        validate_directory(
            &next,
            binding.require_root_owner,
            "worker authority ancestor",
        )?;
        current = next;
    }
    Ok(current)
}

fn openat2_directory(parent: RawFd, name: &CStr) -> io::Result<File> {
    openat2(
        parent,
        name,
        O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW,
    )
}

fn openat2_file(parent: RawFd, name: &CStr) -> io::Result<File> {
    openat2(parent, name, O_RDONLY | O_CLOEXEC | O_NOFOLLOW)
}

fn openat2(parent: RawFd, name: &CStr, flags: u64) -> io::Result<File> {
    let how = OpenHow {
        flags,
        mode: 0,
        resolve: OPEN_RESOLVE,
    };
    // SAFETY: `name` and `how` remain live for the duration of the syscall.
    let raw = unsafe {
        syscall(
            SYS_OPENAT2,
            parent,
            name.as_ptr(),
            &how as *const OpenHow,
            size_of::<OpenHow>(),
        )
    };
    if raw < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: a successful openat2 returns one owned descriptor.
        Ok(unsafe { File::from_raw_fd(raw as RawFd) })
    }
}

fn validate_directory(file: &File, require_root_owner: bool, label: &str) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(invalid(format!("{label} is not a directory")));
    }
    validate_owner_and_mode(&metadata, require_root_owner, label)
}

fn validate_executable(file: &File, require_root_owner: bool, label: &str) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(invalid(format!(
            "{label} must be a single-link regular file"
        )));
    }
    validate_owner_and_mode(&metadata, require_root_owner, label)?;
    if metadata.mode() & 0o111 == 0 {
        return Err(invalid(format!("{label} is not executable")));
    }
    Ok(())
}

fn validate_owner_and_mode(
    metadata: &std::fs::Metadata,
    require_root_owner: bool,
    label: &str,
) -> io::Result<()> {
    if require_root_owner && (metadata.uid() != 0 || metadata.gid() != 0) {
        return Err(invalid(format!("{label} is not owned by root:root")));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(invalid(format!("{label} is group- or other-writable")));
    }
    Ok(())
}

pub(crate) struct LinuxSpawnedWorker {
    pub(crate) stdin: File,
    pub(crate) stdout: File,
    pub(crate) process: LinuxWorkerProcess,
}

struct ProcessState {
    pid: c_int,
    reaped: bool,
}

pub(crate) struct LinuxWorkerProcess {
    state: Mutex<ProcessState>,
    parent_control: Mutex<File>,
    _authority: Arc<dyn LinuxExecAuthority>,
}

impl LinuxWorkerProcess {
    pub(crate) fn is_running(&self) -> io::Result<bool> {
        let mut state = lock(&self.state)?;
        if state.reaped {
            return Ok(false);
        }
        let mut status = 0;
        // SAFETY: pid names this object's child and status is writable.
        let result = unsafe { waitpid(state.pid, &mut status, WNOHANG) };
        if result == 0 {
            Ok(true)
        } else if result == state.pid {
            state.reaped = true;
            Ok(false)
        } else {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ECHILD) {
                state.reaped = true;
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    pub(crate) fn request_cooperative_cancel(&self) -> io::Result<bool> {
        let mut control = lock(&self.parent_control)?;
        control.write(&[b'C']).map(|written| written == 1)
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        let state = lock(&self.state)?;
        if state.reaped {
            return Ok(());
        }
        // Negative pid targets the process group established before exec.
        if unsafe { kill(-state.pid, SIGKILL) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) fn wait(&self) -> io::Result<()> {
        let mut state = lock(&self.state)?;
        reap(&mut state)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> io::Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| invalid("Linux worker lock was poisoned"))
}

fn reap(state: &mut ProcessState) -> io::Result<()> {
    if state.reaped {
        return Ok(());
    }
    loop {
        let mut status = 0;
        // SAFETY: pid names this object's child and status is writable.
        let result = unsafe { waitpid(state.pid, &mut status, 0) };
        if result == state.pid {
            state.reaped = true;
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(EINTR) {
            continue;
        }
        if error.raw_os_error() == Some(ECHILD) {
            state.reaped = true;
            return Ok(());
        }
        return Err(error);
    }
}

pub(crate) fn launch_verified_worker(
    authority: Arc<dyn LinuxExecAuthority>,
    worker_flag: &str,
    bindings: &[(String, String)],
) -> io::Result<LinuxSpawnedWorker> {
    authority.recheck()?;
    #[cfg(not(test))]
    let prepared = PreparedLaunch::new(
        &*authority,
        worker_flag,
        bindings,
        EXEC_HANDSHAKE_TIMEOUT_MS,
    )?;
    #[cfg(test)]
    let prepared = PreparedLaunch::new(
        &*authority,
        worker_flag,
        bindings,
        EXEC_HANDSHAKE_TIMEOUT_MS,
        0,
    )?;
    authority.recheck()?;
    prepared.fork_exec(authority)
}

#[cfg(test)]
fn launch_verified_worker_inner(
    authority: Arc<dyn LinuxExecAuthority>,
    worker_flag: &str,
    bindings: &[(String, String)],
    timeout_ms: c_int,
    child_test_action: u32,
) -> io::Result<LinuxSpawnedWorker> {
    authority.recheck()?;
    let prepared = PreparedLaunch::new(
        &*authority,
        worker_flag,
        bindings,
        timeout_ms,
        child_test_action,
    )?;
    authority.recheck()?;
    prepared.fork_exec(authority)
}

struct PreparedLaunch {
    child_stdin: Fd,
    parent_stdin: Fd,
    parent_stdout: Fd,
    child_stdout: Fd,
    child_liveness: Fd,
    parent_liveness: Fd,
    error_read: Fd,
    error_write: Fd,
    executable: Fd,
    root: Fd,
    argv_storage: Vec<CString>,
    argv: Vec<*const c_char>,
    environment_storage: Vec<CString>,
    environment: Vec<*const c_char>,
    timeout_ms: c_int,
    #[cfg(test)]
    child_test_action: u32,
}

impl PreparedLaunch {
    fn new(
        authority: &dyn LinuxExecAuthority,
        worker_flag: &str,
        bindings: &[(String, String)],
        timeout_ms: c_int,
        #[cfg(test)] child_test_action: u32,
    ) -> io::Result<Self> {
        if timeout_ms <= 0 {
            return Err(invalid("Linux worker exec timeout must be positive"));
        }
        let (child_stdin, parent_stdin) = pipe_cloexec()?;
        let (parent_stdout, child_stdout) = pipe_cloexec()?;
        let (child_liveness, parent_liveness) = pipe_cloexec()?;
        let (error_read, error_write) = pipe_cloexec()?;
        set_nonblocking(error_read.raw())?;

        let executable = duplicate_high(authority.executable_fd())?;
        let root = duplicate_high(authority.dependency_root_fd())?;
        let child_stdin = duplicate_high(child_stdin.raw())?;
        let child_stdout = duplicate_high(child_stdout.raw())?;
        let child_liveness = duplicate_high(child_liveness.raw())?;
        let error_write = duplicate_high(error_write.raw())?;

        let argv_storage = vec![
            CString::new(WORKER_NAME).expect("static worker name"),
            CString::new(worker_flag).map_err(|_| invalid("worker flag contains NUL"))?,
        ];
        let argv = pointer_vector(&argv_storage);
        let mut fields = vec![
            "PATH=/usr/bin:/bin".to_owned(),
            "LANG=C.UTF-8".to_owned(),
            format!("SCRIBE_PRIVATE_PARENT_LIVENESS={LIVENESS_FD}"),
        ];
        for (name, value) in bindings {
            validate_binding(name, value)?;
            fields.push(format!("{name}={value}"));
        }
        fields.sort();
        let environment_storage = fields
            .into_iter()
            .map(|field| CString::new(field).map_err(|_| invalid("environment contains NUL")))
            .collect::<io::Result<Vec<_>>>()?;
        let environment = pointer_vector(&environment_storage);

        Ok(Self {
            child_stdin,
            parent_stdin,
            parent_stdout,
            child_stdout,
            child_liveness,
            parent_liveness,
            error_read,
            error_write,
            executable,
            root,
            argv_storage,
            argv,
            environment_storage,
            environment,
            timeout_ms,
            #[cfg(test)]
            child_test_action,
        })
    }

    fn fork_exec(self, authority: Arc<dyn LinuxExecAuthority>) -> io::Result<LinuxSpawnedWorker> {
        // Touch backing storage here so its lifetime is visibly tied to the fork call.
        let _prepared_storage = (&self.argv_storage, &self.environment_storage);
        // SAFETY: all allocations and descriptors required by the child are prepared.
        let parent_pid = unsafe { getpid() };
        let pid = unsafe { fork() };
        if pid < 0 {
            return Err(io::Error::last_os_error());
        }
        if pid == 0 {
            unsafe { self.child_exec(parent_pid) }
        }

        drop(self.child_stdin);
        drop(self.child_stdout);
        drop(self.child_liveness);
        drop(self.error_write);
        drop(self.executable);
        drop(self.root);

        if let Err(error) = wait_for_exec(self.error_read.raw(), self.timeout_ms) {
            cleanup_failed_child(pid);
            return Err(error);
        }
        drop(self.error_read);

        // SAFETY: these guards uniquely own the parent pipe ends.
        let stdin = unsafe { File::from_raw_fd(self.parent_stdin.into_raw()) };
        let stdout = unsafe { File::from_raw_fd(self.parent_stdout.into_raw()) };
        let parent_control = unsafe { File::from_raw_fd(self.parent_liveness.into_raw()) };
        Ok(LinuxSpawnedWorker {
            stdin,
            stdout,
            process: LinuxWorkerProcess {
                state: Mutex::new(ProcessState { pid, reaped: false }),
                parent_control: Mutex::new(parent_control),
                _authority: authority,
            },
        })
    }

    unsafe fn child_exec(&self, parent_pid: c_int) -> ! {
        // SAFETY: this is the post-fork child. Every referenced descriptor and
        // pointer was prepared before fork and no other Rust code is invoked.
        unsafe {
            child_dup(self.error_write.raw(), ERROR_FD, true, 1);
            child_dup(self.executable.raw(), EXEC_FD, true, 2);
            child_dup(self.root.raw(), ROOT_FD, true, 3);
            child_dup(self.child_liveness.raw(), LIVENESS_FD, false, 4);
            child_dup(self.child_stdin.raw(), 0, false, 5);
            child_dup(self.child_stdout.raw(), 1, false, 6);
            #[cfg(test)]
            {
                if self.child_test_action == 1 {
                    child_fail(90);
                }
                if self.child_test_action == 2 {
                    let _ = poll(std::ptr::null_mut(), 0, -1);
                    child_fail(91);
                }
            }
            if setpgid(0, 0) == -1 {
                child_fail(7);
            }
            if prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0) == -1 {
                child_fail(8);
            }
            if getppid() != parent_pid {
                child_fail(9);
            }
            if prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1 {
                child_fail(10);
            }
            if fchdir(ROOT_FD) == -1 {
                child_fail(11);
            }
            if syscall(SYS_CLOSE_RANGE, 7_u32, c_uint::MAX, 0_u32) == -1 {
                child_fail(12);
            }
            close(ROOT_FD);
            syscall(
                SYS_EXECVEAT,
                EXEC_FD,
                c"".as_ptr(),
                self.argv.as_ptr(),
                self.environment.as_ptr(),
                AT_EMPTY_PATH,
            );
            child_fail(13)
        }
    }
}

fn validate_binding(name: &str, value: &str) -> io::Result<()> {
    if !name.starts_with("SCRIBE_PRIVATE_")
        || name == "SCRIBE_PRIVATE_PARENT_LIVENESS"
        || name.is_empty()
        || name.len() > 128
        || name.contains(['=', '\0'])
        || value.len() > 4096
        || value.contains('\0')
    {
        return Err(invalid("invalid private worker environment binding"));
    }
    Ok(())
}

unsafe fn child_dup(source: RawFd, destination: RawFd, cloexec: bool, stage: u32) {
    let flags = if cloexec { O_CLOEXEC as c_int } else { 0 };
    // SAFETY: source is a prepared high descriptor and destination is fixed.
    unsafe {
        if dup3(source, destination, flags) == -1 {
            child_fail(stage);
        }
    }
}

unsafe fn child_fail(stage: u32) -> ! {
    // SAFETY: errno is thread-local in the post-fork child; ERROR_FD is the
    // fixed CLOEXEC pipe. Both functions are async-signal-safe raw operations.
    unsafe {
        let errno = *__errno_location() as u32;
        let stage = stage.to_le_bytes();
        let errno = errno.to_le_bytes();
        let record = [
            stage[0], stage[1], stage[2], stage[3], errno[0], errno[1], errno[2], errno[3],
        ];
        let _ = write(ERROR_FD, record.as_ptr().cast(), record.len());
        _exit(127)
    }
}

fn wait_for_exec(error_fd: RawFd, timeout_ms: c_int) -> io::Result<()> {
    let deadline = std::time::Instant::now()
        .checked_add(std::time::Duration::from_millis(timeout_ms as u64))
        .ok_or_else(|| invalid("Linux worker exec timeout overflowed"))?;
    let mut record = [0_u8; ERROR_RECORD_LEN];
    let mut offset = 0;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Linux worker exec handshake timed out",
            ));
        }
        let poll_timeout = remaining.as_millis().clamp(1, c_int::MAX as u128) as c_int;
        let mut descriptor = PollFd {
            fd: error_fd,
            events: POLLIN | POLLHUP | POLLERR,
            revents: 0,
        };
        // SAFETY: descriptor points to one writable pollfd.
        let result = unsafe { poll(&mut descriptor, 1, poll_timeout) };
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Linux worker exec handshake timed out",
            ));
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(EINTR) {
                continue;
            }
            return Err(error);
        }
        // SAFETY: the destination slice is writable for its remaining length.
        let count = unsafe {
            read(
                error_fd,
                record[offset..].as_mut_ptr().cast(),
                record.len() - offset,
            )
        };
        if count > 0 {
            offset += count as usize;
            if offset == record.len() {
                let stage = u32::from_le_bytes(record[..4].try_into().unwrap());
                let errno = i32::from_le_bytes(record[4..].try_into().unwrap());
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Linux worker child setup failed at stage {stage} (errno {errno})"),
                ));
            }
            continue;
        }
        if count == 0 {
            return if offset == 0 {
                Ok(())
            } else {
                Err(invalid("truncated Linux worker exec error record"))
            };
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            continue;
        }
        return Err(error);
    }
}

fn cleanup_failed_child(pid: c_int) {
    // The child may have failed before setpgid, so target both group and pid.
    unsafe {
        kill(-pid, SIGKILL);
        kill(pid, SIGKILL);
    }
    loop {
        let result = unsafe { waitpid(pid, std::ptr::null_mut(), 0) };
        if result == pid {
            return;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(EINTR) {
            return;
        }
    }
}

struct Fd(RawFd);

impl Fd {
    fn raw(&self) -> RawFd {
        self.0
    }

    fn into_raw(mut self) -> RawFd {
        let raw = self.0;
        self.0 = -1;
        raw
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        if self.0 >= 0 {
            unsafe { close(self.0) };
        }
    }
}

fn pipe_cloexec() -> io::Result<(Fd, Fd)> {
    let mut descriptors = [-1; 2];
    // SAFETY: descriptors is writable for two integers.
    if unsafe { pipe2(descriptors.as_mut_ptr(), O_CLOEXEC as c_int) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((Fd(descriptors[0]), Fd(descriptors[1])))
}

fn duplicate_high(source: RawFd) -> io::Result<Fd> {
    // SAFETY: fcntl duplicates a live descriptor and returns owned state.
    let raw = unsafe { fcntl(source, F_DUPFD_CLOEXEC, CHILD_FD_BASE) };
    if raw < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(Fd(raw))
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { fcntl(fd, F_GETFL) };
    if flags < 0 || unsafe { fcntl(fd, F_SETFL, flags | O_NONBLOCK) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn pointer_vector(storage: &[CString]) -> Vec<*const c_char> {
    storage
        .iter()
        .map(|value| value.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect()
}

fn parse_sha256(value: &str) -> io::Result<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("expected a 64-character SHA-256 digest"));
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        digest[index] = (hex(pair[0])? << 4) | hex(pair[1])?;
    }
    Ok(digest)
}

fn hex(byte: u8) -> io::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(invalid("SHA-256 digest must be lowercase hexadecimal")),
    }
}

fn sha256_file(file: &File) -> io::Result<[u8; 32]> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut sha = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sha.update(&buffer[..count]);
    }
    Ok(sha.finish())
}

struct Sha256 {
    state: [u32; 8],
    block: [u8; 64],
    used: usize,
    bytes: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            used: 0,
            bytes: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) {
        self.bytes = self.bytes.wrapping_add(input.len() as u64);
        while !input.is_empty() {
            let count = (64 - self.used).min(input.len());
            self.block[self.used..self.used + count].copy_from_slice(&input[..count]);
            self.used += count;
            input = &input[count..];
            if self.used == 64 {
                self.compress();
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bit_length = self.bytes.wrapping_mul(8);
        self.block[self.used] = 0x80;
        self.used += 1;
        if self.used > 56 {
            self.block[self.used..].fill(0);
            self.compress();
            self.used = 0;
        }
        self.block[self.used..56].fill(0);
        self.block[56..].copy_from_slice(&bit_length.to_be_bytes());
        self.compress();
        let mut output = [0_u8; 32];
        for (chunk, word) in output.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        output
    }

    fn compress(&mut self) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut words = [0_u32; 64];
        for (index, chunk) in self.block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes(chunk.try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (state, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(value);
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        worker: PathBuf,
        digest: String,
    }

    impl Fixture {
        fn new() -> Self {
            Self::from_executable(std::env::current_exe().unwrap())
        }

        fn launcher() -> Self {
            Self::from_executable(
                std::env::var_os("SCRIBE_LINUX_WORKER_FIXTURE")
                    .expect("test script must set SCRIBE_LINUX_WORKER_FIXTURE"),
            )
        }

        fn from_executable(executable: impl AsRef<Path>) -> Self {
            let base = std::env::temp_dir().join(format!(
                "scribe-linux-authority-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = base.join("scribe");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            let worker = root.join(WORKER_NAME);
            std::fs::copy(executable, &worker).unwrap();
            std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o555)).unwrap();
            let digest = digest(&worker);
            Self {
                base,
                root,
                worker,
                digest,
            }
        }

        fn open(&self) -> io::Result<Arc<InstalledWorkerAuthority>> {
            InstalledWorkerAuthority::open_test_fixture(&self.base, "scribe", &self.digest)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn digest(path: &Path) -> String {
        let file = File::open(path).unwrap();
        sha256_file(&file)
            .unwrap()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn sha256_matches_known_vector() {
        let fixture = Fixture::new();
        let vector = fixture.base.join("vector");
        std::fs::write(&vector, b"abc").unwrap();
        assert_eq!(
            digest(&vector),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn exact_descriptor_authority_opens_and_rechecks() {
        let fixture = Fixture::new();
        let authority = fixture.open().unwrap();
        authority.recheck().unwrap();
        assert_eq!(
            FileIdentity::from_file(&authority.executable).unwrap(),
            authority.executable_identity
        );
    }

    #[test]
    fn wrong_digest_mode_name_symlink_and_hardlink_are_rejected() {
        let fixture = Fixture::new();
        assert!(
            InstalledWorkerAuthority::open_test_fixture(&fixture.base, "scribe", &"00".repeat(32))
                .is_err()
        );

        std::fs::set_permissions(&fixture.worker, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(fixture.open().is_err());
        std::fs::set_permissions(&fixture.worker, std::fs::Permissions::from_mode(0o555)).unwrap();

        let renamed = fixture.root.join("wrong-name");
        std::fs::rename(&fixture.worker, &renamed).unwrap();
        assert!(fixture.open().is_err());
        std::fs::rename(&renamed, &fixture.worker).unwrap();

        let hardlink = fixture.root.join("hardlink");
        std::fs::hard_link(&fixture.worker, &hardlink).unwrap();
        assert!(fixture.open().is_err());
        std::fs::remove_file(&hardlink).unwrap();

        let real = fixture.root.join("real-worker");
        std::fs::rename(&fixture.worker, &real).unwrap();
        symlink(&real, &fixture.worker).unwrap();
        assert!(fixture.open().is_err());
    }

    #[test]
    fn retained_authority_rejects_in_place_mutation_and_path_replacement() {
        let fixture = Fixture::new();
        let authority = fixture.open().unwrap();
        std::fs::set_permissions(&fixture.worker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.worker)
            .unwrap();
        file.write_all(b"mutation").unwrap();
        drop(file);
        assert!(authority.recheck().is_err());

        let fixture = Fixture::new();
        let authority = fixture.open().unwrap();
        let displaced = fixture.root.join("displaced");
        std::fs::rename(&fixture.worker, &displaced).unwrap();
        std::fs::copy(&displaced, &fixture.worker).unwrap();
        std::fs::set_permissions(&fixture.worker, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(authority.recheck().is_err());
    }

    #[test]
    fn writable_root_and_magic_link_root_are_rejected() {
        let fixture = Fixture::new();
        std::fs::set_permissions(&fixture.root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(fixture.open().is_err());

        let fixture = Fixture::new();
        let magic = fixture.base.join("magic-root");
        symlink(
            format!("/proc/self/fd/{}", fixture.open().unwrap().root.as_raw_fd()),
            &magic,
        )
        .unwrap();
        assert!(
            InstalledWorkerAuthority::open_test_fixture(
                &fixture.base,
                "magic-root",
                &fixture.digest
            )
            .is_err()
        );
    }

    #[test]
    fn non_root_worker_owner_is_rejected() {
        let fixture = Fixture::new();
        let worker = File::open(&fixture.worker).unwrap();
        if worker.metadata().unwrap().uid() == 0 {
            assert_eq!(unsafe { fchown(worker.as_raw_fd(), 1, 1) }, 0);
        }
        assert_ne!(worker.metadata().unwrap().uid(), 0);
        assert!(validate_executable(&worker, true, "owner fixture").is_err());
    }

    fn launch_fixture(fixture: &Fixture, mode: &str) -> io::Result<LinuxSpawnedWorker> {
        let authority: Arc<dyn LinuxExecAuthority> = fixture.open()?;
        launch_verified_worker(
            authority,
            "--scribe-inference-worker",
            &[("SCRIBE_PRIVATE_TEST_MODE".to_owned(), mode.to_owned())],
        )
    }

    #[test]
    fn execveat_success_sanitizes_environment_and_bounds_inherited_fds() {
        // SAFETY: the standalone script runs these tests on one thread.
        unsafe {
            std::env::set_var("LD_PRELOAD", "/tmp/hostile.so");
            std::env::set_var("SCRIBE_PRIVATE_HOSTILE", "not-inherited");
        }
        let fixture = Fixture::launcher();
        let mut spawned = launch_fixture(&fixture, "inspect").unwrap();
        spawned.stdin.write_all(b"hello").unwrap();
        drop(spawned.stdin);
        let mut output = String::new();
        spawned.stdout.read_to_string(&mut output).unwrap();
        spawned.process.wait().unwrap();
        assert!(output.contains("ARGS=--scribe-inference-worker"));
        assert!(
            output
                .contains("ENV=LANG,PATH,SCRIBE_PRIVATE_PARENT_LIVENESS,SCRIBE_PRIVATE_TEST_MODE")
        );
        assert!(!output.contains("LD_PRELOAD"));
        assert!(!output.contains("SCRIBE_PRIVATE_HOSTILE"));
        assert!(output.contains(&format!("CWD={}", fixture.root.display())));
        assert!(output.contains("INPUT=hello"));
        let fds = output
            .lines()
            .find_map(|line| line.strip_prefix("FDS="))
            .unwrap()
            .split(',')
            .map(|value| value.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert!(
            fds.iter().all(|fd| *fd <= 5),
            "unexpected inherited FDs: {fds:?}"
        );
        assert!(fds.contains(&LIVENESS_FD));
    }

    struct RawAuthority {
        executable: File,
        root: File,
        replacement: Option<File>,
        rechecks: AtomicUsize,
    }

    impl LinuxExecAuthority for RawAuthority {
        fn executable_fd(&self) -> RawFd {
            self.executable.as_raw_fd()
        }

        fn dependency_root_fd(&self) -> RawFd {
            self.root.as_raw_fd()
        }

        fn recheck(&self) -> io::Result<()> {
            if self.rechecks.fetch_add(1, Ordering::SeqCst) == 1 {
                if let Some(replacement) = &self.replacement {
                    // SAFETY: dup3 atomically replaces only the authority's
                    // externally visible descriptor after preparation.
                    if unsafe {
                        dup3(
                            replacement.as_raw_fd(),
                            self.executable.as_raw_fd(),
                            O_CLOEXEC as c_int,
                        )
                    } == -1
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn external_authority_fd_swap_cannot_redirect_prepared_exec_descriptor() {
        let fixture = Fixture::launcher();
        let authority: Arc<dyn LinuxExecAuthority> = Arc::new(RawAuthority {
            executable: File::open(&fixture.worker).unwrap(),
            root: File::open(&fixture.root).unwrap(),
            replacement: Some(File::open("/bin/false").unwrap()),
            rechecks: AtomicUsize::new(0),
        });
        let mut spawned = launch_verified_worker(
            authority,
            "--scribe-inference-worker",
            &[("SCRIBE_PRIVATE_TEST_MODE".to_owned(), "inspect".to_owned())],
        )
        .unwrap();
        drop(spawned.stdin);
        let mut output = String::new();
        spawned.stdout.read_to_string(&mut output).unwrap();
        spawned.process.wait().unwrap();
        assert!(output.contains("ARGS=--scribe-inference-worker"));
    }

    #[test]
    fn child_setup_exec_errors_and_hangs_are_bounded_and_reaped() {
        let fixture = Fixture::launcher();
        let authority: Arc<dyn LinuxExecAuthority> = fixture.open().unwrap();
        let forced = launch_verified_worker_inner(
            Arc::clone(&authority),
            "--scribe-inference-worker",
            &[],
            500,
            1,
        )
        .err()
        .expect("forced child failure");
        assert!(forced.to_string().contains("stage 90 (errno"));

        let started = Instant::now();
        let timeout =
            launch_verified_worker_inner(authority, "--scribe-inference-worker", &[], 150, 2)
                .err()
                .expect("forced child hang");
        assert_eq!(timeout.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));

        let directory_authority: Arc<dyn LinuxExecAuthority> = Arc::new(RawAuthority {
            executable: File::open(&fixture.root).unwrap(),
            root: File::open(&fixture.root).unwrap(),
            replacement: None,
            rechecks: AtomicUsize::new(0),
        });
        let exec_error = launch_verified_worker_inner(
            directory_authority,
            "--scribe-inference-worker",
            &[],
            500,
            0,
        )
        .err()
        .expect("directory exec must fail");
        assert!(exec_error.to_string().contains("stage 13 (errno"));

        let mut status = 0;
        assert_eq!(unsafe { waitpid(-1, &mut status, WNOHANG) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(ECHILD));
    }

    #[test]
    fn cooperative_cancel_and_hung_process_termination_reap_workers() {
        let fixture = Fixture::launcher();
        let spawned = launch_fixture(&fixture, "cooperative").unwrap();
        assert!(spawned.process.request_cooperative_cancel().unwrap());
        spawned.process.wait().unwrap();
        assert!(!spawned.process.is_running().unwrap());

        let spawned = launch_fixture(&fixture, "hang").unwrap();
        assert!(spawned.process.is_running().unwrap());
        spawned.process.terminate().unwrap();
        spawned.process.wait().unwrap();
        assert!(!spawned.process.is_running().unwrap());
    }

    #[test]
    fn termination_targets_entire_worker_process_group() {
        let fixture = Fixture::launcher();
        let spawned = launch_fixture(&fixture, "process-group").unwrap();
        let mut reader = BufReader::new(spawned.stdout.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let pids = line
            .trim()
            .strip_prefix("PIDS=")
            .unwrap()
            .split(',')
            .map(|value| value.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        spawned.process.terminate().unwrap();
        spawned.process.wait().unwrap();
        for pid in pids {
            assert!(
                wait_until_not_running(pid),
                "pid {pid} survived process-group termination"
            );
        }
    }

    fn wait_until_not_running(pid: c_int) -> bool {
        for _ in 0..100 {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
                Ok(value) if value.split_whitespace().nth(2) == Some("Z") => return true,
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        false
    }

    #[test]
    fn parent_death_signal_kills_worker_after_launcher_parent_exits() {
        let fixture = Fixture::launcher();
        let (read_end, write_end) = pipe_cloexec().unwrap();
        let launcher_pid = unsafe { fork() };
        assert!(launcher_pid >= 0);
        if launcher_pid == 0 {
            drop(read_end);
            let spawned = launch_fixture(&fixture, "hang").unwrap();
            let worker_pid = lock(&spawned.process.state).unwrap().pid;
            let bytes = worker_pid.to_le_bytes();
            unsafe {
                write(write_end.raw(), bytes.as_ptr().cast(), bytes.len());
                _exit(0);
            }
        }
        drop(write_end);
        let mut bytes = [0_u8; 4];
        let count = unsafe { read(read_end.raw(), bytes.as_mut_ptr().cast(), bytes.len()) };
        assert_eq!(count, 4);
        let worker_pid = i32::from_le_bytes(bytes);
        let mut status = 0;
        assert_eq!(
            unsafe { waitpid(launcher_pid, &mut status, 0) },
            launcher_pid
        );
        assert!(
            wait_until_not_running(worker_pid),
            "worker {worker_pid} survived its launcher's death"
        );
    }
}
