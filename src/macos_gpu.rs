//! macOS-native Metal device witnesses for the Metal worker only.
//!
//! Stable identity comes only from `MTLDevice.registryID`. Provider registry
//! indexes remain process-local and are remapped from the current Metal device
//! set on every worker start.

use crate::backend_policy::{DeviceClass, GpuVendor};
use anyhow::{Result, bail};
#[cfg(any(target_os = "macos", test))]
use std::ffi::c_char;

const MAX_METAL_DEVICES: usize = 16;
#[cfg(any(target_os = "macos", test))]
const METAL_METADATA_OK: u32 = 0;
#[cfg(any(target_os = "macos", test))]
const METAL_METADATA_UNAVAILABLE: u32 = 1;
#[cfg(any(target_os = "macos", test))]
const METAL_METADATA_OVERFLOW: u32 = 2;
#[cfg(any(target_os = "macos", test))]
const METAL_METADATA_INVALID_ARGUMENT: u32 = 3;
#[cfg(any(target_os = "macos", test))]
const METAL_METADATA_INSUFFICIENT_CAPACITY: u32 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetalDevice {
    pub(crate) registry_id: u64,
    pub(crate) stable_identity: String,
    pub(crate) display_name: String,
    pub(crate) vendor: GpuVendor,
    pub(crate) device_class: DeviceClass,
    pub(crate) memory_total_bytes: u64,
    pub(crate) memory_available_bytes: u64,
    pub(crate) is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderMetalDevice {
    pub(crate) process_index: usize,
    pub(crate) display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemappedMetalDevice {
    pub(crate) process_index: usize,
    pub(crate) metal: MetalDevice,
}

pub(crate) fn stable_registry_identity(registry_id: u64) -> String {
    format!("metal-registry:{registry_id:016x}")
}

pub(crate) fn discover_devices() -> Result<Vec<MetalDevice>> {
    platform::discover_devices()
}

/// Returns the complete native Metal snapshot used only by the short-lived
/// metadata worker. Unlike provider discovery, an observed empty snapshot is
/// valid and no default-device fallback is synthesized.
#[cfg(target_os = "macos")]
pub(crate) fn discover_metadata_devices() -> Result<Vec<MetalDevice>> {
    platform::discover_metadata_devices()
}

#[cfg(any(target_os = "macos", test))]
fn validate_metadata_status(status: u32, count: usize) -> Result<usize> {
    match status {
        METAL_METADATA_OK => {}
        METAL_METADATA_UNAVAILABLE => bail!("Metal metadata enumeration is unavailable"),
        METAL_METADATA_OVERFLOW => bail!("Metal metadata enumeration exceeded the device bound"),
        METAL_METADATA_INVALID_ARGUMENT => {
            bail!("Metal metadata enumeration rejected its output arguments")
        }
        METAL_METADATA_INSUFFICIENT_CAPACITY => {
            bail!("Metal metadata enumeration exceeded its prepared capacity")
        }
        _ => bail!("Metal metadata enumeration returned an unknown status"),
    }
    if count > MAX_METAL_DEVICES {
        bail!("Metal metadata enumeration returned an oversized count");
    }
    Ok(count)
}

#[cfg(any(target_os = "macos", test))]
fn parse_native_device_name(name: &[c_char; 256]) -> Result<String> {
    let name_end = name
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| anyhow::anyhow!("Metal device name is not NUL-terminated"))?;
    let name_bytes = name[..name_end]
        .iter()
        .map(|byte| *byte as u8)
        .collect::<Vec<_>>();
    let name = std::str::from_utf8(&name_bytes)
        .map_err(|_| anyhow::anyhow!("Metal device name is not UTF-8"))?
        .trim();
    if name.is_empty() || name.len() > 255 || !name.is_ascii() {
        bail!("Metal device has no bounded ASCII display name");
    }
    Ok(name.to_owned())
}

pub(crate) fn remap_provider_devices(
    provider_devices: &[ProviderMetalDevice],
    metal_devices: &[MetalDevice],
) -> Result<Vec<RemappedMetalDevice>> {
    if provider_devices.is_empty() || provider_devices.len() > MAX_METAL_DEVICES {
        bail!("Metal provider device list is empty or oversized");
    }
    if metal_devices.is_empty() || metal_devices.len() > MAX_METAL_DEVICES {
        bail!("Metal registry device list is empty or oversized");
    }
    let mut used = vec![false; metal_devices.len()];
    let mut remapped = Vec::with_capacity(provider_devices.len());
    for provider in provider_devices {
        let provider_name = normalized_name(&provider.display_name);
        let exact = metal_devices
            .iter()
            .enumerate()
            .filter(|(index, device)| {
                !used[*index]
                    && !provider_name.is_empty()
                    && normalized_name(&device.display_name) == provider_name
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let available = metal_devices
            .iter()
            .enumerate()
            .filter(|(index, _)| !used[*index])
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let defaults = available
            .iter()
            .copied()
            .filter(|index| metal_devices[*index].is_default)
            .collect::<Vec<_>>();
        let selected = match exact.as_slice() {
            [index] => *index,
            [] if available.len() == 1 => available[0],
            [] if provider_devices.len() == 1 && defaults.len() == 1 => defaults[0],
            [] => bail!("Metal provider device has no unambiguous MTLDevice registry identity"),
            _ => bail!("Metal provider device name maps to multiple MTLDevice identities"),
        };
        used[selected] = true;
        remapped.push(RemappedMetalDevice {
            process_index: provider.process_index,
            metal: metal_devices[selected].clone(),
        });
    }
    remapped.sort_by(|left, right| left.metal.stable_identity.cmp(&right.metal.stable_identity));
    if remapped
        .windows(2)
        .any(|pair| pair[0].metal.stable_identity == pair[1].metal.stable_identity)
    {
        bail!("Metal provider devices mapped to a duplicate registry identity");
    }
    Ok(remapped)
}

fn normalized_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn vendor_from_name(name: &str) -> GpuVendor {
    let name = name.to_ascii_lowercase();
    if name.contains("apple") {
        GpuVendor::Apple
    } else if name.contains("amd") || name.contains("radeon") {
        GpuVendor::Amd
    } else if name.contains("intel") {
        GpuVendor::Intel
    } else {
        GpuVendor::Other
    }
}

fn device_class(low_power: bool, _removable: bool, unified: bool) -> DeviceClass {
    if unified {
        DeviceClass::UnifiedGpu
    } else if low_power {
        DeviceClass::IntegratedGpu
    } else {
        DeviceClass::DiscreteGpu
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NativeMetalDevice {
        registry_id: u64,
        memory_total_bytes: u64,
        memory_available_bytes: u64,
        is_default: u8,
        is_low_power: u8,
        is_removable: u8,
        has_unified_memory: u8,
        name: [c_char; 256],
    }

    impl Default for NativeMetalDevice {
        fn default() -> Self {
            Self {
                registry_id: 0,
                memory_total_bytes: 0,
                memory_available_bytes: 0,
                is_default: 0,
                is_low_power: 0,
                is_removable: 0,
                has_unified_memory: 0,
                name: [0; 256],
            }
        }
    }

    unsafe extern "C" {
        fn scribe_macos_copy_metal_devices(
            devices: *mut NativeMetalDevice,
            capacity: usize,
        ) -> usize;
        fn scribe_macos_copy_metal_metadata_devices(
            devices: *mut NativeMetalDevice,
            capacity: usize,
            count: *mut usize,
        ) -> u32;
    }

    pub(super) fn discover_devices() -> Result<Vec<MetalDevice>> {
        // SAFETY: a null output is the shim's documented bounded-size query.
        let count = unsafe { scribe_macos_copy_metal_devices(std::ptr::null_mut(), 0) };
        if count == 0 || count > MAX_METAL_DEVICES {
            bail!("Metal device enumeration returned an empty or oversized list");
        }
        let mut native = vec![NativeMetalDevice::default(); count];
        // SAFETY: native owns `count` correctly laid-out writable entries.
        let observed = unsafe { scribe_macos_copy_metal_devices(native.as_mut_ptr(), count) };
        if observed != count {
            bail!("Metal device enumeration changed during discovery");
        }
        parse_native_devices(native)
    }

    pub(super) fn discover_metadata_devices() -> Result<Vec<MetalDevice>> {
        let mut native = vec![NativeMetalDevice::default(); MAX_METAL_DEVICES];
        let mut count = 0;
        // SAFETY: native owns the metadata contract's complete bounded output
        // capacity and count points to one writable usize. One call captures
        // one snapshot, avoiding a query/copy topology race.
        let status = unsafe {
            scribe_macos_copy_metal_metadata_devices(native.as_mut_ptr(), native.len(), &mut count)
        };
        let count = validate_metadata_status(status, count)?;
        native.truncate(count);
        parse_native_devices(native)
    }

    fn parse_native_devices(native: Vec<NativeMetalDevice>) -> Result<Vec<MetalDevice>> {
        native
            .into_iter()
            .map(|device| {
                if device.registry_id == 0 {
                    bail!("Metal device has no registry identity");
                }
                let name = parse_native_device_name(&device.name)?;
                let vendor = vendor_from_name(&name);
                Ok(MetalDevice {
                    registry_id: device.registry_id,
                    stable_identity: stable_registry_identity(device.registry_id),
                    display_name: name,
                    vendor,
                    device_class: device_class(
                        device.is_low_power != 0,
                        device.is_removable != 0,
                        device.has_unified_memory != 0,
                    ),
                    memory_total_bytes: device.memory_total_bytes,
                    memory_available_bytes: device
                        .memory_available_bytes
                        .min(device.memory_total_bytes),
                    is_default: device.is_default != 0,
                })
            })
            .collect()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn discover_devices() -> Result<Vec<MetalDevice>> {
        bail!("Metal device discovery requires macOS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u64, name: &str, is_default: bool) -> MetalDevice {
        MetalDevice {
            registry_id: id,
            stable_identity: stable_registry_identity(id),
            display_name: name.to_owned(),
            vendor: vendor_from_name(name),
            device_class: device_class(name.contains("Intel"), false, name.contains("Apple")),
            memory_total_bytes: 8 << 30,
            memory_available_bytes: 6 << 30,
            is_default,
        }
    }

    #[test]
    fn registry_identity_is_fixed_width_lowercase_hex() {
        assert_eq!(
            stable_registry_identity(0xABCD),
            "metal-registry:000000000000abcd"
        );
    }

    #[test]
    fn provider_indexes_remap_by_current_device_name_not_enumeration_order() {
        let native = vec![
            device(2, "AMD Radeon Pro 5500M", false),
            device(1, "Intel UHD Graphics 630", true),
        ];
        let provider = vec![
            ProviderMetalDevice {
                process_index: 9,
                display_name: "Intel UHD Graphics 630".to_owned(),
            },
            ProviderMetalDevice {
                process_index: 3,
                display_name: "AMD Radeon Pro 5500M".to_owned(),
            },
        ];
        let mapped = remap_provider_devices(&provider, &native).unwrap();
        assert_eq!(
            mapped[0].metal.stable_identity,
            "metal-registry:0000000000000001"
        );
        assert_eq!(mapped[0].process_index, 9);
        assert_eq!(
            mapped[1].metal.stable_identity,
            "metal-registry:0000000000000002"
        );
        assert_eq!(mapped[1].process_index, 3);
    }

    #[test]
    fn single_generic_provider_resolves_only_to_the_unique_default() {
        let mapped = remap_provider_devices(
            &[ProviderMetalDevice {
                process_index: 4,
                display_name: "Metal".to_owned(),
            }],
            &[
                device(2, "AMD Radeon Pro", false),
                device(1, "Intel Iris Plus", true),
            ],
        )
        .unwrap();
        assert_eq!(mapped[0].metal.registry_id, 1);
    }

    #[test]
    fn mac_device_facts_cover_apple_intel_and_amd_classes() {
        assert_eq!(vendor_from_name("Apple M4 Max"), GpuVendor::Apple);
        assert_eq!(vendor_from_name("Intel Iris"), GpuVendor::Intel);
        assert_eq!(vendor_from_name("AMD Radeon"), GpuVendor::Amd);
        assert_eq!(device_class(false, false, true), DeviceClass::UnifiedGpu);
        assert_eq!(device_class(true, false, false), DeviceClass::IntegratedGpu);
        assert_eq!(device_class(false, true, false), DeviceClass::DiscreteGpu);
    }

    #[test]
    fn macos_native_metadata_status_distinguishes_empty_from_every_failure() {
        assert_eq!(validate_metadata_status(METAL_METADATA_OK, 0).unwrap(), 0);
        assert_eq!(
            validate_metadata_status(METAL_METADATA_OK, MAX_METAL_DEVICES).unwrap(),
            MAX_METAL_DEVICES
        );
        assert!(validate_metadata_status(METAL_METADATA_OK, MAX_METAL_DEVICES + 1).is_err());
        for status in [
            METAL_METADATA_UNAVAILABLE,
            METAL_METADATA_OVERFLOW,
            METAL_METADATA_INVALID_ARGUMENT,
            METAL_METADATA_INSUFFICIENT_CAPACITY,
            u32::MAX,
        ] {
            assert!(validate_metadata_status(status, 0).is_err());
        }
    }

    #[test]
    fn provider_remap_still_rejects_empty_inference_device_sets() {
        let provider = [ProviderMetalDevice {
            process_index: 0,
            display_name: "Metal".to_owned(),
        }];
        assert!(remap_provider_devices(&provider, &[]).is_err());
        assert!(remap_provider_devices(&[], &[device(1, "Apple M4", true)]).is_err());
    }

    #[test]
    fn macos_native_device_name_parser_requires_an_in_bounds_ascii_nul() {
        let mut valid = [0 as c_char; 256];
        for (slot, byte) in valid.iter_mut().zip(b"  Apple M4  \0") {
            *slot = *byte as c_char;
        }
        assert_eq!(parse_native_device_name(&valid).unwrap(), "Apple M4");

        let unterminated = [b'A' as c_char; 256];
        assert!(parse_native_device_name(&unterminated).is_err());

        let mut invalid_utf8 = [0 as c_char; 256];
        invalid_utf8[0] = 0xff_u8 as c_char;
        assert!(parse_native_device_name(&invalid_utf8).is_err());
        assert!(parse_native_device_name(&[0 as c_char; 256]).is_err());
    }
}
