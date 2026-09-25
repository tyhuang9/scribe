use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTERADDRESS, D3DKMT_ADAPTERINFO, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2,
    D3DKMT_MEMORY_SEGMENT_GROUP_LOCAL, D3DKMT_MEMORY_SEGMENT_GROUP_NON_LOCAL,
    D3DKMT_QUERYADAPTERINFO, D3DKMT_QUERYVIDEOMEMORYINFO, D3DKMTCloseAdapter, D3DKMTEnumAdapters2,
    D3DKMTQueryAdapterInfo, D3DKMTQueryVideoMemoryInfo, KMTQAITYPE_ADAPTERADDRESS,
    KMTQAITYPE_PHYSICALADAPTERCOUNT,
};
use windows_sys::Win32::Foundation::{HANDLE, LUID, STATUS_BUFFER_TOO_SMALL};
use windows_sys::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};

use crate::onnx_worker::WorkerObservationLease;

const MAX_ADAPTERS: usize = 64;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct SegmentSummary {
    pub(crate) sampled_max_current_usage_bytes: u64,
    pub(crate) sampled_max_current_reservation_bytes: u64,
    pub(crate) sampled_min_budget_bytes: u64,
    pub(crate) sampled_min_available_for_reservation_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct VideoMemorySummary {
    pub(crate) local: SegmentSummary,
    pub(crate) non_local: SegmentSummary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct TelemetrySummary {
    pub(crate) sample_count: u64,
    pub(crate) sampled_max_private_usage_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) video_memory: Option<VideoMemorySummary>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SegmentSample {
    budget: u64,
    current_usage: u64,
    current_reservation: u64,
    available_for_reservation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TelemetrySample {
    private_usage: u64,
    local: Option<SegmentSample>,
    non_local: Option<SegmentSample>,
}

trait TelemetryProbe: Send + Sync + 'static {
    fn sample(&self) -> Result<TelemetrySample>;
}

struct WindowsTelemetryProbe {
    lease: WorkerObservationLease,
    adapter: Option<Arc<AdapterHandle>>,
}

impl WindowsTelemetryProbe {
    fn cpu(lease: WorkerObservationLease) -> Self {
        Self {
            lease,
            adapter: None,
        }
    }

    fn gpu(lease: WorkerObservationLease, stable_device: &str) -> Result<Self> {
        Ok(Self {
            lease,
            adapter: Some(Arc::new(resolve_adapter(stable_device)?)),
        })
    }
}

impl TelemetryProbe for WindowsTelemetryProbe {
    fn sample(&self) -> Result<TelemetrySample> {
        self.lease.require_current()?;
        let process = self.lease.process_handle();
        let private_usage = query_private_usage(process)?;
        let (local, non_local) = match &self.adapter {
            Some(adapter) => (
                Some(query_video_memory(
                    process,
                    adapter.0,
                    D3DKMT_MEMORY_SEGMENT_GROUP_LOCAL,
                )?),
                Some(query_video_memory(
                    process,
                    adapter.0,
                    D3DKMT_MEMORY_SEGMENT_GROUP_NON_LOCAL,
                )?),
            ),
            None => (None, None),
        };
        self.lease.require_current()?;
        Ok(TelemetrySample {
            private_usage,
            local,
            non_local,
        })
    }
}

pub(crate) struct SamplingSession {
    stop: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<TelemetrySummary, String>>>>,
    thread: Option<JoinHandle<()>>,
}

impl SamplingSession {
    pub(crate) fn cpu(lease: WorkerObservationLease) -> Result<Self> {
        Self::start(Arc::new(WindowsTelemetryProbe::cpu(lease)))
    }

    pub(crate) fn gpu(lease: WorkerObservationLease, stable_device: &str) -> Result<Self> {
        Self::start(Arc::new(WindowsTelemetryProbe::gpu(lease, stable_device)?))
    }

    fn start(probe: Arc<dyn TelemetryProbe>) -> Result<Self> {
        // Fail before inference if even the first handle-bound sample is not
        // available. The first sample is retained as part of the request
        // window rather than being a capability-only preflight.
        let first = probe.sample()?;
        let stop = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let thread_stop = Arc::clone(&stop);
        let thread_result = Arc::clone(&result);
        let thread = std::thread::Builder::new()
            .name("scribe-gpu-observation-telemetry".to_owned())
            .spawn(move || {
                let mut samples = vec![first];
                while !thread_stop.load(Ordering::Acquire) {
                    std::thread::sleep(SAMPLE_INTERVAL);
                    if thread_stop.load(Ordering::Acquire) {
                        break;
                    }
                    match probe.sample() {
                        Ok(sample) => samples.push(sample),
                        Err(error) => {
                            if let Ok(mut slot) = thread_result.lock() {
                                *slot = Some(Err(format!("{error:#}")));
                            }
                            return;
                        }
                    }
                }
                let aggregated = aggregate_samples(&samples).map_err(|error| format!("{error:#}"));
                if let Ok(mut slot) = thread_result.lock() {
                    *slot = Some(aggregated);
                }
            })
            .context("could not start bounded worker telemetry sampling")?;
        Ok(Self {
            stop,
            result,
            thread: Some(thread),
        })
    }

    pub(crate) fn finish(mut self) -> Result<TelemetrySummary> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("sampling thread exists until finish")
            .join()
            .map_err(|_| anyhow!("worker telemetry sampling thread panicked"))?;
        self.result
            .lock()
            .map_err(|_| anyhow!("worker telemetry result lock was poisoned"))?
            .take()
            .ok_or_else(|| anyhow!("worker telemetry sampler returned no result"))?
            .map_err(anyhow::Error::msg)
    }
}

impl Drop for SamplingSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn aggregate_samples(samples: &[TelemetrySample]) -> Result<TelemetrySummary> {
    if samples.is_empty() {
        bail!("worker telemetry produced no request-window samples")
    }
    if samples
        .iter()
        .any(|sample| sample.local.is_some() != sample.non_local.is_some())
    {
        bail!("worker telemetry omitted one required memory segment")
    }
    let has_video = samples[0].local.is_some() && samples[0].non_local.is_some();
    if samples
        .iter()
        .any(|sample| (sample.local.is_some() && sample.non_local.is_some()) != has_video)
    {
        bail!("worker telemetry changed video-memory availability during sampling")
    }
    let summarize = |select: fn(&TelemetrySample) -> Option<SegmentSample>| -> Result<_> {
        let segments = samples
            .iter()
            .map(select)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("worker telemetry omitted a required memory segment"))?;
        Ok(SegmentSummary {
            sampled_max_current_usage_bytes: segments
                .iter()
                .map(|segment| segment.current_usage)
                .max()
                .expect("samples is nonempty"),
            sampled_max_current_reservation_bytes: segments
                .iter()
                .map(|segment| segment.current_reservation)
                .max()
                .expect("samples is nonempty"),
            sampled_min_budget_bytes: segments
                .iter()
                .map(|segment| segment.budget)
                .min()
                .expect("samples is nonempty"),
            sampled_min_available_for_reservation_bytes: segments
                .iter()
                .map(|segment| segment.available_for_reservation)
                .min()
                .expect("samples is nonempty"),
        })
    };
    Ok(TelemetrySummary {
        sample_count: u64::try_from(samples.len())
            .map_err(|_| anyhow!("worker telemetry sample count overflowed"))?,
        sampled_max_private_usage_bytes: samples
            .iter()
            .map(|sample| sample.private_usage)
            .max()
            .expect("samples is nonempty"),
        video_memory: if has_video {
            Some(VideoMemorySummary {
                local: summarize(|sample| sample.local)?,
                non_local: summarize(|sample| sample.non_local)?,
            })
        } else {
            None
        },
    })
}

fn query_private_usage(process: HANDLE) -> Result<u64> {
    let mut counters = unsafe { zeroed::<PROCESS_MEMORY_COUNTERS_EX>() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: process is a retained live child-process handle, counters is
    // writable, and the size describes the extended structure.
    let ok = unsafe {
        GetProcessMemoryInfo(
            process,
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not sample worker current private usage");
    }
    u64::try_from(counters.PrivateUsage)
        .map_err(|_| anyhow!("worker private usage exceeded the report integer range"))
}

fn query_video_memory(process: HANDLE, adapter: u32, segment: i32) -> Result<SegmentSample> {
    let mut query = D3DKMT_QUERYVIDEOMEMORYINFO {
        hProcess: process,
        hAdapter: adapter,
        MemorySegmentGroup: segment,
        Budget: 0,
        CurrentUsage: 0,
        CurrentReservation: 0,
        AvailableForReservation: 0,
        PhysicalAdapterIndex: 0,
    };
    // SAFETY: query points to the exact windows-sys ABI structure and both
    // the retained process and enumerated adapter handles remain live.
    let status = unsafe { D3DKMTQueryVideoMemoryInfo(&mut query) };
    if status < 0 {
        bail!("D3DKMTQueryVideoMemoryInfo failed with NTSTATUS {status:#x}")
    }
    Ok(SegmentSample {
        budget: query.Budget,
        current_usage: query.CurrentUsage,
        current_reservation: query.CurrentReservation,
        available_for_reservation: query.AvailableForReservation,
    })
}

struct AdapterHandle(u32);

unsafe impl Send for AdapterHandle {}
unsafe impl Sync for AdapterHandle {}

impl Drop for AdapterHandle {
    fn drop(&mut self) {
        let close = D3DKMT_CLOSEADAPTER { hAdapter: self.0 };
        // SAFETY: this guard exclusively owns the enumerated adapter handle.
        let _ = unsafe { D3DKMTCloseAdapter(&close) };
    }
}

fn resolve_adapter(stable_device: &str) -> Result<AdapterHandle> {
    let expected = AdapterIdentity::parse(stable_device)?;
    let mut query = D3DKMT_ENUMADAPTERS2 {
        NumAdapters: 0,
        pAdapters: std::ptr::null_mut(),
    };
    // SAFETY: the first call supplies a writable count and no adapter buffer.
    let status = unsafe { D3DKMTEnumAdapters2(&mut query) };
    if (status < 0 && status != STATUS_BUFFER_TOO_SMALL)
        || query.NumAdapters == 0
        || query.NumAdapters as usize > MAX_ADAPTERS
    {
        bail!("Windows adapter enumeration is unavailable or oversized")
    }
    let mut adapters = vec![unsafe { zeroed::<D3DKMT_ADAPTERINFO>() }; query.NumAdapters as usize];
    query.pAdapters = adapters.as_mut_ptr();
    // SAFETY: pAdapters points to NumAdapters writable entries.
    let status = unsafe { D3DKMTEnumAdapters2(&mut query) };
    if status < 0 || query.NumAdapters as usize > adapters.len() {
        bail!("Windows adapter enumeration changed or failed")
    }
    adapters.truncate(query.NumAdapters as usize);
    // Guard every returned handle before any fallible adapter query. If one
    // query fails, the iterator and its remaining items still close all of
    // the handles returned by D3DKMTEnumAdapters2.
    let guarded = adapters
        .into_iter()
        .map(|adapter| (AdapterHandle(adapter.hAdapter), adapter.AdapterLuid))
        .collect::<Vec<_>>();
    let mut matched = Vec::new();
    for (owned, luid) in guarded {
        if adapter_matches(&owned, luid, expected)? {
            matched.push(owned);
        }
    }
    if matched.len() != 1 {
        bail!("selected GPU identity did not map to exactly one Windows adapter")
    }
    Ok(matched.pop().expect("checked exact adapter count"))
}

#[derive(Clone, Copy)]
enum AdapterIdentity {
    Luid([u8; 8]),
    Pci {
        bus: u32,
        device: u32,
        function: u32,
    },
}

impl AdapterIdentity {
    fn parse(value: &str) -> Result<Self> {
        if let Some(hex) = value.strip_prefix("native:luid:") {
            if hex.len() != 16 || !is_lower_hex(hex) {
                bail!("stable LUID identity is not canonical lowercase hexadecimal")
            }
            let mut bytes = [0_u8; 8];
            for (index, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
                bytes[index] = u8::from_str_radix(std::str::from_utf8(chunk)?, 16)?;
            }
            return Ok(Self::Luid(bytes));
        }
        if let Some((bus, device, function)) = crate::onnx_worker::parse_native_pci_location(value)
        {
            return Ok(Self::Pci {
                bus,
                device,
                function,
            });
        }
        bail!("selected GPU identity cannot be independently mapped to a Windows LUID")
    }
}

fn is_lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn adapter_matches(adapter: &AdapterHandle, luid: LUID, expected: AdapterIdentity) -> Result<bool> {
    let mut physical_count = 0_u32;
    query_adapter_info(
        adapter.0,
        KMTQAITYPE_PHYSICALADAPTERCOUNT,
        &mut physical_count,
    )?;
    if physical_count != 1 {
        bail!("linked or ambiguous Windows physical adapters are unsupported")
    }
    match expected {
        AdapterIdentity::Luid(bytes) => {
            let mut observed = [0_u8; 8];
            observed[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
            observed[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
            Ok(observed == bytes)
        }
        AdapterIdentity::Pci {
            bus,
            device,
            function,
        } => {
            let mut address = unsafe { zeroed::<D3DKMT_ADAPTERADDRESS>() };
            query_adapter_info(adapter.0, KMTQAITYPE_ADAPTERADDRESS, &mut address)?;
            Ok(address.BusNumber == bus
                && address.DeviceNumber == device
                && address.FunctionNumber == function)
        }
    }
}

fn query_adapter_info<T>(adapter: u32, query_type: i32, value: &mut T) -> Result<()> {
    let mut query = D3DKMT_QUERYADAPTERINFO {
        hAdapter: adapter,
        Type: query_type,
        pPrivateDriverData: (value as *mut T).cast(),
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    // SAFETY: value is writable for the exact declared byte size and remains
    // live through the synchronous adapter query.
    let status = unsafe { D3DKMTQueryAdapterInfo(&mut query) };
    if status < 0 {
        bail!("D3DKMTQueryAdapterInfo failed with NTSTATUS {status:#x}")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_observation_telemetry_aggregates_current_values_with_exact_units() {
        let samples = [
            TelemetrySample {
                private_usage: 10,
                local: Some(SegmentSample {
                    budget: 100,
                    current_usage: 20,
                    current_reservation: 3,
                    available_for_reservation: 70,
                }),
                non_local: Some(SegmentSample {
                    budget: 200,
                    current_usage: 40,
                    current_reservation: 5,
                    available_for_reservation: 150,
                }),
            },
            TelemetrySample {
                private_usage: 12,
                local: Some(SegmentSample {
                    budget: 90,
                    current_usage: 25,
                    current_reservation: 4,
                    available_for_reservation: 60,
                }),
                non_local: Some(SegmentSample {
                    budget: 180,
                    current_usage: 35,
                    current_reservation: 7,
                    available_for_reservation: 140,
                }),
            },
        ];
        let summary = aggregate_samples(&samples).unwrap();
        assert_eq!(summary.sample_count, 2);
        assert_eq!(summary.sampled_max_private_usage_bytes, 12);
        let video = summary.video_memory.unwrap();
        assert_eq!(video.local.sampled_max_current_usage_bytes, 25);
        assert_eq!(video.local.sampled_min_budget_bytes, 90);
        assert_eq!(video.non_local.sampled_max_current_reservation_bytes, 7);
        assert_eq!(
            video.non_local.sampled_min_available_for_reservation_bytes,
            140
        );
    }

    #[test]
    fn capture_observation_telemetry_rejects_missing_or_mixed_segments() {
        assert!(aggregate_samples(&[]).is_err());
        let cpu = TelemetrySample {
            private_usage: 1,
            local: None,
            non_local: None,
        };
        assert_eq!(aggregate_samples(&[cpu]).unwrap().video_memory, None);
        let mixed = TelemetrySample {
            private_usage: 2,
            local: Some(SegmentSample {
                budget: 1,
                current_usage: 1,
                current_reservation: 0,
                available_for_reservation: 0,
            }),
            non_local: None,
        };
        assert!(aggregate_samples(&[cpu, mixed]).is_err());
    }

    #[test]
    fn capture_observation_adapter_identity_is_canonical_and_never_uses_index_zero() {
        assert!(matches!(
            AdapterIdentity::parse("native:luid:0102030405060708").unwrap(),
            AdapterIdentity::Luid(_)
        ));
        assert!(matches!(
            AdapterIdentity::parse("native:pci:0000:01:00.0").unwrap(),
            AdapterIdentity::Pci { .. }
        ));
        assert!(matches!(
            AdapterIdentity::parse("native:0000:01:00.0").unwrap(),
            AdapterIdentity::Pci { .. }
        ));
        assert!(matches!(
            AdapterIdentity::parse("native:00000000:01:00.0").unwrap(),
            AdapterIdentity::Pci { .. }
        ));
        assert!(AdapterIdentity::parse("native:pci:0001:01:00.0").is_err());
        assert!(AdapterIdentity::parse("native:00000001:01:00.0").is_err());
        assert!(AdapterIdentity::parse("native:0000:01:20.0").is_err());
        assert!(AdapterIdentity::parse("native:0000:01:00.8").is_err());
        assert!(AdapterIdentity::parse("native:uuid:00112233445566778899aabbccddeeff").is_err());
        assert!(AdapterIdentity::parse("native:luid:ABCDEF0000000000").is_err());
        assert!(AdapterIdentity::parse("0").is_err());
    }

    #[test]
    fn capture_observation_windows_sdk_abi_matches_the_x64_contract() {
        assert_eq!(size_of::<D3DKMT_ADAPTERINFO>(), 20);
        assert_eq!(size_of::<D3DKMT_QUERYVIDEOMEMORYINFO>(), 56);
        assert_eq!(
            std::mem::offset_of!(D3DKMT_QUERYVIDEOMEMORYINFO, Budget),
            16
        );
        assert_eq!(D3DKMT_MEMORY_SEGMENT_GROUP_LOCAL, 0);
        assert_eq!(D3DKMT_MEMORY_SEGMENT_GROUP_NON_LOCAL, 1);
    }
}
