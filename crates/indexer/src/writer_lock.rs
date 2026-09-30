use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Exclusive writer ownership for one resolved index destination.
///
/// The adjacent lock file stays on disk after release: unlinking it could let
/// another process lock a different inode while an existing owner still runs.
pub struct IndexWriterGuard {
    lock_file: File,
    index_path: PathBuf,
    lock_path: PathBuf,
}

impl IndexWriterGuard {
    /// Acquires the writer lock without waiting.
    ///
    /// # Errors
    /// Returns `WouldBlock` when another owner holds the lock, or a filesystem
    /// error for inaccessible destinations or nonregular lock files.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let index_path = resolve_index_path(path, true)?;
        let lock_path = adjacent_lock_path(&index_path);
        match fs::symlink_metadata(&lock_path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "index lock must be a regular file",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&lock_path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "index lock must be a regular file",
            ));
        }
        match file.try_lock() {
            Ok(()) => Ok(Self {
                lock_file: file,
                index_path,
                lock_path,
            }),
            Err(TryLockError::WouldBlock) => {
                Err(io::Error::new(io::ErrorKind::WouldBlock, "index is busy"))
            }
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    #[must_use]
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    #[must_use]
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

impl Drop for IndexWriterGuard {
    fn drop(&mut self) {
        // A concurrent fork can retain this open file description until exec.
        let _ = self.lock_file.unlock();
    }
}

pub(crate) fn adjacent_lock_path(index_path: &Path) -> PathBuf {
    let mut name = index_path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    index_path.with_file_name(name)
}

pub(crate) fn publication_prefix(index_path: &Path) -> OsString {
    let mut prefix = OsString::from(".");
    prefix.push(index_path.file_name().unwrap_or_default());
    prefix.push(".aifs-tmp-");
    prefix
}

pub(crate) fn resolve_index_path(path: &Path, create_parent: bool) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    match fs::metadata(&absolute) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "index destination must be a regular file",
                ));
            }
            return fs::canonicalize(absolute);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = absolute.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "index destination has no parent",
        )
    })?;
    let name = absolute.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "index destination has no filename",
        )
    })?;
    if create_parent {
        fs::create_dir_all(parent)?;
        Ok(fs::canonicalize(parent)?.join(name))
    } else {
        Ok(resolve_existing_ancestor(parent)?.join(name))
    }
}

fn resolve_existing_ancestor(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(error)?;
            let name = path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid index parent")
            })?;
            Ok(resolve_existing_ancestor(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn dropping_writer_releases_lock_with_a_duplicate_descriptor_alive() {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "aifs-writer-duplicate-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let index = path.join("index.txt");
        let guard = IndexWriterGuard::acquire(&index).unwrap();
        // dup and fork share flock ownership through the open file description.
        let inherited = guard.lock_file.try_clone().unwrap();
        assert_eq!(
            IndexWriterGuard::acquire(&index).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(guard);
        let replacement = IndexWriterGuard::acquire(&index).unwrap();
        drop(inherited);
        assert_eq!(
            IndexWriterGuard::acquire(&index).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(replacement);
        fs::remove_dir_all(path).unwrap();
    }
}
