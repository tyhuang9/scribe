use std::fs::File;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};

use super::{HistoryError, HistoryResult, MAX_HISTORY_AUDIO_FRAMES};

static AUDIO_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn initialize_root(root: &Path) -> HistoryResult<()> {
    fs::create_dir_all(root)?;
    reject_link_or_reparse(root)?;
    secure_directory(root)?;
    let audio = root.join("audio");
    fs::create_dir_all(&audio)?;
    reject_link_or_reparse(&audio)?;
    secure_directory(&audio)?;
    Ok(())
}

pub(super) fn stage_audio(
    root: &Path,
    history_id: i64,
    audio: &PreparedAudio,
) -> HistoryResult<PathBuf> {
    validate_prepared_audio(audio)?;
    let audio_dir = root.join("audio");
    reject_link_or_reparse(&audio_dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = AUDIO_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let relative = PathBuf::from("audio").join(format!("{history_id}-{stamp}-{sequence}.wav"));
    let destination = resolve_audio_path(root, &relative, false)?;
    let temporary = audio_dir.join(format!(".stage-{history_id}-{stamp}-{sequence}.tmp"));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&temporary)?;
        secure_file(&temporary)?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: PREPARED_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file, spec)
            .map_err(|error| HistoryError::Audio(error.to_string()))?;
        for sample in &audio.samples {
            let scaled = (*sample * i16::MAX as f32)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            writer
                .write_sample(scaled)
                .map_err(|error| HistoryError::Audio(error.to_string()))?;
        }
        writer
            .finalize()
            .map_err(|error| HistoryError::Audio(error.to_string()))?;
        OpenOptions::new()
            .write(true)
            .open(&temporary)?
            .sync_all()?;
        fs::rename(&temporary, &destination)?;
        sync_directory(&audio_dir)?;
        Ok::<_, HistoryError>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(relative)
}

pub(super) fn load_audio(root: &Path, relative: &Path) -> HistoryResult<PreparedAudio> {
    let path = resolve_audio_path(root, relative, true)?;
    let file = open_no_follow_regular(&path)?;
    decode_prepared_audio(file)
}

pub(super) fn decode_prepared_audio(file: File) -> HistoryResult<PreparedAudio> {
    let mut reader = hound::WavReader::new(file)
        .map_err(|error| HistoryError::Audio(format!("invalid stored WAV: {error}")))?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != PREPARED_SAMPLE_RATE
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
        || reader.len() == 0
        || usize::try_from(reader.len()).map_or(true, |len| len > MAX_HISTORY_AUDIO_FRAMES)
    {
        return Err(HistoryError::Audio(
            "stored WAV is not bounded non-empty mono 16 kHz PCM16".into(),
        ));
    }
    let frame_count = usize::try_from(reader.len())
        .map_err(|_| HistoryError::Audio("stored WAV frame count exceeds this platform".into()))?;
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(frame_count)
        .map_err(|error| HistoryError::Audio(format!("stored WAV is too large: {error}")))?;
    for sample in reader.samples::<i16>() {
        samples.push(
            sample.map_err(|error| HistoryError::Audio(format!("invalid stored WAV: {error}")))?
                as f32
                / i16::MAX as f32,
        );
    }
    if samples.len() != frame_count {
        return Err(HistoryError::Audio(
            "stored WAV frame count changed while decoding".into(),
        ));
    }
    PreparedAudio::from_captured_mono(samples, PREPARED_SAMPLE_RATE, 1, frame_count)
        .map_err(|error| HistoryError::Audio(error.to_string()))
}

pub(super) fn resolve_audio_path(
    root: &Path,
    relative: &Path,
    must_exist: bool,
) -> HistoryResult<PathBuf> {
    let mut components = relative.components();
    if components.next() != Some(Component::Normal("audio".as_ref()))
        || components
            .clone()
            .any(|component| !matches!(component, Component::Normal(_)))
        || relative.extension().and_then(|value| value.to_str()) != Some("wav")
    {
        return Err(HistoryError::UnsafePath(relative.display().to_string()));
    }
    let root = fs::canonicalize(root)?;
    reject_link_or_reparse(&root)?;
    let audio_dir = root.join("audio");
    reject_link_or_reparse(&audio_dir)?;
    let canonical_audio = fs::canonicalize(&audio_dir)?;
    if !canonical_audio.starts_with(&root) {
        return Err(HistoryError::UnsafePath(relative.display().to_string()));
    }
    let candidate = root.join(relative);
    if must_exist {
        reject_link_or_reparse(&candidate)?;
        let canonical = fs::canonicalize(&candidate)?;
        if !canonical.starts_with(&canonical_audio) || !canonical.is_file() {
            return Err(HistoryError::UnsafePath(relative.display().to_string()));
        }
        secure_file(&canonical)?;
        Ok(canonical)
    } else {
        if candidate.exists() {
            return Err(HistoryError::UnsafePath(format!(
                "refusing to replace existing file {}",
                relative.display()
            )));
        }
        Ok(candidate)
    }
}

pub(super) fn remove_audio(root: &Path, relative: &Path) -> HistoryResult<()> {
    let path = match resolve_audio_path(root, relative, true) {
        Ok(path) => path,
        Err(HistoryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    fs::remove_file(path)?;
    sync_directory(&root.join("audio"))?;
    Ok(())
}

pub(super) fn reconcile_audio_directory(
    root: &Path,
    referenced: &std::collections::HashSet<PathBuf>,
) -> HistoryResult<(usize, usize)> {
    let audio_dir = root.join("audio");
    reject_link_or_reparse(&audio_dir)?;
    let mut orphaned = 0;
    let mut temporary = 0;
    for entry in fs::read_dir(&audio_dir)? {
        let entry = entry?;
        let path = entry.path();
        reject_link_or_reparse(&path)?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".stage-") && name.ends_with(".tmp") {
            fs::remove_file(path)?;
            temporary += 1;
            continue;
        }
        let relative = PathBuf::from("audio").join(entry.file_name());
        if path.extension().and_then(|extension| extension.to_str()) == Some("wav")
            && !referenced.contains(&relative)
        {
            fs::remove_file(path)?;
            orphaned += 1;
        } else if referenced.contains(&relative) {
            secure_file(&path)?;
        }
    }
    if orphaned + temporary > 0 {
        sync_directory(&audio_dir)?;
    }
    Ok((orphaned, temporary))
}

fn validate_prepared_audio(audio: &PreparedAudio) -> HistoryResult<()> {
    if audio.sample_rate != PREPARED_SAMPLE_RATE
        || audio.samples.is_empty()
        || audio.samples.len() > MAX_HISTORY_AUDIO_FRAMES
        || audio
            .samples
            .iter()
            .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        return Err(HistoryError::Audio(
            "audio must be bounded non-empty finite mono 16 kHz prepared samples".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_regular_file_or_missing(path: &Path) -> HistoryResult<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    reject_link_or_reparse(path)?;
    if !metadata.is_file() {
        return Err(HistoryError::UnsafePath(path.display().to_string()));
    }
    Ok(true)
}

pub(super) fn secure_existing_file(path: &Path) -> HistoryResult<()> {
    if !validate_regular_file_or_missing(path)? {
        return Err(HistoryError::UnsafePath(path.display().to_string()));
    }
    secure_file(path)
}

pub(super) fn validate_database_files_before_open(database_path: &Path) -> HistoryResult<()> {
    validate_regular_file_or_missing(database_path)?;
    let file_name = database_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| HistoryError::UnsafePath(database_path.display().to_string()))?;
    for suffix in ["-wal", "-shm"] {
        validate_regular_file_or_missing(
            &database_path.with_file_name(format!("{file_name}{suffix}")),
        )?;
    }
    Ok(())
}

pub(super) fn secure_database_files(database_path: &Path) -> HistoryResult<()> {
    if validate_regular_file_or_missing(database_path)? {
        secure_file(database_path)?;
    }
    let file_name = database_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| HistoryError::UnsafePath(database_path.display().to_string()))?;
    for suffix in ["-wal", "-shm"] {
        let path = database_path.with_file_name(format!("{file_name}{suffix}"));
        if validate_regular_file_or_missing(&path)? {
            secure_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> HistoryResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(windows)]
fn secure_directory(path: &Path) -> HistoryResult<()> {
    secure_windows_path(path)
}

#[cfg(unix)]
fn secure_file(path: &Path) -> HistoryResult<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(windows)]
fn secure_file(path: &Path) -> HistoryResult<()> {
    secure_windows_path(path)
}

#[cfg(not(any(unix, windows)))]
fn secure_directory(_path: &Path) -> HistoryResult<()> {
    Err(HistoryError::UnsafePath(
        "history permission hardening is unavailable on this platform".into(),
    ))
}

#[cfg(not(any(unix, windows)))]
fn secure_file(_path: &Path) -> HistoryResult<()> {
    Err(HistoryError::UnsafePath(
        "history permission hardening is unavailable on this platform".into(),
    ))
}

#[cfg(windows)]
fn secure_windows_path(path: &Path) -> HistoryResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let user_sid = current_process_user_sid()?;
    // Protected DACL: full control for this process user and LocalSystem only.
    let sddl = format!("D:P(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;SY)")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32,
        )));
    }
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let apply_error = (applied == 0).then(|| unsafe { GetLastError() });
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(code) = apply_error {
        return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
            code as i32,
        )));
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn current_process_user_sid() -> HistoryResult<String> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = 0;
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32,
        )));
    }
    let result = (|| {
        let mut length = 0;
        let queried =
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length) };
        let query_error = unsafe { GetLastError() };
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        if length == 0 || (queried == 0 && query_error != ERROR_INSUFFICIENT_BUFFER) {
            return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
                query_error as i32,
            )));
        }
        let words = usize::try_from(length)
            .map_err(|_| HistoryError::Io(std::io::Error::other("token user data is too large")))?
            .div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0usize; words];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast::<c_void>(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32,
            )));
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } == 0 {
            return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
                unsafe { GetLastError() } as i32,
            )));
        }
        let sid_length = unsafe { (0..).find(|&index| *sid.add(index) == 0).unwrap() };
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid, sid_length) });
        unsafe {
            LocalFree(sid.cast());
        }
        Ok(sid)
    })();
    let close_error = (unsafe { CloseHandle(token) } == 0).then(|| unsafe { GetLastError() });
    match result {
        Ok(sid) => {
            if let Some(code) = close_error {
                return Err(HistoryError::Io(std::io::Error::from_raw_os_error(
                    code as i32,
                )));
            }
            Ok(sid)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn open_no_follow_regular(path: &Path) -> HistoryResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(HistoryError::UnsafePath(path.display().to_string()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(HistoryError::UnsafePath(path.display().to_string()));
        }
    }
    Ok(file)
}

#[cfg(any(unix, windows))]
fn reject_link_or_reparse(path: &Path) -> HistoryResult<()> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    if metadata.file_type().is_symlink() {
        return Err(HistoryError::UnsafePath(path.display().to_string()));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(HistoryError::UnsafePath(path.display().to_string()));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn reject_link_or_reparse(_path: &Path) -> HistoryResult<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> HistoryResult<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> HistoryResult<()> {
    Ok(())
}
