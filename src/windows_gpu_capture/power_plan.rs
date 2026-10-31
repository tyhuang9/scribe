//! Read-only, sampled active BASE-scheme index guard for capture experiments.
//!
//! This is not an effective-policy/overlay/thermal/isolation observation and
//! its digest is NOT the qualification contract's `power_plan_sha256`.
use std::cell::Cell;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, LocalFree};
use windows_sys::Win32::System::Power::{
    ACCESS_INDIVIDUAL_SETTING, ACCESS_SUBGROUP, PowerEnumerate, PowerGetActiveScheme,
    PowerReadACValueIndex, PowerReadDCValueIndex,
};
use windows_sys::core::GUID;

const METHOD: &str = "powrprof_active_base_scheme_ac_dc_indexes_v1";
const DOMAIN: &[u8] = b"SCRIBE-WINDOWS-BASE-POWER-SCHEME-SNAPSHOT-V1\0";
const MAX_GROUPS: usize = 64;
const MAX_SETTINGS: usize = 1024;
const MAX_CHECKS: u32 = 256;
const READ_BUDGET: Duration = Duration::from_secs(3);
const NO_SUBGROUP: [u8; 16] = [
    0xfe, 0xa3, 0x41, 0x3e, 0x7e, 0x05, 0x49, 0x11, 0x9a, 0x71, 0x70, 0x03, 0x31, 0xf1, 0xc2, 0x94,
];
type Id = [u8; 16];
type ReadResult<T> = std::result::Result<T, GuardFailure>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum GuardFailure {
    QueryFailed,
    MalformedResult,
    InventoryBound,
    ReadBudgetExceeded,
    StateChanged,
    EvidenceBound,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Setting {
    id: Id,
    ac: u32,
    dc: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Group {
    id: Id,
    settings: Vec<Setting>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    scheme: Id,
    groups: Vec<Group>,
}

trait Reader {
    fn active(&mut self) -> ReadResult<Id>;
    fn entry(&mut self, scheme: Id, group: Option<Id>, index: u32) -> ReadResult<Option<Id>>;
    fn indices(&mut self, scheme: Id, group: Id, setting: Id) -> ReadResult<(u32, u32)>;
}

fn valid_id(id: Id) -> ReadResult<Id> {
    if id == [0; 16] {
        Err(GuardFailure::MalformedResult)
    } else {
        Ok(id)
    }
}

fn enumerate(
    reader: &mut impl Reader,
    scheme: Id,
    group: Option<Id>,
    bound: usize,
) -> ReadResult<Vec<Id>> {
    let mut ids = Vec::new();
    // Include one terminal probe: accepting a truncated prefix is not success.
    for index in 0..=bound {
        match reader.entry(scheme, group, index as u32)? {
            None => {
                ids.sort_unstable();
                return Ok(ids);
            }
            Some(id) => {
                let id = valid_id(id)?;
                if ids.contains(&id) {
                    return Err(GuardFailure::MalformedResult);
                }
                if ids.len() == bound {
                    return Err(GuardFailure::InventoryBound);
                }
                ids.push(id);
            }
        }
    }
    Err(GuardFailure::InventoryBound)
}

fn snapshot(reader: &mut impl Reader) -> ReadResult<Snapshot> {
    let scheme = valid_id(reader.active()?)?;
    let mut ids = enumerate(reader, scheme, None, MAX_GROUPS)?;
    // Root settings are a separate documented namespace, visited exactly once.
    if !ids.contains(&NO_SUBGROUP) {
        if ids.len() == MAX_GROUPS {
            return Err(GuardFailure::InventoryBound);
        }
        ids.push(NO_SUBGROUP);
        ids.sort_unstable();
    }
    let mut groups = Vec::new();
    let mut count = 0;
    for id in ids {
        let settings = enumerate(reader, scheme, Some(id), MAX_SETTINGS - count)?;
        count += settings.len();
        let settings = settings
            .into_iter()
            .map(|setting| {
                let (ac, dc) = reader.indices(scheme, id, setting)?;
                Ok(Setting {
                    id: setting,
                    ac,
                    dc,
                })
            })
            .collect::<ReadResult<Vec<_>>>()?;
        groups.push(Group { id, settings });
    }
    if count == 0 {
        return Err(GuardFailure::MalformedResult);
    }
    if valid_id(reader.active()?)? != scheme {
        return Err(GuardFailure::StateChanged);
    }
    Ok(Snapshot { scheme, groups })
}

fn stable_snapshot(reader: &mut impl Reader) -> ReadResult<Snapshot> {
    let first = snapshot(reader)?;
    if snapshot(reader)? != first {
        return Err(GuardFailure::StateChanged);
    }
    Ok(first)
}

fn fingerprint(snapshot: &Snapshot) -> String {
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    hash.update(snapshot.scheme);
    hash.update((snapshot.groups.len() as u32).to_le_bytes());
    for group in &snapshot.groups {
        hash.update(group.id);
        hash.update((group.settings.len() as u32).to_le_bytes());
        for setting in &group.settings {
            hash.update(setting.id);
            hash.update(setting.ac.to_le_bytes());
            hash.update(setting.dc.to_le_bytes());
        }
    }
    super::hex(&hash.finalize())
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct GuardReport {
    method: &'static str,
    snapshot_sha256: String,
    subgroup_count: usize,
    setting_count: usize,
    checks_attempted: u32,
    checks_completed: u32,
    query_elapsed_micros: u64,
    effective_policy_observed: bool,
    continuous_stability_established: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<GuardFailure>,
}

impl GuardReport {
    #[cfg(test)]
    pub(super) fn fixture(checks_completed: u32, failure: Option<GuardFailure>) -> Self {
        Self {
            method: METHOD,
            snapshot_sha256: "a".repeat(64),
            subgroup_count: 1,
            setting_count: 1,
            checks_attempted: checks_completed + u32::from(failure.is_some()),
            checks_completed,
            query_elapsed_micros: 7,
            effective_policy_observed: false,
            continuous_stability_established: false,
            failure,
        }
    }

    pub(super) fn validate(&self, incomplete: bool) -> bool {
        self.method == METHOD
            && super::canonical_sha256(&self.snapshot_sha256).is_ok()
            && (1..=MAX_GROUPS).contains(&self.subgroup_count)
            && (1..=MAX_SETTINGS).contains(&self.setting_count)
            && (1..=MAX_CHECKS).contains(&self.checks_attempted)
            && (1..=self.checks_attempted).contains(&self.checks_completed)
            && (self.failure.is_some() || self.checks_completed == self.checks_attempted)
            && (incomplete || self.failure.is_none())
            && !self.effective_policy_observed
            && !self.continuous_stability_established
    }

    pub(super) fn completed_checks(&self) -> u32 {
        self.checks_completed
    }
}

pub(super) trait SchemeGuard {
    fn verify_unchanged(&self) -> ReadResult<()>;
    fn report(&self) -> GuardReport;
}

pub(super) struct PowerPlanGuard {
    baseline: Snapshot,
    attempts: Cell<u32>,
    completed: Cell<u32>,
    elapsed: Cell<u64>,
    failure: Cell<Option<GuardFailure>>,
}

impl PowerPlanGuard {
    pub(super) fn capture() -> ReadResult<Self> {
        let started = Instant::now();
        let baseline = stable_snapshot(&mut NativeReader::new())?;
        Ok(Self {
            baseline,
            attempts: Cell::new(1),
            completed: Cell::new(1),
            elapsed: Cell::new(
                u64::try_from(started.elapsed().as_micros())
                    .map_err(|_| GuardFailure::EvidenceBound)?,
            ),
            failure: Cell::new(None),
        })
    }

    fn verify_with(
        &self,
        read: impl FnOnce() -> ReadResult<Snapshot>,
        elapsed: impl FnOnce() -> ReadResult<u64>,
    ) -> ReadResult<()> {
        if let Some(failure) = self.failure.get() {
            return Err(failure);
        }
        let outcome = (|| {
            if self.attempts.get() == MAX_CHECKS {
                return Err(GuardFailure::EvidenceBound);
            }
            self.attempts.set(self.attempts.get() + 1);
            let observed = read();
            self.elapsed.set(
                self.elapsed
                    .get()
                    .checked_add(elapsed()?)
                    .ok_or(GuardFailure::EvidenceBound)?,
            );
            if observed? != self.baseline {
                return Err(GuardFailure::StateChanged);
            }
            self.completed.set(self.completed.get() + 1);
            Ok(())
        })();
        if let Err(failure) = outcome {
            self.failure.set(Some(failure));
        }
        outcome
    }
}

impl SchemeGuard for PowerPlanGuard {
    fn verify_unchanged(&self) -> ReadResult<()> {
        let started = Instant::now();
        self.verify_with(
            || stable_snapshot(&mut NativeReader::new()),
            || {
                u64::try_from(started.elapsed().as_micros())
                    .map_err(|_| GuardFailure::EvidenceBound)
            },
        )
    }

    fn report(&self) -> GuardReport {
        GuardReport {
            method: METHOD,
            snapshot_sha256: fingerprint(&self.baseline),
            subgroup_count: self.baseline.groups.len(),
            setting_count: self
                .baseline
                .groups
                .iter()
                .map(|group| group.settings.len())
                .sum(),
            checks_attempted: self.attempts.get(),
            checks_completed: self.completed.get(),
            query_elapsed_micros: self.elapsed.get(),
            effective_policy_observed: false,
            continuous_stability_established: false,
            failure: self.failure.get(),
        }
    }
}

struct NativeReader {
    deadline: Instant,
}

impl NativeReader {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + READ_BUDGET,
        }
    }

    fn budget(&self) -> ReadResult<()> {
        if Instant::now() >= self.deadline {
            Err(GuardFailure::ReadBudgetExceeded)
        } else {
            Ok(())
        }
    }
}

fn native_guid(id: Id) -> GUID {
    GUID::from_u128(u128::from_be_bytes(id))
}

fn canonical_guid(guid: GUID) -> Id {
    let mut id = [0; 16];
    id[..4].copy_from_slice(&guid.data1.to_be_bytes());
    id[4..6].copy_from_slice(&guid.data2.to_be_bytes());
    id[6..8].copy_from_slice(&guid.data3.to_be_bytes());
    id[8..].copy_from_slice(&guid.data4);
    id
}

// SAFETY: a nonnull successful query output must point to a readable GUID;
// release must accept exactly the nonnull allocation supplied by query.
unsafe fn active_query(
    query: impl FnOnce(&mut *mut GUID) -> u32,
    release: impl FnOnce(*mut GUID) -> bool,
    budget: impl Fn() -> ReadResult<()>,
) -> ReadResult<Id> {
    budget()?;
    let mut pointer = std::ptr::null_mut();
    let status = query(&mut pointer);
    let value = if status == ERROR_SUCCESS && !pointer.is_null() {
        // SAFETY: the caller guarantees a readable successful output.
        Some(canonical_guid(unsafe { *pointer }))
    } else {
        None
    };
    // Release precedes every post-call failure, including budget expiry.
    let released = pointer.is_null() || release(pointer);
    budget()?;
    if !released {
        return Err(GuardFailure::QueryFailed);
    }
    value.ok_or(GuardFailure::QueryFailed)
}

fn entry_query(
    query: impl FnOnce(&mut GUID, &mut u32) -> u32,
    budget: impl Fn() -> ReadResult<()>,
) -> ReadResult<Option<Id>> {
    budget()?;
    let mut output = GUID::from_u128(0);
    let mut size = std::mem::size_of::<GUID>() as u32;
    let status = query(&mut output, &mut size);
    budget()?;
    match status {
        ERROR_NO_MORE_ITEMS => Ok(None),
        ERROR_SUCCESS if size == std::mem::size_of::<GUID>() as u32 => {
            Ok(Some(canonical_guid(output)))
        }
        ERROR_SUCCESS => Err(GuardFailure::MalformedResult),
        _ => Err(GuardFailure::QueryFailed),
    }
}

fn index_queries(
    ac_query: impl FnOnce(&mut u32) -> u32,
    dc_query: impl FnOnce(&mut u32) -> u32,
    budget: impl Fn() -> ReadResult<()>,
) -> ReadResult<(u32, u32)> {
    let mut ac = 0;
    let mut dc = 0;
    budget()?;
    let status = ac_query(&mut ac);
    budget()?;
    if status != ERROR_SUCCESS {
        return Err(GuardFailure::QueryFailed);
    }
    let status = dc_query(&mut dc);
    budget()?;
    if status != ERROR_SUCCESS {
        return Err(GuardFailure::QueryFailed);
    }
    Ok((ac, dc))
}

impl Reader for NativeReader {
    fn active(&mut self) -> ReadResult<Id> {
        // SAFETY: the synchronous system query supplies its own GUID allocation;
        // only that allocation is passed exactly once to its documented releaser.
        unsafe {
            active_query(
                |pointer| PowerGetActiveScheme(std::ptr::null_mut(), pointer),
                |pointer| LocalFree(pointer.cast()).is_null(),
                || self.budget(),
            )
        }
    }

    fn entry(&mut self, scheme: Id, group: Option<Id>, index: u32) -> ReadResult<Option<Id>> {
        let scheme = native_guid(scheme);
        let group = group.map(native_guid);
        // SAFETY: all inputs live for the synchronous call; buffer is exactly one GUID.
        entry_query(
            |output, size| unsafe {
                PowerEnumerate(
                    std::ptr::null_mut(),
                    &scheme,
                    group
                        .as_ref()
                        .map_or(std::ptr::null(), |guid| guid as *const _),
                    if group.is_some() {
                        ACCESS_INDIVIDUAL_SETTING
                    } else {
                        ACCESS_SUBGROUP
                    },
                    index,
                    (output as *mut GUID).cast(),
                    size,
                )
            },
            || self.budget(),
        )
    }

    fn indices(&mut self, scheme: Id, group: Id, setting: Id) -> ReadResult<(u32, u32)> {
        let (scheme, group, setting) = (
            native_guid(scheme),
            native_guid(group),
            native_guid(setting),
        );
        // SAFETY: reserved key is null; GUIDs and DWORD outputs live for each call.
        index_queries(
            |ac| unsafe {
                PowerReadACValueIndex(std::ptr::null_mut(), &scheme, &group, &setting, ac)
            },
            |dc| unsafe {
                PowerReadDCValueIndex(std::ptr::null_mut(), &scheme, &group, &setting, dc)
            },
            || self.budget(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, VecDeque};

    fn id(value: u8) -> Id {
        let mut id = [0; 16];
        id[15] = value;
        id
    }

    struct Fixture {
        scheme: Id,
        active_sequence: VecDeque<Id>,
        groups: Vec<Id>,
        settings: BTreeMap<Id, Vec<Id>>,
        ac: u32,
        dc: u32,
        calls: usize,
        fail_at: Option<usize>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                scheme: id(1),
                active_sequence: VecDeque::new(),
                groups: vec![id(2)],
                settings: BTreeMap::from([(id(2), vec![id(3), id(4)]), (NO_SUBGROUP, vec![id(5)])]),
                ac: 0,
                dc: 7,
                calls: 0,
                fail_at: None,
            }
        }

        fn step(&mut self) -> ReadResult<()> {
            self.calls += 1;
            if self.fail_at == Some(self.calls) {
                Err(GuardFailure::QueryFailed)
            } else {
                Ok(())
            }
        }
    }

    impl Reader for Fixture {
        fn active(&mut self) -> ReadResult<Id> {
            self.step()?;
            Ok(self.active_sequence.pop_front().unwrap_or(self.scheme))
        }

        fn entry(&mut self, scheme: Id, group: Option<Id>, index: u32) -> ReadResult<Option<Id>> {
            self.step()?;
            assert_eq!(scheme, self.scheme);
            Ok(match group {
                None => self.groups.get(index as usize).copied(),
                Some(group) => self
                    .settings
                    .get(&group)
                    .and_then(|items| items.get(index as usize))
                    .copied(),
            })
        }

        fn indices(&mut self, scheme: Id, _group: Id, _setting: Id) -> ReadResult<(u32, u32)> {
            self.step()?;
            assert_eq!(scheme, self.scheme);
            Ok((self.ac, self.dc))
        }
    }

    fn guard() -> PowerPlanGuard {
        PowerPlanGuard {
            baseline: stable_snapshot(&mut Fixture::new()).unwrap(),
            attempts: Cell::new(1),
            completed: Cell::new(1),
            elapsed: Cell::new(0),
            failure: Cell::new(None),
        }
    }

    #[test]
    fn canonical_order_and_explicit_root_settings_preserve_zero_indices() {
        let baseline = stable_snapshot(&mut Fixture::new()).unwrap();
        let mut reordered = Fixture::new();
        reordered.groups.push(NO_SUBGROUP);
        reordered.groups.reverse();
        reordered.settings.get_mut(&id(2)).unwrap().reverse();
        let observed = stable_snapshot(&mut reordered).unwrap();
        assert_eq!(baseline, observed);
        assert_eq!(fingerprint(&baseline), fingerprint(&observed));
        // Independently encoded vector: RFC GUID bytes, little-endian counts
        // and DWORD indices, domain-separated and SHA-256 hashed.
        assert_eq!(
            fingerprint(&baseline),
            "898c9126e2ce7301287d6f42a19cfa694722648de6f50b7589029f9ce1c2a765"
        );
        assert_eq!(baseline.groups.len(), 2);
        assert_eq!(
            baseline
                .groups
                .iter()
                .map(|group| group.settings.len())
                .sum::<usize>(),
            3
        );
        assert!(
            baseline
                .groups
                .iter()
                .flat_map(|group| &group.settings)
                .all(|setting| setting.ac == 0)
        );
    }

    #[test]
    fn every_bound_identity_and_ac_dc_value_affects_digest() {
        let baseline = snapshot(&mut Fixture::new()).unwrap();
        let digest = fingerprint(&baseline);
        for field in 0..5 {
            let mut changed = baseline.clone();
            match field {
                0 => changed.scheme = id(9),
                1 => changed.groups[0].id = id(9),
                2 => changed.groups[0].settings[0].id = id(9),
                3 => changed.groups[0].settings[0].ac = 1,
                4 => changed.groups[0].settings[0].dc = 8,
                _ => unreachable!(),
            }
            assert_ne!(fingerprint(&changed), digest);
        }
    }

    #[test]
    fn guid_encoding_is_canonical_not_native_struct_endianness() {
        let bytes = NO_SUBGROUP;
        let native = native_guid(bytes);
        assert_eq!(native.data1, 0xfea3413e);
        assert_eq!(native.data2, 0x7e05);
        assert_eq!(canonical_guid(native), bytes);
    }

    #[test]
    fn duplicate_or_zero_identifiers_and_empty_inventory_reject() {
        for case in 0..5 {
            let mut fixture = Fixture::new();
            match case {
                0 => fixture.scheme = [0; 16],
                1 => fixture.groups.push(id(2)),
                2 => fixture.settings.get_mut(&id(2)).unwrap().push(id(3)),
                3 => fixture
                    .settings
                    .insert(id(2), vec![[0; 16]])
                    .map(|_| ())
                    .unwrap(),
                4 => fixture.settings.clear(),
                _ => unreachable!(),
            }
            assert_eq!(snapshot(&mut fixture), Err(GuardFailure::MalformedResult));
        }
    }

    #[test]
    fn every_reader_stage_error_denies_a_stable_snapshot() {
        let mut baseline = Fixture::new();
        stable_snapshot(&mut baseline).unwrap();
        for fail_at in 1..=baseline.calls {
            let mut fixture = Fixture::new();
            fixture.fail_at = Some(fail_at);
            assert_eq!(
                stable_snapshot(&mut fixture),
                Err(GuardFailure::QueryFailed)
            );
        }
    }

    #[test]
    fn each_full_traversal_rechecks_active_scheme_at_its_end() {
        for sequence in [[id(1), id(9), id(1), id(1)], [id(1), id(1), id(1), id(9)]] {
            let mut fixture = Fixture::new();
            fixture.active_sequence = sequence.into();
            assert_eq!(
                stable_snapshot(&mut fixture),
                Err(GuardFailure::StateChanged)
            );
        }
    }

    #[test]
    fn bounded_enumeration_requires_the_terminal_probe() {
        for length in (MAX_GROUPS - 1)..=(MAX_GROUPS + 1) {
            let mut fixture = Fixture::new();
            fixture.groups = (0..length)
                .map(|index| {
                    let mut value = [0; 16];
                    value[12..].copy_from_slice(&(index as u32 + 1).to_be_bytes());
                    value
                })
                .collect();
            let result = enumerate(&mut fixture, id(1), None, MAX_GROUPS);
            if length <= MAX_GROUPS {
                assert_eq!(result.unwrap().len(), length);
            } else {
                assert_eq!(result, Err(GuardFailure::InventoryBound));
            }
            assert!(fixture.calls <= MAX_GROUPS + 1);
        }
    }

    #[test]
    fn aggregate_setting_limit_is_not_reset_for_each_subgroup() {
        for extra in [false, true] {
            let mut fixture = Fixture::new();
            fixture.settings.insert(NO_SUBGROUP, Vec::new());
            fixture.settings.insert(
                id(2),
                (0..MAX_SETTINGS)
                    .map(|index| {
                        let mut value = [0; 16];
                        value[12..].copy_from_slice(&(index as u32 + 1).to_be_bytes());
                        value
                    })
                    .collect(),
            );
            if extra {
                fixture.settings.insert(NO_SUBGROUP, vec![id(8)]);
                assert_eq!(snapshot(&mut fixture), Err(GuardFailure::InventoryBound));
            } else {
                assert_eq!(
                    snapshot(&mut fixture)
                        .unwrap()
                        .groups
                        .iter()
                        .map(|group| group.settings.len())
                        .sum::<usize>(),
                    MAX_SETTINGS
                );
            }
        }
    }

    #[test]
    fn original_baseline_and_first_failure_are_immutable() {
        let guard = guard();
        let baseline = guard.baseline.clone();
        assert_eq!(
            guard.verify_with(|| Ok(baseline.clone()), || Ok(17)),
            Ok(())
        );
        let mut changed = baseline.clone();
        changed.scheme = id(8);
        assert_eq!(
            guard.verify_with(|| Ok(changed), || Ok(23)),
            Err(GuardFailure::StateChanged)
        );
        assert_eq!(
            guard.verify_with(
                || panic!("latched failure must not read"),
                || panic!("must not time")
            ),
            Err(GuardFailure::StateChanged)
        );
        assert_eq!(guard.baseline, baseline);
        let report = guard.report();
        assert_eq!(report.checks_attempted, 3);
        assert_eq!(report.checks_completed, 2);
        assert_eq!(report.query_elapsed_micros, 40);
    }

    #[test]
    fn failed_reads_budget_and_evidence_overflow_latch() {
        for failure in [
            GuardFailure::QueryFailed,
            GuardFailure::ReadBudgetExceeded,
            GuardFailure::MalformedResult,
        ] {
            let guard = guard();
            assert_eq!(guard.verify_with(|| Err(failure), || Ok(3)), Err(failure));
            assert_eq!(guard.failure.get(), Some(failure));
        }
        let guard = guard();
        guard.attempts.set(MAX_CHECKS);
        assert_eq!(
            guard.verify_with(|| panic!("bounded before reading"), || Ok(0)),
            Err(GuardFailure::EvidenceBound)
        );
        let guard = self::guard();
        guard.elapsed.set(u64::MAX);
        assert_eq!(
            guard.verify_with(|| Ok(guard.baseline.clone()), || Ok(1)),
            Err(GuardFailure::EvidenceBound)
        );
    }

    #[test]
    fn report_is_scoped_closed_and_contains_no_raw_plan_details() {
        let guard = guard();
        let report = serde_json::to_value(guard.report()).unwrap();
        assert_eq!(
            report["method"],
            "powrprof_active_base_scheme_ac_dc_indexes_v1"
        );
        assert_eq!(report["effective_policy_observed"], false);
        assert_eq!(report["continuous_stability_established"], false);
        assert_eq!(report["snapshot_sha256"].as_str().unwrap().len(), 64);
        assert!(report.get("scheme").is_none());
        assert!(report.get("settings").is_none());
        assert!(report.get("power_plan_sha256").is_none());
        assert!(report.get("failure").is_none());
    }

    #[test]
    fn malformed_guard_summaries_cannot_be_published_as_success() {
        for mutate in [
            |report: &mut GuardReport| report.method = "unknown",
            |report: &mut GuardReport| report.snapshot_sha256 = "A".repeat(64),
            |report: &mut GuardReport| report.subgroup_count = 0,
            |report: &mut GuardReport| report.subgroup_count = MAX_GROUPS + 1,
            |report: &mut GuardReport| report.setting_count = 0,
            |report: &mut GuardReport| report.setting_count = MAX_SETTINGS + 1,
            |report: &mut GuardReport| report.checks_attempted = MAX_CHECKS + 1,
            |report: &mut GuardReport| report.checks_completed = 0,
            |report: &mut GuardReport| report.checks_completed = report.checks_attempted + 1,
            |report: &mut GuardReport| report.checks_attempted += 1,
            |report: &mut GuardReport| report.effective_policy_observed = true,
            |report: &mut GuardReport| report.continuous_stability_established = true,
            |report: &mut GuardReport| report.failure = Some(GuardFailure::StateChanged),
        ] {
            let mut report = guard().report();
            assert!(report.validate(false));
            mutate(&mut report);
            assert!(!report.validate(false));
        }
        assert!(GuardReport::fixture(1, Some(GuardFailure::StateChanged)).validate(true));
    }

    #[test]
    fn expired_native_budget_rejects_before_any_query() {
        let reader = NativeReader {
            deadline: Instant::now() - Duration::from_secs(1),
        };
        assert_eq!(reader.budget(), Err(GuardFailure::ReadBudgetExceeded));
    }

    #[test]
    fn native_active_adapter_releases_once_before_postquery_denial() {
        for status in [ERROR_SUCCESS, 5] {
            for nonnull in [false, true] {
                for release_ok in [false, true] {
                    for expired in [false, true] {
                        let mut guid = native_guid(id(1));
                        let pointer = &mut guid as *mut GUID;
                        let frees = Cell::new(0);
                        let budgets = Cell::new(0);
                        // SAFETY: fixture pointer lives through the call; the
                        // fake releaser counts without freeing stack storage.
                        let result = unsafe {
                            active_query(
                                |output| {
                                    if nonnull {
                                        *output = pointer;
                                    }
                                    status
                                },
                                |allocation| {
                                    assert_eq!(allocation, pointer);
                                    frees.set(frees.get() + 1);
                                    release_ok
                                },
                                || {
                                    budgets.set(budgets.get() + 1);
                                    if budgets.get() == 2 && expired {
                                        assert_eq!(frees.get(), u32::from(nonnull));
                                        Err(GuardFailure::ReadBudgetExceeded)
                                    } else {
                                        Ok(())
                                    }
                                },
                            )
                        };
                        assert_eq!(frees.get(), u32::from(nonnull));
                        assert_eq!(budgets.get(), 2);
                        let expected = if expired {
                            Err(GuardFailure::ReadBudgetExceeded)
                        } else if status == ERROR_SUCCESS && nonnull && release_ok {
                            Ok(id(1))
                        } else {
                            Err(GuardFailure::QueryFailed)
                        };
                        assert_eq!(result, expected);
                    }
                }
            }
        }
        // SAFETY: the expired precheck must prevent both fixture callbacks.
        assert_eq!(
            unsafe {
                active_query(
                    |_| panic!("queried after budget"),
                    |_| panic!("freed without query"),
                    || Err(GuardFailure::ReadBudgetExceeded),
                )
            },
            Err(GuardFailure::ReadBudgetExceeded)
        );
    }

    #[test]
    fn native_enumeration_adapter_rejects_wrong_size_and_unexpected_status() {
        for size in [0, 15, 16, 17, u32::MAX] {
            for status in [ERROR_SUCCESS, ERROR_NO_MORE_ITEMS, 5, 234] {
                let result = entry_query(
                    |output, bytes| {
                        assert_eq!(*bytes, 16);
                        *output = native_guid(id(1));
                        *bytes = size;
                        status
                    },
                    || Ok(()),
                );
                let expected = match status {
                    ERROR_NO_MORE_ITEMS => Ok(None),
                    ERROR_SUCCESS if size == 16 => Ok(Some(id(1))),
                    ERROR_SUCCESS => Err(GuardFailure::MalformedResult),
                    _ => Err(GuardFailure::QueryFailed),
                };
                assert_eq!(result, expected);
            }
        }
        let budgets = Cell::new(0);
        assert_eq!(
            entry_query(
                |_, _| ERROR_SUCCESS,
                || {
                    budgets.set(budgets.get() + 1);
                    if budgets.get() == 2 {
                        Err(GuardFailure::ReadBudgetExceeded)
                    } else {
                        Ok(())
                    }
                }
            ),
            Err(GuardFailure::ReadBudgetExceeded)
        );
    }

    #[test]
    fn native_index_queries_stop_after_failure_or_budget_and_preserve_zero() {
        for ac_status in [ERROR_SUCCESS, 5] {
            for dc_status in [ERROR_SUCCESS, 5] {
                for expired_at in 0..=3 {
                    let budgets = Cell::new(0);
                    let calls = Cell::new(0);
                    let result = index_queries(
                        |ac| {
                            calls.set(calls.get() + 1);
                            *ac = 0;
                            ac_status
                        },
                        |dc| {
                            calls.set(calls.get() + 1);
                            *dc = 0;
                            dc_status
                        },
                        || {
                            budgets.set(budgets.get() + 1);
                            if budgets.get() == expired_at {
                                Err(GuardFailure::ReadBudgetExceeded)
                            } else {
                                Ok(())
                            }
                        },
                    );
                    let expected_calls = if expired_at == 1 {
                        0
                    } else if ac_status != ERROR_SUCCESS || expired_at == 2 {
                        1
                    } else {
                        2
                    };
                    assert_eq!(calls.get(), expected_calls);
                    if expired_at == 1
                        || expired_at == 2
                        || (expired_at == 3 && expected_calls == 2)
                    {
                        assert_eq!(result, Err(GuardFailure::ReadBudgetExceeded));
                    } else if ac_status != ERROR_SUCCESS || dc_status != ERROR_SUCCESS {
                        assert_eq!(result, Err(GuardFailure::QueryFailed));
                    } else {
                        assert_eq!(result, Ok((0, 0)));
                    }
                }
            }
        }
    }

    #[test]
    fn full_snapshot_group_cap_includes_the_root_namespace() {
        for root_present in [false, true] {
            let mut fixture = Fixture::new();
            fixture.groups = (1..=MAX_GROUPS as u8).map(id).collect();
            if root_present {
                fixture.groups[MAX_GROUPS - 1] = NO_SUBGROUP;
            }
            let observed = snapshot(&mut fixture);
            if root_present {
                assert_eq!(observed.unwrap().groups.len(), MAX_GROUPS);
            } else {
                assert_eq!(observed, Err(GuardFailure::InventoryBound));
            }
        }
    }

    #[test]
    fn double_traversal_rejects_setting_inventory_and_index_drift() {
        struct Changing {
            fixture: Fixture,
            active_calls: u8,
            inventory: bool,
        }
        impl Reader for Changing {
            fn active(&mut self) -> ReadResult<Id> {
                self.active_calls += 1;
                if self.active_calls == 3 {
                    if self.inventory {
                        self.fixture.settings.get_mut(&id(2)).unwrap().push(id(8));
                    } else {
                        self.fixture.ac += 1;
                    }
                }
                self.fixture.active()
            }
            fn entry(
                &mut self,
                scheme: Id,
                group: Option<Id>,
                index: u32,
            ) -> ReadResult<Option<Id>> {
                self.fixture.entry(scheme, group, index)
            }
            fn indices(&mut self, scheme: Id, group: Id, setting: Id) -> ReadResult<(u32, u32)> {
                self.fixture.indices(scheme, group, setting)
            }
        }
        for inventory in [false, true] {
            assert_eq!(
                stable_snapshot(&mut Changing {
                    fixture: Fixture::new(),
                    active_calls: 0,
                    inventory
                }),
                Err(GuardFailure::StateChanged)
            );
        }
    }

    #[test]
    #[ignore = "supervised read-only native power-scheme query; no setting changes or GPU inference"]
    fn native_read_only_base_scheme_guard() {
        let guard = PowerPlanGuard::capture().expect("native base-scheme snapshot unavailable");
        guard
            .verify_unchanged()
            .expect("native base-scheme drift or query failure");
        assert_eq!(guard.report().checks_completed, 2);
        assert!(guard.report().setting_count > 0);
        println!("SCRIBE_NATIVE_BASE_POWER_SCHEME_GUARD_PASS");
    }
}
