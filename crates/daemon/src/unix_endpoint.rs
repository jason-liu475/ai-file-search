//! Private Unix endpoints. The endpoint lock also holds the bounded recovery record.
//! The private directory and processes running as the same UID are trusted.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, Metadata, Permissions};
use std::future::Future;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{SocketAddr, UnixListener as StdUnixListener};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use tokio::net::{UnixListener, UnixStream};

const DEFAULT_ENDPOINT: &str = "aifs-service";
const MAX_RECORD_BYTES: u64 = 512;
const RECORD_VERSION: u32 = 1;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);

pub(super) fn resolve(endpoint: &str, state_path: &Path) -> io::Result<String> {
    if endpoint != DEFAULT_ENDPOINT {
        return endpoint_string(&canonical_endpoint(Path::new(endpoint))?);
    }
    resolve_default(
        state_path,
        std::env::var_os("XDG_RUNTIME_DIR")
            .as_deref()
            .map(Path::new),
        &std::env::temp_dir(),
    )
}

fn resolve_default(state_path: &Path, xdg: Option<&Path>, temp: &Path) -> io::Result<String> {
    let state_path = canonical_state_path(state_path)?;
    let directory = if let Some(xdg) = xdg {
        validate_directory(xdg)?;
        fs::canonicalize(xdg)?.join("ai-file-search")
    } else {
        fs::canonicalize(temp)?.join(format!("aifs-{}", effective_uid()))
    };
    ensure_private_directory(&directory)?;
    let endpoint = directory.join(format!("s-{:016x}.sock", state_hash(&state_path)));
    endpoint_string(&canonical_endpoint(&endpoint)?)
}

fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn endpoint_string(path: &Path) -> io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid("resolved Unix endpoint is not UTF-8"))
}

fn canonical_endpoint(endpoint: &Path) -> io::Result<PathBuf> {
    if !endpoint.is_absolute() || endpoint.as_os_str().as_bytes().contains(&0) {
        return Err(invalid(
            "Unix endpoint must be an absolute path without NUL bytes",
        ));
    }
    let name = endpoint
        .file_name()
        .ok_or_else(|| invalid("Unix endpoint must have a socket filename"))?;
    let parent = endpoint
        .parent()
        .ok_or_else(|| invalid("Unix endpoint must have a parent directory"))?;
    validate_directory(parent)?;
    let parent = fs::canonicalize(parent)?;
    validate_directory(&parent)?;
    let endpoint = parent.join(name);
    endpoint_string(&endpoint)?;
    // Use the platform's sockaddr limit, not a Linux-only hard-coded limit.
    SocketAddr::from_pathname(&endpoint)?;
    socket_identity(&endpoint)?;
    Ok(endpoint)
}

fn canonical_state_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let filename = absolute
        .file_name()
        .ok_or_else(|| invalid("state path must have a filename"))?;
    let parent = absolute.parent().expect("absolute state parent");
    let mut resolved = fs::canonicalize(Path::new("/"))?;
    // Resolve existing ancestors, including aliases, before appending absent ones.
    for component in parent.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        resolved = fs::canonicalize(&resolved)?;
                        if !fs::metadata(&resolved)?.is_dir() {
                            return Err(invalid("state parent is not a directory"));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            Component::Prefix(_) => return Err(invalid("invalid Unix state path")),
        }
    }
    Ok(resolved.join(filename))
}

fn state_hash(path: &Path) -> u64 {
    // Stable FNV-1a over lossless path bytes; DefaultHasher has no stability contract.
    path.as_os_str()
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn validate_directory(path: &Path) -> io::Result<Identity> {
    validate_directory_for_uid(path, effective_uid())
}

fn validate_directory_for_uid(path: &Path, uid: u32) -> io::Result<Identity> {
    if !path.is_absolute() {
        return Err(invalid("runtime or endpoint parent must be absolute"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(invalid(
            "runtime or endpoint parent must be a real, owned private directory",
        ));
    }
    Ok(Identity::from_metadata(&metadata))
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let identity = validate_directory(path)?;
    let directory = File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if Identity::from_metadata(&directory.metadata()?) != identity {
        return Err(occupied("runtime directory changed while opening"));
    }
    directory.set_permissions(Permissions::from_mode(0o700))?;
    if validate_directory(path)? != identity {
        return Err(occupied(
            "runtime directory changed while setting permissions",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
    uid: u32,
}

impl Identity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct OwnershipRecord {
    version: u32,
    // Cooperative same-UID namespace correlation, not authentication.
    owner_state_hash: u64,
    socket: Identity,
}

fn socket_identity(path: &Path) -> io::Result<Option<Identity>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == effective_uid() => {
            Ok(Some(Identity::from_metadata(&metadata)))
        }
        Ok(_) => Err(occupied(
            "endpoint is not an owned socket; refusing to replace it",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn lock_path(endpoint: &Path) -> PathBuf {
    let mut name = OsStr::new(".").to_os_string();
    name.push(endpoint.file_name().expect("validated endpoint filename"));
    name.push(".aifs-endpoint.lock");
    endpoint.with_file_name(name)
}

fn validate_lock_metadata(metadata: &Metadata) -> io::Result<()> {
    validate_lock_metadata_for_uid(metadata, effective_uid())
}

fn validate_lock_metadata_for_uid(metadata: &Metadata, uid: u32) -> io::Result<()> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != uid
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(invalid(
            "endpoint lock/record must be an owned, singly-linked regular 0600 file",
        ));
    }
    Ok(())
}

fn verify_lock(path: &Path, file: &File) -> io::Result<()> {
    let descriptor = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    validate_lock_metadata(&descriptor)?;
    validate_lock_metadata(&named)?;
    if Identity::from_metadata(&descriptor) != Identity::from_metadata(&named) {
        return Err(occupied("endpoint lock identity changed"));
    }
    Ok(())
}

fn open_lock(path: &Path) -> io::Result<File> {
    let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let (file, created) = match rustix::fs::open(
        path,
        flags | OFlags::CREATE | OFlags::EXCL,
        Mode::RUSR | Mode::WUSR,
    ) {
        Ok(fd) => (File::from(fd), true),
        Err(error) if error == rustix::io::Errno::EXIST => {
            validate_lock_metadata(&fs::symlink_metadata(path)?)?;
            (
                File::from(rustix::fs::open(path, flags, Mode::empty())?),
                false,
            )
        }
        Err(error) => return Err(error.into()),
    };
    if created {
        file.set_permissions(Permissions::from_mode(0o600))?;
    }
    verify_lock(path, &file)?;
    file.try_lock().map_err(io::Error::from)?;
    verify_lock(path, &file)?;
    Ok(file)
}

fn read_record(file: &mut File) -> io::Result<Option<OwnershipRecord>> {
    if file.metadata()?.len() > MAX_RECORD_BYTES {
        return Err(invalid("endpoint ownership record exceeds size limit"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("endpoint ownership record exceeds size limit"));
    }
    if bytes.is_empty() {
        return Ok(None);
    }
    let record: OwnershipRecord =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid endpoint ownership record"))?;
    if record.version != RECORD_VERSION || record.socket.uid != effective_uid() {
        return Err(invalid(
            "endpoint ownership record has an invalid version or owner",
        ));
    }
    Ok(Some(record))
}

fn write_record(file: &mut File, socket: Identity, owner_state_hash: u64) -> io::Result<()> {
    let bytes = serde_json::to_vec(&OwnershipRecord {
        version: RECORD_VERSION,
        owner_state_hash,
        socket,
    })
    .map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("endpoint ownership record exceeds size limit"));
    }
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

struct PendingBind {
    endpoint: PathBuf,
    owner_state_hash: u64,
    parent_identity: Identity,
    lock_path: PathBuf,
    lock: File,
    existing: Option<Identity>,
}

impl PendingBind {
    fn prepare(endpoint: &Path, state_path: &Path) -> io::Result<Self> {
        let endpoint = canonical_endpoint(endpoint)?;
        let owner_state_hash = state_hash(&canonical_state_path(state_path)?);
        let parent_identity = validate_directory(endpoint.parent().expect("endpoint parent"))?;
        let lock_path = lock_path(&endpoint);
        let mut lock = open_lock(&lock_path)?;
        let record = read_record(&mut lock)?;
        let existing = socket_identity(&endpoint)?;
        if existing.is_some()
            && record
                .as_ref()
                .is_some_and(|record| record.owner_state_hash != owner_state_hash)
        {
            return Err(occupied(
                "endpoint ownership record belongs to a different state path",
            ));
        }
        if let Some(socket) = existing
            && record.as_ref().map(|record| record.socket) != Some(socket)
        {
            return Err(occupied("existing socket has no matching ownership record"));
        }
        Ok(Self {
            endpoint,
            owner_state_hash,
            parent_identity,
            lock_path,
            lock,
            existing,
        })
    }

    async fn finish_with_probe<F, T>(self, connect: F) -> io::Result<(EndpointGuard, UnixListener)>
    where
        F: Future<Output = io::Result<T>>,
    {
        if self.existing.is_some() {
            require_connection_refused(connect).await?;
        }
        let (guard, listener) = blocking(move || self.finish()).await?;
        Ok((guard, UnixListener::from_std(listener)?))
    }

    fn finish(mut self) -> io::Result<(EndpointGuard, StdUnixListener)> {
        if validate_directory(self.endpoint.parent().expect("endpoint parent"))?
            != self.parent_identity
        {
            return Err(occupied("endpoint parent identity changed"));
        }
        verify_lock(&self.lock_path, &self.lock)?;
        if let Some(existing) = self.existing {
            // Recheck both the record and the socket after the asynchronous probe.
            if read_record(&mut self.lock)?.is_none_or(|record| {
                record.socket != existing || record.owner_state_hash != self.owner_state_hash
            }) || socket_identity(&self.endpoint)? != Some(existing)
            {
                return Err(occupied(
                    "socket or ownership record changed during stale probe",
                ));
            }
            fs::remove_file(&self.endpoint)?;
        } else if socket_identity(&self.endpoint)?.is_some() {
            return Err(occupied("endpoint appeared while acquiring ownership"));
        }
        let listener = StdUnixListener::bind(&self.endpoint)?;
        let socket =
            socket_identity(&self.endpoint)?.ok_or_else(|| occupied("bound socket disappeared"))?;
        let mut guard = EndpointGuard {
            endpoint: self.endpoint,
            parent_identity: self.parent_identity,
            lock_path: self.lock_path,
            lock: self.lock,
            socket: Some(socket),
        };
        guard.verify_socket(socket)?;
        fs::set_permissions(&guard.endpoint, Permissions::from_mode(0o600))?;
        guard.verify_socket(socket)?;
        listener.set_nonblocking(true)?;
        write_record(&mut guard.lock, socket, self.owner_state_hash)?;
        guard.verify_socket(socket)?;
        Ok((guard, listener))
    }
}

async fn require_connection_refused<F, T>(connect: F) -> io::Result<()>
where
    F: Future<Output = io::Result<T>>,
{
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => Ok(()),
        _ => Err(occupied("endpoint is live or its liveness is ambiguous")),
    }
}

async fn blocking<F, T>(work: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(io::Error::other)?
}

/// Retain this guard until the listener is dropped. The lock is never unlinked.
pub(super) struct EndpointGuard {
    endpoint: PathBuf,
    parent_identity: Identity,
    lock_path: PathBuf,
    lock: File,
    socket: Option<Identity>,
}

impl EndpointGuard {
    pub(super) async fn bind(
        endpoint: &Path,
        state_path: &Path,
    ) -> io::Result<(Self, UnixListener)> {
        let endpoint = endpoint.to_path_buf();
        let state_path = state_path.to_path_buf();
        let pending = blocking(move || PendingBind::prepare(&endpoint, &state_path)).await?;
        let endpoint = pending.endpoint.clone();
        pending
            .finish_with_probe(UnixStream::connect(endpoint))
            .await
    }

    pub(super) fn artifact_paths(&self) -> Vec<PathBuf> {
        vec![self.lock_path.clone()]
    }

    fn verify_socket(&self, socket: Identity) -> io::Result<()> {
        if validate_directory(self.endpoint.parent().expect("endpoint parent"))?
            != self.parent_identity
        {
            return Err(occupied("endpoint parent identity changed"));
        }
        verify_lock(&self.lock_path, &self.lock)?;
        if socket_identity(&self.endpoint)? != Some(socket) {
            return Err(occupied("owned socket identity changed"));
        }
        Ok(())
    }
}

impl Drop for EndpointGuard {
    fn drop(&mut self) {
        if let Some(socket) = self.socket
            && self.verify_socket(socket).is_ok()
        {
            let _ = fs::remove_file(&self.endpoint);
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn occupied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::AddrInUse, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            // /tmp keeps native socket paths short even on macOS CI.
            let base = fs::canonicalize("/tmp").unwrap();
            for _ in 0..32 {
                let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = base.join(format!("aifs-u-{}-{sequence}", std::process::id()));
                match DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => {
                        fs::set_permissions(&path, Permissions::from_mode(0o700)).unwrap();
                        return Self(path);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("fixture creation failed: {error}"),
                }
            }
            panic!("fixture collision limit exceeded");
        }

        fn endpoint(&self) -> PathBuf {
            self.0.join("s.sock")
        }

        fn state_path(&self) -> PathBuf {
            self.0.join("state.json")
        }

        fn owner_hash(&self) -> u64 {
            state_hash(&canonical_state_path(&self.state_path()).unwrap())
        }

        fn private_child(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            ensure_private_directory(&path).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    async fn crashed_socket(fixture: &Fixture) -> (PathBuf, Identity) {
        let endpoint = fixture.endpoint();
        let (mut guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        let identity = socket_identity(&endpoint).unwrap().unwrap();
        // Simulate process exit: close the listener/lock without running socket cleanup.
        guard.socket = None;
        drop(listener);
        drop(guard);
        (endpoint, identity)
    }

    fn assert_unchanged(path: &Path, identity: Identity) {
        assert_eq!(
            Identity::from_metadata(&fs::symlink_metadata(path).unwrap()),
            identity
        );
    }

    fn overwrite_record(endpoint: &Path, record: &OwnershipRecord) {
        let mut file = open_lock(&lock_path(endpoint)).unwrap();
        file.set_len(0).unwrap();
        file.write_all(&serde_json::to_vec(record).unwrap())
            .unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn runtime_directory_validation_and_private_child_modes() {
        let fixture = Fixture::new();
        let state = fixture.0.join("state.json");
        let resolved = resolve_default(&state, Some(&fixture.0), &fixture.0).unwrap();
        let child = fixture.0.join("ai-file-search");
        assert_eq!(fs::metadata(&child).unwrap().mode() & 0o777, 0o700);
        assert_eq!(Path::new(&resolved).parent(), Some(child.as_path()));
        for mode in [0o710, 0o701, 0o750, 0o707, 0o777] {
            fs::set_permissions(&fixture.0, Permissions::from_mode(mode)).unwrap();
            assert!(resolve_default(&state, Some(&fixture.0), &fixture.0).is_err());
            assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o777, mode);
        }
        fs::set_permissions(&fixture.0, Permissions::from_mode(0o700)).unwrap();
        assert!(resolve_default(&state, Some(Path::new("relative")), &fixture.0).is_err());
        assert!(resolve_default(&state, Some(Path::new("")), &fixture.0).is_err());
        let link = fixture.0.join("runtime-link");
        symlink(&fixture.0, &link).unwrap();
        assert!(resolve_default(&state, Some(&link), &fixture.0).is_err());
        let file = fixture.0.join("runtime-file");
        fs::write(&file, b"unchanged").unwrap();
        assert!(resolve_default(&state, Some(&file), &fixture.0).is_err());
        assert_eq!(fs::read(file).unwrap(), b"unchanged");
    }

    #[test]
    fn default_fallback_is_per_user_private_and_state_specific() {
        let fixture = Fixture::new();
        let state = fixture.0.join("state.json");
        let first = resolve_default(&state, None, &fixture.0).unwrap();
        assert_eq!(first, resolve_default(&state, None, &fixture.0).unwrap());
        let other = resolve_default(&fixture.0.join("other.json"), None, &fixture.0).unwrap();
        assert_ne!(first, other);
        let parent = Path::new(&first).parent().unwrap();
        assert_eq!(parent, fixture.0.join(format!("aifs-{}", effective_uid())));
        assert_eq!(fs::metadata(parent).unwrap().mode() & 0o777, 0o700);
        assert_eq!(Path::new(&first).file_name().unwrap().len(), 23);
        fs::set_permissions(parent, Permissions::from_mode(0o755)).unwrap();
        assert!(resolve_default(&state, None, &fixture.0).is_err());
        assert_eq!(fs::metadata(parent).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn app_child_links_and_insecure_existing_directories_are_rejected() {
        let fixture = Fixture::new();
        let runtime = fixture.private_child("runtime");
        let child = runtime.join("ai-file-search");
        symlink(&fixture.0, &child).unwrap();
        assert!(resolve_default(&fixture.0.join("state"), Some(&runtime), &fixture.0).is_err());
        assert!(
            fs::symlink_metadata(&child)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&child).unwrap();
        fs::create_dir(&child).unwrap();
        fs::set_permissions(&child, Permissions::from_mode(0o755)).unwrap();
        assert!(resolve_default(&fixture.0.join("state"), Some(&runtime), &fixture.0).is_err());
        assert_eq!(fs::metadata(&child).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn state_parent_aliases_and_absent_descendants_hash_consistently() {
        let fixture = Fixture::new();
        let real = fixture.private_child("real");
        let alias = fixture.0.join("alias");
        symlink(&real, &alias).unwrap();
        for suffix in ["state.json", "not-created/sub/state.json"] {
            let expected =
                resolve_default(&real.join(suffix), Some(&fixture.0), &fixture.0).unwrap();
            assert_eq!(
                expected,
                resolve_default(&alias.join(suffix), Some(&fixture.0), &fixture.0).unwrap()
            );
        }
        assert_eq!(
            resolve_default(&real.join("./state.json"), Some(&fixture.0), &fixture.0).unwrap(),
            resolve_default(&real.join("state.json"), Some(&fixture.0), &fixture.0).unwrap()
        );
        let long_state = real.join("x".repeat(300));
        assert!(resolve_default(&long_state, Some(&fixture.0), &fixture.0).is_ok());
    }

    #[test]
    fn custom_aliases_use_the_canonical_private_parent() {
        let fixture = Fixture::new();
        let real = fixture.private_child("real");
        let parent = real.join("parent");
        ensure_private_directory(&parent).unwrap();
        let alias = fixture.0.join("alias");
        symlink(&real, &alias).unwrap();
        let expected = parent.join("s.sock");
        assert_eq!(
            canonical_endpoint(&alias.join("parent/s.sock")).unwrap(),
            expected
        );
        assert_eq!(
            canonical_endpoint(&parent.join("./s.sock")).unwrap(),
            expected
        );
        assert!(canonical_endpoint(&alias.join("s.sock")).is_err());
    }

    #[test]
    fn relative_long_nul_and_non_utf8_resolved_endpoints_are_rejected() {
        let fixture = Fixture::new();
        for endpoint in ["", "s.sock", "relative/s.sock", "/", "/tmp/\0sock"] {
            assert!(resolve(endpoint, &fixture.0.join("state")).is_err());
        }
        assert!(canonical_endpoint(&fixture.0.join("x".repeat(200))).is_err());
        let invalid_utf8 = fixture.0.join(OsString::from_vec(vec![b'd', 0xff]));
        ensure_private_directory(&invalid_utf8).unwrap();
        let parent = invalid_utf8.join("parent");
        ensure_private_directory(&parent).unwrap();
        let alias = fixture.0.join("utf8-alias");
        symlink(&invalid_utf8, &alias).unwrap();
        let endpoint = alias.join("parent/s.sock");
        assert_eq!(
            resolve(endpoint.to_str().unwrap(), &fixture.0.join("state"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(fs::read_dir(parent).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn bind_modes_record_artifacts_and_normal_cleanup() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        let lock = lock_path(&endpoint);
        assert_eq!(fs::metadata(&endpoint).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o777, 0o600);
        assert_eq!(guard.artifact_paths(), vec![lock.clone()]);
        let record: OwnershipRecord = serde_json::from_slice(&fs::read(&lock).unwrap()).unwrap();
        assert_eq!(record.socket, socket_identity(&endpoint).unwrap().unwrap());
        assert_eq!(record.owner_state_hash, fixture.owner_hash());
        assert!(fs::metadata(&lock).unwrap().len() <= MAX_RECORD_BYTES);
        let lock_identity = Identity::from_metadata(&fs::metadata(&lock).unwrap());
        drop(listener);
        drop(guard);
        assert!(!endpoint.exists());
        assert_unchanged(&lock, lock_identity);
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        assert_unchanged(&lock, lock_identity);
        drop(listener);
        drop(guard);
    }

    #[tokio::test]
    async fn same_endpoint_different_states_and_aliases_share_the_lock() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        let endpoint_text = endpoint.to_str().unwrap();
        let first = resolve(endpoint_text, &fixture.0.join("first.json")).unwrap();
        let second = resolve(endpoint_text, &fixture.0.join("second.json")).unwrap();
        assert_eq!(first, second);
        let (guard, listener) =
            EndpointGuard::bind(Path::new(&first), &fixture.0.join("first.json"))
                .await
                .unwrap();
        let identity = socket_identity(&endpoint).unwrap().unwrap();
        assert_eq!(
            EndpointGuard::bind(Path::new(&second), &fixture.0.join("second.json"))
                .await
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            EndpointGuard::bind(&fixture.0.join("./s.sock"), &fixture.state_path())
                .await
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_unchanged(&endpoint, identity);
        drop(listener);
        drop(guard);
    }

    #[tokio::test]
    async fn released_endpoint_can_change_state_owner_without_replacing_the_lock() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        let lock = lock_path(&endpoint);
        let lock_identity = Identity::from_metadata(&fs::metadata(&lock).unwrap());
        drop(listener);
        drop(guard);
        assert!(!endpoint.exists());

        let other_state = fixture.0.join("other.json");
        let (guard, listener) = EndpointGuard::bind(&endpoint, &other_state).await.unwrap();
        assert_unchanged(&lock, lock_identity);
        let record: OwnershipRecord = serde_json::from_slice(&fs::read(&lock).unwrap()).unwrap();
        assert_eq!(
            record.owner_state_hash,
            state_hash(&canonical_state_path(&other_state).unwrap())
        );
        assert_ne!(record.owner_state_hash, fixture.owner_hash());
        drop(listener);
        drop(guard);
        assert!(!endpoint.exists());
        assert_unchanged(&lock, lock_identity);
    }

    #[tokio::test]
    async fn managed_stale_socket_recovers_without_replacing_the_lock() {
        let fixture = Fixture::new();
        let (endpoint, _) = crashed_socket(&fixture).await;
        let lock = lock_path(&endpoint);
        let identity = Identity::from_metadata(&fs::metadata(&lock).unwrap());
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        assert_unchanged(&lock, identity);
        let client = UnixStream::connect(&endpoint).await.unwrap();
        let (_server, _) = listener.accept().await.unwrap();
        drop(client);
        drop(listener);
        drop(guard);
        assert!(!endpoint.exists());
        assert_unchanged(&lock, identity);
    }

    #[tokio::test]
    async fn foreign_unrecorded_stale_and_live_sockets_are_retained() {
        for live in [false, true] {
            let fixture = Fixture::new();
            let endpoint = fixture.endpoint();
            let listener = StdUnixListener::bind(&endpoint).unwrap();
            let identity = socket_identity(&endpoint).unwrap().unwrap();
            let listener = if live {
                Some(listener)
            } else {
                drop(listener);
                None
            };
            assert!(
                EndpointGuard::bind(&endpoint, &fixture.state_path())
                    .await
                    .is_err()
            );
            assert_unchanged(&endpoint, identity);
            if live {
                let client = UnixStream::connect(&endpoint).await.unwrap();
                drop(client);
            }
            drop(listener);
        }
    }

    #[tokio::test]
    async fn regular_files_and_leaf_links_are_retained() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        fs::write(&endpoint, b"foreign").unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_eq!(fs::read(&endpoint).unwrap(), b"foreign");
        assert!(!lock_path(&endpoint).exists());
        let target = fixture.0.join("target");
        fs::rename(&endpoint, &target).unwrap();
        symlink(&target, &endpoint).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert!(
            fs::symlink_metadata(&endpoint)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), b"foreign");
        fs::remove_file(&endpoint).unwrap();
        symlink(fixture.0.join("absent"), &endpoint).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert!(
            fs::symlink_metadata(&endpoint)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[tokio::test]
    async fn forged_mismatched_or_corrupt_records_never_authorize_removal() {
        for variant in 0..6 {
            let fixture = Fixture::new();
            let (endpoint, identity) = crashed_socket(&fixture).await;
            let mut record = OwnershipRecord {
                version: RECORD_VERSION,
                owner_state_hash: fixture.owner_hash(),
                socket: identity,
            };
            match variant {
                0 => record.socket.inode ^= 1,
                1 => record.socket.device ^= 1,
                2 => record.socket.uid ^= 1,
                3 => record.version += 1,
                _ => {}
            }
            if variant < 4 {
                overwrite_record(&endpoint, &record);
            } else {
                let mut file = open_lock(&lock_path(&endpoint)).unwrap();
                file.set_len(0).unwrap();
                let bytes = if variant == 4 {
                    b"not-json".to_vec()
                } else {
                    vec![b'x'; 513]
                };
                file.write_all(&bytes).unwrap();
            }
            let before = fs::read(lock_path(&endpoint)).unwrap();
            assert!(
                EndpointGuard::bind(&endpoint, &fixture.state_path())
                    .await
                    .is_err()
            );
            assert_unchanged(&endpoint, identity);
            assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn even_a_matching_record_does_not_authorize_removing_a_live_socket() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        let listener = StdUnixListener::bind(&endpoint).unwrap();
        let identity = socket_identity(&endpoint).unwrap().unwrap();
        overwrite_record(
            &endpoint,
            &OwnershipRecord {
                version: RECORD_VERSION,
                owner_state_hash: fixture.owner_hash(),
                socket: identity,
            },
        );
        let before = fs::read(lock_path(&endpoint)).unwrap();
        assert_eq!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AddrInUse
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
        drop(listener);
    }

    #[tokio::test]
    async fn lock_links_non_regular_files_and_insecure_modes_are_rejected() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        let lock = lock_path(&endpoint);
        let target = fixture.0.join("target");
        fs::write(&target, b"untouched").unwrap();
        symlink(&target, &lock).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        assert!(
            fs::symlink_metadata(&lock)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_file(&lock).unwrap();
        fs::create_dir(&lock).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert!(lock.is_dir());
        fs::remove_dir(&lock).unwrap();
        fs::write(&lock, b"untouched").unwrap();
        fs::set_permissions(&lock, Permissions::from_mode(0o644)).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_eq!(fs::read(&lock).unwrap(), b"untouched");
        assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o777, 0o644);
        fs::set_permissions(&lock, Permissions::from_mode(0o600)).unwrap();
        let hard_link = fixture.0.join("record-hard-link");
        fs::hard_link(&lock, &hard_link).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_eq!(fs::read(hard_link).unwrap(), b"untouched");
        assert!(!endpoint.exists());
    }

    #[tokio::test]
    async fn held_lock_prevents_stale_recovery_and_remains_permanent() {
        let fixture = Fixture::new();
        let (endpoint, identity) = crashed_socket(&fixture).await;
        let lock = lock_path(&endpoint);
        let holder = open_lock(&lock).unwrap();
        let before = fs::read(&lock).unwrap();
        assert_eq!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(&lock).unwrap(), before);
        drop(holder);
        assert!(lock.exists());
    }

    #[tokio::test]
    async fn cleanup_preserves_replacement_socket_file_or_link() {
        for variant in 0..3 {
            let fixture = Fixture::new();
            let endpoint = fixture.endpoint();
            let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .unwrap();
            let moved = fixture.0.join("old.sock");
            fs::rename(&endpoint, &moved).unwrap();
            let mut replacement_listener = None;
            match variant {
                0 => replacement_listener = Some(StdUnixListener::bind(&endpoint).unwrap()),
                1 => fs::write(&endpoint, b"replacement").unwrap(),
                _ => symlink(&moved, &endpoint).unwrap(),
            }
            let replacement = Identity::from_metadata(&fs::symlink_metadata(&endpoint).unwrap());
            drop(listener);
            drop(guard);
            assert_unchanged(&endpoint, replacement);
            drop(replacement_listener);
        }
    }

    #[tokio::test]
    async fn replacement_during_probe_is_not_unlinked() {
        let fixture = Fixture::new();
        let (endpoint, _) = crashed_socket(&fixture).await;
        let pending = PendingBind::prepare(&endpoint, &fixture.state_path()).unwrap();
        fs::rename(&endpoint, fixture.0.join("old.sock")).unwrap();
        let replacement = StdUnixListener::bind(&endpoint).unwrap();
        let identity = socket_identity(&endpoint).unwrap().unwrap();
        assert!(
            pending
                .finish_with_probe(async {
                    Err::<(), _>(io::Error::from(io::ErrorKind::ConnectionRefused))
                })
                .await
                .is_err()
        );
        assert_unchanged(&endpoint, identity);
        drop(replacement);
    }

    #[tokio::test]
    async fn timeouts_and_ambiguous_errors_are_not_stale_authority() {
        let fixture = Fixture::new();
        let (endpoint, identity) = crashed_socket(&fixture).await;
        let before = fs::read(lock_path(&endpoint)).unwrap();
        let pending = PendingBind::prepare(&endpoint, &fixture.state_path()).unwrap();
        let started = tokio::time::Instant::now();
        let ticker = tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            tokio::time::Instant::now()
        });
        assert!(
            pending
                .finish_with_probe(std::future::pending::<io::Result<()>>())
                .await
                .is_err()
        );
        assert!(ticker.await.unwrap().duration_since(started) < CONNECT_TIMEOUT);
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
        for kind in [
            io::ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::Interrupted,
        ] {
            let pending = PendingBind::prepare(&endpoint, &fixture.state_path()).unwrap();
            assert!(
                pending
                    .finish_with_probe(async { Err::<(), _>(io::Error::from(kind)) })
                    .await
                    .is_err()
            );
            assert_unchanged(&endpoint, identity);
            assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
        }
    }

    #[tokio::test]
    async fn insecure_custom_parent_is_rejected_without_creating_artifacts() {
        let fixture = Fixture::new();
        fs::set_permissions(&fixture.0, Permissions::from_mode(0o710)).unwrap();
        assert!(
            EndpointGuard::bind(&fixture.endpoint(), &fixture.state_path())
                .await
                .is_err()
        );
        assert!(!fixture.endpoint().exists());
        assert!(!lock_path(&fixture.endpoint()).exists());
        assert_eq!(fs::metadata(&fixture.0).unwrap().mode() & 0o777, 0o710);
        fs::set_permissions(&fixture.0, Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn directory_and_record_owner_checks_use_effective_uid() {
        let fixture = Fixture::new();
        assert_eq!(validate_directory(&fixture.0).unwrap().uid, effective_uid());
        assert!(validate_directory_for_uid(&fixture.0, effective_uid() ^ 1).is_err());
        let endpoint = fixture.endpoint();
        let mut file = open_lock(&lock_path(&endpoint)).unwrap();
        assert!(
            validate_lock_metadata_for_uid(&file.metadata().unwrap(), effective_uid() ^ 1).is_err()
        );
        let forged = OwnershipRecord {
            version: RECORD_VERSION,
            owner_state_hash: fixture.owner_hash(),
            socket: Identity {
                device: 1,
                inode: 1,
                uid: effective_uid() ^ 1,
            },
        };
        file.write_all(&serde_json::to_vec(&forged).unwrap())
            .unwrap();
        assert!(read_record(&mut file).is_err());
    }

    #[tokio::test]
    async fn changed_record_after_probe_and_unknown_record_fields_fail_closed() {
        let fixture = Fixture::new();
        let (endpoint, identity) = crashed_socket(&fixture).await;
        let mut pending = PendingBind::prepare(&endpoint, &fixture.state_path()).unwrap();
        write_record(
            &mut pending.lock,
            Identity {
                inode: identity.inode ^ 1,
                ..identity
            },
            fixture.owner_hash(),
        )
        .unwrap();
        let before = fs::read(lock_path(&endpoint)).unwrap();
        assert!(
            pending
                .finish_with_probe(async {
                    Err::<(), _>(io::Error::from(io::ErrorKind::ConnectionRefused))
                })
                .await
                .is_err()
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
        let mut file = open_lock(&lock_path(&endpoint)).unwrap();
        let mut record = serde_json::to_value(OwnershipRecord {
            version: RECORD_VERSION,
            owner_state_hash: fixture.owner_hash(),
            socket: identity,
        })
        .unwrap();
        record["socket"]["type"] = serde_json::json!("regular-file");
        file.set_len(0).unwrap();
        file.write_all(&serde_json::to_vec(&record).unwrap())
            .unwrap();
        drop(file);
        let before = fs::read(lock_path(&endpoint)).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
    }

    #[tokio::test]
    async fn matching_forged_record_cannot_authorize_a_regular_file() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        fs::write(&endpoint, b"foreign regular file").unwrap();
        let identity = Identity::from_metadata(&fs::metadata(&endpoint).unwrap());
        overwrite_record(
            &endpoint,
            &OwnershipRecord {
                version: RECORD_VERSION,
                owner_state_hash: fixture.owner_hash(),
                socket: identity,
            },
        );
        let before = fs::read(lock_path(&endpoint)).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(&endpoint).unwrap(), b"foreign regular file");
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
    }

    #[tokio::test]
    async fn replaced_lock_identity_blocks_cleanup_and_stale_recovery() {
        let fixture = Fixture::new();
        let endpoint = fixture.endpoint();
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        let socket = socket_identity(&endpoint).unwrap().unwrap();
        let lock = lock_path(&endpoint);
        fs::rename(&lock, fixture.0.join("old.lock")).unwrap();
        let replacement = open_lock(&lock).unwrap();
        let lock_identity = Identity::from_metadata(&replacement.metadata().unwrap());
        drop(listener);
        drop(guard);
        assert_unchanged(&endpoint, socket);
        assert_unchanged(&lock, lock_identity);
        drop(replacement);
        let pending = PendingBind::prepare(&endpoint, &fixture.state_path());
        assert!(pending.is_err());
        assert_unchanged(&endpoint, socket);
        assert_unchanged(&lock, lock_identity);
    }

    #[tokio::test]
    async fn replaced_parent_identity_blocks_cleanup() {
        let fixture = Fixture::new();
        let parent = fixture.private_child("parent");
        let endpoint = parent.join("s.sock");
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        fs::rename(&parent, fixture.0.join("old-parent")).unwrap();
        ensure_private_directory(&parent).unwrap();
        let replacement = StdUnixListener::bind(&endpoint).unwrap();
        let identity = socket_identity(&endpoint).unwrap().unwrap();
        drop(listener);
        drop(guard);
        assert_unchanged(&endpoint, identity);
        drop(replacement);
    }

    #[tokio::test]
    async fn different_state_cannot_adopt_a_stale_managed_endpoint() {
        let fixture = Fixture::new();
        let (endpoint, identity) = crashed_socket(&fixture).await;
        let lock = lock_path(&endpoint);
        let before = fs::read(&lock).unwrap();
        assert_eq!(
            EndpointGuard::bind(&endpoint, &fixture.0.join("other-state.json"))
                .await
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AddrInUse
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(&lock).unwrap(), before);
        let (guard, listener) = EndpointGuard::bind(&endpoint, &fixture.state_path())
            .await
            .unwrap();
        drop(listener);
        drop(guard);
        assert!(!endpoint.exists());
        // The permanent namespace association survives graceful shutdown too.
        let before = fs::read(&lock).unwrap();
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.0.join("other-state.json"))
                .await
                .is_err()
        );
        assert!(!endpoint.exists());
        assert_eq!(fs::read(&lock).unwrap(), before);
    }

    #[tokio::test]
    async fn canonical_state_parent_alias_can_recover_the_same_owner() {
        let fixture = Fixture::new();
        let (endpoint, _) = crashed_socket(&fixture).await;
        let alias = fixture.0.join("state-parent-alias");
        symlink(&fixture.0, &alias).unwrap();
        let alias_state = alias.join("state.json");
        assert_eq!(
            state_hash(&canonical_state_path(&alias_state).unwrap()),
            fixture.owner_hash()
        );
        let (guard, listener) = EndpointGuard::bind(&endpoint, &alias_state).await.unwrap();
        drop(listener);
        drop(guard);
        assert!(!endpoint.exists());
    }

    #[tokio::test]
    async fn owner_hash_changed_after_probe_is_not_stale_authority() {
        let fixture = Fixture::new();
        let (endpoint, identity) = crashed_socket(&fixture).await;
        let mut pending = PendingBind::prepare(&endpoint, &fixture.state_path()).unwrap();
        write_record(&mut pending.lock, identity, fixture.owner_hash() ^ 1).unwrap();
        let before = fs::read(lock_path(&endpoint)).unwrap();
        assert!(
            pending
                .finish_with_probe(async {
                    Err::<(), _>(io::Error::from(io::ErrorKind::ConnectionRefused))
                })
                .await
                .is_err()
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
        assert!(
            EndpointGuard::bind(&endpoint, &fixture.state_path())
                .await
                .is_err()
        );
        assert_unchanged(&endpoint, identity);
        assert_eq!(fs::read(lock_path(&endpoint)).unwrap(), before);
    }
}
