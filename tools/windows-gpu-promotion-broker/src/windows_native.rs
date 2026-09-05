use std::ffi::c_void;
use std::fmt::{Display, Formatter};
use std::mem::{size_of, zeroed};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FILE_NOT_FOUND,
    ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_HANDLE, ERROR_IO_PENDING, ERROR_MORE_DATA,
    ERROR_NO_MORE_ITEMS, ERROR_NONE_MAPPED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED,
    ERROR_SEM_TIMEOUT, ERROR_SUCCESS, GetLastError, HANDLE, HANDLE_FLAG_INHERIT,
    INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetSecurityInfo, SDDL_REVISION_1, SE_REGISTRY_KEY,
};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, AddAccessAllowedAceEx, AddAce, DACL_SECURITY_INFORMATION, EqualSid,
    GetAce, GetKernelObjectSecurity, GetLengthSid, GetSecurityDescriptorControl,
    GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, INHERITED_ACE,
    InitializeAcl, InitializeSecurityDescriptor, IsTokenRestricted, IsValidAcl,
    IsValidSecurityDescriptor, IsValidSid, LookupAccountSidW, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, RevertToSelf, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SecurityIdentification, SetKernelObjectSecurity,
    SetSecurityDescriptorDacl, SidTypeUser, TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER, TokenGroups,
    TokenImpersonationLevel, TokenRestrictedSids, TokenSessionId, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES,
    FILE_READ_DATA, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    READ_CONTROL, ReadFile, SECURITY_EFFECTIVE_ONLY, SECURITY_IDENTIFICATION,
    SECURITY_SQOS_PRESENT, SYNCHRONIZE, WRITE_DAC, WriteFile,
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
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_SZ, RegCloseKey,
    RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegQueryValueExW,
};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
    SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken,
    OpenThreadToken, PROCESS_QUERY_LIMITED_INFORMATION, SetEvent, WaitForMultipleObjects,
    WaitForSingleObject,
};

use crate::protocol::{
    MAX_ACK_FRAME, MAX_REQUEST_FRAME, MAX_RESPONSE_FRAME, PIPE_ENDPOINT, SERVICE_NAME, SERVICE_SID,
    decode_ack_frame, decode_request_frame, decode_response_frame, encode_ack_frame,
    encode_request_frame, encode_response_frame,
};
use crate::{BrokerAckV1, BrokerRequestV1, BrokerResponseV1, PromotionIntent};

const CONNECT_TIMEOUT_MS: u32 = 2_000;
const IO_TIMEOUT_MS: u32 = 5_000;
const ERROR_SERVICE_SPECIFIC_ERROR_CODE: u32 = 1_066;
const LOCAL_SERVICE_SID: &str = "S-1-5-19";
const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
const AUTHORIZATION_POLICY_PATH: &str = r"SOFTWARE\Scribe\GpuPromotionBroker\v1\Authorization";
const AUTHORIZATION_SCHEMA_VALUE: &str = "SchemaVersion";
const AUTHORIZED_CLIENT_SID_VALUE: &str = "AuthorizedClientSid";
const AUTHORIZATION_SCHEMA_VERSION: u32 = 1;
const KEY_ALL_ACCESS_MASK: u32 = 0x000f_003f;
const KEY_READ_MASK: u32 = 0x0002_0019;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const ACCESS_ALLOWED_OBJECT_ACE_TYPE: u8 = 5;
const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 6;
const ACCESS_ALLOWED_CALLBACK_ACE_TYPE: u8 = 9;
const ACCESS_DENIED_CALLBACK_ACE_TYPE: u8 = 10;
const ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE: u8 = 11;
const ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE: u8 = 12;
const ACL_REVISION: u8 = 2;
const ACL_REVISION_DS: u8 = 4;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const GROUP_ENABLED: u32 = 4;
const GROUP_USE_FOR_DENY_ONLY: u32 = 16;
const MAX_KERNEL_SECURITY_DESCRIPTOR_BYTES: u32 = 65_535;
const MAX_ACCOUNT_NAME_UTF16_UNITS: u32 = 1_024;
const PROCESS_CLIENT_QUERY_ACCESS: u32 = PROCESS_QUERY_LIMITED_INFORMATION;
const TOKEN_CLIENT_QUERY_ACCESS: u32 = TOKEN_QUERY;
const SERVICE_TOKEN_MANAGEMENT_ACCESS: u32 = TOKEN_QUERY | READ_CONTROL | WRITE_DAC;
const CLIENT_PIPE_ACCESS: u32 = 0x00100183;
const _: () = assert!(
    CLIENT_PIPE_ACCESS
        == FILE_READ_DATA
            | FILE_WRITE_DATA
            | FILE_READ_ATTRIBUTES
            | FILE_WRITE_ATTRIBUTES
            | SYNCHRONIZE
);

static SERVICE_STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
static SERVICE_STATUS_HANDLE_VALUE: AtomicIsize = AtomicIsize::new(0);
static SERVICE_CONTROL_STATUS_ERROR: AtomicU32 = AtomicU32::new(ERROR_SUCCESS);

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
        return Err(classify_connection_error(unsafe { GetLastError() }));
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
    if pipe.is_null() || pipe == INVALID_HANDLE_VALUE {
        return Err(classify_connection_error(unsafe { GetLastError() }));
    }
    let pipe = OwnedHandle(pipe);
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
    let response = decode_response_frame(&response_frame, &request)
        .map_err(|_| ClientTransportError::Protocol)?;
    let ack = BrokerAckV1::for_response(&request, &response)
        .map_err(|_| ClientTransportError::Protocol)?;
    let ack_frame =
        encode_ack_frame(&ack, &request, &response).map_err(|_| ClientTransportError::Protocol)?;
    write_message(pipe.raw(), &ack_frame, None, IO_TIMEOUT_MS)
        .map_err(|_| ClientTransportError::Io)?;
    Ok(response)
}

pub fn run_service_dispatcher() -> Result<()> {
    let stop_event = OwnedHandle::new(unsafe { CreateEventW(null(), 1, 0, null()) })?;
    SERVICE_STOP_EVENT.store(stop_event.raw() as isize, Ordering::Release);
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
    let started = unsafe { StartServiceCtrlDispatcherW(entries.as_ptr()) };
    let dispatch_error = (started == 0).then(std::io::Error::last_os_error);
    SERVICE_STOP_EVENT.store(0, Ordering::Release);
    drop(stop_event);
    if let Some(error) = dispatch_error {
        Err(error).context("could not connect the broker service to SCM")
    } else {
        Ok(())
    }
}

unsafe extern "system" fn service_main(argument_count: u32, _arguments: *mut *mut u16) {
    let result = catch_unwind(AssertUnwindSafe(|| service_main_inner(argument_count)));
    if result.is_err() {
        report_stopped(1);
        SERVICE_STATUS_HANDLE_VALUE.store(0, Ordering::Release);
    }
}

fn service_main_inner(argument_count: u32) {
    SERVICE_CONTROL_STATUS_ERROR.store(ERROR_SUCCESS, Ordering::Release);
    let service_name = wide(SERVICE_NAME);
    let status_handle = unsafe {
        RegisterServiceCtrlHandlerExW(service_name.as_ptr(), Some(service_control_handler), null())
    };
    if status_handle.is_null() {
        return;
    }
    SERVICE_STATUS_HANDLE_VALUE.store(status_handle as isize, Ordering::Release);
    if report_status(SERVICE_START_PENDING, 0, 1, 5_000).is_err() {
        report_stopped(1);
        SERVICE_STATUS_HANDLE_VALUE.store(0, Ordering::Release);
        return;
    }

    let result = run_service(argument_count);
    let control_status_error = SERVICE_CONTROL_STATUS_ERROR.swap(ERROR_SUCCESS, Ordering::AcqRel);
    let exit_code = if control_status_error != ERROR_SUCCESS {
        control_status_error
    } else if result.is_ok() {
        0
    } else {
        1
    };

    report_stopped(exit_code);
    SERVICE_STATUS_HANDLE_VALUE.store(0, Ordering::Release);
}

unsafe extern "system" fn service_control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match catch_unwind(AssertUnwindSafe(|| match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            // Signaling the accepted stop is authoritative. A failed STOP_PENDING
            // update must not strand the service; preserve its error for the
            // checked terminal STOPPED update instead.
            let status_error = report_status_raw(SERVICE_STOP_PENDING, 0, 1, 5_000, 0);
            if status_error != ERROR_SUCCESS {
                let _ = SERVICE_CONTROL_STATUS_ERROR.compare_exchange(
                    ERROR_SUCCESS,
                    status_error,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            let stop_event = SERVICE_STOP_EVENT.load(Ordering::Acquire) as HANDLE;
            if stop_event.is_null() {
                std::process::abort();
            }
            if unsafe { SetEvent(stop_event) } == 0 {
                std::process::abort();
            }
            ERROR_SUCCESS
        }
        SERVICE_CONTROL_INTERROGATE => ERROR_SUCCESS,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    })) {
        Ok(result) => result,
        Err(_) => ERROR_SERVICE_SPECIFIC_ERROR_CODE,
    }
}

fn run_service(argument_count: u32) -> Result<()> {
    if argument_count != 1 {
        bail!("broker service arguments are outside the fixed contract");
    }
    harden_dll_search()?;
    let service = authenticate_service_process()?;
    let stop_event = SERVICE_STOP_EVENT.load(Ordering::Acquire) as HANDLE;
    if stop_event.is_null() {
        bail!("broker service stop event is unavailable");
    }

    let policy = load_authorization_policy()?;
    grant_authorized_client_query_access(
        service.token.raw(),
        &policy.authorized_client_sid,
        TOKEN_CLIENT_QUERY_ACCESS,
    )?;
    grant_authorized_client_query_access(
        service.process,
        &policy.authorized_client_sid,
        PROCESS_CLIENT_QUERY_ACCESS,
    )?;
    let pipe = create_server_pipe(&policy)?;
    if unsafe { WaitForSingleObject(stop_event, 0) } == WAIT_OBJECT_0 {
        return Ok(());
    }
    report_status(
        SERVICE_RUNNING,
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN,
        0,
        0,
    )?;
    serve_loop(pipe, stop_event, &policy)
}

fn report_stopped(exit_code: u32) {
    if report_status_raw(SERVICE_STOPPED, 0, 0, 0, exit_code) != ERROR_SUCCESS {
        std::process::abort();
    }
}

fn report_status(state: u32, accepted: u32, checkpoint: u32, wait_hint: u32) -> Result<()> {
    let error = report_status_raw(state, accepted, checkpoint, wait_hint, 0);
    if error == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(error as i32))
            .context("could not report broker service status")
    }
}

fn report_status_raw(
    state: u32,
    accepted: u32,
    checkpoint: u32,
    wait_hint: u32,
    exit_code: u32,
) -> u32 {
    let handle = SERVICE_STATUS_HANDLE_VALUE.load(Ordering::Acquire) as SERVICE_STATUS_HANDLE;
    if handle.is_null() {
        return ERROR_INVALID_HANDLE;
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
    if unsafe { SetServiceStatus(handle, &status) } == 0 {
        unsafe { GetLastError() }
    } else {
        ERROR_SUCCESS
    }
}

fn serve_loop(pipe: OwnedHandle, stop_event: HANDLE, policy: &AuthorizationPolicy) -> Result<()> {
    loop {
        match connect_pipe(pipe.raw(), stop_event, IO_TIMEOUT_MS) {
            Ok(()) => {}
            Err(NativeIoError::Stopped) => return Ok(()),
            Err(NativeIoError::TimedOut) => continue,
            Err(_) => return Err(anyhow!("broker pipe connect failed")),
        }

        let _ = serve_connection(pipe.raw(), stop_event, policy);
        unsafe {
            DisconnectNamedPipe(pipe.raw());
        }
        if unsafe { WaitForSingleObject(stop_event, 0) } == WAIT_OBJECT_0 {
            return Ok(());
        }
    }
}

fn serve_connection(pipe: HANDLE, stop_event: HANDLE, policy: &AuthorizationPolicy) -> Result<()> {
    let frame = match read_message(pipe, MAX_REQUEST_FRAME, Some(stop_event), IO_TIMEOUT_MS) {
        Ok(frame) => frame,
        Err(_) => return Ok(()),
    };

    authenticate_client(pipe, &policy.authorized_client_sid)?;
    let request = match decode_request_frame(&frame) {
        Ok(request) => request,
        Err(_) => return Ok(()),
    };
    let response = BrokerResponseV1::not_provisioned(&request)?;
    let response_frame = encode_response_frame(&response, &request)?;
    write_message(pipe, &response_frame, Some(stop_event), IO_TIMEOUT_MS)
        .map_err(|_| anyhow!("broker response write failed"))?;
    let ack_frame = read_message(pipe, MAX_ACK_FRAME, Some(stop_event), IO_TIMEOUT_MS)
        .map_err(|_| anyhow!("broker response acknowledgement failed"))?;
    decode_ack_frame(&ack_frame, &request, &response)?;
    Ok(())
}

fn create_server_pipe(policy: &AuthorizationPolicy) -> Result<OwnedHandle> {
    let pipe_sddl = policy.pipe_sddl();
    let descriptor = LocalAllocation::security_descriptor(&pipe_sddl)?;
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

#[derive(Debug)]
struct AuthorizationPolicy {
    authorized_client_sid: String,
}

impl AuthorizationPolicy {
    fn pipe_sddl(&self) -> String {
        format!(
            "D:P(A;;GA;;;{SERVICE_SID})(A;;0x{CLIENT_PIPE_ACCESS:08x};;;{})",
            self.authorized_client_sid
        )
    }
}

fn load_authorization_policy() -> Result<AuthorizationPolicy> {
    let path = wide(AUTHORIZATION_POLICY_PATH);
    let mut key: HKEY = null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            path.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        bail!("broker client authorization policy is unavailable");
    }
    let key = RegistryHandle::new(key)?;
    require_exact_policy_shape(key.raw())?;
    require_exact_policy_security(key.raw())?;

    let schema = query_registry_value(key.raw(), AUTHORIZATION_SCHEMA_VALUE, REG_DWORD, 4)?;
    if schema.as_slice() != AUTHORIZATION_SCHEMA_VERSION.to_le_bytes() {
        bail!("broker client authorization policy schema is noncanonical");
    }
    let sid_bytes = query_registry_value(key.raw(), AUTHORIZED_CLIENT_SID_VALUE, REG_SZ, 184)?;
    if sid_bytes.len() < 4 || sid_bytes.len() % 2 != 0 {
        bail!("broker authorized client SID value is noncanonical");
    }
    let sid_words = sid_bytes
        .chunks_exact(2)
        .map(|word| u16::from_le_bytes([word[0], word[1]]))
        .collect::<Vec<_>>();
    if sid_words.last() != Some(&0) || sid_words[..sid_words.len() - 1].contains(&0) {
        bail!("broker authorized client SID value is noncanonical");
    }
    let authorized_client_sid = String::from_utf16(&sid_words[..sid_words.len() - 1])
        .context("broker authorized client SID is not valid UTF-16")?;
    validate_authorized_client_sid(&authorized_client_sid)?;

    // Read both values again after all structural and security checks. An
    // administrator can replace policy only for a future service lifetime;
    // a racing mutation must not produce a mixed snapshot.
    if query_registry_value(key.raw(), AUTHORIZATION_SCHEMA_VALUE, REG_DWORD, 4)? != schema
        || query_registry_value(key.raw(), AUTHORIZED_CLIENT_SID_VALUE, REG_SZ, 184)? != sid_bytes
    {
        bail!("broker client authorization policy changed during startup");
    }
    Ok(AuthorizationPolicy {
        authorized_client_sid,
    })
}

fn require_exact_policy_shape(key: HKEY) -> Result<()> {
    let mut subkeys = 0;
    let mut values = 0;
    let status = unsafe {
        RegQueryInfoKeyW(
            key,
            null_mut(),
            null_mut(),
            null(),
            &mut subkeys,
            null_mut(),
            null_mut(),
            &mut values,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS || subkeys != 0 || values != 2 {
        bail!("broker client authorization policy shape is noncanonical");
    }

    // Registry lookup is case-insensitive, so querying only the canonical
    // names would accept differently-cased aliases. Enumerate the exact two
    // names first and compare their UTF-16 spelling ordinally.
    let mut actual_names = Vec::with_capacity(2);
    for index in 0..2 {
        let mut name = [0_u16; AUTHORIZED_CLIENT_SID_VALUE.len() + 1];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                name.as_mut_ptr(),
                &mut length,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS || length == 0 || length as usize >= name.len() {
            bail!("broker client authorization policy value names are noncanonical");
        }
        actual_names.push(name[..length as usize].to_vec());
    }
    let mut terminal_name = [0_u16; AUTHORIZED_CLIENT_SID_VALUE.len() + 1];
    let mut terminal_length = terminal_name.len() as u32;
    if unsafe {
        RegEnumValueW(
            key,
            2,
            terminal_name.as_mut_ptr(),
            &mut terminal_length,
            null(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    } != ERROR_NO_MORE_ITEMS
    {
        bail!("broker client authorization policy value inventory changed during enumeration");
    }
    actual_names.sort_unstable();
    let mut expected_names = [
        AUTHORIZATION_SCHEMA_VALUE
            .encode_utf16()
            .collect::<Vec<_>>(),
        AUTHORIZED_CLIENT_SID_VALUE
            .encode_utf16()
            .collect::<Vec<_>>(),
    ];
    expected_names.sort_unstable();
    if actual_names.as_slice() != expected_names.as_slice() {
        bail!("broker client authorization policy value names are noncanonical");
    }
    Ok(())
}

fn query_registry_value(
    key: HKEY,
    name: &str,
    expected_type: u32,
    maximum_bytes: u32,
) -> Result<Vec<u8>> {
    let name = wide(name);
    let mut value_type = 0;
    let mut length = 0;
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            null(),
            &mut value_type,
            null_mut(),
            &mut length,
        )
    };
    if status != ERROR_SUCCESS
        || value_type != expected_type
        || length == 0
        || length > maximum_bytes
    {
        bail!("broker client authorization policy value is noncanonical");
    }
    let mut value = vec![0_u8; length as usize];
    let mut read_length = length;
    let status = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            null(),
            &mut value_type,
            value.as_mut_ptr(),
            &mut read_length,
        )
    };
    if status != ERROR_SUCCESS || value_type != expected_type || read_length != length {
        bail!("broker client authorization policy changed while being read");
    }
    Ok(value)
}

fn require_exact_policy_security(key: HKEY) -> Result<()> {
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            key,
            SE_REGISTRY_KEY,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS || descriptor.is_null() {
        bail!("broker client authorization policy security is unreadable");
    }
    let descriptor = LocalAllocation(descriptor.cast());
    let system = LocalAllocation::sid(SYSTEM_SID)?;
    if owner.is_null() || unsafe { EqualSid(owner, system.as_sid()) } == 0 {
        bail!("broker client authorization policy owner is not SYSTEM");
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor.as_ptr(), &mut control, &mut revision) }
        == 0
        || control & SE_DACL_PROTECTED == 0
    {
        bail!("broker client authorization policy DACL is not protected");
    }
    if dacl.is_null()
        || unsafe { (*dacl).AclRevision } != ACL_REVISION
        || unsafe { (*dacl).AceCount } != 3
    {
        bail!("broker client authorization policy ACE inventory is noncanonical");
    }

    let expected = [
        (SYSTEM_SID, KEY_ALL_ACCESS_MASK),
        (ADMINISTRATORS_SID, KEY_ALL_ACCESS_MASK),
        (SERVICE_SID, KEY_READ_MASK),
    ];
    let mut matched = [false; 3];
    for index in 0..3 {
        let mut raw_ace = null_mut();
        if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 || raw_ace.is_null() {
            bail!("broker client authorization policy ACE is unreadable");
        }
        let ace = unsafe { &*(raw_ace.cast::<ACCESS_ALLOWED_ACE>()) };
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE || ace.Header.AceFlags != 0 {
            bail!("broker client authorization policy ACE type is noncanonical");
        }
        let sid: PSID = (&ace.SidStart as *const u32).cast_mut().cast();
        if unsafe { IsValidSid(sid) } == 0 {
            bail!("broker client authorization policy ACE SID is invalid");
        }
        let expected_ace_size = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>()
            + unsafe { GetLengthSid(sid) } as usize;
        if ace.Header.AceSize as usize != expected_ace_size {
            bail!("broker client authorization policy ACE size is noncanonical");
        }
        let mut found = None;
        for (position, (expected_sid, expected_mask)) in expected.iter().enumerate() {
            let expected_sid = LocalAllocation::sid(expected_sid)?;
            if unsafe { EqualSid(sid, expected_sid.as_sid()) } != 0 {
                if ace.Mask != *expected_mask || matched[position] {
                    bail!("broker client authorization policy ACE rights are noncanonical");
                }
                found = Some(position);
                break;
            }
        }
        let position = found
            .ok_or_else(|| anyhow!("broker client authorization policy contains an extra ACE"))?;
        matched[position] = true;
    }
    if matched.iter().any(|present| !present) {
        bail!("broker client authorization policy is missing a required ACE");
    }
    Ok(())
}

fn validate_authorized_client_sid(value: &str) -> Result<()> {
    let sid = LocalAllocation::sid(value)?;
    let canonical = sid.to_string()?;
    if canonical != value {
        bail!("broker authorized client SID is noncanonical");
    }
    validate_authorized_client_sid_shape(value)?;
    require_resolved_user_sid(sid.as_sid())
}

fn validate_authorized_client_sid_shape(value: &str) -> Result<()> {
    let components = value.split('-').collect::<Vec<_>>();
    let rid = components
        .get(7)
        .and_then(|component| component.parse::<u32>().ok());
    if components.len() != 8
        || components[..4] != ["S", "1", "5", "21"]
        || components[4..]
            .iter()
            .any(|component| component.parse::<u32>().is_err())
        || rid.is_none_or(|rid| rid < 1_000)
    {
        bail!("broker authorized client SID is not a dedicated account SID");
    }
    Ok(())
}

fn require_resolved_user_sid(sid: PSID) -> Result<()> {
    let mut name_length = 0;
    let mut domain_length = 0;
    let mut sid_type = 0;
    let sized = unsafe {
        LookupAccountSidW(
            null(),
            sid,
            null_mut(),
            &mut name_length,
            null_mut(),
            &mut domain_length,
            &mut sid_type,
        )
    };
    let size_error = unsafe { GetLastError() };
    if size_error == ERROR_NONE_MAPPED {
        bail!("broker authorized client SID does not resolve to an account");
    }
    if sized != 0
        || size_error != ERROR_INSUFFICIENT_BUFFER
        || name_length == 0
        || name_length > MAX_ACCOUNT_NAME_UTF16_UNITS
        || domain_length > MAX_ACCOUNT_NAME_UTF16_UNITS
    {
        bail!("broker authorized client account lookup is outside the fixed contract");
    }
    let mut name = vec![0_u16; name_length as usize];
    let mut domain = vec![0_u16; domain_length.max(1) as usize];
    let mut name_capacity = name.len() as u32;
    let mut domain_capacity = domain.len() as u32;
    if unsafe {
        LookupAccountSidW(
            null(),
            sid,
            name.as_mut_ptr(),
            &mut name_capacity,
            domain.as_mut_ptr(),
            &mut domain_capacity,
            &mut sid_type,
        )
    } == 0
    {
        bail!("broker authorized client SID could not be resolved safely");
    }
    require_user_sid_type(sid_type)
}

fn require_user_sid_type(sid_type: i32) -> Result<()> {
    if sid_type != SidTypeUser {
        bail!("broker authorized client SID does not identify a user account");
    }
    Ok(())
}

struct RegistryHandle(HKEY);

impl RegistryHandle {
    fn new(handle: HKEY) -> Result<Self> {
        if handle.is_null() {
            bail!("invalid broker authorization policy handle");
        }
        Ok(Self(handle))
    }

    fn raw(&self) -> HKEY {
        self.0
    }
}

impl Drop for RegistryHandle {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

struct AuthenticatedService {
    process: HANDLE,
    token: OwnedHandle,
}

fn authenticate_service_process() -> Result<AuthenticatedService> {
    let process = unsafe { GetCurrentProcess() };
    let token = open_process_token_with_access(process, SERVICE_TOKEN_MANAGEMENT_ACCESS)?;
    require_token_session_zero(token.raw())?;
    require_user_sid(token.raw(), LOCAL_SERVICE_SID)?;
    require_restricted_service_sid(token.raw())?;
    Ok(AuthenticatedService { process, token })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AclSnapshot {
    revision: u8,
    aces: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KernelObjectSecuritySnapshot {
    owner: Vec<u8>,
    dacl: AclSnapshot,
}

fn grant_authorized_client_query_access(
    handle: HANDLE,
    authorized_client_sid: &str,
    access_mask: u32,
) -> Result<()> {
    if access_mask != TOKEN_CLIENT_QUERY_ACCESS && access_mask != PROCESS_CLIENT_QUERY_ACCESS {
        bail!("broker object query access mask is outside the fixed contract");
    }
    let client_sid = LocalAllocation::sid(authorized_client_sid)?;
    let client_sid_bytes = client_sid.sid_bytes()?;
    let before = read_kernel_object_security(handle)?;
    if before.owner == client_sid_bytes {
        bail!("broker client must not own a service kernel object");
    }
    expected_query_acl(&before.dacl, &client_sid_bytes, access_mask)?;
    let replacement = build_query_acl(&before.dacl, client_sid.as_sid(), access_mask)?;
    require_exact_query_acl_delta(
        &before.dacl,
        &snapshot_acl(replacement.as_ptr().cast())?,
        &client_sid_bytes,
        access_mask,
    )?;
    if read_kernel_object_security(handle)? != before {
        bail!("broker kernel object security changed before query access was applied");
    }
    set_kernel_object_dacl(handle, replacement.as_ptr().cast())?;
    let after = read_kernel_object_security(handle)?;
    if after.owner != before.owner || after.owner == client_sid_bytes {
        bail!("broker kernel object query access failed exact post-write verification");
    }
    require_exact_query_acl_delta(&before.dacl, &after.dacl, &client_sid_bytes, access_mask)?;
    Ok(())
}

fn read_kernel_object_security(handle: HANDLE) -> Result<KernelObjectSecuritySnapshot> {
    let information = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut length = 0;
    let sized = unsafe { GetKernelObjectSecurity(handle, information, null_mut(), 0, &mut length) };
    let size_error = unsafe { GetLastError() };
    if sized != 0
        || size_error != ERROR_INSUFFICIENT_BUFFER
        || length < size_of::<SECURITY_DESCRIPTOR>() as u32
        || length > MAX_KERNEL_SECURITY_DESCRIPTOR_BYTES
    {
        bail!("broker kernel object security size is outside the fixed contract");
    }
    let words = (length as usize).div_ceil(size_of::<usize>());
    let mut descriptor = vec![0_usize; words];
    let mut actual_length = length;
    let descriptor_pointer = descriptor.as_mut_ptr().cast();
    if unsafe {
        GetKernelObjectSecurity(
            handle,
            information,
            descriptor_pointer,
            length,
            &mut actual_length,
        )
    } == 0
        || actual_length != length
        || unsafe { IsValidSecurityDescriptor(descriptor_pointer) } == 0
    {
        bail!("broker kernel object security is unreadable");
    }

    let mut owner: PSID = null_mut();
    let mut owner_defaulted = 0;
    if unsafe { GetSecurityDescriptorOwner(descriptor_pointer, &mut owner, &mut owner_defaulted) }
        == 0
        || owner.is_null()
    {
        bail!("broker kernel object owner is unreadable");
    }
    let owner = copy_descriptor_sid(&descriptor, length as usize, owner)?;

    let mut dacl_present = 0;
    let mut dacl_defaulted = 0;
    let mut dacl: *mut ACL = null_mut();
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor_pointer,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        )
    } == 0
        || dacl_present == 0
        || dacl.is_null()
        || !pointer_range_is_within(
            descriptor.as_ptr().cast(),
            length as usize,
            dacl.cast(),
            size_of::<ACL>(),
        )
    {
        bail!("broker kernel object DACL is absent or unreadable");
    }
    let acl_size = unsafe { (*dacl).AclSize as usize };
    if !pointer_range_is_within(
        descriptor.as_ptr().cast(),
        length as usize,
        dacl.cast(),
        acl_size,
    ) {
        bail!("broker kernel object DACL exceeds its security descriptor");
    }
    Ok(KernelObjectSecuritySnapshot {
        owner,
        dacl: snapshot_acl(dacl)?,
    })
}

fn copy_descriptor_sid(
    descriptor: &[usize],
    descriptor_length: usize,
    sid: PSID,
) -> Result<Vec<u8>> {
    if unsafe { IsValidSid(sid) } == 0 {
        bail!("broker kernel object owner SID is invalid");
    }
    let sid_length = unsafe { GetLengthSid(sid) } as usize;
    if !pointer_range_is_within(
        descriptor.as_ptr().cast(),
        descriptor_length,
        sid.cast(),
        sid_length,
    ) {
        bail!("broker kernel object owner SID exceeds its security descriptor");
    }
    Ok(unsafe { std::slice::from_raw_parts(sid.cast(), sid_length) }.to_vec())
}

fn pointer_range_is_within(
    container: *const u8,
    container_length: usize,
    member: *const u8,
    member_length: usize,
) -> bool {
    let start = container as usize;
    let end = start.checked_add(container_length);
    let member_start = member as usize;
    let member_end = member_start.checked_add(member_length);
    end.is_some_and(|end| member_start >= start && member_end.is_some_and(|value| value <= end))
}

fn snapshot_acl(acl: *const ACL) -> Result<AclSnapshot> {
    if acl.is_null() || unsafe { IsValidAcl(acl) } == 0 {
        bail!("broker kernel object DACL is invalid");
    }
    let revision = unsafe { (*acl).AclRevision };
    if revision != ACL_REVISION && revision != ACL_REVISION_DS {
        bail!("broker kernel object DACL revision is unsupported");
    }
    let acl_length = unsafe { (*acl).AclSize as usize };
    let acl_start = acl.cast::<u8>();
    let mut aces = Vec::with_capacity(unsafe { (*acl).AceCount as usize });
    for index in 0..unsafe { (*acl).AceCount as u32 } {
        let mut raw_ace = null_mut();
        if unsafe { GetAce(acl, index, &mut raw_ace) } == 0
            || raw_ace.is_null()
            || !pointer_range_is_within(acl_start, acl_length, raw_ace.cast(), 4)
        {
            bail!("broker kernel object ACE is unreadable");
        }
        let ace_size = unsafe { *(raw_ace.cast::<u8>().add(2).cast::<u16>()) } as usize;
        if ace_size < 4 || !pointer_range_is_within(acl_start, acl_length, raw_ace.cast(), ace_size)
        {
            bail!("broker kernel object ACE size is invalid");
        }
        aces.push(unsafe { std::slice::from_raw_parts(raw_ace.cast(), ace_size) }.to_vec());
    }
    Ok(AclSnapshot { revision, aces })
}

fn expected_query_acl(
    original: &AclSnapshot,
    client_sid: &[u8],
    access_mask: u32,
) -> Result<AclSnapshot> {
    if access_mask != TOKEN_CLIENT_QUERY_ACCESS && access_mask != PROCESS_CLIENT_QUERY_ACCESS {
        bail!("broker object query access mask is outside the fixed contract");
    }
    if client_sid.len() < 8
        || client_sid[0] != 1
        || 8_usize
            .checked_add(client_sid[1] as usize * 4)
            .is_none_or(|length| length != client_sid.len())
    {
        bail!("broker client SID bytes are noncanonical");
    }
    if original.revision != ACL_REVISION && original.revision != ACL_REVISION_DS {
        bail!("broker kernel object DACL revision is unsupported");
    }

    let mut inherited = false;
    let mut saw_explicit_allow = false;
    let mut insertion = original.aces.len();
    for (index, ace) in original.aces.iter().enumerate() {
        if ace.len() < 4 || ace[2..4] != (ace.len() as u16).to_le_bytes() {
            bail!("broker kernel object ACE bytes are noncanonical");
        }
        if ace
            .windows(client_sid.len())
            .any(|candidate| candidate == client_sid)
        {
            bail!("broker client already appears in a service kernel object DACL");
        }
        if ace[1] & INHERITED_ACE as u8 != 0 {
            if !inherited {
                inherited = true;
                insertion = index;
            }
            continue;
        }
        if inherited {
            bail!("broker kernel object DACL has an explicit ACE after inheritance");
        }
        match ace[0] {
            ACCESS_DENIED_ACE_TYPE
            | ACCESS_DENIED_OBJECT_ACE_TYPE
            | ACCESS_DENIED_CALLBACK_ACE_TYPE
            | ACCESS_DENIED_CALLBACK_OBJECT_ACE_TYPE => {
                if saw_explicit_allow {
                    bail!("broker kernel object DACL has a deny ACE after an allow ACE");
                }
            }
            ACCESS_ALLOWED_ACE_TYPE
            | ACCESS_ALLOWED_OBJECT_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_ACE_TYPE
            | ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE => saw_explicit_allow = true,
            _ => bail!("broker kernel object DACL has an unsupported explicit ACE type"),
        }
    }

    let ace_size = 8_usize
        .checked_add(client_sid.len())
        .ok_or_else(|| anyhow!("broker query ACE size overflowed"))?;
    let ace_size = u16::try_from(ace_size).context("broker query ACE is oversized")?;
    let mut query_ace = Vec::with_capacity(ace_size as usize);
    query_ace.push(ACCESS_ALLOWED_ACE_TYPE);
    query_ace.push(0);
    query_ace.extend_from_slice(&ace_size.to_le_bytes());
    query_ace.extend_from_slice(&access_mask.to_le_bytes());
    query_ace.extend_from_slice(client_sid);

    let mut aces = original.aces.clone();
    aces.insert(insertion, query_ace);
    Ok(AclSnapshot {
        revision: original.revision,
        aces,
    })
}

fn require_exact_query_acl_delta(
    original: &AclSnapshot,
    actual: &AclSnapshot,
    client_sid: &[u8],
    access_mask: u32,
) -> Result<()> {
    if *actual != expected_query_acl(original, client_sid, access_mask)? {
        bail!("broker query ACL changed the expected ACE inventory");
    }
    Ok(())
}

fn build_query_acl(
    original: &AclSnapshot,
    client_sid: PSID,
    access_mask: u32,
) -> Result<Vec<usize>> {
    let client_sid_length = unsafe { GetLengthSid(client_sid) } as usize;
    let query_ace_length = 8_usize
        .checked_add(client_sid_length)
        .ok_or_else(|| anyhow!("broker query ACE size overflowed"))?;
    let acl_length = original
        .aces
        .iter()
        .try_fold(size_of::<ACL>() + query_ace_length, |length, ace| {
            length.checked_add(ace.len())
        })
        .ok_or_else(|| anyhow!("broker query ACL size overflowed"))?;
    let acl_length = u32::try_from(acl_length).context("broker query ACL is oversized")?;
    if acl_length > u16::MAX as u32 {
        bail!("broker query ACL exceeds the Windows ACL size limit");
    }
    let mut allocation = vec![0_usize; (acl_length as usize).div_ceil(size_of::<usize>())];
    let acl = allocation.as_mut_ptr().cast::<ACL>();
    if unsafe { InitializeAcl(acl, acl_length, original.revision as u32) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not initialize broker query ACL");
    }
    let insertion = original
        .aces
        .iter()
        .position(|ace| ace[1] & INHERITED_ACE as u8 != 0)
        .unwrap_or(original.aces.len());
    for index in 0..=original.aces.len() {
        if index == insertion
            && unsafe {
                AddAccessAllowedAceEx(acl, original.revision as u32, 0, access_mask, client_sid)
            } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("could not add exact broker query ACE");
        }
        if let Some(ace) = original.aces.get(index)
            && unsafe {
                AddAce(
                    acl,
                    original.revision as u32,
                    u32::MAX,
                    ace.as_ptr().cast(),
                    ace.len() as u32,
                )
            } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("could not preserve broker kernel object ACE");
        }
    }
    Ok(allocation)
}

fn set_kernel_object_dacl(handle: HANDLE, dacl: *const ACL) -> Result<()> {
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        bail!("refusing to apply an absent or invalid broker DACL");
    }
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { zeroed() };
    let descriptor_pointer = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
    if unsafe { InitializeSecurityDescriptor(descriptor_pointer, SECURITY_DESCRIPTOR_REVISION) }
        == 0
        || unsafe { SetSecurityDescriptorDacl(descriptor_pointer, 1, dacl, 0) } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("could not construct broker DACL update");
    }
    if unsafe { SetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION, descriptor_pointer) }
        == 0
    {
        return Err(std::io::Error::last_os_error()).context("could not apply broker DACL update");
    }
    Ok(())
}

struct AuthenticatedServer {
    process_id: u32,
    _process: OwnedHandle,
    _token: OwnedHandle,
}

fn authenticate_server(pipe: HANDLE) -> Result<AuthenticatedServer> {
    let process_id = pipe_server_process_id(pipe)?;
    let process =
        OwnedHandle::new(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) })?;
    let token = open_process_token(process.raw())?;
    require_token_session_zero(token.raw())?;
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

fn authenticate_client(pipe: HANDLE, authorized_client_sid: &str) -> Result<()> {
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
    require_user_sid(token.raw(), authorized_client_sid)?;
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

fn require_token_session_zero(token: HANDLE) -> Result<()> {
    let mut session_id = u32::MAX;
    let mut length = size_of::<u32>() as u32;
    if unsafe {
        GetTokenInformation(
            token,
            TokenSessionId,
            (&mut session_id as *mut u32).cast(),
            length,
            &mut length,
        )
    } == 0
        || length != size_of::<u32>() as u32
        || session_id != 0
    {
        bail!("broker service process is outside session zero");
    }
    Ok(())
}

fn open_process_token(process: HANDLE) -> Result<OwnedHandle> {
    open_process_token_with_access(process, TOKEN_QUERY)
}

fn open_process_token_with_access(process: HANDLE, access: u32) -> Result<OwnedHandle> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, access, &mut token) } == 0 {
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

    fn to_string(&self) -> Result<String> {
        let mut value = null_mut();
        if unsafe { ConvertSidToStringSidW(self.as_sid(), &mut value) } == 0 || value.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("could not canonicalize broker SID");
        }
        let value = Self(value.cast());
        let pointer = value.0.cast::<u16>();
        let mut length = 0;
        while unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        String::from_utf16(unsafe { std::slice::from_raw_parts(pointer, length) })
            .context("canonical broker SID is not valid UTF-16")
    }

    fn as_sid(&self) -> PSID {
        self.0
    }

    fn sid_bytes(&self) -> Result<Vec<u8>> {
        if unsafe { IsValidSid(self.as_sid()) } == 0 {
            bail!("broker SID allocation is invalid");
        }
        let length = unsafe { GetLengthSid(self.as_sid()) } as usize;
        Ok(unsafe { std::slice::from_raw_parts(self.as_sid().cast(), length) }.to_vec())
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

fn classify_connection_error(error: u32) -> ClientTransportError {
    match error {
        ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY | ERROR_SEM_TIMEOUT => {
            ClientTransportError::Unavailable
        }
        ERROR_ACCESS_DENIED => ClientTransportError::Authentication,
        _ => ClientTransportError::Io,
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
    fn dedicated_client_pipe_rights_exclude_instance_and_acl_authority() {
        const FILE_APPEND_DATA: u32 = 4;
        const WRITE_DAC: u32 = 0x0004_0000;
        const WRITE_OWNER: u32 = 0x0008_0000;
        let policy = AuthorizationPolicy {
            authorized_client_sid: "S-1-5-21-1-2-3-1000".to_owned(),
        };
        let pipe_sddl = policy.pipe_sddl();
        assert_eq!(CLIENT_PIPE_ACCESS & FILE_APPEND_DATA, 0);
        assert_eq!(CLIENT_PIPE_ACCESS & WRITE_DAC, 0);
        assert_eq!(CLIENT_PIPE_ACCESS & WRITE_OWNER, 0);
        assert!(pipe_sddl.contains("0x00100183;;;S-1-5-21-1-2-3-1000)"));
        assert!(pipe_sddl.contains(&format!("GA;;;{SERVICE_SID})")));
        for forbidden in [";;;AU)", ";;;BU)", ";;;BA)", ";;;WD)", ";;;AN)"] {
            assert!(!pipe_sddl.contains(forbidden));
        }
        LocalAllocation::security_descriptor(&pipe_sddl).unwrap();
    }

    #[test]
    fn authorization_policy_shape_accepts_only_canonical_dedicated_account_sids() {
        validate_authorized_client_sid_shape("S-1-5-21-1-2-3-1000").unwrap();
        for forbidden in [
            "S-1-1-0",
            "S-1-5-7",
            "S-1-5-11",
            "S-1-5-18",
            "S-1-5-19",
            "S-1-5-20",
            "S-1-5-32-544",
            "S-1-5-32-545",
            SERVICE_SID,
            "S-1-5-21-1-2-3-500",
            "s-1-5-21-1-2-3-1000",
        ] {
            assert!(
                validate_authorized_client_sid_shape(forbidden).is_err(),
                "accepted dangerous or noncanonical SID {forbidden}"
            );
        }
    }

    #[test]
    fn authorization_policy_native_lookup_accepts_user_and_rejects_group() {
        let token = open_process_token(unsafe { GetCurrentProcess() }).unwrap();
        let information = token_information(token.raw(), TokenUser).unwrap();
        let user = unsafe { &*(information.as_ptr().cast::<TOKEN_USER>()) };
        require_resolved_user_sid(user.User.Sid).unwrap();

        let group = LocalAllocation::sid(ADMINISTRATORS_SID).unwrap();
        assert!(require_resolved_user_sid(group.as_sid()).is_err());
    }

    #[test]
    fn service_snapshots_policy_before_first_pipe_creation() {
        let source = include_str!("windows_native.rs");
        let start = source.find("fn run_service(argument_count").unwrap();
        let end = source[start..].find("\nfn report_stopped").unwrap() + start;
        let service = &source[start..end];
        let authenticate = service.find("authenticate_service_process()?").unwrap();
        let load = service.find("load_authorization_policy()?").unwrap();
        let token_grant = service.find("TOKEN_CLIENT_QUERY_ACCESS").unwrap();
        let process_grant = service.find("PROCESS_CLIENT_QUERY_ACCESS").unwrap();
        let pipe = service.find("create_server_pipe(&policy)?").unwrap();
        let serve = service
            .find("serve_loop(pipe, stop_event, &policy)")
            .unwrap();
        assert!(
            authenticate < load
                && load < token_grant
                && token_grant < process_grant
                && process_grant < pipe
                && pipe < serve
        );
    }

    fn test_ace(ace_type: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
        let size = u16::try_from(8 + sid.len()).unwrap();
        let mut ace = vec![ace_type, flags];
        ace.extend_from_slice(&size.to_le_bytes());
        ace.extend_from_slice(&mask.to_le_bytes());
        ace.extend_from_slice(sid);
        ace
    }

    #[test]
    fn query_acl_delta_preserves_existing_aces_and_inserts_before_inheritance() {
        let client_sid = LocalAllocation::sid("S-1-5-21-1-2-3-1000").unwrap();
        let client = client_sid.sid_bytes().unwrap();
        let system = LocalAllocation::sid(SYSTEM_SID)
            .unwrap()
            .sid_bytes()
            .unwrap();
        let service = LocalAllocation::sid(SERVICE_SID)
            .unwrap()
            .sid_bytes()
            .unwrap();
        let administrators = LocalAllocation::sid(ADMINISTRATORS_SID)
            .unwrap()
            .sid_bytes()
            .unwrap();
        let original = AclSnapshot {
            revision: ACL_REVISION,
            aces: vec![
                test_ace(ACCESS_DENIED_ACE_TYPE, 0, 0x20, &system),
                test_ace(ACCESS_ALLOWED_ACE_TYPE, 0, 0x100, &service),
                test_ace(
                    ACCESS_ALLOWED_ACE_TYPE,
                    INHERITED_ACE as u8,
                    0x200,
                    &administrators,
                ),
            ],
        };
        let actual = expected_query_acl(&original, &client, PROCESS_CLIENT_QUERY_ACCESS).unwrap();
        assert_eq!(actual.revision, original.revision);
        assert_eq!(actual.aces.len(), original.aces.len() + 1);
        assert_eq!(actual.aces[0], original.aces[0]);
        assert_eq!(actual.aces[1], original.aces[1]);
        assert_eq!(actual.aces[3], original.aces[2]);
        assert_eq!(actual.aces[2][0], ACCESS_ALLOWED_ACE_TYPE);
        assert_eq!(actual.aces[2][1], 0);
        assert_eq!(
            u32::from_le_bytes(actual.aces[2][4..8].try_into().unwrap()),
            0x1000
        );
        assert_eq!(&actual.aces[2][8..], client.as_slice());
        require_exact_query_acl_delta(&original, &actual, &client, PROCESS_CLIENT_QUERY_ACCESS)
            .unwrap();
        let built =
            build_query_acl(&original, client_sid.as_sid(), PROCESS_CLIENT_QUERY_ACCESS).unwrap();
        assert_eq!(snapshot_acl(built.as_ptr().cast()).unwrap(), actual);
    }

    #[test]
    fn query_acl_delta_rejects_preexisting_or_noncanonical_authority() {
        let client = LocalAllocation::sid("S-1-5-21-1-2-3-1000")
            .unwrap()
            .sid_bytes()
            .unwrap();
        let system = LocalAllocation::sid(SYSTEM_SID)
            .unwrap()
            .sid_bytes()
            .unwrap();
        let client_ace = test_ace(ACCESS_ALLOWED_OBJECT_ACE_TYPE, 0, 0x1000, &client);
        assert!(
            expected_query_acl(
                &AclSnapshot {
                    revision: ACL_REVISION_DS,
                    aces: vec![client_ace],
                },
                &client,
                PROCESS_CLIENT_QUERY_ACCESS,
            )
            .is_err()
        );

        let allow = test_ace(ACCESS_ALLOWED_ACE_TYPE, 0, 1, &system);
        let deny = test_ace(ACCESS_DENIED_ACE_TYPE, 0, 2, &system);
        let inherited = test_ace(ACCESS_ALLOWED_ACE_TYPE, INHERITED_ACE as u8, 3, &system);
        for invalid in [
            vec![allow.clone(), deny],
            vec![inherited, allow.clone()],
            vec![test_ace(2, 0, 1, &system)],
            vec![vec![0, 0, 3, 0]],
        ] {
            assert!(
                expected_query_acl(
                    &AclSnapshot {
                        revision: ACL_REVISION,
                        aces: invalid,
                    },
                    &client,
                    TOKEN_CLIENT_QUERY_ACCESS,
                )
                .is_err()
            );
        }
        assert!(
            expected_query_acl(
                &AclSnapshot {
                    revision: 3,
                    aces: vec![allow],
                },
                &client,
                TOKEN_CLIENT_QUERY_ACCESS,
            )
            .is_err()
        );
    }

    #[test]
    fn query_acl_delta_rejects_mutation_reordering_and_extra_authority() {
        let client = LocalAllocation::sid("S-1-5-21-1-2-3-1000")
            .unwrap()
            .sid_bytes()
            .unwrap();
        let system = LocalAllocation::sid(SYSTEM_SID)
            .unwrap()
            .sid_bytes()
            .unwrap();
        let original = AclSnapshot {
            revision: ACL_REVISION,
            aces: vec![
                test_ace(ACCESS_DENIED_ACE_TYPE, 0, 1, &system),
                test_ace(ACCESS_ALLOWED_ACE_TYPE, 0, 2, &system),
            ],
        };
        let expected = expected_query_acl(&original, &client, TOKEN_CLIENT_QUERY_ACCESS).unwrap();
        let query_index = 2;
        let mut candidates = Vec::new();
        let mut wrong_type = expected.clone();
        wrong_type.aces[query_index][0] = ACCESS_DENIED_ACE_TYPE;
        candidates.push(wrong_type);
        let mut wrong_flags = expected.clone();
        wrong_flags.aces[query_index][1] = INHERITED_ACE as u8;
        candidates.push(wrong_flags);
        let mut wrong_mask = expected.clone();
        wrong_mask.aces[query_index][4] ^= 1;
        candidates.push(wrong_mask);
        let mut wrong_sid = expected.clone();
        *wrong_sid.aces[query_index].last_mut().unwrap() ^= 1;
        candidates.push(wrong_sid);
        let mut missing = expected.clone();
        missing.aces.remove(query_index);
        candidates.push(missing);
        let mut extra = expected.clone();
        extra.aces.push(expected.aces[query_index].clone());
        candidates.push(extra);
        let mut reordered = expected.clone();
        reordered.aces.swap(0, 1);
        candidates.push(reordered);
        let mut mutated_existing = expected.clone();
        mutated_existing.aces[0][4] ^= 1;
        candidates.push(mutated_existing);
        for candidate in candidates {
            assert!(
                require_exact_query_acl_delta(
                    &original,
                    &candidate,
                    &client,
                    TOKEN_CLIENT_QUERY_ACCESS,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn client_query_masks_exclude_object_control_and_impersonation_rights() {
        for forbidden in [
            0x0000_0001,
            0x0000_0010,
            0x0000_0040,
            0x0000_0400,
            0x0001_0000,
            READ_CONTROL,
            WRITE_DAC,
            0x0008_0000,
            SYNCHRONIZE,
        ] {
            assert_eq!(PROCESS_CLIENT_QUERY_ACCESS & forbidden, 0);
        }
        for forbidden in [
            0x0000_0001,
            0x0000_0002,
            0x0000_0004,
            0x0000_0010,
            0x0000_0020,
            0x0000_0040,
            0x0000_0080,
            0x0000_0100,
            0x0001_0000,
            READ_CONTROL,
            WRITE_DAC,
            0x0008_0000,
        ] {
            assert_eq!(TOKEN_CLIENT_QUERY_ACCESS & forbidden, 0);
        }
        assert_eq!(PROCESS_CLIENT_QUERY_ACCESS, 0x0000_1000);
        assert_eq!(TOKEN_CLIENT_QUERY_ACCESS, 0x0000_0008);
    }

    #[test]
    fn server_authentication_uses_retained_token_for_session_zero() {
        let source = include_str!("windows_native.rs");
        let start = source.find("fn authenticate_server(").unwrap();
        let end = source[start..].find("\nfn revalidate_server(").unwrap() + start;
        let authentication = &source[start..end];
        let process = authentication.find("OpenProcess(").unwrap();
        let token = authentication
            .find("open_process_token(process.raw())?")
            .unwrap();
        let session = authentication
            .find("require_token_session_zero(token.raw())?")
            .unwrap();
        let user = authentication
            .find("require_user_sid(token.raw(), LOCAL_SERVICE_SID)?")
            .unwrap();
        let restricted = authentication
            .find("require_restricted_service_sid(token.raw())?")
            .unwrap();
        assert!(process < token && token < session && session < user && user < restricted);
        assert!(!authentication.contains("ProcessIdToSessionId"));

        let production = source.split_once("#[cfg(test)]").unwrap().0;
        for forbidden in [
            "ProcessIdToSessionId",
            "SetTokenInformation",
            "TokenDefaultDacl",
            "AdjustTokenPrivileges",
        ] {
            assert!(!production.contains(forbidden));
        }
    }

    #[test]
    fn absent_or_invalid_replacement_dacl_is_rejected_before_write() {
        assert!(set_kernel_object_dacl(null_mut(), null()).is_err());
        let invalid = [0_usize; 1];
        assert!(snapshot_acl(invalid.as_ptr().cast()).is_err());
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
    fn connection_errors_distinguish_absence_identity_and_io_failures() {
        for error in [ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT] {
            assert_eq!(
                classify_connection_error(error),
                ClientTransportError::Unavailable
            );
        }
        assert_eq!(
            classify_connection_error(ERROR_ACCESS_DENIED),
            ClientTransportError::Authentication
        );
        assert_eq!(
            classify_connection_error(u32::MAX),
            ClientTransportError::Io
        );
    }

    #[test]
    fn dispatcher_owns_stop_event_for_the_complete_callback_lifetime() {
        let source = include_str!("windows_native.rs");
        let start = source.find("pub fn run_service_dispatcher(").unwrap();
        let end = source[start..]
            .find("\nunsafe extern \"system\" fn service_main")
            .unwrap()
            + start;
        let dispatcher = &source[start..end];
        let create = dispatcher.find("CreateEventW").unwrap();
        let dispatch = dispatcher.find("StartServiceCtrlDispatcherW").unwrap();
        let clear = dispatcher.rfind("SERVICE_STOP_EVENT.store(0").unwrap();
        let close = dispatcher.rfind("drop(stop_event)").unwrap();
        assert!(create < dispatch && dispatch < clear && clear < close);

        let service_start = source.find("fn run_service(argument_count").unwrap();
        let service_end =
            source[service_start..].find("\nfn report_stopped").unwrap() + service_start;
        assert!(!source[service_start..service_end].contains("CreateEventW"));
    }

    #[test]
    fn first_pipe_handle_is_retained_across_clients_and_timeouts() {
        let source = include_str!("windows_native.rs");
        let start = source.find("fn serve_loop(").unwrap();
        let end = source[start..].find("\nfn serve_connection(").unwrap() + start;
        let serve_loop = &source[start..end];
        assert!(!serve_loop.contains("create_server_pipe"));
        assert!(!serve_loop.contains("drop(pipe)"));
        assert!(serve_loop.contains("Err(NativeIoError::TimedOut) => continue"));
    }

    #[test]
    fn connection_authenticates_and_reverts_before_parsing_or_replying() {
        let source = include_str!("windows_native.rs");
        let start = source.find("fn serve_connection(").unwrap();
        let end = source[start..].find("\nfn create_server_pipe(").unwrap() + start;
        let connection = &source[start..end];
        let authenticate = connection
            .find("authenticate_client(pipe, &policy.authorized_client_sid)?")
            .unwrap();
        let decode = connection.find("decode_request_frame(&frame)").unwrap();
        let respond = connection
            .find("BrokerResponseV1::not_provisioned")
            .unwrap();
        let write_response = connection
            .find("write_message(pipe, &response_frame")
            .unwrap();
        let read_ack = connection.find("read_message(pipe, MAX_ACK_FRAME").unwrap();
        let validate_ack = connection.find("decode_ack_frame").unwrap();
        assert!(
            authenticate < decode
                && decode < respond
                && respond < write_response
                && write_response < read_ack
                && read_ack < validate_ack
        );

        let authenticate_start = source.find("fn authenticate_client(").unwrap();
        let authenticate_end = source[authenticate_start..]
            .find("\nstruct ImpersonationGuard")
            .unwrap()
            + authenticate_start;
        let client_auth = &source[authenticate_start..authenticate_end];
        assert!(client_auth.contains("impersonation.revert_or_abort();"));

        let guard_start = source.find("impl ImpersonationGuard").unwrap();
        let guard_end = source[guard_start..]
            .find("\nfn require_token_session_zero")
            .unwrap()
            + guard_start;
        let guard = &source[guard_start..guard_end];
        assert_eq!(guard.matches("std::process::abort();").count(), 2);
    }

    #[test]
    fn client_acknowledges_only_after_validating_the_correlated_response() {
        let source = include_str!("windows_native.rs");
        let start = source.find("pub fn request_promotion(").unwrap();
        let end = source[start..]
            .find("\npub fn run_service_dispatcher(")
            .unwrap()
            + start;
        let client = &source[start..end];
        let decode = client.find("decode_response_frame").unwrap();
        let construct_ack = client.find("BrokerAckV1::for_response").unwrap();
        let write_ack = client.find("write_message(pipe.raw(), &ack_frame").unwrap();
        let authenticate_response = client.find("revalidate_server").unwrap();
        assert_eq!(client.matches("revalidate_server").count(), 1);
        assert!(authenticate_response < decode);
        assert!(decode < construct_ack && construct_ack < write_ack);
    }

    #[test]
    fn service_controls_signal_or_terminate_and_preserve_status_failures() {
        let source = include_str!("windows_native.rs");
        let start = source
            .find("unsafe extern \"system\" fn service_control_handler")
            .unwrap();
        let end = source[start..].find("\nfn run_service(").unwrap() + start;
        let handler = &source[start..end];
        assert!(handler.contains("report_status_raw"));
        assert!(handler.contains("if unsafe { SetEvent(stop_event) } == 0"));
        assert!(handler.contains("SERVICE_CONTROL_STATUS_ERROR.compare_exchange"));
        assert_eq!(handler.matches("std::process::abort();").count(), 2);
        assert!(handler.contains("ERROR_CALL_NOT_IMPLEMENTED"));
    }

    #[test]
    fn accepted_stop_is_signaled_even_when_stop_pending_cannot_be_reported() {
        let source = include_str!("windows_native.rs");
        let start = source
            .find("unsafe extern \"system\" fn service_control_handler")
            .unwrap();
        let end = source[start..].find("\nfn run_service(").unwrap() + start;
        let handler = &source[start..end];
        let report_pending = handler
            .find("report_status_raw(SERVICE_STOP_PENDING")
            .unwrap();
        let signal = handler.find("SetEvent(stop_event)").unwrap();
        assert!(report_pending < signal);
        assert!(handler.contains("ERROR_SUCCESS"));

        let report_start = source.find("fn report_stopped(").unwrap();
        let report_end = source[report_start..].find("\nfn report_status(").unwrap() + report_start;
        let report_stopped = &source[report_start..report_end];
        assert!(report_stopped.contains("std::process::abort();"));

        let service_main_start = source.find("fn service_main_inner(").unwrap();
        let service_main_end = source[service_main_start..]
            .find("\nunsafe extern \"system\" fn service_control_handler")
            .unwrap()
            + service_main_start;
        let service_main = &source[service_main_start..service_main_end];
        assert!(service_main.contains("SERVICE_CONTROL_STATUS_ERROR.swap"));
        assert!(service_main.contains("report_stopped(exit_code)"));
    }
}
