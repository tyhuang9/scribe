use std::fmt;

const ACCOUNT_PREFIX: &[u8] = b"release-security-epoch-v1/";
const PAYLOAD_PREFIX: &[u8] = b"scribe-gpu-release-security-epoch-v1:";
const EPOCH_DECIMAL_WIDTH: usize = 20;
pub(super) const MAX_MARKERS: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct EpochMarker {
    pub(super) account: Vec<u8>,
    pub(super) payload: Vec<u8>,
}

impl EpochMarker {
    pub(super) fn canonical(epoch: u64) -> Self {
        let epoch = format!("{epoch:020}");
        let mut account = Vec::with_capacity(ACCOUNT_PREFIX.len() + EPOCH_DECIMAL_WIDTH);
        account.extend_from_slice(ACCOUNT_PREFIX);
        account.extend_from_slice(epoch.as_bytes());
        let mut payload = Vec::with_capacity(PAYLOAD_PREFIX.len() + EPOCH_DECIMAL_WIDTH);
        payload.extend_from_slice(PAYLOAD_PREFIX);
        payload.extend_from_slice(epoch.as_bytes());
        Self { account, payload }
    }

    fn epoch(&self) -> Result<u64, AdmissionError> {
        let account_epoch = parse_epoch(&self.account, ACCOUNT_PREFIX)?;
        let payload_epoch = parse_epoch(&self.payload, PAYLOAD_PREFIX)?;
        if account_epoch != payload_epoch || *self != Self::canonical(account_epoch) {
            return Err(AdmissionError::CorruptStore);
        }
        Ok(account_epoch)
    }
}

fn parse_epoch(bytes: &[u8], prefix: &[u8]) -> Result<u64, AdmissionError> {
    let digits = bytes
        .strip_prefix(prefix)
        .filter(|digits| digits.len() == EPOCH_DECIMAL_WIDTH)
        .ok_or(AdmissionError::CorruptStore)?;
    if !digits.iter().all(u8::is_ascii_digit) {
        return Err(AdmissionError::CorruptStore);
    }
    std::str::from_utf8(digits)
        .ok()
        .and_then(|digits| digits.parse::<u64>().ok())
        .ok_or(AdmissionError::CorruptStore)
}

pub(super) trait MarkerStore {
    fn scan(&mut self) -> Result<Vec<EpochMarker>, AdmissionError>;
    fn append(&mut self, marker: &EpochMarker) -> Result<(), AdmissionError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionError {
    CorruptStore,
    StoreUnavailable,
    Contended,
    TimedOut,
    Downgrade { candidate: u64, floor: u64 },
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("device rollback authority rejected GPU admission")
    }
}

fn scan_floor(store: &mut impl MarkerStore) -> Result<u64, AdmissionError> {
    let markers = store.scan()?;
    if markers.len() > MAX_MARKERS {
        return Err(AdmissionError::CorruptStore);
    }
    markers
        .iter()
        .try_fold(0_u64, |floor, marker| Ok(floor.max(marker.epoch()?)))
}

pub(super) fn admit_with_store(
    candidate: u64,
    store: &mut impl MarkerStore,
) -> Result<(), AdmissionError> {
    let observed = scan_floor(store)?;
    if candidate < observed {
        return Err(AdmissionError::Downgrade {
            candidate,
            floor: observed,
        });
    }

    store.append(&EpochMarker::canonical(candidate))?;

    let final_floor = scan_floor(store)?;
    if candidate < final_floor {
        return Err(AdmissionError::Downgrade {
            candidate,
            floor: final_floor,
        });
    }
    if final_floor < candidate {
        return Err(AdmissionError::CorruptStore);
    }
    Ok(())
}

#[cfg(all(target_os = "macos", scribe_macos_keychain_authority))]
mod macos {
    use std::ffi::{CString, c_char, c_int, c_uchar, c_ulong};
    use std::sync::OnceLock;
    use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
    use std::time::Duration;

    use super::{AdmissionError, EpochMarker, MAX_MARKERS, MarkerStore, admit_with_store};

    const REQUEST_TIMEOUT: Duration = Duration::from_millis(500);
    const NATIVE_FIELD_CAPACITY: usize = 96;
    const NATIVE_OK: c_int = 0;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NativeEpochMarker {
        account: [c_uchar; NATIVE_FIELD_CAPACITY],
        account_len: c_ulong,
        payload: [c_uchar; NATIVE_FIELD_CAPACITY],
        payload_len: c_ulong,
    }

    unsafe extern "C" {
        fn scribe_macos_keychain_epoch_scan(
            access_group: *const c_char,
            markers: *mut NativeEpochMarker,
            capacity: c_ulong,
            count: *mut c_ulong,
        ) -> c_int;
        fn scribe_macos_keychain_epoch_append(
            access_group: *const c_char,
            account: *const c_uchar,
            account_len: c_ulong,
            payload: *const c_uchar,
            payload_len: c_ulong,
        ) -> c_int;
    }

    struct NativeStore {
        access_group: CString,
    }

    impl NativeStore {
        fn new(access_group: &str) -> Result<Self, AdmissionError> {
            let access_group =
                CString::new(access_group).map_err(|_| AdmissionError::StoreUnavailable)?;
            Ok(Self { access_group })
        }
    }

    impl MarkerStore for NativeStore {
        fn scan(&mut self) -> Result<Vec<EpochMarker>, AdmissionError> {
            let mut markers = vec![
                NativeEpochMarker {
                    account: [0; NATIVE_FIELD_CAPACITY],
                    account_len: 0,
                    payload: [0; NATIVE_FIELD_CAPACITY],
                    payload_len: 0,
                };
                MAX_MARKERS
            ];
            let mut count = 0;
            // SAFETY: the shim receives fixed-capacity writable records and a valid C string.
            let status = unsafe {
                scribe_macos_keychain_epoch_scan(
                    self.access_group.as_ptr(),
                    markers.as_mut_ptr(),
                    markers.len() as c_ulong,
                    &mut count,
                )
            };
            if status != NATIVE_OK || count as usize > markers.len() {
                return Err(AdmissionError::StoreUnavailable);
            }
            markers
                .into_iter()
                .take(count as usize)
                .map(|marker| {
                    let account_len = marker.account_len as usize;
                    let payload_len = marker.payload_len as usize;
                    if account_len > marker.account.len() || payload_len > marker.payload.len() {
                        return Err(AdmissionError::CorruptStore);
                    }
                    Ok(EpochMarker {
                        account: marker.account[..account_len].to_vec(),
                        payload: marker.payload[..payload_len].to_vec(),
                    })
                })
                .collect()
        }

        fn append(&mut self, marker: &EpochMarker) -> Result<(), AdmissionError> {
            // SAFETY: all pointers remain valid for the duration of the synchronous shim call.
            let status = unsafe {
                scribe_macos_keychain_epoch_append(
                    self.access_group.as_ptr(),
                    marker.account.as_ptr(),
                    marker.account.len() as c_ulong,
                    marker.payload.as_ptr(),
                    marker.payload.len() as c_ulong,
                )
            };
            (status == NATIVE_OK)
                .then_some(())
                .ok_or(AdmissionError::StoreUnavailable)
        }
    }

    struct Request {
        epoch: u64,
        response: SyncSender<Result<(), AdmissionError>>,
    }

    enum Client {
        Ready(SyncSender<Request>),
        Unavailable,
    }

    static CLIENT: OnceLock<Client> = OnceLock::new();

    fn worker(receiver: Receiver<Request>, access_group: String) {
        while let Ok(request) = receiver.recv() {
            let result = NativeStore::new(&access_group)
                .and_then(|mut store| admit_with_store(request.epoch, &mut store));
            let _ = request.response.try_send(result);
        }
    }

    pub(super) fn admit(epoch: u64) -> Result<(), AdmissionError> {
        let access_group =
            option_env!("SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP").unwrap_or("");
        if access_group.is_empty() {
            return Err(AdmissionError::StoreUnavailable);
        }
        let client = CLIENT.get_or_init(|| {
            let (sender, receiver) = mpsc::sync_channel(1);
            match std::thread::Builder::new()
                .name("scribe-keychain-epoch".to_owned())
                .spawn({
                    let access_group = access_group.to_owned();
                    move || worker(receiver, access_group)
                }) {
                Ok(_) => Client::Ready(sender),
                Err(_) => Client::Unavailable,
            }
        });
        let Client::Ready(sender) = client else {
            return Err(AdmissionError::StoreUnavailable);
        };
        let (response, result) = mpsc::sync_channel(1);
        match sender.try_send(Request { epoch, response }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(AdmissionError::Contended),
            Err(TrySendError::Disconnected(_)) => return Err(AdmissionError::StoreUnavailable),
        }
        result
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|_| AdmissionError::TimedOut)?
    }
}

#[cfg(all(target_os = "macos", scribe_macos_keychain_authority))]
pub(super) use macos::admit;

#[cfg(all(target_os = "macos", not(scribe_macos_keychain_authority)))]
pub(super) fn admit(_epoch: u64) -> Result<(), AdmissionError> {
    Err(AdmissionError::StoreUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeStore {
        markers: Vec<EpochMarker>,
        fail_scan_at: Option<usize>,
        fail_append: Option<AdmissionError>,
        scans: usize,
        concurrent_high: Option<u64>,
    }

    impl MarkerStore for FakeStore {
        fn scan(&mut self) -> Result<Vec<EpochMarker>, AdmissionError> {
            self.scans += 1;
            if self.fail_scan_at == Some(self.scans) {
                return Err(AdmissionError::StoreUnavailable);
            }
            if self.scans == 2
                && let Some(epoch) = self.concurrent_high.take()
            {
                self.markers.push(EpochMarker::canonical(epoch));
            }
            Ok(self.markers.clone())
        }

        fn append(&mut self, marker: &EpochMarker) -> Result<(), AdmissionError> {
            if let Some(error) = self.fail_append {
                return Err(error);
            }
            if !self.markers.contains(marker) {
                self.markers.push(marker.clone());
            }
            Ok(())
        }
    }

    #[test]
    fn first_same_higher_and_lower_epochs_are_monotonic() {
        let mut store = FakeStore::default();
        assert_eq!(admit_with_store(1, &mut store), Ok(()));
        assert_eq!(admit_with_store(1, &mut store), Ok(()));
        assert_eq!(store.markers.len(), 1, "duplicate marker stays idempotent");
        assert_eq!(admit_with_store(4, &mut store), Ok(()));
        assert!(matches!(
            admit_with_store(3, &mut store),
            Err(AdmissionError::Downgrade {
                candidate: 3,
                floor: 4
            })
        ));
    }

    #[test]
    fn final_reread_rejects_a_lower_candidate_that_loses_a_race() {
        let mut store = FakeStore {
            concurrent_high: Some(9),
            ..FakeStore::default()
        };
        assert!(matches!(
            admit_with_store(5, &mut store),
            Err(AdmissionError::Downgrade {
                candidate: 5,
                floor: 9
            })
        ));
    }

    #[test]
    fn malformed_overflow_and_excessive_results_are_rejected() {
        let mut malformed = FakeStore {
            markers: vec![EpochMarker {
                account: b"release-security-epoch-v1/00000000000000000001".to_vec(),
                payload: b"scribe-gpu-release-security-epoch-v1:00000000000000000002".to_vec(),
            }],
            ..FakeStore::default()
        };
        assert_eq!(
            admit_with_store(2, &mut malformed),
            Err(AdmissionError::CorruptStore)
        );

        let mut overflow = FakeStore {
            markers: vec![EpochMarker {
                account: b"release-security-epoch-v1/99999999999999999999".to_vec(),
                payload: b"scribe-gpu-release-security-epoch-v1:99999999999999999999".to_vec(),
            }],
            ..FakeStore::default()
        };
        assert_eq!(
            admit_with_store(2, &mut overflow),
            Err(AdmissionError::CorruptStore)
        );

        let mut excessive = FakeStore {
            markers: vec![EpochMarker::canonical(1); MAX_MARKERS + 1],
            ..FakeStore::default()
        };
        assert_eq!(
            admit_with_store(2, &mut excessive),
            Err(AdmissionError::CorruptStore)
        );
    }

    #[test]
    fn scan_append_and_final_scan_failures_all_deny() {
        let failures = [
            FakeStore {
                fail_scan_at: Some(1),
                ..FakeStore::default()
            },
            FakeStore {
                fail_append: Some(AdmissionError::StoreUnavailable),
                ..FakeStore::default()
            },
            FakeStore {
                fail_scan_at: Some(2),
                ..FakeStore::default()
            },
            FakeStore {
                fail_append: Some(AdmissionError::Contended),
                ..FakeStore::default()
            },
            FakeStore {
                fail_append: Some(AdmissionError::TimedOut),
                ..FakeStore::default()
            },
        ];
        for (mut store, expected) in failures.into_iter().zip([
            AdmissionError::StoreUnavailable,
            AdmissionError::StoreUnavailable,
            AdmissionError::StoreUnavailable,
            AdmissionError::Contended,
            AdmissionError::TimedOut,
        ]) {
            assert_eq!(admit_with_store(1, &mut store), Err(expected));
        }
    }
}
