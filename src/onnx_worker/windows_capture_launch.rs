//! Windows capture-only worker launch with atomic job assignment.
//!
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST` makes the configured affinity job apply
//! before the new process is allowed to run. This module deliberately has no
//! fallback to `Command::spawn` or post-spawn assignment.

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_INSUFFICIENT_BUFFER, GENERIC_WRITE, GetLastError,
    HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::JobObjects::{
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
    QueryInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Memory::{GetProcessHeap, HEAP_ZERO_MEMORY, HeapAlloc, HeapFree};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

use super::{
    CaptureWorkerAffinity, DiagnosticProbeState, MonotonicDeadline, ParentLivenessChannel,
    WorkerProcess, windows_worker_current_directory,
};
use crate::windows_gpu_capture::affinity::PreparedCaptureAffinity;

const MAX_WINDOWS_COMMAND_LINE_UNITS: usize = 32_767;
const CLEANUP_EXIT_CODE: u32 = 1;

pub(super) struct LaunchedCaptureWorker {
    pub(super) stdin: File,
    pub(super) stdout: File,
    pub(super) process: Arc<dyn WorkerProcess>,
}

struct NativeLaunchedCaptureWorker {
    stdin: File,
    stdout: File,
    process: Arc<NativeCaptureWorkerProcess>,
}

pub(super) fn launch(
    command: &Command,
    affinity: Arc<CaptureWorkerAffinity>,
    parent_liveness: ParentLivenessChannel,
    diagnostic: Option<&Arc<DiagnosticProbeState>>,
) -> Result<LaunchedCaptureWorker> {
    let launched = launch_with_validation(
        command,
        affinity,
        parent_liveness,
        diagnostic,
        &NATIVE_LAUNCH_API,
        |prepared, affinity, process| prepared.validate_process(affinity, process),
    )?;
    Ok(LaunchedCaptureWorker {
        stdin: launched.stdin,
        stdout: launched.stdout,
        process: launched.process,
    })
}

fn launch_with_validation(
    command: &Command,
    affinity: Arc<CaptureWorkerAffinity>,
    mut parent_liveness: ParentLivenessChannel,
    diagnostic: Option<&Arc<DiagnosticProbeState>>,
    api: &NativeLaunchApi<'_>,
    validate: impl FnOnce(&PreparedCaptureAffinity, &CaptureWorkerAffinity, HANDLE) -> Result<()>,
) -> Result<NativeLaunchedCaptureWorker> {
    let application = canonical_application(command.get_program())?;
    let mut command_line = command_line(command.get_program(), command.get_args())?;
    let environment = environment_block(command)?;
    let current_directory = trusted_current_directory(command)?;
    let prepared = affinity.prepare()?;

    let (child_stdin, parent_stdin) = pipe_for_child(false)?;
    let (child_stdout, parent_stdout) = pipe_for_child(true)?;
    let child_stderr = (api.stderr)(diagnostic.is_some())?;
    let liveness = parent_liveness.child_handle();
    require_inheritable(liveness, "parent-liveness reader")?;

    let inherited = [
        child_stdin.as_raw_handle() as HANDLE,
        child_stdout.as_raw_handle() as HANDLE,
        child_stderr.as_raw_handle() as HANDLE,
        liveness,
    ];
    validate_inherited_handles(&inherited)?;
    let process_info = create_process_with_api(
        api,
        &application,
        &mut command_line,
        &environment,
        &current_directory,
        inherited,
        prepared.handle(),
    )?;

    // Adopt both returned handles before any operation that can fail.
    // SAFETY: successful CreateProcessW returned two new owned handles.
    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread) };
    if let Some(probe) = diagnostic {
        probe.process_started();
        probe.retain_worker_process_handle_raw(process_info.hProcess);
    }
    drop(thread); // The primary thread was never suspended.
    parent_liveness.child_spawned();
    drop(child_stdin);
    drop(child_stdout);
    drop(child_stderr);

    if let Err(validation) = validate(&prepared, &affinity, process_info.hProcess) {
        if let Some(probe) = diagnostic {
            probe.observe_exit_before_cleanup();
        }
        return cleanup_failed_launch(
            validation.context("atomic capture worker job validation failed"),
            &prepared,
            &process,
            diagnostic,
        );
    }

    let process = Arc::new(NativeCaptureWorkerProcess {
        process,
        affinity_job: prepared,
        parent_liveness,
        affinity,
    });
    Ok(NativeLaunchedCaptureWorker {
        stdin: parent_stdin,
        stdout: parent_stdout,
        process,
    })
}

fn create_process_with_api(
    api: &NativeLaunchApi<'_>,
    application: &[u16],
    command_line: &mut [u16],
    environment: &[u16],
    current_directory: &[u16],
    inherited: [HANDLE; 4],
    job: HANDLE,
) -> Result<PROCESS_INFORMATION> {
    let jobs = [job];
    let mut attributes = AttributeList::new(2, api)?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited.as_ptr().cast(),
        size_of_val(&inherited),
    )?;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
        jobs.as_ptr().cast(),
        size_of_val(&jobs),
    )?;

    // SAFETY: this Win32 POD is initialized field-by-field below.
    let mut startup = unsafe { zeroed::<STARTUPINFOEXW>() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = inherited[0];
    startup.StartupInfo.hStdOutput = inherited[1];
    startup.StartupInfo.hStdError = inherited[2];
    startup.lpAttributeList = attributes.as_ptr();
    (api.create_process)(
        application.as_ptr(),
        command_line.as_mut_ptr(),
        EXTENDED_STARTUPINFO_PRESENT
            | CREATE_UNICODE_ENVIRONMENT
            | CREATE_NO_WINDOW
            | CREATE_NEW_PROCESS_GROUP,
        environment.as_ptr().cast(),
        current_directory.as_ptr(),
        (&startup as *const STARTUPINFOEXW).cast(),
    )
}

struct NativeCaptureWorkerProcess {
    process: OwnedHandle,
    affinity_job: PreparedCaptureAffinity,
    parent_liveness: ParentLivenessChannel,
    affinity: Arc<CaptureWorkerAffinity>,
}

impl NativeCaptureWorkerProcess {
    fn process_handle(&self) -> HANDLE {
        self.process.as_raw_handle() as HANDLE
    }

    #[cfg(test)]
    fn close_parent_liveness_writer_for_test(&mut self) {
        if self.parent_liveness.parent_write != 0 {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(
                    self.parent_liveness.parent_write as HANDLE,
                );
            }
            self.parent_liveness.parent_write = 0;
        }
    }
}

impl WorkerProcess for NativeCaptureWorkerProcess {
    fn is_running(&self) -> Result<bool> {
        process_is_running(self.process_handle())
    }

    fn request_cooperative_cancel(&self) -> Result<bool> {
        self.parent_liveness.request_cancel()
    }

    fn terminate(&self) -> Result<()> {
        if unsafe { TerminateJobObject(self.affinity_job.handle(), CLEANUP_EXIT_CODE) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not terminate atomic capture worker job");
        }
        Ok(())
    }

    fn wait(&self) -> Result<()> {
        match unsafe { WaitForSingleObject(self.process_handle(), INFINITE) } {
            WAIT_OBJECT_0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()).context("could not wait for capture worker"),
        }
    }

    fn wait_before(&self, deadline: MonotonicDeadline) -> Result<()> {
        confirm_job_empty(self.affinity_job.handle(), self.process_handle(), deadline)
    }

    fn duplicate_observation_handle(&self) -> Result<OwnedHandle> {
        if !self.is_running()? {
            bail!("process worker exited before its observation handle was retained");
        }
        duplicate_handle(self.process_handle(), false)
            .context("could not retain atomic capture worker observation handle")
    }

    fn validate_capture_affinity(
        &self,
        expected: Option<&Arc<CaptureWorkerAffinity>>,
    ) -> Result<()> {
        match expected {
            Some(expected) if Arc::ptr_eq(expected, &self.affinity) => {
                if !self.is_running()? {
                    bail!("capture affinity worker exited before enforcement validation");
                }
                self.affinity_job
                    .validate_process(&self.affinity, self.process_handle())
            }
            _ => bail!("capture affinity enforcement is absent or rebound"),
        }
    }
}

fn cleanup_failed_launch<T>(
    original: anyhow::Error,
    job: &PreparedCaptureAffinity,
    process: &OwnedHandle,
    diagnostic: Option<&Arc<DiagnosticProbeState>>,
) -> Result<T> {
    cleanup_failed_launch_with(
        original,
        job.handle(),
        process.as_raw_handle() as HANDLE,
        diagnostic,
        &NATIVE_CLEANUP_API,
    )
}

struct CleanupApi {
    terminate: fn(HANDLE) -> Result<()>,
    confirm_empty: fn(HANDLE, HANDLE, MonotonicDeadline) -> Result<()>,
}

const NATIVE_CLEANUP_API: CleanupApi = CleanupApi {
    terminate: terminate_job,
    confirm_empty: confirm_job_empty,
};

fn terminate_job(job: HANDLE) -> Result<()> {
    if unsafe { TerminateJobObject(job, CLEANUP_EXIT_CODE) } == 0 {
        return Err(std::io::Error::last_os_error()).context("terminate capture worker job");
    }
    Ok(())
}

fn cleanup_failed_launch_with<T>(
    original: anyhow::Error,
    job: HANDLE,
    process: HANDLE,
    diagnostic: Option<&Arc<DiagnosticProbeState>>,
    api: &CleanupApi,
) -> Result<T> {
    let terminate_error = (api.terminate)(job).err();
    let deadline = diagnostic.map(|probe| probe.cleanup_deadline).map_or_else(
        || MonotonicDeadline::after_for(Duration::from_secs(2), "capture launch cleanup"),
        Ok,
    );
    let wait_error = match deadline.and_then(|deadline| (api.confirm_empty)(job, process, deadline))
    {
        Ok(()) => {
            if let Some(probe) = diagnostic {
                probe.cleanup_confirmed();
            }
            None
        }
        Err(error) => Some(error),
    };
    match (terminate_error, wait_error) {
        (None, None) => Err(original.context("created worker was terminated and reaped")),
        (Some(terminate), None) => bail!(
            "{original:#}; job termination reported {terminate}, but worker exit was confirmed"
        ),
        (terminate, Some(wait)) => {
            let terminate = terminate
                .map(|error| error.to_string())
                .unwrap_or_else(|| "ok".to_owned());
            bail!(
                "{original:#}; created worker cleanup was unconfirmed (terminate: {terminate}, reap: {wait:#})"
            )
        }
    }
}

fn confirm_job_empty(job: HANDLE, process: HANDLE, deadline: MonotonicDeadline) -> Result<()> {
    loop {
        let leader_exited = match unsafe { WaitForSingleObject(process, 0) } {
            WAIT_OBJECT_0 => true,
            WAIT_TIMEOUT => false,
            _ => {
                return Err(std::io::Error::last_os_error())
                    .context("poll capture worker leader cleanup");
            }
        };
        // SAFETY: Win32 POD may be zero initialized and the output buffer has
        // exactly the size declared to QueryInformationJobObject.
        let mut accounting = unsafe { zeroed::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() };
        let mut returned = 0;
        if unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                &mut returned,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("query capture worker job cleanup accounting");
        }
        if returned as usize != size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() {
            bail!("capture worker job cleanup accounting returned an invalid size");
        }
        if leader_exited && accounting.ActiveProcesses == 0 {
            return Ok(());
        }
        deadline.remaining()?;
        std::thread::yield_now();
    }
}

fn process_is_running(process: HANDLE) -> Result<bool> {
    match unsafe { WaitForSingleObject(process, 0) } {
        WAIT_TIMEOUT => Ok(true),
        WAIT_OBJECT_0 => Ok(false),
        _ => Err(std::io::Error::last_os_error()).context("query capture worker state"),
    }
}

fn canonical_application(program: &OsStr) -> Result<Vec<u16>> {
    let path = std::path::Path::new(program);
    if !path.is_absolute() {
        bail!("capture worker application path is not absolute");
    }
    let canonical = path
        .canonicalize()
        .context("canonicalize capture worker application path")?;
    if canonical != path {
        bail!("capture worker application path is not the verified canonical path");
    }
    wide_nul(path.as_os_str(), "application path")
}

fn trusted_current_directory(command: &Command) -> Result<Vec<u16>> {
    let directory = command
        .get_current_dir()
        .ok_or_else(|| anyhow!("capture worker current directory is missing"))?;
    if !directory.is_absolute()
        || directory.canonicalize()? != directory
        || windows_worker_current_directory()? != directory
    {
        bail!("capture worker current directory is not the canonical trusted directory");
    }
    wide_nul(directory.as_os_str(), "current directory")
}

fn command_line<'a>(program: &OsStr, args: impl Iterator<Item = &'a OsStr>) -> Result<Vec<u16>> {
    let mut line = Vec::new();
    append_quoted_argument(&mut line, program)?;
    for argument in args {
        line.push(b' ' as u16);
        append_quoted_argument(&mut line, argument)?;
    }
    line.push(0);
    if line.len() > MAX_WINDOWS_COMMAND_LINE_UNITS {
        bail!("capture worker command line exceeds the Windows UTF-16 bound");
    }
    Ok(line)
}

fn append_quoted_argument(output: &mut Vec<u16>, argument: &OsStr) -> Result<()> {
    let units = argument.encode_wide().collect::<Vec<_>>();
    if units.contains(&0) {
        bail!("capture worker argument contains NUL");
    }
    output.push(b'"' as u16);
    let mut slashes = 0usize;
    for unit in units {
        if unit == b'\\' as u16 {
            slashes += 1;
        } else {
            if unit == b'"' as u16 {
                output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            } else {
                output.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            }
            slashes = 0;
            output.push(unit);
        }
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
    Ok(())
}

fn environment_block(command: &Command) -> Result<Vec<u16>> {
    let mut entries = Vec::new();
    for (name, value) in command.get_envs() {
        let value = value.ok_or_else(|| anyhow!("capture worker environment contains removal"))?;
        let name_wide = name.encode_wide().collect::<Vec<_>>();
        let value_wide = value.encode_wide().collect::<Vec<_>>();
        if name_wide.is_empty()
            || name_wide.contains(&0)
            || name_wide.contains(&(b'=' as u16))
            || value_wide.contains(&0)
            || !name_wide
                .iter()
                .all(|unit| u8::try_from(*unit).is_ok_and(|byte| byte.is_ascii()))
        {
            bail!("capture worker environment contains a malformed name or value");
        }
        let normalized = name_wide
            .iter()
            .map(|unit| match u8::try_from(*unit) {
                Ok(byte) if byte.is_ascii_lowercase() => byte.to_ascii_uppercase() as u16,
                _ => *unit,
            })
            .collect::<Vec<_>>();
        entries.push((normalized, name_wide, value_wide));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        bail!("capture worker environment contains case-insensitive duplicate names");
    }
    let mut block = Vec::new();
    for (_, name, value) in entries {
        block.extend(name);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

fn wide_nul(value: &OsStr, label: &str) -> Result<Vec<u16>> {
    let mut wide = value.encode_wide().collect::<Vec<_>>();
    if wide.is_empty() || wide.contains(&0) {
        bail!("capture worker {label} is empty or contains NUL");
    }
    wide.push(0);
    Ok(wide)
}

fn pipe_for_child(child_writes: bool) -> Result<(OwnedHandle, File)> {
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let mut read = null_mut();
    let mut write = null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, &security, 0) } == 0 {
        return Err(std::io::Error::last_os_error()).context("create capture worker pipe");
    }
    // SAFETY: CreatePipe returned two new owned handles.
    let read = unsafe { OwnedHandle::from_raw_handle(read) };
    let write = unsafe { OwnedHandle::from_raw_handle(write) };
    let (child, parent) = if child_writes {
        (write, read)
    } else {
        (read, write)
    };
    if unsafe { SetHandleInformation(parent.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, 0) }
        == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("protect capture worker parent pipe end from inheritance");
    }
    // SAFETY: ownership of exactly the parent pipe end moves into File.
    let parent = unsafe { File::from_raw_handle(parent.into_raw_handle()) };
    Ok((child, parent))
}

fn inherited_stderr(null_stderr: bool) -> Result<OwnedHandle> {
    if null_stderr {
        let name = "NUL\0".encode_utf16().collect::<Vec<_>>();
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &security,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("open capture worker NUL stderr");
        }
        // SAFETY: CreateFileW returned one new owned handle.
        return Ok(unsafe { OwnedHandle::from_raw_handle(handle) });
    }
    inherited_stderr_from(|| unsafe { GetStdHandle(STD_ERROR_HANDLE) })
}

fn inherited_stderr_from(get_stderr: impl FnOnce() -> HANDLE) -> Result<OwnedHandle> {
    let stderr = get_stderr();
    if stderr.is_null() || stderr == INVALID_HANDLE_VALUE {
        bail!("capture worker inherited stderr is unavailable");
    }
    duplicate_handle(stderr, true).context("duplicate capture worker inherited stderr")
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(n_std_handle: u32) -> HANDLE;
}
const STD_ERROR_HANDLE: u32 = u32::MAX - 11;

fn duplicate_handle(source: HANDLE, inheritable: bool) -> Result<OwnedHandle> {
    let mut duplicate = null_mut();
    if unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source,
            GetCurrentProcess(),
            &mut duplicate,
            0,
            inheritable as i32,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
        || duplicate.is_null()
    {
        return Err(std::io::Error::last_os_error()).context("duplicate Windows handle");
    }
    // SAFETY: DuplicateHandle returned one new owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicate) })
}

fn require_inheritable(handle: HANDLE, label: &str) -> Result<()> {
    use windows_sys::Win32::Foundation::GetHandleInformation;
    let mut flags = 0;
    if unsafe { GetHandleInformation(handle, &mut flags) } == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("query {label}"));
    }
    if flags & HANDLE_FLAG_INHERIT == 0 {
        bail!("capture worker {label} is not inheritable");
    }
    Ok(())
}

fn validate_inherited_handles(handles: &[HANDLE; 4]) -> Result<()> {
    for (index, handle) in handles.iter().copied().enumerate() {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            bail!("capture worker inherited handle {index} is invalid");
        }
        require_inheritable(handle, "restricted inherited handle")?;
        if handles[..index].contains(&handle) {
            bail!("capture worker restricted handle list contains a duplicate");
        }
    }
    Ok(())
}

type AttributePointer = LPPROC_THREAD_ATTRIBUTE_LIST;

#[derive(Clone, Copy)]
struct NativeLaunchApi<'a> {
    stderr: &'a dyn Fn(bool) -> Result<OwnedHandle>,
    attribute_size: &'a dyn Fn(u32) -> Result<usize>,
    attribute_allocate: &'a dyn Fn(usize) -> Result<(HANDLE, AttributePointer)>,
    attribute_initialize: &'a dyn Fn(AttributePointer, u32, &mut usize) -> Result<()>,
    attribute_update: &'a dyn Fn(AttributePointer, usize, *const c_void, usize) -> Result<()>,
    attribute_delete: &'a dyn Fn(AttributePointer),
    attribute_free: &'a dyn Fn(HANDLE, AttributePointer),
    create_process: &'a dyn Fn(
        *const u16,
        *mut u16,
        u32,
        *const c_void,
        *const u16,
        *const STARTUPINFOW,
    ) -> Result<PROCESS_INFORMATION>,
}

const NATIVE_LAUNCH_API: NativeLaunchApi<'static> = NativeLaunchApi {
    stderr: &inherited_stderr,
    attribute_size: &native_attribute_size,
    attribute_allocate: &native_attribute_allocate,
    attribute_initialize: &native_attribute_initialize,
    attribute_update: &native_attribute_update,
    attribute_delete: &native_attribute_delete,
    attribute_free: &native_attribute_free,
    create_process: &native_create_process,
};

fn native_attribute_size(count: u32) -> Result<usize> {
    let mut bytes = 0;
    let first = unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes) };
    if first != 0 || bytes == 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        bail!("could not size capture worker process attribute list");
    }
    Ok(bytes)
}

fn native_attribute_allocate(bytes: usize) -> Result<(HANDLE, AttributePointer)> {
    let heap = unsafe { GetProcessHeap() };
    if heap.is_null() {
        return Err(std::io::Error::last_os_error()).context("get process heap");
    }
    let ptr = unsafe { HeapAlloc(heap, HEAP_ZERO_MEMORY, bytes) } as AttributePointer;
    if ptr.is_null() {
        bail!("allocate capture worker process attribute list");
    }
    Ok((heap, ptr))
}

fn native_attribute_initialize(ptr: AttributePointer, count: u32, bytes: &mut usize) -> Result<()> {
    if unsafe { InitializeProcThreadAttributeList(ptr, count, 0, bytes) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("initialize capture worker process attribute list");
    }
    Ok(())
}

fn native_attribute_update(
    ptr: AttributePointer,
    attribute: usize,
    value: *const c_void,
    bytes: usize,
) -> Result<()> {
    if unsafe { UpdateProcThreadAttribute(ptr, 0, attribute, value, bytes, null_mut(), null()) }
        == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("update capture worker process attribute list");
    }
    Ok(())
}

fn native_attribute_delete(ptr: AttributePointer) {
    unsafe { DeleteProcThreadAttributeList(ptr) };
}

fn native_attribute_free(heap: HANDLE, ptr: AttributePointer) {
    unsafe {
        HeapFree(heap, 0, ptr.cast());
    }
}

fn native_create_process(
    application: *const u16,
    command_line: *mut u16,
    flags: u32,
    environment: *const c_void,
    current_directory: *const u16,
    startup: *const STARTUPINFOW,
) -> Result<PROCESS_INFORMATION> {
    // SAFETY: output POD is writable; the caller keeps every input buffer and handle alive.
    let mut process_info = unsafe { zeroed::<PROCESS_INFORMATION>() };
    if unsafe {
        CreateProcessW(
            application,
            command_line,
            null(),
            null(),
            1,
            flags,
            environment,
            current_directory,
            startup,
            &mut process_info,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("atomic capture worker CreateProcessW failed");
    }
    Ok(process_info)
}

struct AttributeList<'a> {
    heap: HANDLE,
    ptr: AttributePointer,
    initialized: bool,
    api: &'a NativeLaunchApi<'a>,
}

impl<'a> AttributeList<'a> {
    fn new(count: u32, api: &'a NativeLaunchApi<'a>) -> Result<Self> {
        let mut bytes = (api.attribute_size)(count)?;
        let (heap, ptr) = (api.attribute_allocate)(bytes)?;
        let mut list = Self {
            heap,
            ptr,
            initialized: false,
            api,
        };
        (api.attribute_initialize)(ptr, count, &mut bytes)?;
        list.initialized = true;
        Ok(list)
    }

    fn as_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.ptr
    }

    fn update(&mut self, attribute: usize, value: *const c_void, bytes: usize) -> Result<()> {
        (self.api.attribute_update)(self.ptr, attribute, value, bytes)
    }
}

impl Drop for AttributeList<'_> {
    fn drop(&mut self) {
        if self.initialized {
            (self.api.attribute_delete)(self.ptr);
        }
        (self.api.attribute_free)(self.heap, self.ptr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::windows::ffi::OsStringExt;
    use std::sync::mpsc::sync_channel;
    use windows_sys::Win32::System::JobObjects::{
        IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessAffinityMask};

    use crate::onnx_worker::PARENT_CONTROL_CANCEL;
    use crate::windows_gpu_capture::affinity::RequestedWorkerAffinity;

    fn decode(units: &[u16]) -> String {
        String::from_utf16(units).unwrap()
    }

    #[test]
    fn crt_argv_quoting_preserves_empty_spaces_quotes_and_trailing_slashes() {
        let cases = [
            ("", "\"\""),
            ("plain", "\"plain\""),
            ("two words", "\"two words\""),
            ("a\\\"b", "\"a\\\\\\\"b\""),
            ("tail\\", "\"tail\\\\\""),
        ];
        for (input, expected) in cases {
            let mut output = Vec::new();
            append_quoted_argument(&mut output, OsStr::new(input)).unwrap();
            assert_eq!(decode(&output), expected);
        }
    }

    #[test]
    fn environment_is_sorted_case_insensitively_and_double_nul_terminated() {
        let mut command = Command::new("ignored");
        command.env_clear().env("z", "雪").env("Alpha", "one");
        let block = environment_block(&command).unwrap();
        assert_eq!(block.last(), Some(&0));
        assert_eq!(block[block.len() - 2], 0);
        assert_eq!(
            OsString::from_wide(&block[..block.len() - 1]).to_string_lossy(),
            "Alpha=one\0z=雪\0"
        );
    }

    #[test]
    fn command_environment_coalesces_case_insensitive_names_before_encoding() {
        let mut command = Command::new("ignored");
        command.env_clear().env("Path", "one").env("PATH", "two");
        // std::process::Command already coalesces names on Windows; this still
        // proves the encoded block contains one deterministic entry.
        let block = environment_block(&command).unwrap();
        assert_eq!(block.iter().filter(|unit| **unit == b'=' as u16).count(), 1);
    }

    #[test]
    fn native_input_guards_reject_untrusted_shapes_and_preserve_unicode_values() {
        assert!(canonical_application(OsStr::new("relative.exe")).is_err());
        let mut removal = Command::new("ignored");
        removal.env_remove("SYSTEMROOT");
        assert!(removal.get_envs().any(|(_, value)| value.is_none()));
        assert!(environment_block(&removal).is_err());
        let mut non_ascii_name = Command::new("ignored");
        non_ascii_name.env_clear().env("NÄME", "value");
        assert!(environment_block(&non_ascii_name).is_err());
        let mut unicode_value = Command::new("ignored");
        unicode_value.env_clear().env("NAME", "雪😀");
        let block = environment_block(&unicode_value).unwrap();
        assert!(
            OsString::from_wide(&block)
                .to_string_lossy()
                .contains("雪😀")
        );
        let oversized = OsString::from_wide(&vec![b'x' as u16; MAX_WINDOWS_COMMAND_LINE_UNITS]);
        assert!(command_line(OsStr::new("x"), [oversized.as_os_str()].into_iter()).is_err());

        let mut wrong_cwd = Command::new("ignored");
        wrong_cwd.current_dir(std::env::temp_dir().canonicalize().unwrap());
        assert!(trusted_current_directory(&wrong_cwd).is_err());
    }

    #[test]
    fn exact_handle_list_validation_rejects_invalid_duplicate_and_parent_ends() {
        use windows_sys::Win32::Foundation::GetHandleInformation;

        let mut children = Vec::new();
        let mut parents = Vec::new();
        for child_writes in [false, true, false, true] {
            let (child, parent) = pipe_for_child(child_writes).unwrap();
            children.push(child);
            parents.push(parent);
        }
        let exact = [
            children[0].as_raw_handle() as HANDLE,
            children[1].as_raw_handle() as HANDLE,
            children[2].as_raw_handle() as HANDLE,
            children[3].as_raw_handle() as HANDLE,
        ];
        validate_inherited_handles(&exact).unwrap();

        let mut null = exact;
        null[0] = null_mut();
        assert!(validate_inherited_handles(&null).is_err());
        let mut invalid = exact;
        invalid[1] = INVALID_HANDLE_VALUE;
        assert!(validate_inherited_handles(&invalid).is_err());
        let mut duplicate = exact;
        duplicate[3] = duplicate[0];
        assert!(validate_inherited_handles(&duplicate).is_err());
        let mut parent_end = exact;
        parent_end[2] = parents[0].as_raw_handle() as HANDLE;
        assert!(validate_inherited_handles(&parent_end).is_err());

        for parent in &parents {
            let mut flags = u32::MAX;
            assert_ne!(
                unsafe { GetHandleInformation(parent.as_raw_handle() as HANDLE, &mut flags) },
                0
            );
            assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
        }
    }

    #[test]
    fn native_cleanup_poll_reports_actual_wait_and_job_query_errors() {
        let query_error = confirm_job_empty(
            INVALID_HANDLE_VALUE,
            unsafe { GetCurrentProcess() },
            MonotonicDeadline::after_for(Duration::from_secs(1), "invalid job query").unwrap(),
        )
        .unwrap_err();
        assert!(
            format!("{query_error:#}").contains("query capture worker job cleanup accounting"),
            "{query_error:#}"
        );

        let wait_error = confirm_job_empty(
            null_mut(),
            1usize as HANDLE,
            MonotonicDeadline::after_for(Duration::from_secs(1), "invalid process wait").unwrap(),
        )
        .unwrap_err();
        assert!(
            format!("{wait_error:#}").contains("poll capture worker leader cleanup"),
            "{wait_error:#}"
        );
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum InjectedNativeFailure {
        Size,
        Allocate,
        Initialize,
        HandleList,
        JobList,
        CreateProcess,
    }

    #[test]
    fn pre_create_native_failures_release_partial_attribute_state_without_fallback() {
        for failure in [
            InjectedNativeFailure::Size,
            InjectedNativeFailure::Allocate,
            InjectedNativeFailure::Initialize,
            InjectedNativeFailure::HandleList,
            InjectedNativeFailure::JobList,
            InjectedNativeFailure::CreateProcess,
        ] {
            let update_calls = Cell::new(0);
            let create_calls = Cell::new(0);
            let delete_calls = Cell::new(0);
            let free_calls = Cell::new(0);
            let stderr = |null_stderr| inherited_stderr(null_stderr);
            let size = |_: u32| {
                if failure == InjectedNativeFailure::Size {
                    bail!("injected attribute sizing failure");
                }
                Ok(64)
            };
            let allocate = |_: usize| {
                if failure == InjectedNativeFailure::Allocate {
                    bail!("injected attribute allocation failure");
                }
                let dangling = std::ptr::NonNull::<usize>::dangling().as_ptr();
                Ok((dangling.cast(), dangling.cast()))
            };
            let initialize = |_: AttributePointer, _: u32, _: &mut usize| {
                if failure == InjectedNativeFailure::Initialize {
                    bail!("injected attribute initialization failure");
                }
                Ok(())
            };
            let update = |_: AttributePointer, _: usize, _: *const c_void, _: usize| {
                let call = update_calls.get() + 1;
                update_calls.set(call);
                if (failure == InjectedNativeFailure::HandleList && call == 1)
                    || (failure == InjectedNativeFailure::JobList && call == 2)
                {
                    bail!("injected attribute update failure {call}");
                }
                Ok(())
            };
            let delete = |_: AttributePointer| delete_calls.set(delete_calls.get() + 1);
            let free = |_: HANDLE, _: AttributePointer| free_calls.set(free_calls.get() + 1);
            let create = |_: *const u16,
                          _: *mut u16,
                          _: u32,
                          _: *const c_void,
                          _: *const u16,
                          _: *const STARTUPINFOW| {
                create_calls.set(create_calls.get() + 1);
                bail!("injected CreateProcessW failure")
            };
            let api = NativeLaunchApi {
                stderr: &stderr,
                attribute_size: &size,
                attribute_allocate: &allocate,
                attribute_initialize: &initialize,
                attribute_update: &update,
                attribute_delete: &delete,
                attribute_free: &free,
                create_process: &create,
            };
            let mut command_line = [0_u16];
            let error = create_process_with_api(
                &api,
                &[0],
                &mut command_line,
                &[0, 0],
                &[0],
                [null_mut(); 4],
                null_mut(),
            )
            .err()
            .expect("injected native boundary must fail");
            assert!(
                format!("{error:#}").contains("injected"),
                "{failure:?}: {error:#}"
            );
            let reached_allocation = !matches!(
                failure,
                InjectedNativeFailure::Size | InjectedNativeFailure::Allocate
            );
            let initialized = matches!(
                failure,
                InjectedNativeFailure::HandleList
                    | InjectedNativeFailure::JobList
                    | InjectedNativeFailure::CreateProcess
            );
            assert_eq!(
                free_calls.get(),
                usize::from(reached_allocation),
                "{failure:?}"
            );
            assert_eq!(delete_calls.get(), usize::from(initialized), "{failure:?}");
            assert_eq!(
                create_calls.get(),
                usize::from(failure == InjectedNativeFailure::CreateProcess),
                "{failure:?}"
            );
        }
    }

    #[test]
    fn unavailable_stderr_is_a_pre_create_failure_without_global_handle_mutation() {
        for unavailable in [null_mut(), INVALID_HANDLE_VALUE] {
            let create_calls = Cell::new(0);
            let stderr = move |_: bool| inherited_stderr_from(|| unavailable);
            let create = |_: *const u16,
                          _: *mut u16,
                          _: u32,
                          _: *const c_void,
                          _: *const u16,
                          _: *const STARTUPINFOW| {
                create_calls.set(create_calls.get() + 1);
                unreachable!()
            };
            let api = NativeLaunchApi {
                stderr: &stderr,
                create_process: &create,
                ..NATIVE_LAUNCH_API
            };
            let mut process_mask = 0;
            let mut system_mask = 0;
            assert_ne!(
                unsafe {
                    GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
                },
                0
            );
            let allowed = 1usize << process_mask.trailing_zeros();
            let affinity = Arc::new(
                RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                    .unwrap()
                    .admit()
                    .unwrap(),
            );
            let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
            let mut command = Command::new(&executable);
            super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
            super::super::configure_worker_environment(&mut command);
            let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
            let probe = DiagnosticProbeState::new(
                MonotonicDeadline::after_for(Duration::from_secs(1), "stderr injection").unwrap(),
            );
            let error = launch_with_validation(
                &command,
                affinity,
                liveness,
                Some(&probe),
                &api,
                |_, _, _| unreachable!(),
            )
            .err()
            .expect("unavailable stderr must fail before process creation");
            assert_eq!(
                format!("{error:#}"),
                "capture worker inherited stderr is unavailable"
            );
            assert_eq!(create_calls.get(), 0);
            assert!(
                !probe
                    .process_started
                    .load(std::sync::atomic::Ordering::Acquire)
            );
        }
    }

    #[test]
    #[ignore = "launched only by native_atomic_launch_assigns_job_before_parent_permit"]
    fn native_atomic_launch_fixture() {
        if std::env::var_os("SCRIBE_FIXTURE_EXIT_259").is_some() {
            std::process::exit(259);
        }
        if std::env::var_os("SCRIBE_LONG_PATH_FIXTURE").is_some() {
            let expected_argument = std::env::var("SCRIBE_LONG_PATH_EXPECTED_ARGUMENT").unwrap();
            let expected_value = std::env::var("SCRIBE_LONG_PATH_EXPECTED_VALUE").unwrap();
            assert!(std::env::args_os().any(|argument| argument == OsStr::new(&expected_argument)));
            assert_eq!(expected_value, "environment space 雪😀\\");
            println!("SCRIBE_LONG_PATH_READY");
            std::io::stdout().flush().unwrap();
            let mut permit = [0_u8; 1];
            std::io::stdin().read_exact(&mut permit).unwrap();
            assert_eq!(permit, [0x5a]);
            return;
        }
        if std::env::var_os("SCRIBE_FIXTURE_SPAWN_DESCENDANT").is_some() {
            #[allow(
                clippy::zombie_processes,
                reason = "parent tests retain the owned job and prove descendant cleanup by explicit termination or kill-on-close"
            )]
            let _descendant = Command::new(std::env::current_exe().unwrap())
                .arg("--ignored")
                .arg("--exact")
                .arg("onnx_worker::windows_capture_launch::tests::native_descendant_fixture")
                .arg("--nocapture")
                .spawn()
                .unwrap();
            let mut permit = [0_u8; 1];
            std::io::stdin().read_exact(&mut permit).unwrap();
            assert_eq!(permit, [0x5a]);
            return;
        }
        if std::env::var_os("SCRIBE_FIXTURE_BLOCK_FOR_CLEANUP").is_some() {
            let mut byte = [0_u8; 1];
            let _ = std::io::stdin().read_exact(&mut byte);
            return;
        }
        if std::env::var_os("SCRIBE_ATOMIC_LAUNCH_FIXTURE").is_none() {
            return;
        }
        if std::env::var_os("SCRIBE_FIXTURE_PRIVATE_STDERR").is_some() {
            eprintln!("SCRIBE_PRIVATE_STDERR");
        }
        let mut in_job = 0;
        assert_ne!(
            unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) },
            0
        );
        assert_ne!(in_job, 0, "fixture started outside the atomic job");
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        assert_ne!(process_mask, 0);
        assert_eq!(process_mask & !system_mask, 0);
        let expected =
            usize::from_str_radix(&std::env::var("SCRIBE_ATOMIC_EXPECTED_MASK").unwrap(), 16)
                .unwrap();
        assert_ne!(expected, 0);
        assert_eq!(process_mask & !expected, 0);
        if let Ok(sentinel) = std::env::var("SCRIBE_ATOMIC_SENTINEL_HANDLE") {
            let sentinel = usize::from_str_radix(&sentinel, 16).unwrap() as HANDLE;
            let expected_sentinel = *b"SCRIBE-NO-INHERIT";
            let mut observed_sentinel = [0_u8; 17];
            let mut peeked = 0;
            let mut available = 0;
            let exact_sentinel_leaked = unsafe {
                windows_sys::Win32::System::Pipes::PeekNamedPipe(
                    sentinel,
                    observed_sentinel.as_mut_ptr().cast(),
                    observed_sentinel.len() as u32,
                    &mut peeked,
                    &mut available,
                    null_mut(),
                )
            } != 0
                && peeked as usize == expected_sentinel.len()
                && available as usize == expected_sentinel.len()
                && observed_sentinel == expected_sentinel;
            assert!(
                !exact_sentinel_leaked,
                "unlisted inheritable sentinel pipe leaked into child"
            );
        }
        println!("SCRIBE_ATOMIC_READY:{process_mask:016x}:{system_mask:016x}");
        std::io::stdout().flush().unwrap();
        let mut permit = [0_u8; 1];
        std::io::stdin().read_exact(&mut permit).unwrap();
        assert_eq!(permit, [0x5a]);
        let liveness = std::env::var(super::super::PARENT_LIVENESS_ENV)
            .unwrap()
            .parse::<usize>()
            .unwrap() as HANDLE;
        let mut liveness = unsafe { File::from_raw_handle(liveness) };
        if std::env::var_os("SCRIBE_FIXTURE_EXPECT_LIVENESS_EOF").is_some() {
            let mut remaining = Vec::new();
            liveness.read_to_end(&mut remaining).unwrap();
            assert!(remaining.is_empty());
            println!("SCRIBE_LIVENESS_EOF");
            std::io::stdout().flush().unwrap();
        } else {
            let mut cancel = [0_u8; 1];
            liveness.read_exact(&mut cancel).unwrap();
            assert_eq!(cancel, [PARENT_CONTROL_CANCEL]);
        }
    }

    #[test]
    #[ignore = "launched only by native_exited_leader_termination_reaps_job_descendant"]
    fn native_descendant_fixture() {
        println!("SCRIBE_DESCENDANT_READY");
        std::io::stdout().flush().unwrap();
        let liveness = std::env::var(super::super::PARENT_LIVENESS_ENV)
            .unwrap()
            .parse::<usize>()
            .unwrap() as HANDLE;
        let mut liveness = unsafe { File::from_raw_handle(liveness) };
        let mut byte = [0_u8; 1];
        let _ = liveness.read_exact(&mut byte);
    }

    #[test]
    #[ignore = "launched only by nested-job controller fixture"]
    fn native_nested_child_fixture() {
        println!("SCRIBE_NESTED_CHILD_READY");
        std::io::stdout().flush().unwrap();
        let mut permit = [0_u8; 1];
        std::io::stdin().read_exact(&mut permit).unwrap();
        assert_eq!(permit, [0x5a]);
    }

    #[test]
    #[ignore = "launched only by native nested-job behavior tests"]
    fn native_nested_job_fixture() {
        println!("SCRIBE_NESTED_CONTROLLER_READY");
        std::io::stdout().flush().unwrap();
        let mut outer_permit = [0_u8; 1];
        std::io::stdin().read_exact(&mut outer_permit).unwrap();
        assert_eq!(outer_permit, [0x5a]);

        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();

        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_nested_child_fixture")
            .arg("--nocapture");
        super::super::configure_worker_environment(&mut command);
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        if std::env::var("SCRIBE_NESTED_MODE").unwrap() == "capacity" {
            let error = launch(&command, affinity, liveness, None)
                .err()
                .expect("ancestor active-process capacity must reject nested child launch");
            println!("SCRIBE_NESTED_CAPACITY_REJECTED:{error:#}");
            std::io::stdout().flush().unwrap();
            return;
        }

        let mut child = launch(&command, affinity, liveness, None).unwrap();
        child.stdin.write_all(&[0x5a]).unwrap();
        drop(child.stdin);
        child
            .process
            .wait_before(
                MonotonicDeadline::after_for(Duration::from_secs(5), "nested child fixture")
                    .unwrap(),
            )
            .unwrap();
        let mut output = String::new();
        child.stdout.read_to_string(&mut output).unwrap();
        assert!(output.contains("SCRIBE_NESTED_CHILD_READY"), "{output:?}");
        println!("SCRIBE_NESTED_COMPATIBLE_OK");
        std::io::stdout().flush().unwrap();
    }

    fn run_nested_job_fixture(mode: &str) -> String {
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_nested_job_fixture")
            .arg("--nocapture");
        super::super::configure_worker_environment(&mut command);
        command.env("SCRIBE_NESTED_MODE", mode);
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        let mut launched = launch_with_validation(
            &command,
            affinity,
            liveness,
            None,
            &NATIVE_LAUNCH_API,
            |prepared, affinity, process| prepared.validate_process(affinity, process),
        )
        .unwrap();
        let (line_tx, line_rx) = sync_channel(16);
        let drain = std::thread::spawn(move || {
            let mut output = BufReader::new(launched.stdout);
            loop {
                let mut line = String::new();
                match output.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = line_tx.send(None);
                        break;
                    }
                    Ok(_) => {
                        let _ = line_tx.send(Some(line));
                    }
                }
            }
        });
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = ready_deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("nested controller readiness deadline elapsed");
            let line = line_rx.recv_timeout(remaining).unwrap().unwrap();
            if line.contains("SCRIBE_NESTED_CONTROLLER_READY") {
                break;
            }
        }

        if mode == "capacity" {
            let mut limits = unsafe { zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
            let mut returned = 0;
            assert_ne!(
                unsafe {
                    QueryInformationJobObject(
                        launched.process.affinity_job.handle(),
                        JobObjectExtendedLimitInformation,
                        (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                        &mut returned,
                    )
                },
                0
            );
            assert_eq!(
                returned as usize,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
            );
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            assert_ne!(
                unsafe {
                    SetInformationJobObject(
                        launched.process.affinity_job.handle(),
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    )
                },
                0
            );
        }

        launched.stdin.write_all(&[0x5a]).unwrap();
        drop(launched.stdin);
        let result_marker = if mode == "capacity" {
            "SCRIBE_NESTED_CAPACITY_REJECTED:"
        } else {
            "SCRIBE_NESTED_COMPATIBLE_OK"
        };
        let output_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut observed = String::new();
        loop {
            let remaining = output_deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("nested controller output deadline elapsed");
            match line_rx.recv_timeout(remaining).unwrap() {
                Some(line) => {
                    observed.push_str(&line);
                    if line.contains(result_marker) {
                        break;
                    }
                }
                None => panic!("nested controller exited before {result_marker}: {observed:?}"),
            }
        }
        launched
            .process
            .wait_before(
                MonotonicDeadline::after_for(Duration::from_secs(5), "nested controller exit")
                    .unwrap(),
            )
            .unwrap();
        let eof_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = eof_deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("nested controller stdout EOF deadline elapsed");
            match line_rx.recv_timeout(remaining).unwrap() {
                Some(line) => observed.push_str(&line),
                None => break,
            }
        }
        drain.join().unwrap();
        observed
    }

    #[test]
    fn disposable_fixture_supports_compatible_nested_capture_job() {
        let output = run_nested_job_fixture("compatible");
        assert!(output.contains("SCRIBE_NESTED_COMPATIBLE_OK"), "{output:?}");
    }

    #[test]
    fn disposable_fixture_ancestor_capacity_rejects_nested_child_creation() {
        let output = run_nested_job_fixture("capacity");
        assert!(
            output.contains("SCRIBE_NESTED_CAPACITY_REJECTED:"),
            "{output:?}"
        );
        assert!(
            output.contains("atomic capture worker CreateProcessW failed"),
            "{output:?}"
        );
        assert!(!output.contains("SCRIBE_NESTED_CHILD_READY"), "{output:?}");
    }

    #[test]
    fn zero_poll_does_not_confuse_exit_code_259_with_a_running_process() {
        use std::os::windows::io::AsRawHandle;

        let executable = std::env::current_exe().unwrap();
        let mut child = Command::new(executable)
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_atomic_launch_fixture")
            .arg("--nocapture")
            .env("SCRIBE_FIXTURE_EXIT_259", "1")
            .spawn()
            .unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(child.as_raw_handle() as HANDLE, 5_000) },
            WAIT_OBJECT_0
        );
        let _ = child.wait().unwrap();
        assert!(!process_is_running(child.as_raw_handle() as HANDLE).unwrap());
    }

    struct OwnedExecutableTree {
        file: Option<std::path::PathBuf>,
        directories: Vec<std::path::PathBuf>,
    }

    impl OwnedExecutableTree {
        fn create() -> Self {
            let temp = std::env::temp_dir().canonicalize().unwrap();
            assert!(temp.is_absolute());
            let unique = format!(
                "scribe atomic launch {} {}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let root = temp.join(unique);
            assert_eq!(root.parent(), Some(temp.as_path()));
            std::fs::create_dir(&root).unwrap();
            let mut directories = vec![root.clone()];
            let mut directory = root;
            let file_name = "worker copy 雪.exe";
            while directory.join(file_name).as_os_str().encode_wide().count() <= 300 {
                directory = directory.join(format!(
                    "nested path with spaces 雪 {:02}",
                    directories.len()
                ));
                std::fs::create_dir(&directory).unwrap();
                directories.push(directory.clone());
            }
            let file = directory.join(file_name);
            assert!(file.is_absolute() && file.starts_with(&directories[0]));
            std::fs::copy(std::env::current_exe().unwrap(), &file).unwrap();
            Self {
                file: Some(file),
                directories,
            }
        }

        fn executable(&self) -> std::path::PathBuf {
            let canonical = self.file.as_ref().unwrap().canonicalize().unwrap();
            assert!(canonical.is_absolute());
            assert!(canonical.as_os_str().encode_wide().count() > 260);
            assert!(canonical.to_string_lossy().contains('雪'));
            assert!(canonical.to_string_lossy().contains(' '));
            canonical
        }

        fn cleanup(mut self) {
            self.remove_exact().unwrap();
        }

        fn remove_exact(&mut self) -> std::io::Result<()> {
            if let Some(file) = self.file.as_ref() {
                assert!(file.is_absolute() && file.starts_with(&self.directories[0]));
                std::fs::remove_file(file)?;
                self.file = None;
            }
            while let Some(directory) = self.directories.last() {
                assert!(directory.is_absolute());
                std::fs::remove_dir(directory)?;
                self.directories.pop();
            }
            Ok(())
        }
    }

    impl Drop for OwnedExecutableTree {
        fn drop(&mut self) {
            let _ = self.remove_exact();
        }
    }

    #[test]
    fn native_launch_accepts_canonical_long_space_unicode_executable_and_lossless_inputs() {
        let tree = OwnedExecutableTree::create();
        let executable = tree.executable();
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let expected_argument = "argument space 雪😀\\";
        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_atomic_launch_fixture")
            .arg("--nocapture")
            .arg("--skip")
            .arg(expected_argument);
        super::super::configure_worker_environment(&mut command);
        command
            .env("SCRIBE_LONG_PATH_FIXTURE", "1")
            .env("SCRIBE_LONG_PATH_EXPECTED_ARGUMENT", expected_argument)
            .env(
                "SCRIBE_LONG_PATH_EXPECTED_VALUE",
                "environment space 雪😀\\",
            );
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        let mut launched = launch(&command, affinity, liveness, None).unwrap();
        let (output_tx, output_rx) = sync_channel(1);
        let drain = std::thread::spawn(move || {
            let mut output = String::new();
            let result = launched.stdout.read_to_string(&mut output).map(|_| output);
            let _ = output_tx.send(result);
        });
        launched.stdin.write_all(&[0x5a]).unwrap();
        drop(launched.stdin);
        launched
            .process
            .wait_before(
                MonotonicDeadline::after_for(Duration::from_secs(5), "long path fixture").unwrap(),
            )
            .unwrap();
        let output = output_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(output.contains("SCRIBE_LONG_PATH_READY"), "{output:?}");
        drain.join().unwrap();
        drop(launched.process);
        tree.cleanup();
    }

    #[test]
    fn native_atomic_launch_assigns_job_before_parent_permit() {
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1,
        };
        let mut sentinel_read = null_mut();
        let mut sentinel_write = null_mut();
        assert_ne!(
            unsafe { CreatePipe(&mut sentinel_read, &mut sentinel_write, &security, 0) },
            0
        );
        let sentinel_read = unsafe { OwnedHandle::from_raw_handle(sentinel_read) };
        let sentinel_write = unsafe { OwnedHandle::from_raw_handle(sentinel_write) };
        let sentinel_frame = *b"SCRIBE-NO-INHERIT";
        let mut sentinel_written = 0;
        assert_ne!(
            unsafe {
                windows_sys::Win32::Storage::FileSystem::WriteFile(
                    sentinel_write.as_raw_handle() as HANDLE,
                    sentinel_frame.as_ptr().cast(),
                    sentinel_frame.len() as u32,
                    &mut sentinel_written,
                    null_mut(),
                )
            },
            0
        );
        assert_eq!(sentinel_written as usize, sentinel_frame.len());
        let mut stderr_read = null_mut();
        let mut stderr_write = null_mut();
        assert_ne!(
            unsafe { CreatePipe(&mut stderr_read, &mut stderr_write, &security, 0) },
            0
        );
        assert_ne!(
            unsafe { SetHandleInformation(stderr_read, HANDLE_FLAG_INHERIT, 0) },
            0
        );
        let stderr_read = unsafe { OwnedHandle::from_raw_handle(stderr_read) };
        let stderr_write = unsafe { OwnedHandle::from_raw_handle(stderr_write) };
        let stderr_write = RefCell::new(Some(stderr_write));
        let stderr_provider = |_: bool| {
            stderr_write
                .borrow_mut()
                .take()
                .ok_or_else(|| anyhow!("private stderr writer already consumed"))
        };
        let api = NativeLaunchApi {
            stderr: &stderr_provider,
            ..NATIVE_LAUNCH_API
        };
        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_atomic_launch_fixture")
            .arg("--nocapture");
        super::super::configure_worker_environment(&mut command);
        command
            .env("SCRIBE_ATOMIC_LAUNCH_FIXTURE", "1")
            .env("SCRIBE_FIXTURE_PRIVATE_STDERR", "1")
            .env("SCRIBE_ATOMIC_EXPECTED_MASK", format!("{allowed:016x}"))
            .env(
                "SCRIBE_ATOMIC_SENTINEL_HANDLE",
                format!("{:016x}", sentinel_read.as_raw_handle() as usize),
            );
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        let mut launched = launch_with_validation(
            &command,
            affinity.clone(),
            liveness,
            None,
            &api,
            |prepared, affinity, process| prepared.validate_process(affinity, process),
        )
        .unwrap();
        let (stderr_tx, stderr_rx) = sync_channel(1);
        let stderr_drain = std::thread::spawn(move || {
            let mut stderr = unsafe { File::from_raw_handle(stderr_read.into_raw_handle()) };
            let mut output = String::new();
            let result = stderr.read_to_string(&mut output).map(|_| output);
            let _ = stderr_tx.send(result);
        });
        let (ready_tx, ready_rx) = sync_channel(1);
        let drain = std::thread::spawn(move || {
            let mut output = BufReader::new(launched.stdout);
            let mut sent = false;
            loop {
                let mut line = String::new();
                match output.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if !sent && line.contains("SCRIBE_ATOMIC_READY:") => {
                        let _ = ready_tx.send(line);
                        sent = true;
                    }
                    Ok(_) => {}
                }
            }
            if !sent {
                let _ = ready_tx.send(String::new());
            }
        });
        let ready = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            ready.contains(&format!("SCRIBE_ATOMIC_READY:{allowed:016x}:")),
            "{ready:?}"
        );
        launched
            .process
            .validate_capture_affinity(None)
            .unwrap_err();
        let equal_but_distinct = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        launched
            .process
            .validate_capture_affinity(Some(&equal_but_distinct))
            .unwrap_err();
        launched
            .process
            .validate_capture_affinity(Some(&affinity))
            .unwrap();
        let observation = launched.process.duplicate_observation_handle().unwrap();
        launched.stdin.write_all(&[0x5a]).unwrap();
        drop(launched.stdin);
        assert!(launched.process.request_cooperative_cancel().unwrap());
        launched
            .process
            .wait_before(
                MonotonicDeadline::after_for(Duration::from_secs(5), "atomic fixture exit")
                    .unwrap(),
            )
            .unwrap();
        drain.join().unwrap();
        let stderr = stderr_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(stderr.contains("SCRIBE_PRIVATE_STDERR"), "{stderr:?}");
        stderr_drain.join().unwrap();
        let mut exit_code = u32::MAX;
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Threading::GetExitCodeProcess(
                    observation.as_raw_handle() as HANDLE,
                    &mut exit_code,
                )
            },
            0
        );
        assert_eq!(exit_code, 0);
        assert!(launched.process.duplicate_observation_handle().is_err());
        drop((sentinel_read, sentinel_write));
    }

    #[test]
    fn closing_only_parent_liveness_writer_delivers_eof_while_job_is_retained() {
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_atomic_launch_fixture")
            .arg("--nocapture");
        super::super::configure_worker_environment(&mut command);
        command
            .env("SCRIBE_ATOMIC_LAUNCH_FIXTURE", "1")
            .env("SCRIBE_ATOMIC_EXPECTED_MASK", format!("{allowed:016x}"))
            .env("SCRIBE_FIXTURE_EXPECT_LIVENESS_EOF", "1");
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        let mut launched = launch_with_validation(
            &command,
            affinity,
            liveness,
            None,
            &NATIVE_LAUNCH_API,
            |prepared, affinity, process| prepared.validate_process(affinity, process),
        )
        .unwrap();
        let observation = launched.process.duplicate_observation_handle().unwrap();
        let (line_tx, line_rx) = sync_channel(4);
        let drain = std::thread::spawn(move || {
            let mut output = BufReader::new(launched.stdout);
            loop {
                let mut line = String::new();
                match output.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = line_tx.send(None);
                        break;
                    }
                    Ok(_) => {
                        let _ = line_tx.send(Some(line));
                    }
                }
            }
        });
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = ready_deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("atomic readiness deadline elapsed");
            let line = line_rx.recv_timeout(remaining).unwrap().unwrap();
            if line.contains("SCRIBE_ATOMIC_READY:") {
                break;
            }
        }
        launched.stdin.write_all(&[0x5a]).unwrap();
        Arc::get_mut(&mut launched.process)
            .expect("test retains the sole process owner")
            .close_parent_liveness_writer_for_test();
        let eof_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = eof_deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("liveness EOF frame deadline elapsed");
            let line = line_rx.recv_timeout(remaining).unwrap().unwrap();
            if line.trim() == "SCRIBE_LIVENESS_EOF" {
                break;
            }
        }
        assert_eq!(
            unsafe { WaitForSingleObject(observation.as_raw_handle() as HANDLE, 5_000) },
            WAIT_OBJECT_0
        );
        let stream_deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = stream_deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("stdout EOF deadline elapsed");
            if line_rx.recv_timeout(remaining).unwrap().is_none() {
                break;
            }
        }
        launched
            .process
            .wait_before(
                MonotonicDeadline::after_for(Duration::from_secs(5), "liveness EOF fixture")
                    .unwrap(),
            )
            .unwrap();
        drain.join().unwrap();
    }

    struct GatedDescendantFixture {
        process: Arc<NativeCaptureWorkerProcess>,
        leader: OwnedHandle,
        liveness_writer: OwnedHandle,
        eof_rx: std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
        drain: std::thread::JoinHandle<()>,
    }

    fn launch_gated_descendant() -> GatedDescendantFixture {
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_atomic_launch_fixture")
            .arg("--nocapture");
        super::super::configure_worker_environment(&mut command);
        command.env("SCRIBE_FIXTURE_SPAWN_DESCENDANT", "1");
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        let liveness_writer = duplicate_handle(liveness.parent_write as HANDLE, false).unwrap();
        let launched = launch_with_validation(
            &command,
            affinity,
            liveness,
            None,
            &NATIVE_LAUNCH_API,
            |prepared, affinity, process| prepared.validate_process(affinity, process),
        )
        .unwrap();
        let NativeLaunchedCaptureWorker {
            stdout,
            process,
            mut stdin,
        } = launched;
        let (ready_tx, ready_rx) = sync_channel(1);
        let (eof_tx, eof_rx) = sync_channel(1);
        let drain = std::thread::spawn(move || {
            let mut output = BufReader::new(stdout);
            let mut bytes = Vec::new();
            let mut sent = false;
            let result = loop {
                let mut line = String::new();
                match output.read_line(&mut line) {
                    Ok(0) => break Ok(bytes),
                    Ok(_) => {
                        bytes.extend_from_slice(line.as_bytes());
                        if !sent && line.contains("SCRIBE_DESCENDANT_READY") {
                            let _ = ready_tx.send(());
                            sent = true;
                        }
                    }
                    Err(error) => break Err(error),
                }
            };
            let _ = eof_tx.send(result);
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let leader = process.duplicate_observation_handle().unwrap();
        stdin.write_all(&[0x5a]).unwrap();
        drop(stdin);
        assert_eq!(
            unsafe { WaitForSingleObject(leader.as_raw_handle() as HANDLE, 5_000) },
            WAIT_OBJECT_0
        );
        let mut accounting = unsafe { zeroed::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() };
        let mut returned = 0;
        assert_ne!(
            unsafe {
                QueryInformationJobObject(
                    process.affinity_job.handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    &mut returned,
                )
            },
            0
        );
        assert!(accounting.ActiveProcesses > 0);
        GatedDescendantFixture {
            process,
            leader,
            liveness_writer,
            eof_rx,
            drain,
        }
    }

    #[test]
    fn native_exited_leader_termination_reaps_job_descendant() {
        let fixture = launch_gated_descendant();
        fixture.process.terminate().unwrap();
        fixture
            .process
            .wait_before(
                MonotonicDeadline::after_for(Duration::from_secs(5), "descendant termination")
                    .unwrap(),
            )
            .unwrap();
        fixture
            .eof_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        fixture.drain.join().unwrap();
        drop((fixture.leader, fixture.liveness_writer));
    }

    #[test]
    fn dropping_last_process_owner_kills_exited_leaders_live_descendant() {
        let fixture = launch_gated_descendant();
        let GatedDescendantFixture {
            process,
            leader,
            liveness_writer,
            eof_rx,
            drain,
        } = fixture;
        drop(process);
        eof_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        drain.join().unwrap();
        drop((leader, liveness_writer));
    }

    #[test]
    fn post_create_validation_failure_retains_diagnostic_and_confirms_cleanup() {
        let mut process_mask = 0;
        let mut system_mask = 0;
        assert_ne!(
            unsafe {
                GetProcessAffinityMask(GetCurrentProcess(), &mut process_mask, &mut system_mask)
            },
            0
        );
        let allowed = 1usize << process_mask.trailing_zeros();
        let affinity = Arc::new(
            RequestedWorkerAffinity::parse(&format!("{allowed:016x}"))
                .unwrap()
                .admit()
                .unwrap(),
        );
        let executable = std::env::current_exe().unwrap().canonicalize().unwrap();
        let mut command = Command::new(&executable);
        super::super::configure_worker_current_directory(&mut command, &executable).unwrap();
        command
            .arg("--ignored")
            .arg("--exact")
            .arg("onnx_worker::windows_capture_launch::tests::native_atomic_launch_fixture")
            .arg("--nocapture");
        super::super::configure_worker_environment(&mut command);
        command.env("SCRIBE_FIXTURE_BLOCK_FOR_CLEANUP", "1");
        let liveness = ParentLivenessChannel::attach(&mut command).unwrap();
        let probe = DiagnosticProbeState::new(
            MonotonicDeadline::after_for(Duration::from_secs(2), "native launch cleanup").unwrap(),
        );
        let error = launch_with_validation(
            &command,
            affinity,
            liveness,
            Some(&probe),
            &NATIVE_LAUNCH_API,
            |_, _, _| Err(anyhow!("injected post-create validation failure")),
        )
        .err()
        .expect("post-create injection must fail closed");
        assert!(format!("{error:#}").contains("injected post-create validation failure"));
        assert!(
            probe
                .process_started
                .load(std::sync::atomic::Ordering::Acquire)
        );
        assert!(probe.is_cleanup_confirmed());
        assert_eq!(
            probe.exit_observation_before_parent_termination(),
            super::super::ProbeExitObservation::NotSignaled
        );
    }

    fn injected_terminate_failure(_: HANDLE) -> Result<()> {
        bail!("injected terminate failure")
    }

    fn injected_terminate_success(_: HANDLE) -> Result<()> {
        Ok(())
    }

    fn injected_cleanup_confirmed(_: HANDLE, _: HANDLE, _: MonotonicDeadline) -> Result<()> {
        Ok(())
    }

    fn injected_cleanup_timeout(_: HANDLE, _: HANDLE, _: MonotonicDeadline) -> Result<()> {
        bail!("injected cleanup timeout")
    }

    fn injected_cleanup_query_error(_: HANDLE, _: HANDLE, _: MonotonicDeadline) -> Result<()> {
        bail!("injected job query failure")
    }

    #[test]
    fn cleanup_seam_distinguishes_confirmed_exit_from_unconfirmed_timeout() {
        let confirmed_probe = DiagnosticProbeState::new(
            MonotonicDeadline::after_for(Duration::from_secs(1), "confirmed cleanup").unwrap(),
        );
        confirmed_probe.process_started();
        let confirmed_error = cleanup_failed_launch_with::<()>(
            anyhow!("original validation failure"),
            null_mut(),
            null_mut(),
            Some(&confirmed_probe),
            &CleanupApi {
                terminate: injected_terminate_failure,
                confirm_empty: injected_cleanup_confirmed,
            },
        )
        .unwrap_err();
        let confirmed = format!("{confirmed_error:#}");
        assert!(confirmed.contains("original validation failure"));
        assert!(confirmed.contains("injected terminate failure"));
        assert!(confirmed.contains("exit was confirmed"));
        assert!(confirmed_probe.is_cleanup_confirmed());

        let timeout_probe = DiagnosticProbeState::new(
            MonotonicDeadline::after_for(Duration::from_secs(1), "unconfirmed cleanup").unwrap(),
        );
        timeout_probe.process_started();
        let timeout_error = cleanup_failed_launch_with::<()>(
            anyhow!("original validation failure"),
            null_mut(),
            null_mut(),
            Some(&timeout_probe),
            &CleanupApi {
                terminate: injected_terminate_success,
                confirm_empty: injected_cleanup_timeout,
            },
        )
        .unwrap_err();
        let timeout = format!("{timeout_error:#}");
        assert!(timeout.contains("original validation failure"));
        assert!(timeout.contains("injected cleanup timeout"));
        assert!(timeout.contains("cleanup was unconfirmed"));
        assert!(!timeout_probe.is_cleanup_confirmed());

        let query_probe = DiagnosticProbeState::new(
            MonotonicDeadline::after_for(Duration::from_secs(1), "query failure cleanup").unwrap(),
        );
        query_probe.process_started();
        let query_error = cleanup_failed_launch_with::<()>(
            anyhow!("original validation failure"),
            null_mut(),
            null_mut(),
            Some(&query_probe),
            &CleanupApi {
                terminate: injected_terminate_success,
                confirm_empty: injected_cleanup_query_error,
            },
        )
        .unwrap_err();
        let query = format!("{query_error:#}");
        assert!(query.contains("original validation failure"));
        assert!(query.contains("injected job query failure"));
        assert!(!query_probe.is_cleanup_confirmed());
    }
}
