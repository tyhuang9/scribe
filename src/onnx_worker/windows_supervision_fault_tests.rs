use super::*;

use std::process::{ChildStderr, ChildStdout, ExitStatus};
use std::sync::atomic::AtomicUsize;

const FIXTURE_ENV: &str = "SCRIBE_TEST_WINDOWS_SUPERVISION_FAULT";
const FIXTURE_CRASH: &str = "crash_after_hello";
const FIXTURE_HANG: &str = "hang_after_hello";
const FIXTURE_READY: &[u8] = b"\nSCRIBE_WINDOWS_SUPERVISION_FAULT_READY\n";
const FIXTURE_HELLO_SEEN: &[u8] = b"SCRIBE_WINDOWS_SUPERVISION_HELLO_SEEN\n";
const FIXTURE_TEST_NAME: &str =
    "onnx_worker::windows_supervision_fault_tests::native_startup_fault_child_fixture";
const FIXTURE_CRASH_EXIT: i32 = 73;
const FIXTURE_PROTOCOL_EXIT: i32 = 74;
const FIXTURE_CLEANUP_EXIT: u32 = 75;
const MARKER_BYTE_LIMIT: usize = 16 * 1024;
const NATIVE_WATCHDOG: Duration = Duration::from_secs(10);

#[derive(Default)]
struct BoundedSignal {
    result: Mutex<Option<std::result::Result<(), String>>>,
    changed: Condvar,
}

impl BoundedSignal {
    fn complete(&self, result: Result<()>) {
        let mut slot = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if slot.is_none() {
            *slot = Some(result.map_err(|error| format!("{error:#}")));
            self.changed.notify_all();
        }
    }

    fn wait(&self, timeout: Duration, label: &str) -> Result<()> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("{label} deadline overflowed"))?;
        let mut slot = self
            .result
            .lock()
            .map_err(|_| anyhow!("{label} signal lock was poisoned"))?;
        while slot.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("{label} was not observed within {} ms", timeout.as_millis());
            }
            let (next, timed) = self
                .changed
                .wait_timeout(slot, remaining)
                .map_err(|_| anyhow!("{label} signal lock was poisoned"))?;
            slot = next;
            if timed.timed_out() && slot.is_none() {
                bail!("{label} was not observed within {} ms", timeout.as_millis());
            }
        }
        match slot.as_ref().expect("signal result checked above") {
            Ok(()) => Ok(()),
            Err(error) => bail!("{label} failed: {error}"),
        }
    }
}

struct LaunchChild {
    child: Option<Child>,
}

impl LaunchChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("launch child is owned")
    }

    fn take(&mut self) -> Child {
        self.child.take().expect("launch child is owned")
    }

    fn terminate_and_reap(&mut self) {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        let Some(child) = self.child.as_mut() else {
            return;
        };
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.kill();
        }
        let signaled = unsafe {
            WaitForSingleObject(
                std::os::windows::io::AsRawHandle::as_raw_handle(child) as _,
                NATIVE_WATCHDOG.as_millis() as u32,
            )
        } == WAIT_OBJECT_0;
        if signaled {
            let _ = child.try_wait();
        }
    }
}

impl Drop for LaunchChild {
    fn drop(&mut self) {
        self.terminate_and_reap();
    }
}

struct LaunchObservation {
    process: Arc<OsWorkerProcess>,
    process_handle: std::os::windows::io::OwnedHandle,
}

impl LaunchObservation {
    fn wait_for_reaped_status(&self) -> ExitStatus {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;

        assert_eq!(
            unsafe {
                WaitForSingleObject(
                    std::os::windows::io::AsRawHandle::as_raw_handle(&self.process_handle) as _,
                    NATIVE_WATCHDOG.as_millis() as u32,
                )
            },
            WAIT_OBJECT_0,
            "owned native worker did not become signaled before the outer watchdog"
        );
        let deadline = Instant::now()
            .checked_add(NATIVE_WATCHDOG)
            .expect("native reap observation deadline");
        loop {
            match self.process.child.try_lock() {
                Ok(mut child) => match child.try_wait() {
                    Ok(Some(status)) => return status,
                    Ok(None) => {}
                    Err(error) => panic!("query reaped native worker: {error}"),
                },
                Err(std::sync::TryLockError::WouldBlock) => {}
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    panic!("native worker child lock was poisoned")
                }
            }
            assert!(
                Instant::now() < deadline,
                "signaled native worker was not available to Child::try_wait before the outer watchdog"
            );
            std::thread::yield_now();
        }
    }
}

impl Drop for LaunchObservation {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

        let handle = std::os::windows::io::AsRawHandle::as_raw_handle(&self.process_handle) as _;
        let _ = unsafe { TerminateProcess(handle, FIXTURE_CLEANUP_EXIT) };
        if unsafe { WaitForSingleObject(handle, NATIVE_WATCHDOG.as_millis() as u32) }
            == WAIT_OBJECT_0
            && let Ok(mut child) = self.process.child.try_lock()
        {
            let _ = child.try_wait();
        }
    }
}

struct NativeFaultLauncher {
    mode: &'static str,
    launches: AtomicUsize,
    hello_seen: Arc<BoundedSignal>,
    observation: Mutex<Option<LaunchObservation>>,
    observation_changed: Condvar,
}

impl NativeFaultLauncher {
    fn new(mode: &'static str) -> Self {
        Self {
            mode,
            launches: AtomicUsize::new(0),
            hello_seen: Arc::new(BoundedSignal::default()),
            observation: Mutex::new(None),
            observation_changed: Condvar::new(),
        }
    }

    fn wait_for_hello(&self) -> Result<()> {
        self.hello_seen
            .wait(NATIVE_WATCHDOG, "native fixture Hello")
    }

    fn take_observation(&self) -> Result<LaunchObservation> {
        let deadline = Instant::now()
            .checked_add(NATIVE_WATCHDOG)
            .ok_or_else(|| anyhow!("native observation deadline overflowed"))?;
        let mut observation = self
            .observation
            .lock()
            .map_err(|_| anyhow!("native observation lock was poisoned"))?;
        while observation.is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("native launch observation was not published before the outer watchdog");
            }
            let (next, timed) = self
                .observation_changed
                .wait_timeout(observation, remaining)
                .map_err(|_| anyhow!("native observation lock was poisoned"))?;
            observation = next;
            if timed.timed_out() && observation.is_none() {
                bail!("native launch observation was not published before the outer watchdog");
            }
        }
        Ok(observation.take().expect("observation checked above"))
    }
}

impl WorkerLauncher for NativeFaultLauncher {
    fn launch(&self) -> Result<SpawnedWorker> {
        self.launches.fetch_add(1, Ordering::AcqRel);

        let executable = std::env::current_exe()
            .context("locate native supervision fixture executable")?
            .canonicalize()
            .context("canonicalize native supervision fixture executable")?;
        let mut command = Command::new(&executable);
        configure_worker_current_directory(&mut command, &executable)?;
        configure_worker_environment(&mut command);
        command
            .args([
                "--ignored",
                "--exact",
                FIXTURE_TEST_NAME,
                "--test-threads=1",
                "--nocapture",
            ])
            .env(FIXTURE_ENV, self.mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut parent_liveness = ParentLivenessChannel::attach(&mut command)?;
        configure_hidden_worker_command(&mut command);

        let mut owned = LaunchChild::new(command.spawn().context("launch native fault fixture")?);
        parent_liveness.child_spawned();
        let process_guard =
            bind_worker_process_tree_or_terminate(owned.child_mut(), bind_worker_process_tree)?;
        let process_handle = duplicate_process_handle(owned.child_mut())?;
        let stdin = owned
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| anyhow!("native fault fixture stdin unavailable"))?;
        let stdout = owned
            .child_mut()
            .stdout
            .take()
            .ok_or_else(|| anyhow!("native fault fixture stdout unavailable"))?;
        let stderr = owned
            .child_mut()
            .stderr
            .take()
            .ok_or_else(|| anyhow!("native fault fixture stderr unavailable"))?;

        let hello_seen = Arc::clone(&self.hello_seen);
        std::thread::Builder::new()
            .name("scribe-native-fault-stderr".to_owned())
            .spawn(move || drain_fixture_stderr(stderr, hello_seen))
            .context("start native fixture stderr drain")?;

        let (ready_tx, ready_rx) = sync_channel::<Result<ChildStdout>>(1);
        std::thread::Builder::new()
            .name("scribe-native-fault-ready".to_owned())
            .spawn(move || {
                let _ = ready_tx.send(consume_bounded_marker(
                    stdout,
                    FIXTURE_READY,
                    "native fixture ready marker",
                ));
            })
            .context("start native fixture readiness reader")?;
        let stdout = match ready_rx.recv_timeout(NATIVE_WATCHDOG) {
            Ok(Ok(stdout)) => stdout,
            Ok(Err(error)) => return Err(error),
            Err(error) => {
                return Err(anyhow!(
                    "native fixture readiness exceeded the outer watchdog: {error}"
                ));
            }
        };

        let process = Arc::new(OsWorkerProcess {
            child: Mutex::new(owned.take()),
            _process_guard: process_guard,
            _parent_liveness: parent_liveness,
            #[cfg(feature = "windows-gpu-capture-observation")]
            capture_affinity: None,
        });
        *self
            .observation
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(LaunchObservation {
            process: Arc::clone(&process),
            process_handle,
        });
        self.observation_changed.notify_all();

        Ok(SpawnedWorker {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            process,
            expectation: expected_worker(WorkerRole::Inference),
            frozen_worker_approval: None,
            pack_launch: None,
        })
    }
}

fn duplicate_process_handle(child: &Child) -> Result<std::os::windows::io::OwnedHandle> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let current = unsafe { GetCurrentProcess() };
    let mut duplicate: HANDLE = std::ptr::null_mut();
    let source = std::os::windows::io::AsRawHandle::as_raw_handle(child) as HANDLE;
    if unsafe {
        DuplicateHandle(
            current,
            source,
            current,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
        || duplicate.is_null()
    {
        return Err(std::io::Error::last_os_error())
            .context("duplicate native fixture process handle");
    }
    Ok(unsafe {
        <std::os::windows::io::OwnedHandle as std::os::windows::io::FromRawHandle>::from_raw_handle(
            duplicate,
        )
    })
}

fn consume_bounded_marker<R: Read>(mut reader: R, marker: &[u8], label: &str) -> Result<R> {
    let mut observed = Vec::with_capacity(MARKER_BYTE_LIMIT.min(4096));
    while observed.len() < MARKER_BYTE_LIMIT {
        let mut byte = [0_u8; 1];
        match reader.read(&mut byte) {
            Ok(0) => bail!("{label} stream ended after {} bytes", observed.len()),
            Ok(_) => {
                observed.push(byte[0]);
                if observed.ends_with(marker) {
                    return Ok(reader);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).with_context(|| format!("read {label}")),
        }
    }
    bail!("{label} exceeded the {MARKER_BYTE_LIMIT}-byte bound")
}

fn drain_fixture_stderr(stderr: ChildStderr, hello_seen: Arc<BoundedSignal>) {
    match consume_bounded_marker(stderr, FIXTURE_HELLO_SEEN, "native fixture Hello marker") {
        Ok(mut stderr) => {
            hello_seen.complete(Ok(()));
            let mut discard = [0_u8; 1024];
            loop {
                match stderr.read(&mut discard) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        Err(error) => hello_seen.complete(Err(error)),
    }
}

fn assert_failed_startup_is_cleared(
    supervisor: &ProcessWorkerSupervisor,
    launcher: &NativeFaultLauncher,
) {
    let deadline = Instant::now()
        .checked_add(NATIVE_WATCHDOG)
        .expect("native supervisor settlement deadline");
    loop {
        let state_clear = match supervisor.inner.state.try_lock() {
            Ok(state) => {
                state.current.is_none()
                    && state.active_request.is_none()
                    && state.active_stream.is_none()
                    && state.active_model.is_none()
                    && state.retiring_generations.is_empty()
            }
            Err(std::sync::TryLockError::WouldBlock) => false,
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("process worker supervisor state lock was poisoned")
            }
        };
        let pending_clear = match supervisor.inner.pending.try_lock() {
            Ok(pending) => pending.is_empty(),
            Err(std::sync::TryLockError::WouldBlock) => false,
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("process worker pending lock was poisoned")
            }
        };
        let writer_clear = match supervisor.inner.writer.try_lock() {
            Ok(writer) => writer.is_none(),
            Err(std::sync::TryLockError::WouldBlock) => false,
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("process worker writer lock was poisoned")
            }
        };
        let launches = launcher.launches.load(Ordering::Acquire);
        assert!(
            launches <= 1,
            "failed startup launched a replacement generation"
        );
        if state_clear && pending_clear && writer_clear && launches == 1 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "failed startup did not settle before the outer watchdog (state_clear={state_clear}, pending_clear={pending_clear}, writer_clear={writer_clear}, launches={launches})"
        );
        std::thread::yield_now();
    }
}

fn unstarted_supervisor(launcher: Arc<NativeFaultLauncher>) -> ProcessWorkerSupervisor {
    ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
        launcher,
        SupervisorDeadlines::default(),
    )
}

fn ensure_before_with_outer_watchdog(
    supervisor: &ProcessWorkerSupervisor,
    launcher: &NativeFaultLauncher,
    deadline: MonotonicDeadline,
    cancelled: Option<Arc<AtomicBool>>,
) -> (Result<u64>, LaunchObservation) {
    let ensuring = supervisor.clone();
    let (result_tx, result_rx) = sync_channel(1);
    std::thread::Builder::new()
        .name("scribe-native-fault-ensure".to_owned())
        .spawn(move || {
            let result = ensuring.ensure_generation_before(Some(deadline), cancelled.as_deref());
            let _ = result_tx.send(result);
        })
        .expect("start outer-watched native startup");

    // The caller must own the concrete process and duplicated handle before it
    // waits for ensure_generation_before. If production retirement stalls, an
    // outer timeout unwinds this observation and directly terminates only that
    // exact owned child; the ensure thread is deliberately never joined here.
    let observation = match launcher.take_observation() {
        Ok(observation) => observation,
        Err(observation_error) => {
            let ensure_result = result_rx.recv_timeout(NATIVE_WATCHDOG);
            panic!(
                "native startup did not publish its owned process observation: {observation_error:#}; ensure result: {ensure_result:?}"
            );
        }
    };
    let result = result_rx
        .recv_timeout(NATIVE_WATCHDOG)
        .unwrap_or_else(|error| {
            panic!("native startup exceeded the caller outer watchdog: {error}")
        });
    (result, observation)
}

#[test]
fn native_startup_crash_retires_child() {
    let launcher = Arc::new(NativeFaultLauncher::new(FIXTURE_CRASH));
    let supervisor = unstarted_supervisor(Arc::clone(&launcher));
    let deadline =
        MonotonicDeadline::after_for(Duration::from_secs(10), "native startup crash").unwrap();

    let (result, observation) =
        ensure_before_with_outer_watchdog(&supervisor, &launcher, deadline, None);
    let error = result.expect_err("child exit before Ready must fail startup");
    launcher.wait_for_hello().unwrap();
    assert!(
        !format!("{error:#}").contains("deadline exceeded"),
        "native crash was misclassified as a deadline: {error:#}"
    );
    let status = observation.wait_for_reaped_status();
    assert_eq!(status.code(), Some(FIXTURE_CRASH_EXIT));
    assert_failed_startup_is_cleared(&supervisor, &launcher);
}

#[test]
fn native_startup_timeout_retires_child() {
    let launcher = Arc::new(NativeFaultLauncher::new(FIXTURE_HANG));
    let supervisor = unstarted_supervisor(Arc::clone(&launcher));
    let deadline =
        MonotonicDeadline::after_for(Duration::from_secs(5), "native startup timeout").unwrap();

    let (result, observation) =
        ensure_before_with_outer_watchdog(&supervisor, &launcher, deadline, None);
    let error = result.expect_err("a child that withholds Ready must exceed the startup deadline");
    launcher.wait_for_hello().unwrap();
    assert!(
        format!("{error:#}").contains("deadline exceeded"),
        "native startup timeout returned an unexpected failure: {error:#}"
    );
    let _ = observation.wait_for_reaped_status();
    assert_failed_startup_is_cleared(&supervisor, &launcher);
}

#[test]
fn native_startup_cancellation_retires_child() {
    let launcher = Arc::new(NativeFaultLauncher::new(FIXTURE_HANG));
    let supervisor = unstarted_supervisor(Arc::clone(&launcher));
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelling_launcher = Arc::clone(&launcher);
    let cancelling_flag = Arc::clone(&cancelled);
    let canceller = std::thread::Builder::new()
        .name("scribe-native-fault-canceller".to_owned())
        .spawn(move || {
            cancelling_launcher.wait_for_hello().unwrap();
            cancelling_flag.store(true, Ordering::Release);
        })
        .unwrap();
    let deadline =
        MonotonicDeadline::after_for(Duration::from_secs(30), "native startup cancellation")
            .unwrap();

    let (result, observation) = ensure_before_with_outer_watchdog(
        &supervisor,
        &launcher,
        deadline,
        Some(Arc::clone(&cancelled)),
    );
    let error =
        result.expect_err("explicit cancellation must interrupt startup before its long deadline");
    canceller.join().unwrap();
    assert!(
        Instant::now() < deadline.expires_at,
        "explicit cancellation did not beat the long Hello deadline"
    );
    assert!(
        format!("{error:#}").contains("cancelled"),
        "native startup cancellation returned an unexpected failure: {error:#}"
    );
    let _ = observation.wait_for_reaped_status();
    assert_failed_startup_is_cleared(&supervisor, &launcher);
}

#[test]
#[ignore = "launched only as an owned native supervisor fault fixture"]
fn native_startup_fault_child_fixture() {
    let Some(mode) = std::env::var_os(FIXTURE_ENV) else {
        return;
    };
    let mode = mode.to_string_lossy();
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(FIXTURE_READY)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        std::process::exit(FIXTURE_PROTOCOL_EXIT);
    }
    drop(stdout);

    let mut stdin = std::io::stdin().lock();
    let hello = read_frame(&mut stdin).and_then(parse_parent_control);
    if !matches!(hello, Ok((0, 0, Control::Hello { .. }))) {
        std::process::exit(FIXTURE_PROTOCOL_EXIT);
    }
    let mut stderr = std::io::stderr().lock();
    if stderr
        .write_all(FIXTURE_HELLO_SEEN)
        .and_then(|()| stderr.flush())
        .is_err()
    {
        std::process::exit(FIXTURE_PROTOCOL_EXIT);
    }
    drop(stderr);

    match mode.as_ref() {
        FIXTURE_CRASH => std::process::exit(FIXTURE_CRASH_EXIT),
        FIXTURE_HANG => loop {
            std::thread::park();
        },
        _ => std::process::exit(FIXTURE_PROTOCOL_EXIT),
    }
}
