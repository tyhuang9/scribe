use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn rename(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    if let Some(error) = rename_with_outcome(source, destination, replace)? {
        return Err(error);
    }
    Ok(())
}

pub(crate) fn rename_with_outcome(
    source: &Path,
    destination: &Path,
    replace: bool,
) -> io::Result<Option<io::Error>> {
    rename_platform(source, destination, replace)?;
    Ok(sync_rename_parents(source, destination).err())
}

pub(crate) fn remove(path: &Path) -> io::Result<()> {
    remove_platform(path)
}

#[cfg(unix)]
fn rename_platform(source: &Path, destination: &Path, _replace: bool) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn rename_platform(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn rename_platform(source: &Path, destination: &Path, _replace: bool) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(unix)]
fn sync_rename_parents(source: &Path, destination: &Path) -> io::Result<()> {
    let source_parent = parent(source)?;
    let destination_parent = parent(destination)?;
    fs::File::open(destination_parent)?.sync_all()?;
    if source_parent != destination_parent {
        fs::File::open(source_parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_rename_parents(_source: &Path, _destination: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn remove_platform(path: &Path) -> io::Result<()> {
    if path.exists() {
        remove_now(path)?;
    }
    fs::File::open(parent(path)?)?.sync_all()
}

#[cfg(windows)]
fn remove_platform(path: &Path) -> io::Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};

    if !path.exists() {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("metadata");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tombstone = path.with_file_name(format!(
        ".{name}.removed-{}-{nonce:020}",
        std::process::id()
    ));
    rename_platform(path, &tombstone, false)?;
    // The write-through rename is the logical deletion barrier; reclamation can be retried later.
    let _ = remove_now(&tombstone);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn remove_platform(path: &Path) -> io::Result<()> {
    if path.exists() {
        remove_now(path)?;
    }
    Ok(())
}

fn remove_now(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(unix)]
fn parent(path: &Path) -> io::Result<&Path> {
    path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no containing directory", path.display()),
        )
    })
}
