use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Component, Path, PathBuf};

/// Short-lived serialization for managed startup and state transitions.
#[derive(Debug)]
pub struct ServiceCoordination {
    state_path: PathBuf,
    _file: File,
}

impl ServiceCoordination {
    /// Acquires the adjacent persistent startup lock without blocking.
    ///
    /// # Errors
    ///
    /// Returns `WouldBlock` when another coordinator owns the lock, or an I/O
    /// error resolving the path, creating parents, or opening the lock file.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let state_path = resolve_state_path(path)?;
        let file = acquire_lock(&state_path, ".startup.lock")?;
        Ok(Self {
            state_path,
            _file: file,
        })
    }

    #[must_use]
    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Observes startup ownership without creating any artifacts.
    ///
    /// # Errors
    ///
    /// Returns path/open/locking errors, including non-regular artifacts.
    pub fn startup_active(path: &Path) -> io::Result<bool> {
        probe_lock(&resolve_state_path(path)?, ".startup.lock")
    }

    /// Observes instance ownership without creating any artifacts.
    ///
    /// # Errors
    ///
    /// Returns path/open/locking errors, including non-regular artifacts.
    pub fn instance_active(path: &Path) -> io::Result<bool> {
        probe_lock(&resolve_state_path(path)?, ".instance.lock")
    }
}

/// Exclusive ownership retained for the lifetime of a managed child.
#[derive(Debug)]
pub struct ServiceInstanceGuard {
    state_path: PathBuf,
    _file: File,
}

impl ServiceInstanceGuard {
    /// Acquires the adjacent persistent instance lock without blocking.
    ///
    /// # Errors
    ///
    /// Returns `WouldBlock` for a live owner, or path/open/locking errors.
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let state_path = resolve_state_path(path)?;
        let file = acquire_lock(&state_path, ".instance.lock")?;
        Ok(Self {
            state_path,
            _file: file,
        })
    }

    #[must_use]
    pub fn artifact_paths(&self) -> Vec<PathBuf> {
        vec![
            self.state_path.clone(),
            lock_path(&self.state_path, ".startup.lock"),
            lock_path(&self.state_path, ".instance.lock"),
        ]
    }
}

fn lock_path(state_path: &Path, suffix: &str) -> PathBuf {
    let mut name = state_path
        .file_name()
        .expect("resolved state filename")
        .to_os_string();
    name.push(suffix);
    state_path.with_file_name(name)
}

fn acquire_lock(state_path: &Path, suffix: &str) -> io::Result<File> {
    fs::create_dir_all(state_path.parent().expect("absolute state parent"))?;
    let path = lock_path(state_path, suffix);
    let file = open_lock(&path, true)?;
    file.try_lock().map_err(io::Error::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn probe_lock(state_path: &Path, suffix: &str) -> io::Result<bool> {
    let path = lock_path(state_path, suffix);
    let file = match open_lock(&path, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn open_lock(path: &Path, create: bool) -> io::Result<File> {
    validate_artifact(path)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid_artifact(path));
    }
    validate_artifact(path)?;
    Ok(file)
}

pub(super) fn validate_artifact(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(invalid_artifact(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn invalid_artifact(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "service artifact must be a regular file, not a directory or link: {}",
            path.display()
        ),
    )
}

fn resolve_state_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let filename = absolute.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path must have a filename",
        )
    })?;
    let parent = absolute.parent().expect("absolute state parent");
    let mut components = parent.components().peekable();
    let mut resolved = PathBuf::new();
    while matches!(
        components.peek(),
        Some(Component::Prefix(_) | Component::RootDir)
    ) {
        resolved.push(components.next().expect("root component").as_os_str());
    }
    resolved = fs::canonicalize(resolved)?;
    // Canonicalize existing directories before appending new components so
    // parent aliases share the same persistent locks, even before publication.
    for component in components {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        resolved = fs::canonicalize(&resolved)?;
                        if !fs::metadata(&resolved)?.is_dir() {
                            return Err(io::Error::new(
                                io::ErrorKind::NotADirectory,
                                "state parent is not a directory",
                            ));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Component::Prefix(_) | Component::RootDir => {
                unreachable!("absolute path root already resolved")
            }
        }
    }
    let state_path = resolved.join(filename);
    validate_artifact(&state_path)?;
    match fs::canonicalize(&state_path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(state_path),
        Err(error) => Err(error),
    }
}
