//! Bounded Linux power facts for Auto GPU admission.
//!
//! This module reads only the kernel-provided `/sys/class/power_supply`
//! interface. It has no environment-configured paths, commands, or fallback
//! heuristics: missing, malformed, ambiguous, unreadable, or oversized facts
//! produce `Unknown`, which the Auto policy treats conservatively.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use crate::backend_policy::PowerSource;

const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";
const MAX_POWER_SUPPLIES: usize = 32;
const MAX_ATTRIBUTE_BYTES: usize = 64;

trait LinuxPowerFactSource {
    fn supply_names(&self) -> io::Result<Vec<String>>;
    fn read_attribute(&self, supply_name: &str, attribute: &str) -> io::Result<Vec<u8>>;
}

struct SysfsPowerFactSource;

impl LinuxPowerFactSource for SysfsPowerFactSource {
    fn supply_names(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(POWER_SUPPLY_ROOT)? {
            let entry = entry?;
            if !entry.metadata()?.is_dir() {
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 supply name"))?;
            if !is_safe_supply_name(&name) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsafe power-supply name",
                ));
            }
            names.push(name);
            if names.len() > MAX_POWER_SUPPLIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "too many power supplies",
                ));
            }
        }
        Ok(names)
    }

    fn read_attribute(&self, supply_name: &str, attribute: &str) -> io::Result<Vec<u8>> {
        if !is_safe_supply_name(supply_name) || !matches!(attribute, "type" | "online" | "status") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid power-supply attribute request",
            ));
        }
        read_bounded(
            &Path::new(POWER_SUPPLY_ROOT)
                .join(supply_name)
                .join(attribute),
        )
    }
}

pub(crate) fn power_source() -> PowerSource {
    power_source_from_facts(&SysfsPowerFactSource)
}

fn power_source_from_facts(source: &impl LinuxPowerFactSource) -> PowerSource {
    let Ok(mut names) = source.supply_names() else {
        return PowerSource::Unknown;
    };
    if names.is_empty() || names.len() > MAX_POWER_SUPPLIES {
        return PowerSource::Unknown;
    }
    names.sort_unstable();
    if names.windows(2).any(|pair| pair[0] == pair[1])
        || names.iter().any(|name| !is_safe_supply_name(name))
    {
        return PowerSource::Unknown;
    }

    let mut mains_online = false;
    let mut battery_discharging = false;
    for name in names {
        let Ok(kind) = read_token(source, &name, "type") else {
            return PowerSource::Unknown;
        };
        match kind.as_str() {
            "Mains" => {
                let Ok(online) = read_token(source, &name, "online") else {
                    return PowerSource::Unknown;
                };
                match online.as_str() {
                    "1" => mains_online = true,
                    "0" => {}
                    _ => return PowerSource::Unknown,
                }
            }
            "Battery" => {
                let Ok(status) = read_token(source, &name, "status") else {
                    return PowerSource::Unknown;
                };
                match status.as_str() {
                    "Discharging" => battery_discharging = true,
                    "Charging" | "Full" | "Not charging" | "Unknown" => {}
                    _ => return PowerSource::Unknown,
                }
            }
            "USB" | "UPS" | "Wireless" => {}
            _ => return PowerSource::Unknown,
        }
    }

    if mains_online {
        PowerSource::Ac
    } else if battery_discharging {
        PowerSource::Battery
    } else {
        // A disconnected mains supply plus a non-discharging battery is not
        // sufficient proof of battery operation. Do not guess.
        PowerSource::Unknown
    }
}

fn read_token(
    source: &impl LinuxPowerFactSource,
    supply_name: &str,
    attribute: &str,
) -> io::Result<String> {
    let bytes = source.read_attribute(supply_name, attribute)?;
    if bytes.is_empty() || bytes.len() > MAX_ATTRIBUTE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty or oversized power fact",
        ));
    }
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    if bytes.is_empty() || bytes.iter().any(|byte| !byte.is_ascii_graphic()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "malformed power fact",
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 power fact"))
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(MAX_ATTRIBUTE_BYTES + 1);
    file.by_ref()
        .take((MAX_ATTRIBUTE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ATTRIBUTE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized power fact",
        ));
    }
    Ok(bytes)
}

fn is_safe_supply_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct FixtureFacts {
        names: Vec<String>,
        names_error: bool,
        attributes: BTreeMap<(String, String), Result<Vec<u8>, io::ErrorKind>>,
    }

    impl FixtureFacts {
        fn with_attribute(
            mut self,
            supply: &str,
            attribute: &str,
            value: impl Into<Vec<u8>>,
        ) -> Self {
            if !self.names.iter().any(|name| name == supply) {
                self.names.push(supply.to_owned());
            }
            self.attributes
                .insert((supply.to_owned(), attribute.to_owned()), Ok(value.into()));
            self
        }

        fn with_error(mut self, supply: &str, attribute: &str, kind: io::ErrorKind) -> Self {
            if !self.names.iter().any(|name| name == supply) {
                self.names.push(supply.to_owned());
            }
            self.attributes
                .insert((supply.to_owned(), attribute.to_owned()), Err(kind));
            self
        }
    }

    impl LinuxPowerFactSource for FixtureFacts {
        fn supply_names(&self) -> io::Result<Vec<String>> {
            if self.names_error {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "fixture list denied",
                ))
            } else {
                Ok(self.names.clone())
            }
        }

        fn read_attribute(&self, supply_name: &str, attribute: &str) -> io::Result<Vec<u8>> {
            match self
                .attributes
                .get(&(supply_name.to_owned(), attribute.to_owned()))
            {
                Some(Ok(value)) => Ok(value.clone()),
                Some(Err(kind)) => Err(io::Error::new(*kind, "fixture read denied")),
                None => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "fixture fact missing",
                )),
            }
        }
    }

    fn ac_fixture() -> FixtureFacts {
        FixtureFacts::default()
            .with_attribute("AC", "type", b"Mains\n".to_vec())
            .with_attribute("AC", "online", b"1\n".to_vec())
            .with_attribute("BAT0", "type", b"Battery\n".to_vec())
            .with_attribute("BAT0", "status", b"Charging\n".to_vec())
    }

    fn battery_fixture() -> FixtureFacts {
        FixtureFacts::default()
            .with_attribute("AC", "type", b"Mains\n".to_vec())
            .with_attribute("AC", "online", b"0\n".to_vec())
            .with_attribute("BAT0", "type", b"Battery\n".to_vec())
            .with_attribute("BAT0", "status", b"Discharging\n".to_vec())
    }

    #[test]
    fn detects_kernel_reported_ac_and_battery_states() {
        assert_eq!(power_source_from_facts(&ac_fixture()), PowerSource::Ac);
        assert_eq!(
            power_source_from_facts(&battery_fixture()),
            PowerSource::Battery
        );
    }

    #[test]
    fn ambiguous_and_unreadable_facts_are_unknown() {
        let ambiguous = FixtureFacts::default()
            .with_attribute("AC", "type", b"Mains\n".to_vec())
            .with_attribute("AC", "online", b"0\n".to_vec())
            .with_attribute("BAT0", "type", b"Battery\n".to_vec())
            .with_attribute("BAT0", "status", b"Full\n".to_vec());
        let unreadable = FixtureFacts::default()
            .with_attribute("AC", "type", b"Mains\n".to_vec())
            .with_error("AC", "online", io::ErrorKind::PermissionDenied);
        let list_unreadable = FixtureFacts {
            names_error: true,
            ..FixtureFacts::default()
        };

        assert_eq!(power_source_from_facts(&ambiguous), PowerSource::Unknown);
        assert_eq!(power_source_from_facts(&unreadable), PowerSource::Unknown);
        assert_eq!(
            power_source_from_facts(&list_unreadable),
            PowerSource::Unknown
        );
    }

    #[test]
    fn hostile_malformed_and_oversized_facts_are_unknown() {
        let hostile_name = FixtureFacts::default()
            .with_attribute("../AC", "type", b"Mains\n".to_vec())
            .with_attribute("../AC", "online", b"1\n".to_vec());
        let malformed = FixtureFacts::default()
            .with_attribute("AC", "type", b"Mains\n".to_vec())
            .with_attribute("AC", "online", b"1\n\n".to_vec());
        let oversized = FixtureFacts::default()
            .with_attribute("AC", "type", vec![b'M'; MAX_ATTRIBUTE_BYTES + 1])
            .with_attribute("AC", "online", b"1\n".to_vec());

        assert_eq!(power_source_from_facts(&hostile_name), PowerSource::Unknown);
        assert_eq!(power_source_from_facts(&malformed), PowerSource::Unknown);
        assert_eq!(power_source_from_facts(&oversized), PowerSource::Unknown);
    }
}
