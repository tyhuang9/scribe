//! Worker-local Linux GPU identity and provider-index routing.
//!
//! Stable identity is always a canonical PCI function. Provider indexes are
//! deliberately process-local and are returned only after the current worker
//! reconciles its provider enumeration with two identical kernel fact samples.

use std::collections::{BTreeMap, BTreeSet};

const MAX_GPU_FACTS: usize = 64;
const MAX_PROVIDER_DEVICES: usize = 16;
const MAX_DRIVER_IDENTITY_BYTES: usize = 192;
const MAX_DISPLAY_NAME_BYTES: usize = 256;
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
const MAX_SYSFS_VALUE_BYTES: u64 = 4096;
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
const MAX_NVIDIA_INFORMATION_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxGpuBackend {
    Cuda,
    Vulkan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxGpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PciAddress {
    domain: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl PciAddress {
    fn parse(value: &str) -> Option<Self> {
        if value.len() != 12 || value != value.to_ascii_lowercase() {
            return None;
        }
        let bytes = value.as_bytes();
        if bytes[4] != b':' || bytes[7] != b':' || bytes[10] != b'.' {
            return None;
        }
        let domain = u16::from_str_radix(&value[0..4], 16).ok()?;
        let bus = u8::from_str_radix(&value[5..7], 16).ok()?;
        let device = u8::from_str_radix(&value[8..10], 16).ok()?;
        let function = u8::from_str_radix(&value[11..12], 16).ok()?;
        (device <= 0x1f && function <= 7).then_some(Self {
            domain,
            bus,
            device,
            function,
        })
    }

    fn from_provider_id(value: &str) -> Option<Self> {
        let value = value
            .strip_prefix("native:pci:")
            .or_else(|| value.strip_prefix("pci:"))
            .unwrap_or(value);
        Self::parse(value)
    }

    fn canonical(self) -> String {
        format!(
            "native:pci:{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.device, self.function
        )
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    fn sysfs_name(self) -> String {
        format!(
            "{:04x}:{:02x}:{:02x}.{:x}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

#[derive(Clone, Eq, PartialEq)]
struct LinuxGpuFact {
    address: PciAddress,
    vendor: LinuxGpuVendor,
    driver_identity: String,
    // This alias is intentionally private and this type intentionally does not
    // implement Debug. It may exist only while reconciling one CUDA Hello.
    nvidia_physical_uuid_alias: Option<String>,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
struct VulkanPciDriverFact {
    vendor: LinuxGpuVendor,
    identity: String,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct LinuxGpuFactSnapshot {
    devices: Vec<LinuxGpuFact>,
}

pub(crate) trait LinuxGpuFactSource {
    fn snapshot(&self, backend: LinuxGpuBackend) -> Result<LinuxGpuFactSnapshot, String>;
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ProviderLinuxGpuDevice {
    pub(crate) process_index: usize,
    pub(crate) native_identity_or_alias: Option<String>,
    pub(crate) display_name: String,
    pub(crate) vendor: LinuxGpuVendor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedLinuxGpuDevice {
    pub(crate) process_index: usize,
    pub(crate) stable_device_identity: String,
    pub(crate) driver_identity: String,
    pub(crate) vendor: LinuxGpuVendor,
}

pub(crate) fn route_provider_devices(
    source: &impl LinuxGpuFactSource,
    backend: LinuxGpuBackend,
    provider_devices: &[ProviderLinuxGpuDevice],
) -> Result<Vec<ResolvedLinuxGpuDevice>, String> {
    if provider_devices.is_empty() || provider_devices.len() > MAX_PROVIDER_DEVICES {
        return Err("Linux GPU provider device list is empty or oversized".to_owned());
    }
    let first = source.snapshot(backend)?;
    let second = source.snapshot(backend)?;
    if first != second {
        return Err("Linux GPU device or driver facts changed during routing".to_owned());
    }
    resolve_snapshot(backend, provider_devices, &first)
}

fn resolve_snapshot(
    backend: LinuxGpuBackend,
    provider_devices: &[ProviderLinuxGpuDevice],
    snapshot: &LinuxGpuFactSnapshot,
) -> Result<Vec<ResolvedLinuxGpuDevice>, String> {
    if snapshot.devices.is_empty() || snapshot.devices.len() > MAX_GPU_FACTS {
        return Err("Linux GPU kernel fact set is empty or oversized".to_owned());
    }
    let mut by_address = BTreeMap::new();
    let mut by_uuid = BTreeMap::new();
    for fact in &snapshot.devices {
        validate_driver_identity(&fact.driver_identity)?;
        if by_address.insert(fact.address, fact).is_some() {
            return Err("Linux GPU kernel facts contain a duplicate PCI function".to_owned());
        }
        if let Some(alias) = &fact.nvidia_physical_uuid_alias {
            if !is_canonical_nvidia_physical_uuid(alias) {
                return Err("Linux NVIDIA physical GPU alias is malformed".to_owned());
            }
            if by_uuid.insert(alias.as_str(), fact.address).is_some() {
                return Err("Linux NVIDIA physical GPU alias is ambiguous".to_owned());
            }
        }
    }

    let mut process_indexes = BTreeSet::new();
    let mut stable_addresses = BTreeSet::new();
    let mut resolved = Vec::with_capacity(provider_devices.len());
    for provider in provider_devices {
        if !process_indexes.insert(provider.process_index) {
            return Err("Linux GPU provider reported duplicate process indexes".to_owned());
        }
        if provider.display_name.is_empty()
            || provider.display_name.len() > MAX_DISPLAY_NAME_BYTES
            || !provider
                .display_name
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err("Linux GPU provider display name is malformed".to_owned());
        }
        let native = provider
            .native_identity_or_alias
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Linux GPU provider omitted stable identity".to_owned())?;
        if native.len() > 64 || !native.is_ascii() {
            return Err("Linux GPU provider identity is malformed".to_owned());
        }
        if native.to_ascii_lowercase().starts_with("mig-") {
            return Err("Linux CUDA MIG identities are not physical GPU identities".to_owned());
        }

        let address = if let Some(address) = PciAddress::from_provider_id(native) {
            address
        } else if backend == LinuxGpuBackend::Cuda {
            let alias = native.to_ascii_lowercase();
            if !is_canonical_nvidia_physical_uuid(&alias) {
                return Err(
                    "Linux CUDA provider identity is neither PCI nor a physical GPU UUID"
                        .to_owned(),
                );
            }
            by_uuid.get(alias.as_str()).copied().ok_or_else(|| {
                "Linux CUDA physical GPU UUID has no validated PCI function".to_owned()
            })?
        } else {
            return Err("Linux Vulkan provider omitted canonical PCI identity".to_owned());
        };
        let fact = by_address.get(&address).copied().ok_or_else(|| {
            "Linux GPU provider PCI identity has no kernel device fact".to_owned()
        })?;
        if backend == LinuxGpuBackend::Cuda && fact.vendor != LinuxGpuVendor::Nvidia {
            return Err(
                "Linux CUDA provider identity does not resolve to NVIDIA PCI hardware".to_owned(),
            );
        }
        if provider.vendor != LinuxGpuVendor::Other && provider.vendor != fact.vendor {
            return Err("Linux GPU provider vendor conflicts with kernel PCI metadata".to_owned());
        }
        if !stable_addresses.insert(address) {
            return Err(
                "Linux GPU provider maps multiple logical devices to one physical PCI function"
                    .to_owned(),
            );
        }
        resolved.push(ResolvedLinuxGpuDevice {
            process_index: provider.process_index,
            stable_device_identity: address.canonical(),
            driver_identity: fact.driver_identity.clone(),
            vendor: fact.vendor,
        });
    }
    resolved.sort_by(|left, right| {
        left.stable_device_identity
            .cmp(&right.stable_device_identity)
    });
    Ok(resolved)
}

fn validate_driver_identity(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_DRIVER_IDENTITY_BYTES
        || value != value.to_ascii_lowercase()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'-' | b'_'))
    {
        return Err("Linux GPU driver identity is malformed".to_owned());
    }
    Ok(())
}

fn is_canonical_nvidia_physical_uuid(value: &str) -> bool {
    if value.len() != 40 || !value.starts_with("gpu-") {
        return false;
    }
    let uuid = &value[4..];
    for (index, byte) in uuid.bytes().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase() {
            return false;
        }
    }
    true
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
pub(crate) struct KernelLinuxGpuFactSource;

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
impl LinuxGpuFactSource for KernelLinuxGpuFactSource {
    fn snapshot(&self, backend: LinuxGpuBackend) -> Result<LinuxGpuFactSnapshot, String> {
        kernel_snapshot(backend)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn kernel_snapshot(backend: LinuxGpuBackend) -> Result<LinuxGpuFactSnapshot, String> {
    use std::fs;
    use std::path::Path;

    const PCI_ROOT: &str = "/sys/bus/pci/devices";
    let mut entries = fs::read_dir(PCI_ROOT)
        .map_err(|_| "Linux PCI sysfs root is unavailable".to_owned())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Linux PCI sysfs enumeration failed".to_owned())?;
    if entries.len() > 4096 {
        return Err("Linux PCI sysfs enumeration is oversized".to_owned());
    }
    entries.sort_by_key(|entry| entry.file_name());
    let kernel_release = bounded_fixed_file(
        Path::new("/proc/sys/kernel/osrelease"),
        MAX_SYSFS_VALUE_BYTES,
    )?;
    let kernel_release = bounded_token(&kernel_release)
        .ok_or_else(|| "Linux kernel release witness is malformed".to_owned())?;
    let vulkan_driver_identities = (backend == LinuxGpuBackend::Vulkan)
        .then(vulkan_pci_driver_identities)
        .transpose()?;
    let mut devices = Vec::new();
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Linux PCI sysfs entry name is not UTF-8".to_owned())?;
        let Some(address) = PciAddress::parse(&name) else {
            return Err("Linux PCI sysfs entry name is not canonical".to_owned());
        };
        let root = Path::new(PCI_ROOT).join(&name);
        let class = parse_prefixed_hex(&bounded_fixed_file(
            &root.join("class"),
            MAX_SYSFS_VALUE_BYTES,
        )?)
        .ok_or_else(|| "Linux PCI class fact is malformed".to_owned())?;
        if class >> 16 != 0x03 {
            continue;
        }
        if devices.len() == MAX_GPU_FACTS {
            return Err("Linux GPU kernel fact set is oversized".to_owned());
        }
        let vendor_id = parse_prefixed_hex(&bounded_fixed_file(
            &root.join("vendor"),
            MAX_SYSFS_VALUE_BYTES,
        )?)
        .ok_or_else(|| "Linux PCI vendor fact is malformed".to_owned())?;
        let vendor = match vendor_id {
            0x10de => LinuxGpuVendor::Nvidia,
            0x1002 | 0x1022 => LinuxGpuVendor::Amd,
            0x8086 => LinuxGpuVendor::Intel,
            _ => LinuxGpuVendor::Other,
        };
        let driver_link = fs::read_link(root.join("driver"))
            .map_err(|_| "Linux display PCI function has no bound driver".to_owned())?;
        let driver = driver_link
            .file_name()
            .and_then(|value| value.to_str())
            .and_then(bounded_token)
            .ok_or_else(|| "Linux PCI driver name is malformed".to_owned())?;
        let module_version_path = Path::new("/sys/module").join(&driver).join("version");
        let version = if module_version_path.exists() {
            bounded_token(&bounded_fixed_file(
                &module_version_path,
                MAX_SYSFS_VALUE_BYTES,
            )?)
            .ok_or_else(|| "Linux GPU module version is malformed".to_owned())?
        } else {
            format!("kernel-{kernel_release}")
        };
        let mut driver_identity = format!("linux:{driver}:{version}");
        let nvidia_physical_uuid_alias = if vendor == LinuxGpuVendor::Nvidia {
            nvidia_uuid_for(address)?
        } else {
            None
        };
        if backend == LinuxGpuBackend::Vulkan {
            let vulkan_identity = vulkan_driver_identities
                .as_ref()
                .expect("Vulkan catalog exists for the selected backend")
                .get(&address)
                .ok_or_else(|| {
                    "Linux Vulkan PCI function has no exact Vulkan identity".to_owned()
                })?;
            if vulkan_identity.vendor != vendor {
                return Err("Linux Vulkan PCI vendor conflicts with kernel PCI metadata".to_owned());
            }
            driver_identity.push(':');
            driver_identity.push_str(&vulkan_identity.identity);
        }
        validate_driver_identity(&driver_identity)?;
        devices.push(LinuxGpuFact {
            address,
            vendor,
            driver_identity,
            nvidia_physical_uuid_alias,
        });
    }
    if devices.is_empty() {
        return Err("Linux reported no driver-bound display PCI function".to_owned());
    }
    Ok(LinuxGpuFactSnapshot { devices })
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn bounded_fixed_file(path: &std::path::Path, limit: u64) -> Result<String, String> {
    use std::io::Read;

    let file = std::fs::File::open(path)
        .map_err(|_| "required Linux GPU kernel fact is unavailable".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "required Linux GPU kernel fact cannot be inspected".to_owned())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("required Linux GPU kernel fact is not a bounded regular file".to_owned());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "required Linux GPU kernel fact cannot be read".to_owned())?;
    if bytes.len() as u64 > limit || bytes.contains(&0) {
        return Err("required Linux GPU kernel fact is oversized or contains NUL".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "required Linux GPU kernel fact is not UTF-8".to_owned())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn bounded_token(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')))
    .then_some(value)
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn parse_prefixed_hex(value: &str) -> Option<u32> {
    let value = value.trim();
    let value = value.strip_prefix("0x")?;
    (!value.is_empty() && value.len() <= 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
fn nvidia_uuid_for(address: PciAddress) -> Result<Option<String>, String> {
    use std::path::Path;

    let path = Path::new("/proc/driver/nvidia/gpus")
        .join(address.sysfs_name())
        .join("information");
    if !path.exists() {
        return Ok(None);
    }
    let information = bounded_fixed_file(&path, MAX_NVIDIA_INFORMATION_BYTES)?;
    let aliases = information
        .lines()
        .filter_map(|line| line.strip_prefix("GPU UUID:"))
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let [alias] = aliases.as_slice() else {
        return Err(
            "Linux NVIDIA information has incomplete or duplicate GPU UUID facts".to_owned(),
        );
    };
    if !is_canonical_nvidia_physical_uuid(alias) {
        return Err("Linux NVIDIA information has a malformed physical GPU UUID".to_owned());
    }
    Ok(Some(alias.clone()))
}

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    feature = "vulkan-acceleration"
))]
fn vulkan_pci_driver_identities() -> Result<BTreeMap<PciAddress, VulkanPciDriverFact>, String> {
    use ash::vk;

    struct Instance(ash::Instance);
    impl Drop for Instance {
        fn drop(&mut self) {
            // SAFETY: the guard exclusively owns the instance.
            unsafe { self.0.destroy_instance(None) };
        }
    }

    // SAFETY: this runs only inside the admitted Vulkan worker. The launcher
    // sanitizes loader/layer overrides before the worker reaches Hello.
    let entry = unsafe { ash::Entry::load() }
        .map_err(|_| "could not load the Linux Vulkan loader for PCI identity".to_owned())?;
    let application = vk::ApplicationInfo::builder()
        .application_name(c"scribe-gpu-worker")
        .application_version(1)
        .api_version(vk::API_VERSION_1_1);
    let create_info = vk::InstanceCreateInfo::builder().application_info(&application);
    // SAFETY: the create info contains no layer or extension pointers.
    let instance = Instance(
        unsafe { entry.create_instance(&create_info, None) }
            .map_err(|_| "could not create the Linux Vulkan identity instance".to_owned())?,
    );
    // SAFETY: the instance remains live for all returned handles.
    let physical = unsafe { instance.0.enumerate_physical_devices() }
        .map_err(|_| "could not enumerate Linux Vulkan physical devices".to_owned())?;
    if physical.is_empty() || physical.len() > MAX_GPU_FACTS {
        return Err("Linux Vulkan physical-device list is empty or oversized".to_owned());
    }
    let mut result = BTreeMap::new();
    for device in physical {
        // SAFETY: device belongs to the live instance.
        let extensions = unsafe { instance.0.enumerate_device_extension_properties(device) }
            .map_err(|_| "could not enumerate Linux Vulkan device extensions".to_owned())?;
        let supports_pci = extensions.iter().any(|extension| {
            // SAFETY: Vulkan provides a terminated extension name array.
            let name = unsafe { std::ffi::CStr::from_ptr(extension.extension_name.as_ptr()) };
            name == vk::ExtPciBusInfoFn::name()
        });
        if !supports_pci {
            continue;
        }
        let mut pci = vk::PhysicalDevicePCIBusInfoPropertiesEXT::default();
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut pci)
            .push_next(&mut id)
            .build();
        // SAFETY: pNext structures remain initialized and live for this call.
        unsafe {
            instance
                .0
                .get_physical_device_properties2(device, &mut properties)
        };
        if pci.pci_domain > u16::MAX as u32
            || pci.pci_bus > u8::MAX as u32
            || pci.pci_device > 0x1f
            || pci.pci_function > 7
        {
            return Err("Linux Vulkan returned an invalid PCI function".to_owned());
        }
        if id.driver_uuid.iter().all(|byte| *byte == 0) {
            return Err("Linux Vulkan omitted bounded driver UUID identity".to_owned());
        }
        let address = PciAddress {
            domain: pci.pci_domain as u16,
            bus: pci.pci_bus as u8,
            device: pci.pci_device as u8,
            function: pci.pci_function as u8,
        };
        let mut uuid = String::with_capacity(32);
        use std::fmt::Write as _;
        for byte in id.driver_uuid {
            write!(&mut uuid, "{byte:02x}").expect("writing to String cannot fail");
        }
        let identity = format!(
            "vk-{:04x}-{:08x}-{:08x}-{}",
            properties.properties.vendor_id,
            properties.properties.device_id,
            properties.properties.driver_version,
            uuid
        );
        let vendor = match properties.properties.vendor_id {
            0x10de => LinuxGpuVendor::Nvidia,
            0x1002 | 0x1022 => LinuxGpuVendor::Amd,
            0x8086 => LinuxGpuVendor::Intel,
            _ => LinuxGpuVendor::Other,
        };
        if result
            .insert(address, VulkanPciDriverFact { vendor, identity })
            .is_some()
        {
            return Err("Linux Vulkan returned duplicate PCI identities".to_owned());
        }
    }
    if result.is_empty() {
        return Err("Linux Vulkan exposed no VK_EXT_pci_bus_info device".to_owned());
    }
    Ok(result)
}

#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu",
    not(feature = "vulkan-acceleration")
))]
fn vulkan_pci_driver_identities() -> Result<BTreeMap<PciAddress, VulkanPciDriverFact>, String> {
    Err("Linux Vulkan worker was built without Vulkan acceleration".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn address(value: &str) -> PciAddress {
        PciAddress::parse(value).unwrap()
    }

    fn fact(value: &str, vendor: LinuxGpuVendor, uuid: Option<&str>) -> LinuxGpuFact {
        LinuxGpuFact {
            address: address(value),
            vendor,
            driver_identity: match vendor {
                LinuxGpuVendor::Nvidia => "linux:nvidia:570.26".to_owned(),
                LinuxGpuVendor::Amd => "linux:amdgpu:kernel-6.8.0-52-generic".to_owned(),
                LinuxGpuVendor::Intel => "linux:i915:kernel-6.8.0-52-generic".to_owned(),
                LinuxGpuVendor::Other => "linux:other:kernel-6.8.0-52-generic".to_owned(),
            },
            nvidia_physical_uuid_alias: uuid.map(str::to_owned),
        }
    }

    fn provider(index: usize, identity: &str, vendor: LinuxGpuVendor) -> ProviderLinuxGpuDevice {
        ProviderLinuxGpuDevice {
            process_index: index,
            native_identity_or_alias: Some(identity.to_owned()),
            display_name: "Bounded GPU name".to_owned(),
            vendor,
        }
    }

    struct FakeSource {
        snapshots: Vec<LinuxGpuFactSnapshot>,
        calls: Cell<usize>,
    }

    impl FakeSource {
        fn stable(snapshot: LinuxGpuFactSnapshot) -> Self {
            Self {
                snapshots: vec![snapshot],
                calls: Cell::new(0),
            }
        }
    }

    impl LinuxGpuFactSource for FakeSource {
        fn snapshot(&self, _backend: LinuxGpuBackend) -> Result<LinuxGpuFactSnapshot, String> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            Ok(self.snapshots[call.min(self.snapshots.len() - 1)].clone())
        }
    }

    #[test]
    fn canonical_pci_identity_is_fixed_width_and_preserves_nonzero_domain() {
        let parsed = PciAddress::from_provider_id("000a:0b:1f.7").unwrap();
        assert_eq!(parsed.canonical(), "native:pci:000a:0b:1f.7");
        assert!(PciAddress::from_provider_id("0:a:b.0").is_none());
        assert!(PciAddress::from_provider_id("0000:01:20.0").is_none());
        assert!(PciAddress::from_provider_id("0000:01:00.8").is_none());
        assert!(PciAddress::from_provider_id("000A:01:00.0").is_none());
    }

    #[test]
    fn process_indexes_remap_fresh_from_stable_pci_identity() {
        let snapshot = LinuxGpuFactSnapshot {
            devices: vec![
                fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None),
                fact("0001:02:00.0", LinuxGpuVendor::Nvidia, None),
            ],
        };
        let source = FakeSource::stable(snapshot);
        let first = route_provider_devices(
            &source,
            LinuxGpuBackend::Cuda,
            &[
                provider(7, "0000:01:00.0", LinuxGpuVendor::Nvidia),
                provider(2, "0001:02:00.0", LinuxGpuVendor::Nvidia),
            ],
        )
        .unwrap();
        assert_eq!(first[0].process_index, 7);
        assert_eq!(first[1].process_index, 2);

        let source = FakeSource::stable(LinuxGpuFactSnapshot {
            devices: vec![
                fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None),
                fact("0001:02:00.0", LinuxGpuVendor::Nvidia, None),
            ],
        });
        let second = route_provider_devices(
            &source,
            LinuxGpuBackend::Cuda,
            &[
                provider(3, "0001:02:00.0", LinuxGpuVendor::Nvidia),
                provider(9, "0000:01:00.0", LinuxGpuVendor::Nvidia),
            ],
        )
        .unwrap();
        assert_eq!(second[0].process_index, 9);
        assert_eq!(second[1].process_index, 3);
    }

    #[test]
    fn cuda_physical_uuid_is_ephemeral_and_resolves_only_through_proc_pci_fact() {
        let uuid = "gpu-12345678-1234-5678-9abc-1234567890ab";
        let source = FakeSource::stable(LinuxGpuFactSnapshot {
            devices: vec![fact("0002:03:00.0", LinuxGpuVendor::Nvidia, Some(uuid))],
        });
        let resolved = route_provider_devices(
            &source,
            LinuxGpuBackend::Cuda,
            &[provider(4, uuid, LinuxGpuVendor::Nvidia)],
        )
        .unwrap();
        assert_eq!(
            resolved[0].stable_device_identity,
            "native:pci:0002:03:00.0"
        );
        assert!(!resolved[0].stable_device_identity.contains("gpu-"));
    }

    #[test]
    fn uuid_aliases_never_route_vulkan_and_mig_never_routes_cuda() {
        let uuid = "gpu-12345678-1234-5678-9abc-1234567890ab";
        let source = FakeSource::stable(LinuxGpuFactSnapshot {
            devices: vec![fact("0000:01:00.0", LinuxGpuVendor::Nvidia, Some(uuid))],
        });
        assert!(
            route_provider_devices(
                &source,
                LinuxGpuBackend::Vulkan,
                &[provider(1, uuid, LinuxGpuVendor::Nvidia)],
            )
            .is_err()
        );
        let source = FakeSource::stable(LinuxGpuFactSnapshot {
            devices: vec![fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None)],
        });
        assert!(
            route_provider_devices(
                &source,
                LinuxGpuBackend::Cuda,
                &[provider(
                    1,
                    "MIG-GPU-12345678-1234-5678-9abc-1234567890ab/1/2",
                    LinuxGpuVendor::Nvidia,
                )],
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_and_ambiguous_mappings_fail_closed() {
        let uuid = "gpu-12345678-1234-5678-9abc-1234567890ab";
        for snapshot in [
            LinuxGpuFactSnapshot {
                devices: vec![
                    fact("0000:01:00.0", LinuxGpuVendor::Nvidia, Some(uuid)),
                    fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None),
                ],
            },
            LinuxGpuFactSnapshot {
                devices: vec![
                    fact("0000:01:00.0", LinuxGpuVendor::Nvidia, Some(uuid)),
                    fact("0000:02:00.0", LinuxGpuVendor::Nvidia, Some(uuid)),
                ],
            },
        ] {
            let source = FakeSource::stable(snapshot);
            assert!(
                route_provider_devices(
                    &source,
                    LinuxGpuBackend::Cuda,
                    &[provider(0, uuid, LinuxGpuVendor::Nvidia)],
                )
                .is_err()
            );
        }
    }

    #[test]
    fn logical_duplicates_on_one_bdf_and_duplicate_indexes_fail_closed() {
        let snapshot = LinuxGpuFactSnapshot {
            devices: vec![fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None)],
        };
        for providers in [
            vec![
                provider(1, "0000:01:00.0", LinuxGpuVendor::Nvidia),
                provider(2, "pci:0000:01:00.0", LinuxGpuVendor::Nvidia),
            ],
            vec![
                provider(1, "0000:01:00.0", LinuxGpuVendor::Nvidia),
                provider(1, "0000:01:00.0", LinuxGpuVendor::Nvidia),
            ],
        ] {
            let source = FakeSource::stable(snapshot.clone());
            assert!(route_provider_devices(&source, LinuxGpuBackend::Cuda, &providers).is_err());
        }
    }

    #[test]
    fn missing_conflicting_and_changing_facts_fail_closed() {
        let first = LinuxGpuFactSnapshot {
            devices: vec![fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None)],
        };
        let changed = LinuxGpuFactSnapshot {
            devices: vec![fact("0000:02:00.0", LinuxGpuVendor::Nvidia, None)],
        };
        let source = FakeSource {
            snapshots: vec![first.clone(), changed],
            calls: Cell::new(0),
        };
        assert!(
            route_provider_devices(
                &source,
                LinuxGpuBackend::Cuda,
                &[provider(0, "0000:01:00.0", LinuxGpuVendor::Nvidia)],
            )
            .is_err()
        );

        let mut changed_driver = first.clone();
        changed_driver.devices[0].driver_identity = "linux:nvidia:570.27".to_owned();
        let source = FakeSource {
            snapshots: vec![first.clone(), changed_driver],
            calls: Cell::new(0),
        };
        assert!(
            route_provider_devices(
                &source,
                LinuxGpuBackend::Cuda,
                &[provider(0, "0000:01:00.0", LinuxGpuVendor::Nvidia)],
            )
            .is_err()
        );

        for device in [
            provider(0, "0000:02:00.0", LinuxGpuVendor::Nvidia),
            provider(0, "0000:01:00.0", LinuxGpuVendor::Amd),
        ] {
            let source = FakeSource::stable(first.clone());
            assert!(route_provider_devices(&source, LinuxGpuBackend::Cuda, &[device]).is_err());
        }
    }

    #[test]
    fn vulkan_routes_only_an_exact_pci_claim_and_never_a_name_only_claim() {
        let snapshot = LinuxGpuFactSnapshot {
            devices: vec![fact("0000:0a:00.0", LinuxGpuVendor::Amd, None)],
        };
        let source = FakeSource::stable(snapshot.clone());
        let resolved = route_provider_devices(
            &source,
            LinuxGpuBackend::Vulkan,
            &[provider(6, "pci:0000:0a:00.0", LinuxGpuVendor::Amd)],
        )
        .unwrap();
        assert_eq!(
            resolved[0].stable_device_identity,
            "native:pci:0000:0a:00.0"
        );
        assert_eq!(resolved[0].process_index, 6);

        let source = FakeSource::stable(snapshot);
        let mut missing = provider(6, "ignored", LinuxGpuVendor::Amd);
        missing.native_identity_or_alias = None;
        missing.display_name = "same human readable GPU name".to_owned();
        assert!(route_provider_devices(&source, LinuxGpuBackend::Vulkan, &[missing]).is_err());
    }

    #[test]
    fn malformed_uuid_and_driver_evidence_fail_closed() {
        assert_eq!(LinuxGpuVendor::Intel, LinuxGpuVendor::Intel);
        for invalid in [
            "GPU-12345678-1234-5678-9abc-1234567890ab",
            "gpu-12345678-1234-5678-9abc-1234567890a",
            "gpu-12345678-1234-5678-9abc-1234567890ag",
        ] {
            assert!(!is_canonical_nvidia_physical_uuid(invalid));
        }
        let mut bad = fact("0000:01:00.0", LinuxGpuVendor::Nvidia, None);
        bad.driver_identity = "Linux Driver 570".to_owned();
        let source = FakeSource::stable(LinuxGpuFactSnapshot { devices: vec![bad] });
        assert!(
            route_provider_devices(
                &source,
                LinuxGpuBackend::Cuda,
                &[provider(0, "0000:01:00.0", LinuxGpuVendor::Nvidia)],
            )
            .is_err()
        );
    }
}
