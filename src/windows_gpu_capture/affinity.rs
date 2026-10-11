//! Capture-only worker job affinity. Production capture launch prepares and
//! validates the job before `CreateProcessW` supplies it through
//! `PROC_THREAD_ATTRIBUTE_JOB_LIST`; the post-spawn binder remains test-only
//! for legacy failure-boundary coverage. This is not a thread-count or
//! qualification claim.

use std::mem::{size_of, zeroed};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use anyhow::{Context, Result, bail};
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE};
#[cfg(test)]
use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_AFFINITY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, GetCurrentProcess,
    GetProcessAffinityMask, GetProcessGroupAffinity,
};

const LIMITS: u32 = JOB_OBJECT_LIMIT_AFFINITY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

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
}
