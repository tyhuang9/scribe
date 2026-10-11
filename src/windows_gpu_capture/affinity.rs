//! Capture-only worker and optional collector job affinity. Production worker
//! launch prepares and validates its job before `CreateProcessW` supplies it
//! through `PROC_THREAD_ATTRIBUTE_JOB_LIST`; the post-spawn binder remains
//! test-only for legacy failure-boundary coverage. Collector confinement is a
//! component scheduling control, not host isolation or a qualification claim.

use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_AFFINITY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, GetCurrentProcess,
    GetProcessAffinityMask, GetProcessGroupAffinity,
};

const LIMITS: u32 = JOB_OBJECT_LIMIT_AFFINITY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
const COLLECTOR_LIMITS: u32 = JOB_OBJECT_LIMIT_AFFINITY;

// Small native-call seam shared by real binding and fail-closed cleanup tests.
struct JobApi {
    create: fn() -> Result<OwnedHandle>,
    configure: fn(HANDLE, &JOBOBJECT_EXTENDED_LIMIT_INFORMATION) -> Result<()>,
    #[cfg(test)]
    assign: fn(HANDLE, HANDLE) -> Result<()>,
    readback: fn(HANDLE) -> Result<(u32, JOBOBJECT_EXTENDED_LIMIT_INFORMATION)>,
    membership: fn(HANDLE, HANDLE) -> Result<bool>,
    inheritance: fn(HANDLE) -> Result<u32>,
    process_affinity: fn(HANDLE) -> Result<(usize, usize)>,
    topology: fn(TopologyBoundary) -> Result<CollectorTopology>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TopologyBoundary {
    BeforeBind,
    BeforeReadback,
    AfterReadback,
}

const NATIVE_JOB_API: JobApi = JobApi {
    create: create_job,
    configure: configure_job,
    #[cfg(test)]
    assign: assign_job,
    readback: readback_job,
    membership: job_membership,
    inheritance: job_inheritance,
    process_affinity,
    topology: |_| collector_topology(),
};

/// Native-call seam for the current-process collector job. Every operation is
/// per-call data: tests can replace one boundary without mutable global hooks.
struct CollectorJobApi {
    create: fn() -> Result<CollectorJobHandle>,
    configure: fn(HANDLE, &JOBOBJECT_EXTENDED_LIMIT_INFORMATION) -> Result<()>,
    assign_current: fn(HANDLE) -> Result<()>,
    readback: fn(HANDLE) -> Result<(u32, JOBOBJECT_EXTENDED_LIMIT_INFORMATION)>,
    membership: fn(HANDLE) -> Result<bool>,
    inheritance: fn(HANDLE) -> Result<u32>,
    process_affinity: fn() -> Result<(usize, usize)>,
    topology: fn() -> Result<CollectorTopology>,
}

const NATIVE_COLLECTOR_JOB_API: CollectorJobApi = CollectorJobApi {
    create: create_collector_job,
    configure: configure_job,
    assign_current: assign_current_process,
    readback: readback_job,
    membership: current_process_job_membership,
    inheritance: job_inheritance,
    process_affinity: current_process_affinity,
    topology: collector_topology,
};

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum InjectedAffinityFailure {
    Create,
    Configure,
    Assign,
    Readback,
    Membership,
    Nonmember,
    WrongMask,
    Inheritance,
    ChildAffinity,
    Topology,
    PostAssignmentTopologyDrift,
    PostReadbackTopologyDrift,
    InvalidChildMask,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RequestedWorkerAffinity(u64);

impl RequestedWorkerAffinity {
    pub(crate) fn parse(value: &str) -> Result<Self> {
        if value.len() != 16
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            bail!(
                "worker allowed processor mask must be exactly 16 lowercase hexadecimal characters"
            );
        }
        let mask = u64::from_str_radix(value, 16)?;
        if mask == 0 {
            bail!("worker allowed processor mask must be nonzero");
        }
        Ok(Self(mask))
    }

    pub(crate) fn admit(self) -> Result<CaptureWorkerAffinity> {
        CaptureWorkerAffinity::admit_with(self, collector_topology)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CollectorTopology {
    active_processors: u32,
    process_mask: usize,
    system_mask: usize,
}

/// One immutable admitted constraint shared by CPU, GPU and discovery workers.
#[derive(Debug)]
pub(crate) struct CaptureWorkerAffinity {
    mask: usize,
    topology: CollectorTopology,
}

/// Retains the fresh affinity-only job constraining this disposable collector.
/// The job deliberately has no kill-on-close or breakaway limit: dropping this
/// handle must never terminate the collector that owns it.
pub(crate) struct CollectorAffinityGuard {
    job: CollectorJobHandle,
    mask: usize,
    topology: CollectorTopology,
    effective_process_mask: usize,
}

enum CollectorJobHandle {
    Native(OwnedHandle),
    #[cfg(test)]
    Fake(usize),
}

impl CollectorJobHandle {
    fn raw(&self) -> HANDLE {
        match self {
            Self::Native(handle) => handle.as_raw_handle() as HANDLE,
            #[cfg(test)]
            Self::Fake(handle) => *handle as HANDLE,
        }
    }
}

impl CollectorAffinityGuard {
    pub(crate) fn establish(requested: RequestedWorkerAffinity) -> Result<Self> {
        Self::establish_with(requested, &NATIVE_COLLECTOR_JOB_API)
    }

    fn establish_with(requested: RequestedWorkerAffinity, api: &CollectorJobApi) -> Result<Self> {
        let topology = (api.topology)()?;
        let mask = usize::try_from(requested.0)?;
        validate_requested_mask(mask, topology)?;

        let job = (api.create)()?;
        let raw_job = job.raw();
        // SAFETY: this Win32 POD consists only of integer fields and pointers.
        let mut limits = unsafe { zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = COLLECTOR_LIMITS;
        limits.BasicLimitInformation.Affinity = mask;
        // Configuration and exact readback both precede the irreversible
        // assignment of the current process.
        (api.configure)(raw_job, &limits)?;
        validate_collector_configuration(
            (api.inheritance)(raw_job)?,
            (api.readback)(raw_job)?,
            mask,
        )?;
        if (api.topology)()? != topology {
            bail!("capture processor topology or collector affinity changed before assignment");
        }
        (api.assign_current)(raw_job)?;

        let mut guard = Self {
            job,
            mask,
            topology,
            effective_process_mask: 0,
        };
        guard.effective_process_mask = guard.validate_current_with(api)?;
        Ok(guard)
    }

    pub(crate) fn require_current(&self) -> Result<()> {
        self.require_current_with(&NATIVE_COLLECTOR_JOB_API)
    }

    fn require_current_with(&self, api: &CollectorJobApi) -> Result<()> {
        let effective = self.validate_current_with(api)?;
        if effective != self.effective_process_mask {
            bail!("collector effective affinity changed after constraint assignment");
        }
        Ok(())
    }

    fn validate_current_with(&self, api: &CollectorJobApi) -> Result<usize> {
        let raw_job = self.job.raw();
        validate_collector_configuration(
            (api.inheritance)(raw_job)?,
            (api.readback)(raw_job)?,
            self.mask,
        )?;
        if !(api.membership)(raw_job)? {
            bail!("collector is not a member of its retained affinity job");
        }
        let current = (api.topology)()?;
        let (process_mask, system_mask) = (api.process_affinity)()?;
        if current.active_processors != self.topology.active_processors
            || current.system_mask != self.topology.system_mask
            || current.process_mask != process_mask
            || system_mask != self.topology.system_mask
            || process_mask == 0
            || process_mask & !self.mask != 0
            || process_mask & !system_mask != 0
        {
            bail!("collector affinity job or effective processor topology is invalid");
        }
        Ok(process_mask)
    }
}

fn validate_requested_mask(mask: usize, topology: CollectorTopology) -> Result<()> {
    if topology.active_processors == 0
        || topology.active_processors > usize::BITS
        || topology.system_mask.count_ones() != topology.active_processors
        || topology.process_mask == 0
        || topology.process_mask & !topology.system_mask != 0
        || mask == 0
        || mask & !topology.process_mask != 0
        || mask & !topology.system_mask != 0
    {
        bail!("collector affinity is outside the current process/system allowed processors");
    }
    Ok(())
}

fn validate_collector_configuration(
    handle_flags: u32,
    readback: (u32, JOBOBJECT_EXTENDED_LIMIT_INFORMATION),
    requested_mask: usize,
) -> Result<()> {
    let (returned, limits) = readback;
    if handle_flags != 0
        || returned as usize != size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
        || limits.BasicLimitInformation.LimitFlags != COLLECTOR_LIMITS
        || limits.BasicLimitInformation.Affinity != requested_mask
    {
        bail!("collector affinity job configuration readback is invalid");
    }
    Ok(())
}

/// A freshly configured capture job whose exact non-inherited limits were
/// read back before it can be supplied to `PROC_THREAD_ATTRIBUTE_JOB_LIST`.
pub(crate) struct PreparedCaptureAffinity {
    job: OwnedHandle,
}

impl PreparedCaptureAffinity {
    pub(crate) fn handle(&self) -> HANDLE {
        self.job.as_raw_handle() as HANDLE
    }

    pub(crate) fn validate_process(
        &self,
        policy: &CaptureWorkerAffinity,
        process: HANDLE,
    ) -> Result<()> {
        policy.validate_process_with(self.handle(), process, &NATIVE_JOB_API)
    }
}

impl CaptureWorkerAffinity {
    fn admit_with(
        requested: RequestedWorkerAffinity,
        mut topology: impl FnMut() -> Result<CollectorTopology>,
    ) -> Result<Self> {
        let before = topology()?;
        let mask = usize::try_from(requested.0)?;
        if before.active_processors == 0
            || before.active_processors > usize::BITS
            || before.system_mask.count_ones() != before.active_processors
            || before.process_mask == 0
            || before.process_mask & !before.system_mask != 0
            || mask == 0
            || mask & !before.process_mask != 0
            || mask & !before.system_mask != 0
        {
            bail!("worker affinity is outside the admitted collector/system allowed processors");
        }
        let policy = Self {
            mask,
            topology: before,
        };
        policy.require_topology(topology()?)?;
        Ok(policy)
    }

    fn require_topology(&self, current: CollectorTopology) -> Result<()> {
        if current != self.topology {
            bail!("capture processor topology or collector affinity changed after admission");
        }
        Ok(())
    }

    pub(crate) fn require_current(&self) -> Result<()> {
        self.require_topology(collector_topology()?)
    }

    #[cfg(test)]
    pub(crate) fn bind(&self, process: HANDLE) -> Result<OwnedHandle> {
        self.bind_with(process, &NATIVE_JOB_API)
    }

    pub(crate) fn prepare(&self) -> Result<PreparedCaptureAffinity> {
        self.prepare_with(&NATIVE_JOB_API)
    }

    #[cfg(test)]
    pub(crate) fn bind_injected(
        &self,
        process: HANDLE,
        failure: InjectedAffinityFailure,
    ) -> Result<OwnedHandle> {
        let mut api = NATIVE_JOB_API;
        match failure {
            InjectedAffinityFailure::Create => api.create = || bail!("injected create failure"),
            InjectedAffinityFailure::Configure => {
                api.configure = |_, _| bail!("injected configure failure")
            }
            InjectedAffinityFailure::Assign => {
                api.assign = |_, _| bail!("injected assignment failure")
            }
            InjectedAffinityFailure::Readback => {
                api.readback = |_| bail!("injected readback failure")
            }
            InjectedAffinityFailure::Membership => {
                api.membership = |_, _| bail!("injected membership query failure")
            }
            InjectedAffinityFailure::Nonmember => api.membership = |_, _| Ok(false),
            InjectedAffinityFailure::WrongMask => {
                api.readback = |job| {
                    let (size, mut limits) = readback_job(job)?;
                    limits.BasicLimitInformation.Affinity ^= 1;
                    Ok((size, limits))
                }
            }
            InjectedAffinityFailure::Inheritance => {
                api.inheritance = |_| bail!("injected inheritance query failure")
            }
            InjectedAffinityFailure::ChildAffinity => {
                api.process_affinity = |_| bail!("injected child affinity query failure")
            }
            InjectedAffinityFailure::Topology => {
                api.topology = |_| bail!("injected topology query failure")
            }
            InjectedAffinityFailure::PostAssignmentTopologyDrift => {
                api.topology = |boundary| {
                    let mut topology = collector_topology()?;
                    if boundary != TopologyBoundary::BeforeBind {
                        topology.active_processors += 1;
                    }
                    Ok(topology)
                }
            }
            InjectedAffinityFailure::PostReadbackTopologyDrift => {
                api.topology = |boundary| {
                    let mut topology = collector_topology()?;
                    if boundary == TopologyBoundary::AfterReadback {
                        topology.system_mask ^= 1;
                    }
                    Ok(topology)
                }
            }
            InjectedAffinityFailure::InvalidChildMask => {
                api.process_affinity = |process| {
                    let (_, system) = process_affinity(process)?;
                    Ok((0, system))
                }
            }
        }
        self.bind_with(process, &api)
    }

    #[cfg(test)]
    fn bind_with(&self, process: HANDLE, api: &JobApi) -> Result<OwnedHandle> {
        self.require_topology((api.topology)(TopologyBoundary::BeforeBind)?)?;
        let job = (api.create)()?;
        let raw_job = job.as_raw_handle() as HANDLE;
        // SAFETY: this Win32 POD consists only of integer fields and pointers.
        let mut limits = unsafe { zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = LIMITS;
        limits.BasicLimitInformation.Affinity = self.mask;
        (api.configure)(raw_job, &limits)?;
        (api.assign)(raw_job, process)?;
        // A failed readback drops our kill-on-close job; caller still reaps child.
        self.validate_process_with(raw_job, process, api)?;
        Ok(job)
    }

    pub(crate) fn validate(&self, job: HANDLE, process: HANDLE) -> Result<()> {
        self.validate_process_with(job, process, &NATIVE_JOB_API)
    }

    fn prepare_with(&self, api: &JobApi) -> Result<PreparedCaptureAffinity> {
        self.require_topology((api.topology)(TopologyBoundary::BeforeBind)?)?;
        let job = (api.create)()?;
        let raw_job = job.as_raw_handle() as HANDLE;
        // SAFETY: this Win32 POD consists only of integer fields and pointers.
        let mut limits = unsafe { zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = LIMITS;
        limits.BasicLimitInformation.Affinity = self.mask;
        (api.configure)(raw_job, &limits)?;
        let handle_flags = (api.inheritance)(raw_job)?;
        let (returned, readback) = (api.readback)(raw_job)?;
        self.validate_prepared_readback(
            handle_flags,
            returned,
            readback.BasicLimitInformation.LimitFlags,
            readback.BasicLimitInformation.Affinity,
        )?;
        self.require_topology((api.topology)(TopologyBoundary::BeforeReadback)?)?;
        Ok(PreparedCaptureAffinity { job })
    }

    fn validate_process_with(&self, job: HANDLE, process: HANDLE, api: &JobApi) -> Result<()> {
        self.require_topology((api.topology)(TopologyBoundary::BeforeReadback)?)?;
        let handle_flags = (api.inheritance)(job)?;
        let (returned, limits) = (api.readback)(job)?;
        let member = (api.membership)(job, process)?;
        let (process_mask, system_mask) = (api.process_affinity)(process)?;
        self.validate_readback(
            handle_flags,
            returned,
            limits.BasicLimitInformation.LimitFlags,
            limits.BasicLimitInformation.Affinity,
            member,
            process_mask,
            system_mask,
        )?;
        self.require_topology((api.topology)(TopologyBoundary::AfterReadback)?)
    }

    fn validate_prepared_readback(
        &self,
        handle_flags: u32,
        returned: u32,
        flags: u32,
        mask: usize,
    ) -> Result<()> {
        if handle_flags != 0
            || returned as usize != size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
            || flags != LIMITS
            || mask != self.mask
        {
            bail!("capture affinity job configuration readback is invalid");
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_readback(
        &self,
        handle_flags: u32,
        returned: u32,
        flags: u32,
        mask: usize,
        member: bool,
        process_mask: usize,
        system_mask: usize,
    ) -> Result<()> {
        if handle_flags != 0
            || returned as usize != size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()
            || flags != LIMITS
            || mask != self.mask
            || !member
            || process_mask == 0
            || process_mask & !self.mask != 0
            || system_mask != self.topology.system_mask
            || process_mask & !system_mask != 0
        {
            bail!("capture affinity job readback or owned worker membership is invalid");
        }
        Ok(())
    }
}

fn create_job() -> Result<OwnedHandle> {
    // Null name/security attributes create a fresh unnamed, non-inherited job.
    // SAFETY: both optional pointers are null, returned handle is owned below.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(std::io::Error::last_os_error()).context("create capture affinity job");
    }
    // SAFETY: CreateJobObjectW returned one new owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(job) })
}

fn create_collector_job() -> Result<CollectorJobHandle> {
    create_job().map(CollectorJobHandle::Native)
}

fn job_inheritance(job: HANDLE) -> Result<u32> {
    let mut flags = 0;
    // SAFETY: retained job and one writable DWORD output.
    if unsafe { GetHandleInformation(job, &mut flags) } == 0 {
        return Err(std::io::Error::last_os_error()).context("query capture job inheritance");
    }
    Ok(flags)
}

fn configure_job(job: HANDLE, limits: &JOBOBJECT_EXTENDED_LIMIT_INFORMATION) -> Result<()> {
    // SAFETY: retained job and an initialized input struct of the declared size.
    if unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("configure capture affinity job");
    }
    Ok(())
}

#[cfg(test)]
fn assign_job(job: HANDLE, process: HANDLE) -> Result<()> {
    // SAFETY: both retained handles are live; neither is opened by caller PID.
    if unsafe { AssignProcessToJobObject(job, process) } == 0 {
        return Err(std::io::Error::last_os_error()).context("assign capture affinity job");
    }
    Ok(())
}

fn assign_current_process(job: HANDLE) -> Result<()> {
    // SAFETY: the pseudo-handle always identifies this disposable collector;
    // no PID is opened or accepted from a caller.
    if unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("assign collector to capture affinity job");
    }
    Ok(())
}

fn readback_job(job: HANDLE) -> Result<(u32, JOBOBJECT_EXTENDED_LIMIT_INFORMATION)> {
    // SAFETY: Win32 POD may be zero-initialized.
    let mut limits = unsafe { zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
    let mut returned = 0;
    // SAFETY: outputs are writable for their declared lengths; job is retained.
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("read back capture affinity job");
    }
    Ok((returned, limits))
}

fn job_membership(job: HANDLE, process: HANDLE) -> Result<bool> {
    let mut member = 0;
    // SAFETY: retained handles and one writable BOOL output.
    if unsafe { IsProcessInJob(process, job, &mut member) } == 0 {
        return Err(std::io::Error::last_os_error()).context("query capture job membership");
    }
    Ok(member != 0)
}

fn current_process_job_membership(job: HANDLE) -> Result<bool> {
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle which is not
    // closed and cannot accidentally name another process.
    job_membership(job, unsafe { GetCurrentProcess() })
}

fn current_process_affinity() -> Result<(usize, usize)> {
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle which is not
    // closed and cannot accidentally name another process.
    process_affinity(unsafe { GetCurrentProcess() })
}

fn collector_topology() -> Result<CollectorTopology> {
    // SAFETY: read-only topology queries accept group zero without pointers.
    let before = unsafe { GetActiveProcessorCount(0) };
    // SAFETY: our own process pseudo-handle is valid and never closed.
    let (process_mask, system_mask) = process_affinity(unsafe { GetCurrentProcess() })?;
    let after = unsafe { GetActiveProcessorCount(0) };
    if before == 0 || before > usize::BITS || before != after || system_mask.count_ones() != before
    {
        bail!("capture processor count is unsupported or changed during admission");
    }
    Ok(CollectorTopology {
        active_processors: before,
        process_mask,
        system_mask,
    })
}

fn process_affinity(process: HANDLE) -> Result<(usize, usize)> {
    let group = || -> Result<()> {
        let mut count = 1;
        let mut id = 0;
        // SAFETY: topology query is read-only; count describes one writable group slot.
        if unsafe { GetActiveProcessorGroupCount() } != 1
            || unsafe { GetProcessGroupAffinity(process, &mut count, &mut id) } == 0
            || count != 1
            || id != 0
        {
            bail!("capture affinity requires stable single processor group zero");
        }
        Ok(())
    };
    group()?;
    let mut process_mask = 0;
    let mut system_mask = 0;
    // SAFETY: retained process handle and two writable ULONG_PTR outputs.
    if unsafe { GetProcessAffinityMask(process, &mut process_mask, &mut system_mask) } == 0 {
        return Err(std::io::Error::last_os_error()).context("query capture process affinity");
    }
    group()?;
    if process_mask == 0 || system_mask == 0 || process_mask & !system_mask != 0 {
        bail!("capture process/system affinity masks are invalid");
    }
    Ok((process_mask, system_mask))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAG_PROTECT_FROM_CLOSE};

    fn native_fixture_request() -> RequestedWorkerAffinity {
        let (process, _) = current_process_affinity().expect("fixture process affinity");
        let requested = process & process.wrapping_neg();
        RequestedWorkerAffinity::parse(&format!("{requested:016x}"))
            .expect("fixture single-processor request")
    }

    fn collector_affinity_fixture_command(mode: &str) -> std::process::Child {
        use std::process::{Command, Stdio};

        let name = concat!(module_path!(), "::collector_affinity_native_fixture_child")
            .split_once("::")
            .unwrap()
            .1;
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                name,
                "--test-threads=1",
                "--nocapture",
            ])
            .env("SCRIBE_TEST_COLLECTOR_AFFINITY_CHILD", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap()
    }

    fn drain_fixture_marker(
        stdout: std::process::ChildStdout,
        marker: &'static str,
    ) -> (
        std::sync::mpsc::Receiver<std::result::Result<(), String>>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{BufRead, BufReader};
        use std::sync::mpsc::channel;

        let (sent, received) = channel();
        let reader = std::thread::spawn(move || {
            let mut announced = false;
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if !announced && line.contains(marker) {
                            let _ = sent.send(Ok(()));
                            announced = true;
                        }
                    }
                    Err(error) => {
                        if !announced {
                            let _ = sent.send(Err(error.to_string()));
                        }
                        return;
                    }
                }
            }
            if !announced {
                let _ = sent.send(Err("fixture exited before its gate".to_owned()));
            }
        });
        (received, reader)
    }

    fn bounded_fixture_wait(
        child: &mut std::process::Child,
    ) -> std::result::Result<std::process::ExitStatus, String> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        // SAFETY: Child retains this exact process handle and both waits are bounded.
        let mut waited = unsafe { WaitForSingleObject(child.as_raw_handle() as _, 10_000) };
        if waited != WAIT_OBJECT_0 {
            drop(child.stdin.take());
            let kill = child.kill();
            // SAFETY: same retained exact process handle and bounded cleanup wait.
            waited = unsafe { WaitForSingleObject(child.as_raw_handle() as _, 10_000) };
            if waited != WAIT_OBJECT_0 {
                return Err(format!(
                    "fixture cleanup did not signal after kill attempt {kill:?}: wait={waited}"
                ));
            }
        }
        child
            .wait()
            .map_err(|error| format!("reap signaled fixture: {error}"))
    }

    fn collector_limits(mask: usize) -> JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
        // SAFETY: this Win32 POD consists only of integer fields and pointers.
        let mut limits = unsafe { zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = COLLECTOR_LIMITS;
        limits.BasicLimitInformation.Affinity = mask;
        limits
    }

    fn fake_collector_readback(
        _job: HANDLE,
    ) -> Result<(u32, JOBOBJECT_EXTENDED_LIMIT_INFORMATION)> {
        Ok((
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            collector_limits(3),
        ))
    }

    fn fake_collector_topology() -> Result<CollectorTopology> {
        Ok(CollectorTopology {
            active_processors: 4,
            process_mask: 3,
            system_mask: 15,
        })
    }

    fn fake_collector_api() -> CollectorJobApi {
        CollectorJobApi {
            create: || Ok(CollectorJobHandle::Fake(1)),
            configure: |_, _| Ok(()),
            assign_current: |_| Ok(()),
            readback: fake_collector_readback,
            membership: |_| Ok(true),
            inheritance: |_| Ok(0),
            process_affinity: || Ok((3, 15)),
            topology: fake_collector_topology,
        }
    }

    fn topology() -> CollectorTopology {
        CollectorTopology {
            active_processors: 4,
            process_mask: 0b0111,
            system_mask: 0b1111,
        }
    }

    #[test]
    fn capture_observation_affinity_mask_is_canonical_nonzero_and_bounded() {
        for valid in ["0000000000000001", "8000000000000000", "ffffffffffffffff"] {
            assert!(RequestedWorkerAffinity::parse(valid).is_ok());
        }
        for invalid in [
            "",
            "1",
            "0x00000000000001",
            "0000000000000000",
            "FFFFFFFFFFFFFFFF",
            "00000000000000001",
            " 000000000000001",
            "000000000000001 ",
            "000000000000001\n",
            "+000000000000001",
            "000000000000000g",
            "é00000000000001",
        ] {
            assert!(
                RequestedWorkerAffinity::parse(invalid).is_err(),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn capture_observation_affinity_admission_denies_unknown_masks_and_topology_drift() {
        let request = RequestedWorkerAffinity::parse("0000000000000003").unwrap();
        CaptureWorkerAffinity::admit_with(request, || Ok(topology())).unwrap();
        for mask in [8, 16, u64::MAX] {
            assert!(
                CaptureWorkerAffinity::admit_with(RequestedWorkerAffinity(mask), || Ok(topology()))
                    .is_err()
            );
        }
        assert!(CaptureWorkerAffinity::admit_with(request, || bail!("query failed")).is_err());
        for field in 0..3 {
            let mut calls = 0;
            assert!(
                CaptureWorkerAffinity::admit_with(request, || {
                    calls += 1;
                    let mut value = topology();
                    if calls == 2 {
                        match field {
                            0 => value.active_processors += 1,
                            1 => value.process_mask ^= 4,
                            _ => value.system_mask ^= 8,
                        }
                    }
                    Ok(value)
                })
                .is_err()
            );
        }
    }

    #[test]
    fn capture_observation_affinity_readback_requires_exact_job_and_allowed_child_subset() {
        let policy =
            CaptureWorkerAffinity::admit_with(RequestedWorkerAffinity(3), || Ok(topology()))
                .unwrap();
        let size = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
        for child in [1, 2, 3] {
            policy
                .validate_readback(0, size, LIMITS, 3, true, child, 15)
                .unwrap();
        }
        for field in 0..10 {
            let (mut inherit, mut bytes, mut flags, mut mask, mut member, mut child, mut system) =
                (0, size, LIMITS, 3, true, 3, 15);
            match field {
                0 => inherit = HANDLE_FLAG_INHERIT,
                1 => bytes -= 1,
                2 => flags ^= JOB_OBJECT_LIMIT_AFFINITY,
                3 => flags |= 0x4000,
                4 => mask = 7,
                5 => member = false,
                6 => child = 0,
                7 => child = 4,
                8 => system = 7,
                _ => inherit = HANDLE_FLAG_PROTECT_FROM_CLOSE,
            }
            assert!(
                policy
                    .validate_readback(inherit, bytes, flags, mask, member, child, system)
                    .is_err()
            );
        }
    }

    #[test]
    fn collector_affinity_uses_exact_affinity_only_noninherited_job_and_stable_effective_mask() {
        let api = fake_collector_api();
        assert!(CollectorAffinityGuard::establish_with(RequestedWorkerAffinity(4), &api).is_err());
        let guard = CollectorAffinityGuard::establish_with(RequestedWorkerAffinity(3), &api)
            .expect("fake collector constraint");
        assert_eq!(guard.mask, 3);
        assert_eq!(guard.effective_process_mask, 3);
        guard.require_current_with(&api).unwrap();

        for field in 0..5 {
            let (mut inherit, mut returned, mut limits) = (
                0,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                collector_limits(3),
            );
            match field {
                0 => inherit = HANDLE_FLAG_INHERIT,
                1 => returned -= 1,
                2 => limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                3 => limits.BasicLimitInformation.LimitFlags = 0,
                _ => limits.BasicLimitInformation.Affinity = 1,
            }
            assert!(validate_collector_configuration(inherit, (returned, limits), 3).is_err());
        }
    }

    #[test]
    fn collector_affinity_per_call_failures_are_closed_without_global_injection() {
        let requested = RequestedWorkerAffinity(3);

        let mut api = fake_collector_api();
        api.create = || bail!("injected create failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.configure = |_, _| bail!("injected configure failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.assign_current = |_| bail!("injected assignment failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.readback = |_| bail!("injected readback failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.membership = |_| bail!("injected membership failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.membership = |_| Ok(false);
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.inheritance = |_| bail!("injected inheritance failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.process_affinity = || bail!("injected effective mask failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.process_affinity = || Ok((0, 15));
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());

        let mut api = fake_collector_api();
        api.topology = || bail!("injected topology failure");
        assert!(CollectorAffinityGuard::establish_with(requested, &api).is_err());
    }

    #[test]
    fn collector_affinity_rejects_effective_mask_or_topology_drift_after_assignment() {
        fn narrowed_topology() -> Result<CollectorTopology> {
            Ok(CollectorTopology {
                active_processors: 4,
                process_mask: 1,
                system_mask: 15,
            })
        }
        let guard = CollectorAffinityGuard::establish_with(
            RequestedWorkerAffinity(3),
            &fake_collector_api(),
        )
        .unwrap();

        let mut narrowed = fake_collector_api();
        narrowed.topology = narrowed_topology;
        narrowed.process_affinity = || Ok((1, 15));
        assert!(guard.require_current_with(&narrowed).is_err());

        let mut outside = fake_collector_api();
        outside.topology = || {
            Ok(CollectorTopology {
                active_processors: 4,
                process_mask: 4,
                system_mask: 15,
            })
        };
        outside.process_affinity = || Ok((4, 15));
        assert!(guard.require_current_with(&outside).is_err());

        let mut changed_system = fake_collector_api();
        changed_system.topology = || {
            Ok(CollectorTopology {
                active_processors: 3,
                process_mask: 3,
                system_mask: 7,
            })
        };
        changed_system.process_affinity = || Ok((3, 7));
        assert!(guard.require_current_with(&changed_system).is_err());
    }

    #[test]
    #[ignore = "child-only: assigns this disposable fixture process to a collector job"]
    fn collector_affinity_native_fixture_child() {
        use std::io::{Read, Write};

        let Ok(mode) = std::env::var("SCRIBE_TEST_COLLECTOR_AFFINITY_CHILD") else {
            return;
        };
        let request = native_fixture_request();
        let before = current_process_affinity().unwrap();
        match mode.as_str() {
            "constrained" => {
                let guard = CollectorAffinityGuard::establish(request).unwrap();
                guard.require_current().unwrap();
                let after = current_process_affinity().unwrap();
                assert_eq!(after.0, usize::try_from(request.0).unwrap());
                assert_eq!(after.1, before.1);

                // The existing capture-worker policy is admitted only after
                // collector confinement and prepares the exact retained job
                // used by the atomic JOB_LIST launcher. Atomic attachment is
                // exercised by that launcher's own native fixture suite.
                let worker = request.admit().unwrap();
                let prepared = worker.prepare().unwrap();
                assert!(!prepared.handle().is_null());

                // A disposable descendant inherits the collector job, then
                // receives the existing worker affinity+kill job. This proves
                // real simultaneous membership without assigning the test host.
                use std::os::windows::io::AsRawHandle;
                use std::time::Duration;
                let mut nested = collector_affinity_fixture_command("nested-worker-target");
                let process = nested.as_raw_handle() as HANDLE;
                let worker_job = match worker.bind(process).and_then(|job| {
                    worker.validate(job.as_raw_handle() as HANDLE, process)?;
                    if !job_membership(guard.job.raw(), process)? {
                        bail!("nested fixture omitted inherited collector membership");
                    }
                    Ok(job)
                }) {
                    Ok(job) => job,
                    Err(error) => {
                        drop(nested.stdin.take());
                        let _ = nested.kill();
                        let cleanup = bounded_fixture_wait(&mut nested);
                        panic!("nested worker setup failed: {error:#}; cleanup={cleanup:?}");
                    }
                };
                let stdout = nested.stdout.take().unwrap();
                let (received, reader) = drain_fixture_marker(stdout, "SCRIBE_NESTED_WORKER_READY");
                let observed = received.recv_timeout(Duration::from_secs(10));
                if !observed.as_ref().is_ok_and(|result| result.is_ok()) {
                    drop(nested.stdin.take());
                    let _ = nested.kill();
                }
                if observed.as_ref().is_ok_and(|result| result.is_ok()) {
                    assert!(nested.try_wait().unwrap().is_none());
                }
                drop(worker_job);
                let status = bounded_fixture_wait(&mut nested);
                if let Err(error) = &status {
                    panic!("nested worker cleanup failed: {error}; marker={observed:?}");
                }
                reader.join().unwrap();
                assert_eq!(observed.unwrap(), Ok(()));
                let _ = status.unwrap();
                println!("SCRIBE_COLLECTOR_CONFINED_BEFORE_TIMER");
                std::io::stdout().flush().unwrap();
                let mut permit = [0_u8; 1];
                std::io::stdin().read_exact(&mut permit).unwrap();
                let _measured_boundary = std::time::Instant::now();
                guard.require_current().unwrap();
            }
            "no-policy" => {
                let _measured_boundary = std::time::Instant::now();
                assert_eq!(current_process_affinity().unwrap(), before);
                println!("SCRIBE_COLLECTOR_NO_POLICY_UNCHANGED");
                std::io::stdout().flush().unwrap();
                let mut permit = [0_u8; 1];
                std::io::stdin().read_exact(&mut permit).unwrap();
                assert_eq!(current_process_affinity().unwrap(), before);
            }
            "post-assignment-failure" => {
                let mut api = NATIVE_COLLECTOR_JOB_API;
                api.membership = |_| Ok(false);
                let failed = CollectorAffinityGuard::establish_with(request, &api).is_err();
                // Match the production dispatcher's exit-only boundary after
                // irreversible self-assignment; returning to a UI loop is not
                // an available outcome in this disposable fixture.
                std::process::exit(if failed { 73 } else { 74 });
            }
            "nested-worker-target" => {
                println!("SCRIBE_NESTED_WORKER_READY");
                std::io::stdout().flush().unwrap();
                let mut permit = [0_u8; 1];
                std::io::stdin().read_exact(&mut permit).unwrap();
                panic!("nested worker fixture must be killed by worker job close");
            }
            _ => panic!("unknown collector affinity fixture mode"),
        }
    }

    #[test]
    fn collector_affinity_native_confinement_is_disposable_bounded_and_exit_only() {
        use std::io::Write;
        use std::time::Duration;

        for (mode, marker) in [
            ("constrained", "SCRIBE_COLLECTOR_CONFINED_BEFORE_TIMER"),
            ("no-policy", "SCRIBE_COLLECTOR_NO_POLICY_UNCHANGED"),
        ] {
            let mut child = collector_affinity_fixture_command(mode);
            let stdout = child.stdout.take().unwrap();
            let (received, reader) = drain_fixture_marker(stdout, marker);
            let observed = received.recv_timeout(Duration::from_secs(10));
            let permit = if observed.as_ref().is_ok_and(|result| result.is_ok()) {
                child
                    .stdin
                    .as_mut()
                    .ok_or_else(|| "fixture stdin unavailable".to_owned())
                    .and_then(|stdin| stdin.write_all(&[1]).map_err(|error| error.to_string()))
            } else {
                Err("fixture marker was not observed".to_owned())
            };
            drop(child.stdin.take());
            if permit.is_err() {
                let _ = child.kill();
            }
            let status = bounded_fixture_wait(&mut child);
            if let Err(error) = &status {
                panic!(
                    "collector fixture cleanup failed: {error}; marker={observed:?}; permit={permit:?}"
                );
            }
            reader.join().unwrap();
            assert_eq!(observed.unwrap(), Ok(()));
            assert_eq!(permit, Ok(()));
            assert!(status.unwrap().success());
        }

        let mut child = collector_affinity_fixture_command("post-assignment-failure");
        let status = bounded_fixture_wait(&mut child)
            .unwrap_or_else(|error| panic!("post-assignment fixture cleanup failed: {error}"));
        assert_eq!(status.code(), Some(73));
    }
}
