//! Read-only active Windows power-scheme observation.
//!
//! This is intentionally narrower than effective power settings or a power
//! plan qualification. The active scheme GUID only lets a campaign detect an
//! intervening scheme selection; it does not describe the selected scheme's
//! values, thermal state, or background controls.

use std::ffi::c_void;
use std::ptr;

use serde::Serialize;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::System::Power::PowerGetActiveScheme;
use windows_sys::core::GUID;

pub(super) const SOURCE: &str = "windows_power_get_active_scheme";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum ActivePowerSchemeObservation {
    Observed {
        source: &'static str,
        scheme_guid: String,
    },
    Unavailable {
        reason: &'static str,
    },
}

impl ActivePowerSchemeObservation {
    pub(super) fn scheme_guid(&self) -> Option<&str> {
        match self {
            Self::Observed { scheme_guid, .. } => Some(scheme_guid),
            Self::Unavailable { .. } => None,
        }
    }

    pub(super) fn is_valid(&self) -> bool {
        match self {
            Self::Observed {
                source,
                scheme_guid,
            } => *source == SOURCE && is_canonical_guid(scheme_guid),
            Self::Unavailable { reason } => matches!(*reason, "query_failed" | "invalid_result"),
        }
    }
}

pub(super) trait ActivePowerSchemeReader {
    fn observe_active_scheme(&self) -> ActivePowerSchemeObservation;
}

pub(super) struct WindowsActivePowerSchemeReader;

impl ActivePowerSchemeReader for WindowsActivePowerSchemeReader {
    fn observe_active_scheme(&self) -> ActivePowerSchemeObservation {
        observe_with(&WindowsPowerSchemeNative)
    }
}

trait PowerSchemeNative {
    unsafe fn get_active_scheme(&self, active_scheme: *mut *mut GUID) -> u32;
    unsafe fn local_free(&self, allocation: *mut c_void);
}

struct WindowsPowerSchemeNative;

impl PowerSchemeNative for WindowsPowerSchemeNative {
    unsafe fn get_active_scheme(&self, active_scheme: *mut *mut GUID) -> u32 {
        // PowerGetActiveScheme reserves this argument; NULL selects the
        // current user's active scheme.
        unsafe { PowerGetActiveScheme(ptr::null_mut(), active_scheme) }
    }

    unsafe fn local_free(&self, allocation: *mut c_void) {
        // PowerGetActiveScheme documents this allocator pairing. LocalFree
        // returns NULL on success, which is not useful to this read-only
        // observation after the release has been attempted.
        unsafe {
            let _ = LocalFree(allocation);
        }
    }
}

fn observe_with(native: &impl PowerSchemeNative) -> ActivePowerSchemeObservation {
    let mut allocation = ptr::null_mut();
    let result = unsafe { native.get_active_scheme(&mut allocation) };
    if result != 0 {
        if !allocation.is_null() {
            unsafe { native.local_free(allocation.cast()) };
        }
        return ActivePowerSchemeObservation::Unavailable {
            reason: "query_failed",
        };
    }
    if allocation.is_null() {
        return ActivePowerSchemeObservation::Unavailable {
            reason: "invalid_result",
        };
    }

    let guid = unsafe { *allocation };
    unsafe { native.local_free(allocation.cast()) };
    match canonical_guid(guid) {
        Some(scheme_guid) => ActivePowerSchemeObservation::Observed {
            source: SOURCE,
            scheme_guid,
        },
        None => ActivePowerSchemeObservation::Unavailable {
            reason: "invalid_result",
        },
    }
}

fn canonical_guid(guid: GUID) -> Option<String> {
    let GUID {
        data1,
        data2,
        data3,
        data4,
    } = guid;
    if data1 == 0 && data2 == 0 && data3 == 0 && data4 == [0; 8] {
        return None;
    }
    Some(format!(
        "{data1:08x}-{data2:04x}-{data3:04x}-{0:02x}{1:02x}-{2:02x}{3:02x}{4:02x}{5:02x}{6:02x}{7:02x}",
        data4[0], data4[1], data4[2], data4[3], data4[4], data4[5], data4[6], data4[7],
    ))
}

fn is_canonical_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
        && value != "00000000-0000-0000-0000-000000000000"
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct FakeNative {
        result: u32,
        guid: Option<GUID>,
        free_calls: Cell<u8>,
    }

    impl PowerSchemeNative for FakeNative {
        unsafe fn get_active_scheme(&self, active_scheme: *mut *mut GUID) -> u32 {
            let allocation = self
                .guid
                .as_ref()
                .map_or(ptr::null_mut(), |guid| (guid as *const GUID).cast_mut());
            unsafe { active_scheme.write(allocation) };
            self.result
        }

        unsafe fn local_free(&self, _allocation: *mut c_void) {
            self.free_calls.set(self.free_calls.get() + 1);
        }
    }

    #[test]
    fn serializes_a_valid_guid_in_canonical_lowercase_form_and_releases_it() {
        let native = FakeNative {
            result: 0,
            guid: Some(GUID::from_u128(0x381b4222_f694_41f0_9685_ff5bb260df2e)),
            free_calls: Cell::new(0),
        };

        let observation = observe_with(&native);

        assert_eq!(
            observation,
            ActivePowerSchemeObservation::Observed {
                source: SOURCE,
                scheme_guid: "381b4222-f694-41f0-9685-ff5bb260df2e".to_owned(),
            }
        );
        assert!(observation.is_valid());
        assert_eq!(native.free_calls.get(), 1);
        assert_eq!(
            serde_json::to_value(observation).unwrap(),
            serde_json::json!({
                "status": "observed",
                "source": SOURCE,
                "scheme_guid": "381b4222-f694-41f0-9685-ff5bb260df2e",
            })
        );
    }

    #[test]
    fn query_failure_is_unavailable_and_releases_an_unexpected_allocation() {
        let native = FakeNative {
            result: 5,
            guid: Some(GUID::from_u128(1)),
            free_calls: Cell::new(0),
        };

        assert_eq!(
            observe_with(&native),
            ActivePowerSchemeObservation::Unavailable {
                reason: "query_failed"
            }
        );
        assert_eq!(native.free_calls.get(), 1);
    }

    #[test]
    fn null_success_result_is_invalid_without_a_release() {
        let native = FakeNative {
            result: 0,
            guid: None,
            free_calls: Cell::new(0),
        };

        assert_eq!(
            observe_with(&native),
            ActivePowerSchemeObservation::Unavailable {
                reason: "invalid_result"
            }
        );
        assert_eq!(native.free_calls.get(), 0);
    }

    #[test]
    fn zero_guid_is_invalid_and_released() {
        let native = FakeNative {
            result: 0,
            guid: Some(GUID::from_u128(0)),
            free_calls: Cell::new(0),
        };

        assert_eq!(
            observe_with(&native),
            ActivePowerSchemeObservation::Unavailable {
                reason: "invalid_result"
            }
        );
        assert_eq!(native.free_calls.get(), 1);
    }

    #[test]
    #[ignore = "requires the host Windows power-scheme service"]
    fn native_query_requires_an_observed_active_scheme() {
        let reader = WindowsActivePowerSchemeReader;
        let observation = reader.observe_active_scheme();
        match observation {
            ActivePowerSchemeObservation::Observed { scheme_guid, .. } => {
                eprintln!("active Windows power-scheme GUID: {scheme_guid}");
                assert!(is_canonical_guid(&scheme_guid));
            }
            ActivePowerSchemeObservation::Unavailable { reason } => {
                panic!("active Windows power-scheme query was unavailable: {reason}")
            }
        }
    }
}
