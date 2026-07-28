use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use std::cell::Cell;

static PATH_NONCE: AtomicU64 = AtomicU64::new(0);

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

pub(crate) fn create_dir_all(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return sync_existing_directory(path);
    }

    let mut missing = Vec::new();
    let mut current = path;
    while !current.exists() {
        missing.push(current.to_path_buf());
        current = current.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} has no existing ancestor", path.display()),
            )
        })?;
    }

    if !current.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a directory", current.display()),
        ));
    }
    sync_existing_directory(current)?;

    for directory in missing.iter().rev() {
        create_directory_durably(directory)?;
    }
    Ok(())
}

fn create_directory_durably(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no containing directory", path.display()),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("directory");
    let temporary = parent.join(format!(".{name}.creating-{}", path_nonce()));
    fs::create_dir(&temporary)?;
    if let Err(error) = sync_directory(&temporary) {
        let _ = remove(&temporary);
        return Err(error);
    }
    match rename(&temporary, path, false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {
            let _ = remove(&temporary);
            sync_existing_directory(path)
        }
        Err(error) => {
            let _ = remove(&temporary);
            Err(error)
        }
    }
}

#[cfg(unix)]
fn sync_existing_directory(path: &Path) -> io::Result<()> {
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(windows)]
fn sync_existing_directory(_path: &Path) -> io::Result<()> {
    // Directories created by this helper become visible only through a
    // MOVEFILE_WRITE_THROUGH rename, so a visible destination is already sealed.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_existing_directory(path: &Path) -> io::Result<()> {
    sync_directory(path)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

pub(crate) fn sync_tree(root: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", root.display()),
        ));
    }
    sync_tree_inner(root)
}

fn sync_tree_inner(directory: &Path) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            sync_tree_inner(&path)?;
        } else if metadata.is_file() {
            sync_regular_file(&path)?;
        } else if !metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is not a regular file, directory, or symlink",
                    path.display()
                ),
            ));
        }
    }
    sync_directory(directory)
}

fn sync_regular_file(path: &Path) -> io::Result<()> {
    maybe_inject_sync_tree_failure(SyncTreeFailureKind::File)?;
    sync_regular_file_platform(path)
}

#[cfg(windows)]
fn sync_regular_file_platform(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?
        .sync_all()
}

#[cfg(not(windows))]
fn sync_regular_file_platform(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

fn sync_directory(path: &Path) -> io::Result<()> {
    maybe_inject_sync_tree_failure(SyncTreeFailureKind::Directory)?;
    sync_directory_platform(path)
}

#[cfg(unix)]
fn sync_directory_platform(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory_platform(path: &Path) -> io::Result<()> {
    use std::io::Write;

    let barrier = path.join(format!(".scribe-directory-sync-{}", windows_path_nonce()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&barrier)?;
        file.write_all(b"sync")?;
        file.sync_all()?;
        drop(file);
        // The write-through rename used by remove() is the unprivileged Windows
        // directory metadata barrier after child files have been flushed.
        remove(&barrier)
    })();
    if result.is_err() {
        let _ = remove_now(&barrier);
    }
    result
}

#[cfg(not(any(unix, windows)))]
fn sync_directory_platform(path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "durable directory synchronization is not supported for {}",
            path.display()
        ),
    ))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SyncTreeFailureKind {
    File,
    Directory,
}

#[cfg(not(test))]
#[derive(Clone, Copy)]
enum SyncTreeFailureKind {
    File,
    Directory,
}

#[cfg(test)]
thread_local! {
    static SYNC_TREE_FAILURE: Cell<Option<(SyncTreeFailureKind, usize)>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(crate) struct SyncTreeFailureGuard;

#[cfg(test)]
impl Drop for SyncTreeFailureGuard {
    fn drop(&mut self) {
        SYNC_TREE_FAILURE.set(None);
    }
}

#[cfg(test)]
pub(crate) fn inject_sync_tree_failure(kind: SyncTreeFailureKind) -> SyncTreeFailureGuard {
    inject_sync_tree_failure_after(kind, 0)
}

#[cfg(test)]
fn inject_sync_tree_failure_after(
    kind: SyncTreeFailureKind,
    matching_calls_before_failure: usize,
) -> SyncTreeFailureGuard {
    SYNC_TREE_FAILURE.set(Some((kind, matching_calls_before_failure)));
    SyncTreeFailureGuard
}

#[cfg(test)]
fn maybe_inject_sync_tree_failure(kind: SyncTreeFailureKind) -> io::Result<()> {
    if let Some((expected, remaining)) = SYNC_TREE_FAILURE.get()
        && expected == kind
    {
        if remaining == 0 {
            return Err(io::Error::other(format!(
                "injected staged {} sync failure",
                match kind {
                    SyncTreeFailureKind::File => "file",
                    SyncTreeFailureKind::Directory => "directory",
                }
            )));
        }
        SYNC_TREE_FAILURE.set(Some((expected, remaining - 1)));
    }
    Ok(())
}

#[cfg(not(test))]
fn maybe_inject_sync_tree_failure(_kind: SyncTreeFailureKind) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn rename_platform(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    if !replace {
        ensure_destination_absent(destination)?;
    }
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
fn rename_platform(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
    if !replace {
        ensure_destination_absent(destination)?;
    }
    fs::rename(source, destination)
}

#[cfg(not(windows))]
fn ensure_destination_absent(destination: &Path) -> io::Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", destination.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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
    if !path.exists() {
        return Ok(());
    }
    let tombstone = path.with_file_name(format!(".scribe-removed-{}", windows_path_nonce()));
    rename_platform(path, &tombstone, false)?;
    // The write-through rename is the logical deletion barrier; reclamation can be retried later.
    let _ = remove_now(&tombstone);
    Ok(())
}

#[cfg(windows)]
fn windows_path_nonce() -> String {
    path_nonce()
}

fn path_nonce() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = PATH_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{timestamp:020}-{sequence:016x}", std::process::id())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_directory_creation_propagates_sync_failures() {
        let root = std::env::temp_dir().join(format!(
            "scribe-durable-create-dir-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("one").join("two");
        let injected = inject_sync_tree_failure(SyncTreeFailureKind::Directory);

        let error = create_dir_all(&target).unwrap_err();
        drop(injected);

        assert!(error.to_string().contains("injected staged directory"));
        assert!(!root.join("one").exists());
        assert!(!target.exists());

        create_dir_all(&target).unwrap();

        assert!(target.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn durable_directory_creation_reseals_an_existing_retry_target() {
        let root = std::env::temp_dir().join(format!(
            "scribe-durable-create-existing-retry-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("one");
        fs::create_dir(&target).unwrap();
        let injected = inject_sync_tree_failure(SyncTreeFailureKind::Directory);

        create_dir_all(&target).unwrap_err();
        drop(injected);
        assert!(target.is_dir());

        create_dir_all(&target).unwrap();

        assert!(target.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn durable_directory_creation_persists_each_new_ancestor() {
        let root = std::env::temp_dir().join(format!(
            "scribe-durable-create-dir-success-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("one").join("two");

        create_dir_all(&target).unwrap();

        assert!(target.is_dir());
        assert!(fs::read_dir(&target).unwrap().next().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_replace_rename_preserves_an_existing_destination() {
        let root =
            std::env::temp_dir().join(format!("scribe-durable-no-replace-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, b"source").unwrap();
        fs::write(&destination, b"destination").unwrap();

        let error = rename(&source, &destination, false).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&source).unwrap(), b"source");
        assert_eq!(fs::read(&destination).unwrap(), b"destination");
        let _ = fs::remove_dir_all(root);
    }
}
