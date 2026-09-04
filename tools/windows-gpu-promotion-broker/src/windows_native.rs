use std::ffi::c_void;
use std::fmt::{Display, Formatter};
use std::mem::{size_of, zeroed};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_SEM_TIMEOUT, ERROR_SUCCESS, GetLastError, HANDLE,
    HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::{
    EqualSid, GetTokenInformation, IsTokenRestricted, PSECURITY_DESCRIPTOR, PSID, RevertToSelf,
    SECURITY_ATTRIBUTES, SecurityIdentification, TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER,
    TokenGroups, TokenImpersonationLevel, TokenRestrictedSids, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, SECURITY_EFFECTIVE_ONLY, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, SYNCHRONIZE,
    WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories, SetDllDirectoryW,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId,
    ImpersonateNamedPipeClient, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_MESSAGE, PIPE_WAIT, SetNamedPipeHandleState, WaitNamedPipeW,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, OpenProcess,
    OpenProcessToken, OpenThreadToken, PROCESS_QUERY_LIMITED_INFORMATION, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject,
};

use crate::protocol::{
    MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, PIPE_ENDPOINT, SERVICE_NAME, SERVICE_SID,
    decode_request_frame, decode_response_frame, encode_request_frame, encode_response_frame,
};
use crate::{BrokerRequestV1, BrokerResponseV1, PromotionIntent};

const CONNECT_TIMEOUT_MS: u32 = 2_000;
const IO_TIMEOUT_MS: u32 = 5_000;
const ERROR_SERVICE_SPECIFIC_ERROR_CODE: u32 = 1_066;
const LOCAL_SERVICE_SID: &str = "S-1-5-19";
const AUTHENTICATED_USERS_SID: &str = "S-1-5-11";
const ANONYMOUS_SID: &str = "S-1-5-7";
const GROUP_ENABLED: u32 = 4;
const GROUP_USE_FOR_DENY_ONLY: u32 = 16;
const CLIENT_PIPE_ACCESS: u32 =
    FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;
const PIPE_SDDL: &str = concat!(
    "D:P",
    "(A;;GA;;;LS)",
    "(A;;GA;;;S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137)",
    "(A;;0x00100183;;;AU)"
);

static SERVICE_STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
static SERVICE_STATUS_HANDLE_VALUE: AtomicIsize = AtomicIsize::new(0);
static SERVICE_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientTransportError {
    Unavailable,
    Authentication,
    Protocol,
    Io,
}

impl Display for ClientTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unavailable => "protected broker service is unavailable",
            Self::Authentication => "protected broker service identity was rejected",
            Self::Protocol => "protected broker protocol was rejected",
            Self::Io => "protected broker transport failed",
        })
    }
}

impl std::error::Error for ClientTransportError {}

pub fn harden_dll_search() -> Result<()> {
    if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not restrict broker DLL search directories");
    }
    let empty = [0_u16];
    if unsafe { SetDllDirectoryW(empty.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not remove the current directory from broker DLL search");
    }
    Ok(())
}

pub fn request_promotion(
    intent: &PromotionIntent,
) -> std::result::Result<BrokerResponseV1, ClientTransportError> {
    let nonce = random_nonce().map_err(|_| ClientTransportError::Io)?;
    let request =
        BrokerRequestV1::new(intent.clone(), nonce).map_err(|_| ClientTransportError::Protocol)?;
    let request_frame =
        encode_request_frame(&request).map_err(|_| ClientTransportError::Protocol)?;
    let pipe_name = wide(PIPE_ENDPOINT);

    if unsafe { WaitNamedPipeW(pipe_name.as_ptr(), CONNECT_TIMEOUT_MS) } == 0 {
        return Err(classify_unavailable());
    }
    let pipe = unsafe {
        CreateFileW(
            pipe_name.as_ptr(),
            CLIENT_PIPE_ACCESS,
            0,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED
                | SECURITY_SQOS_PRESENT
                | SECURITY_IDENTIFICATION
                | SECURITY_EFFECTIVE_ONLY,
            null_mut(),
        )
    };
    let pipe = OwnedHandle::new(pipe).map_err(|_| classify_unavailable())?;
    protect_handle(pipe.raw()).map_err(|_| ClientTransportError::Io)?;

    let server =
        authenticate_server(pipe.raw()).map_err(|_| ClientTransportError::Authentication)?;
    let mode = PIPE_READMODE_MESSAGE;
    if unsafe { SetNamedPipeHandleState(pipe.raw(), &mode, null(), null()) } == 0 {
        return Err(ClientTransportError::Protocol);
    }

    write_message(pipe.raw(), &request_frame, None, IO_TIMEOUT_MS)
        .map_err(|_| ClientTransportError::Io)?;
    let response_frame = read_message(pipe.raw(), MAX_RESPONSE_FRAME, None, IO_TIMEOUT_MS)
        .map_err(|error| match error {
            NativeIoError::Oversized => ClientTransportError::Protocol,
            _ => ClientTransportError::Io,
        })?;
    revalidate_server(pipe.raw(), &server).map_err(|_| ClientTransportError::Authentication)?;
    decode_response_frame(&response_frame, &request).map_err(|_| ClientTransportError::Protocol)
}

pub fn run_service_dispatcher() -> Result<()> {
    let mut service_name = wide(SERVICE_NAME);
    let entries = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: service_name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(entries.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not connect the broker service to SCM");
    }
    Ok(())
}

unsafe extern "system" fn service_main(argument_count: u32, _arguments: *mut *mut u16) {
    let result = catch_unwind(AssertUnwindSafe(|| service_main_inner(argument_count)));
    if result.is_err() {
        report_stopped(1);
    }
}

fn service_main_inner(argument_count: u32) {
    let service_name = wide(SERVICE_NAME);
    let status_handle = unsafe {
        RegisterServiceCtrlHandlerExW(service_name.as_ptr(), Some(service_control_handler), null())
    };
    if status_handle.is_null() {
        return;
    }
    SERVICE_STATUS_HANDLE_VALUE.store(status_handle as isize, Ordering::Release);
    SERVICE_STOP_REQUESTED.store(false, Ordering::Release);
    report_status(SERVICE_START_PENDING, 0, 1, 5_000);

    let result = run_service(argument_count);

    report_stopped(if result.is_ok() { 0 } else { 1 });
    SERVICE_STATUS_HANDLE_VALUE.store(0, Ordering::Release);
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    let _ = catch_unwind(AssertUnwindSafe(|| match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            SERVICE_STOP_REQUESTED.store(true, Ordering::Release);
            report_status(SERVICE_STOP_PENDING, 0, 1, 5_000);
            let stop_event = SERVICE_STOP_EVENT.load(Ordering::Acquire) as HANDLE;
            if !stop_event.is_null() {
                unsafe {
                    SetEvent(stop_event);
                }
            }
        }
        SERVICE_CONTROL_INTERROGATE => {}
        _ => {}
    }));
    ERROR_SUCCESS
}

fn run_service(argument_count: u32) -> Result<()> {
    if argument_count != 1 {
        bail!("broker service arguments are outside the fixed contract");
    }
    harden_dll_search()?;
    authenticate_service_process()?;
    let stop_event = OwnedHandle::new(unsafe { CreateEventW(null(), 1, 0, null()) })?;
    SERVICE_STOP_EVENT.store(stop_event.raw() as isize, Ordering::Release);
    if SERVICE_STOP_REQUESTED.load(Ordering::Acquire) {
        unsafe {
            SetEvent(stop_event.raw());
        }
    }

    let result = (|| -> Result<()> {
        let pipe = create_server_pipe()?;
        if unsafe { WaitForSingleObject(stop_event.raw(), 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
        report_status(
            SERVICE_RUNNING,
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
            0,
            0,
        );
        serve_loop(pipe, stop_event.raw())
    })();

    SERVICE_STOP_EVENT.store(0, Ordering::Release);
    drop(stop_event);
    result
}

fn report_stopped(exit_code: u32) {
    report_status_with_exit(SERVICE_STOPPED, 0, 0, 0, exit_code);
}

fn report_status(state: u32, accepted: u32, checkpoint: u32, wait_hint: u32) {
    report_status_with_exit(state, accepted, checkpoint, wait_hint, 0);
}

fn report_status_with_exit(
    state: u32,
    accepted: u32,
    checkpoint: u32,
    wait_hint: u32,
    exit_code: u32,
) {
    let handle = SERVICE_STATUS_HANDLE_VALUE.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if handle.is_null() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: if exit_code == 0 {
            ERROR_SUCCESS
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR_CODE
        },
        dwServiceSpecificExitCode: exit_code,
        dwCheckPoint: checkpoint,
        dwWaitHint: wait_hint,
    };
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

fn serve_loop(mut pipe: OwnedHandle, stop_event: HANDLE) -> Result<()> {
    loop {
        match connect_pipe(pipe.raw(), stop_event, IO_TIMEOUT_MS) {
            Ok(()) => {}
            Err(NativeIoError::Stopped) => return Ok(()),
            Err(NativeIoError::TimedOut) => {
                drop(pipe);
                pipe = create_server_pipe()?;
                continue;
            }
            Err(_) => return Err(anyhow!("broker pipe connect failed")),
        }

        let _ = serve_connection(pipe.raw(), stop_event);
        unsafe {
            DisconnectNamedPipe(pipe.raw());
        }
        if unsafe { WaitForSingleObject(stop_event, 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
        drop(pipe);
        pipe = create_server_pipe()?;
    }
}

fn serve_connection(pipe: HANDLE, stop_event: HANDLE) -> Result<()> {
    let frame = match read_message(pipe, MAX_REQUEST_FRAME, Some(stop_event), IO_TIMEOUT_MS) {
        Ok(frame) => frame,
        Err(_) => return Ok(()),
    };

    authenticate_client(pipe)?;
    let request = match decode_request_frame(&frame) {
        Ok(request) => request,
        Err(_) => return Ok(()),
    };
    let response = BrokerResponseV1::not_provisioned(&request)?;
    let response_frame = encode_response_frame(&response, &request)?;
    let _ = write_message(pipe, &response_frame, Some(stop_event), IO_TIMEOUT_MS);
    Ok(())
}

fn create_server_pipe() -> Result<OwnedHandle> {
    let descriptor = LocalAllocation::security_descriptor(PIPE_SDDL)?;
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    let name = wide(PIPE_ENDPOINT);
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            MAX_RESPONSE_FRAME as u32,
            MAX_REQUEST_FRAME as u32,
            IO_TIMEOUT_MS,
            &security,
        )
    };
    let pipe = OwnedHandle::new(handle).context("could not create fixed broker pipe")?;
    protect_handle(pipe.raw())?;
    Ok(pipe)
}

fn authenticate_service_process() -> Result<()> {
    let process_id = unsafe { GetCurrentProcessId() };
    require_session_zero(process_id)?;
    let token = open_process_token(unsafe { GetCurrentProcess() })?;
    require_user_sid(token.raw(), LOCAL_SERVICE_SID)?;
    require_restricted_service_sid(token.raw())
}

struct AuthenticatedServer {
    process_id: u32,
    _process: OwnedHandle,
    _token: OwnedHandle,
}

fn authenticate_server(pipe: HANDLE) -> Result<AuthenticatedServer> {
    let process_id = pipe_server_process_id(pipe)?;
    require_session_zero(process_id)?;
    let process =
        OwnedHandle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) })?;
    let token = open_process_token(process.raw())?;
    require_user_sid(token.raw(), LOCAL_SERVICE_SID)?;
    require_restricted_service_sid(token.raw())?;
    if pipe_server_process_id(pipe)? != process_id {
        bail!("broker server process changed during authentication");
    }
    Ok(AuthenticatedServer {
        process_id,
        _process: process,
        _token: token,
    })
}

fn revalidate_server(pipe: HANDLE, server: &AuthenticatedServer) -> Result<()> {
    if pipe_server_process_id(pipe)? != server.process_id {
        bail!("broker server process changed during exchange");
    }
    Ok(())
}

fn pipe_server_process_id(pipe: HANDLE) -> Result<u32> {
    let mut process_id = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut process_id) } == 0 || process_id == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not identify broker pipe server");
    }
    Ok(process_id)
}

fn authenticate_client(pipe: HANDLE) -> Result<()> {
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        bail!("could not identify broker pipe client");
    }
    let impersonation = ImpersonationGuard { active: true };
    let mut token = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
        bail!("could not inspect broker pipe client");
    }
    let token = OwnedHandle::new(token)?;
    require_impersonation_level(token.raw())?;
    reject_user_sid(token.raw(), ANONYMOUS_SID)?;
    require_enabled_group_sid(token.raw(), AUTHENTICATED_USERS_SID)?;
    drop(token);
    impersonation.revert_or_abort();
    Ok(())
}

struct ImpersonationGuard {
    active: bool,
}

impl ImpersonationGuard {
    fn revert_or_abort(mut self) {
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
        self.active = false;
    }
}

impl Drop for ImpersonationGuard {
    fn drop(&mut self) {
        if self.active {
            if unsafe { RevertToSelf() } == 0 {
                std::process::abort();
            }
            self.active = false;
        }
    }
}

fn require_session_zero(process_id: u32) -> Result<()> {
    let mut session_id = u32::MAX;
    if unsafe { ProcessIdToSessionId(process_id, &mut session_id) } == 0 || session_id != 0 {
        bail!("broker service process is outside session zero");
    }
    Ok(())
}

fn open_process_token(process: HANDLE) -> Result<OwnedHandle> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error()).context("could not inspect broker token");
    }
    OwnedHandle::new(token)
}

fn require_restricted_service_sid(token: HANDLE) -> Result<()> {
    if unsafe { IsTokenRestricted(token) } == 0 {
        bail!("broker service token is not restricted");
    }
    require_enabled_group_sid(token, SERVICE_SID)?;
    require_sid_in_information(token, TokenRestrictedSids, SERVICE_SID, false)
}

fn require_user_sid(token: HANDLE, expected: &str) -> Result<()> {
    let information = token_information(token, TokenUser)?;
    let user = unsafe { &*(information.as_ptr().cast::<TOKEN_USER>()) };
    let expected = LocalAllocation::sid(expected)?;
    if unsafe { EqualSid(user.User.Sid, expected.as_sid()) } == 0 {
        bail!("broker token user does not match");
    }
    Ok(())
}

fn reject_user_sid(token: HANDLE, rejected: &str) -> Result<()> {
    let information = token_information(token, TokenUser)?;
    let user = unsafe { &*(information.as_ptr().cast::<TOKEN_USER>()) };
    let rejected = LocalAllocation::sid(rejected)?;
    if unsafe { EqualSid(user.User.Sid, rejected.as_sid()) } != 0 {
        bail!("anonymous broker client is forbidden");
    }
    Ok(())
}

fn require_enabled_group_sid(token: HANDLE, expected: &str) -> Result<()> {
    require_sid_in_information(token, TokenGroups, expected, true)
}

fn require_sid_in_information(
    token: HANDLE,
    class: i32,
    expected: &str,
    require_enabled: bool,
) -> Result<()> {
    let information = token_information(token, class)?;
    let groups = unsafe { &*(information.as_ptr().cast::<TOKEN_GROUPS>()) };
    let expected = LocalAllocation::sid(expected)?;
    let entries =
        unsafe { std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize) };
    let present = entries.iter().any(|entry| {
        (unsafe { EqualSid(entry.Sid, expected.as_sid()) }) != 0
            && (!require_enabled
                || (entry.Attributes & GROUP_ENABLED != 0
                    && entry.Attributes & GROUP_USE_FOR_DENY_ONLY == 0))
    });
    if !present {
        bail!("required broker token group is absent");
    }
    Ok(())
}

fn require_impersonation_level(token: HANDLE) -> Result<()> {
    let information = token_information(token, TokenImpersonationLevel)?;
    let level = unsafe { *information.as_ptr().cast::<i32>() };
    if level != SecurityIdentification {
        bail!("broker client impersonation level is outside the contract");
    }
    Ok(())
}

struct AlignedInformation {
    words: Vec<usize>,
}

impl AlignedInformation {
    fn as_ptr(&self) -> *const u8 {
        self.words.as_ptr().cast()
    }
}

fn token_information(token: HANDLE, class: i32) -> Result<AlignedInformation> {
    let mut length = 0;
    unsafe {
        GetTokenInformation(token, class, null_mut(), 0, &mut length);
    }
    if length == 0 {
        return Err(std::io::Error::last_os_error()).context("could not size broker token data");
    }
    let words = (length as usize).div_ceil(size_of::<usize>());
    let mut information = AlignedInformation {
        words: vec![0; words],
    };
    if unsafe {
        GetTokenInformation(
            token,
            class,
            information.words.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("could not read broker token data");
    }
    Ok(information)
}

struct LocalAllocation(*mut c_void);

impl LocalAllocation {
    fn sid(value: &str) -> Result<Self> {
        let value = wide(value);
        let mut sid: PSID = null_mut();
        if unsafe { ConvertStringSidToSidW(value.as_ptr(), &mut sid) } == 0 {
            return Err(std::io::Error::last_os_error()).context("could not construct broker SID");
        }
        Ok(Self(sid))
    }

    fn security_descriptor(value: &str) -> Result<Self> {
        let value = wide(value);
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                value.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("could not construct broker pipe security descriptor");
        }
        Ok(Self(descriptor))
    }

    fn as_sid(&self) -> PSID {
        self.0
    }

    fn as_ptr(&self) -> PSECURITY_DESCRIPTOR {
        self.0
    }
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
            self.0 = null_mut();
        }
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> Result<Self> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).context("invalid broker handle");
        }
        Ok(Self(handle))
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
            self.0 = null_mut();
        }
    }
}

fn protect_handle(handle: HANDLE) -> Result<()> {
    if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not protect broker handle inheritance");
    }
    Ok(())
}

#[derive(Debug)]
enum NativeIoError {
    Failed,
    Oversized,
    Stopped,
    TimedOut,
}

fn connect_pipe(
    pipe: HANDLE,
    stop_event: HANDLE,
    timeout_ms: u32,
) -> std::result::Result<(), NativeIoError> {
    let operation = OverlappedOperation::new().map_err(|_| NativeIoError::Failed)?;
    let connected = unsafe { ConnectNamedPipe(pipe, operation.as_mut_ptr()) };
    if connected != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    if error != ERROR_IO_PENDING {
        return Err(NativeIoError::Failed);
    }
    operation
        .wait(pipe, Some(stop_event), timeout_ms)
        .map(|_| ())
}

fn read_message(
    pipe: HANDLE,
    maximum: usize,
    stop_event: Option<HANDLE>,
    timeout_ms: u32,
) -> std::result::Result<Vec<u8>, NativeIoError> {
    let mut buffer = vec![0_u8; maximum + 1];
    let operation = OverlappedOperation::new().map_err(|_| NativeIoError::Failed)?;
    let mut immediate = 0;
    let read = unsafe {
        ReadFile(
            pipe,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut immediate,
            operation.as_mut_ptr(),
        )
    };
    let count = if read != 0 {
        immediate
    } else {
        let error = unsafe { GetLastError() };
        if error == ERROR_MORE_DATA {
            return Err(NativeIoError::Oversized);
        }
        if error != ERROR_IO_PENDING {
            return Err(NativeIoError::Failed);
        }
        operation.wait(pipe, stop_event, timeout_ms)?
    };
    if count as usize > maximum {
        return Err(NativeIoError::Oversized);
    }
    buffer.truncate(count as usize);
    Ok(buffer)
}

fn write_message(
    pipe: HANDLE,
    bytes: &[u8],
    stop_event: Option<HANDLE>,
    timeout_ms: u32,
) -> std::result::Result<(), NativeIoError> {
    let operation = OverlappedOperation::new().map_err(|_| NativeIoError::Failed)?;
    let mut immediate = 0;
    let written = unsafe {
        WriteFile(
            pipe,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut immediate,
            operation.as_mut_ptr(),
        )
    };
    let count = if written != 0 {
        immediate
    } else {
        if unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(NativeIoError::Failed);
        }
        operation.wait(pipe, stop_event, timeout_ms)?
    };
    if count as usize != bytes.len() {
        return Err(NativeIoError::Failed);
    }
    Ok(())
}

struct OverlappedOperation {
    event: OwnedHandle,
    overlapped: std::cell::UnsafeCell<OVERLAPPED>,
}

impl OverlappedOperation {
    fn new() -> Result<Self> {
        let event = OwnedHandle::new(unsafe { CreateEventW(null(), 1, 0, null()) })?;
        let mut overlapped: OVERLAPPED = unsafe { zeroed() };
        overlapped.hEvent = event.raw();
        Ok(Self {
            event,
            overlapped: std::cell::UnsafeCell::new(overlapped),
        })
    }

    fn as_mut_ptr(&self) -> *mut OVERLAPPED {
        self.overlapped.get()
    }

    fn wait(
        &self,
        pipe: HANDLE,
        stop_event: Option<HANDLE>,
        timeout_ms: u32,
    ) -> std::result::Result<u32, NativeIoError> {
        let handles = if let Some(stop_event) = stop_event {
            vec![self.event.raw(), stop_event]
        } else {
            vec![self.event.raw()]
        };
        let wait = unsafe {
            WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout_ms)
        };
        if wait == WAIT_OBJECT_0 {
            return self.completed(pipe);
        }
        let outcome = if stop_event.is_some() && wait == WAIT_OBJECT_0 + 1 {
            NativeIoError::Stopped
        } else if wait == WAIT_TIMEOUT {
            NativeIoError::TimedOut
        } else {
            NativeIoError::Failed
        };
        unsafe {
            CancelIoEx(pipe, self.as_mut_ptr());
        }
        let mut ignored = 0;
        unsafe {
            GetOverlappedResult(pipe, self.as_mut_ptr(), &mut ignored, 1);
        }
        Err(outcome)
    }

    fn completed(&self, pipe: HANDLE) -> std::result::Result<u32, NativeIoError> {
        let mut transferred = 0;
        if unsafe { GetOverlappedResult(pipe, self.as_mut_ptr(), &mut transferred, 0) } == 0 {
            return if unsafe { GetLastError() } == ERROR_MORE_DATA {
                Err(NativeIoError::Oversized)
            } else {
                Err(NativeIoError::Failed)
            };
        }
        Ok(transferred)
    }
}

fn random_nonce() -> Result<String> {
    let mut nonce = [0_u8; 32];
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            nonce.as_mut_ptr(),
            nonce.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        bail!("could not generate broker correlation nonce");
    }
    Ok(crate::encode_hex(&nonce))
}

fn classify_unavailable() -> ClientTransportError {
    match unsafe { GetLastError() } {
        ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY | ERROR_SEM_TIMEOUT => {
            ClientTransportError::Unavailable
        }
        _ => ClientTransportError::Unavailable,
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_identity_matches_service_sid_vector() {
        assert_eq!(SERVICE_NAME, "ScribeGpuPromotionBroker");
        assert_eq!(PIPE_ENDPOINT, r"\\.\pipe\ScribeGpuPromotionBroker.v1");
        assert_eq!(
            SERVICE_SID,
            "S-1-5-80-3848011089-2849881844-525567724-3342831801-3217684137"
        );
    }

    #[test]
    fn authenticated_user_pipe_rights_exclude_instance_and_acl_authority() {
        const FILE_APPEND_DATA: u32 = 4;
        const WRITE_DAC: u32 = 0x0004_0000;
        const WRITE_OWNER: u32 = 0x0008_0000;
        assert_eq!(CLIENT_PIPE_ACCESS & FILE_APPEND_DATA, 0);
        assert_eq!(CLIENT_PIPE_ACCESS & WRITE_DAC, 0);
        assert_eq!(CLIENT_PIPE_ACCESS & WRITE_OWNER, 0);
        assert!(PIPE_SDDL.contains(";;;AU)"));
        for forbidden in [";;;BA)", ";;;WD)", ";;;AN)"] {
            assert!(!PIPE_SDDL.contains(forbidden));
        }
        LocalAllocation::security_descriptor(PIPE_SDDL).unwrap();
    }

    #[test]
    fn nonce_uses_canonical_fixed_lowercase_hex() {
        let nonce = random_nonce().unwrap();
        assert_eq!(nonce.len(), 64);
        assert!(
            nonce
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[test]
    fn connection_authenticates_and_reverts_before_parsing_or_replying() {
        let source = include_str!("windows_native.rs");
        let start = source.find("fn serve_connection(").unwrap();
        let end = source[start..].find("\nfn create_server_pipe(").unwrap() + start;
        let connection = &source[start..end];
        let authenticate = connection.find("authenticate_client(pipe)?").unwrap();
        let decode = connection.find("decode_request_frame(&frame)").unwrap();
        let respond = connection
            .find("BrokerResponseV1::not_provisioned")
            .unwrap();
        assert!(authenticate < decode && decode < respond);

        let authenticate_start = source.find("fn authenticate_client(").unwrap();
        let authenticate_end = source[authenticate_start..]
            .find("\nstruct ImpersonationGuard")
            .unwrap()
            + authenticate_start;
        let client_auth = &source[authenticate_start..authenticate_end];
        assert!(client_auth.contains("impersonation.revert_or_abort();"));

        let guard_start = source.find("impl ImpersonationGuard").unwrap();
        let guard_end = source[guard_start..]
            .find("\nfn require_session_zero")
            .unwrap()
            + guard_start;
        let guard = &source[guard_start..guard_end];
        assert_eq!(guard.matches("std::process::abort();").count(), 2);
    }
}
