use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

#[path = "service_ownership.rs"]
mod service_ownership;

pub use service_ownership::{ServiceCoordination, ServiceInstanceGuard};

pub const DEFAULT_ENDPOINT: &str = "aifs-service";
pub const SERVICE_STATE_ENV: &str = "AIFS_SERVICE_STATE";

const MAX_STATE_BYTES: u64 = 64 * 1024;
const TEMP_ATTEMPTS: usize = 32;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceState {
    pub endpoint: String,
    pub pid: u32,
    pub index_path: PathBuf,
    pub started_unix_seconds: u64,
    #[serde(default)]
    pub auto_refresh_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    Starting,
    Running(ServiceState),
    Stale(ServiceState),
    Stopped,
    Unresponsive(Option<ServiceState>),
    Error(String),
}

#[must_use]
pub fn default_state_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SERVICE_STATE_ENV) {
        return PathBuf::from(path);
    }

    #[cfg(windows)]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local_app_data)
                .join("ai-file-search")
                .join("service-state.json");
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg_state_home) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(xdg_state_home)
                .join("ai-file-search")
                .join("service-state.json");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("ai-file-search")
                .join("service-state.json");
        }
    }

    std::env::temp_dir()
        .join("ai-file-search")
        .join("service-state.json")
}

/// Reads the advisory service state file.
///
/// # Errors
///
/// Returns `InvalidInput` for a non-regular file or link, an I/O error when
/// the file cannot be read, or `InvalidData` for malformed or oversized state.
pub fn read_state(path: &Path) -> io::Result<Option<ServiceState>> {
    service_ownership::validate_artifact(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service state must be a regular file",
        ));
    }
    let mut contents = Vec::new();
    BufReader::new(file.take(MAX_STATE_BYTES + 1)).read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "service state exceeds the 64 KiB limit",
        ));
    }
    serde_json::from_slice(&contents)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Writes the advisory service state file.
///
/// # Errors
///
/// Returns an I/O error when publication fails, `InvalidInput` for a
/// non-regular target, or `InvalidData` when serialized state including its
/// newline exceeds 64 KiB. Failed publication leaves existing bytes intact.
pub fn write_state(path: &Path, state: &ServiceState) -> io::Result<()> {
    service_ownership::validate_artifact(path)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let (mut temporary, file) = create_state_temp(path)?;
    let mut writer = StateWriter {
        inner: BufWriter::new(file),
        remaining: MAX_STATE_BYTES,
    };
    serde_json::to_writer_pretty(&mut writer, state).map_err(|error| {
        io::Error::new(
            error.io_error_kind().unwrap_or(io::ErrorKind::InvalidData),
            error,
        )
    })?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    writer.inner.get_ref().sync_all()?;
    drop(writer);
    fs::rename(&temporary.path, path)?;
    temporary.published = true;
    Ok(())
}

struct StateWriter {
    inner: BufWriter<File>,
    remaining: u64,
}

impl Write for StateWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() as u64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "service state exceeds the 64 KiB limit",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct StateTemporary {
    path: PathBuf,
    published: bool,
}

impl Drop for StateTemporary {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn create_state_temp(path: &Path) -> io::Result<(StateTemporary, File)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    create_state_temp_at(path, timestamp, &TEMP_SEQUENCE)
}

fn create_state_temp_at(
    path: &Path,
    timestamp: u128,
    sequence: &AtomicU64,
) -> io::Result<(StateTemporary, File)> {
    let filename = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state path must have a filename",
        )
    })?;
    for _ in 0..TEMP_ATTEMPTS {
        let mut name = std::ffi::OsString::from(".");
        name.push(filename);
        name.push(format!(
            ".aifs-tmp-{}-{}-{}.tmp",
            std::process::id(),
            timestamp,
            sequence.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary_path = path.with_file_name(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary_path) {
            Ok(file) => {
                return Ok((
                    StateTemporary {
                        path: temporary_path,
                        published: false,
                    },
                    file,
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique service state temporary file after 32 attempts",
    ))
}

/// Removes the advisory service state file.
///
/// # Errors
///
/// Returns an I/O error when a present state file cannot be removed.
pub fn remove_state(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Removes state only when every field still matches the expected owner.
///
/// Callers must hold startup coordination across this check and removal.
///
/// # Errors
///
/// Returns read/parse errors or an error removing a matching state file.
pub fn remove_state_if_matches(path: &Path, expected: &ServiceState) -> io::Result<bool> {
    if read_state(path)?.as_ref() != Some(expected) {
        return Ok(false);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[must_use]
pub fn render_status_text(status: &ServiceStatus) -> String {
    match status {
        ServiceStatus::Starting => "starting\n".to_owned(),
        ServiceStatus::Running(state) => render_state_text("running", state),
        ServiceStatus::Stale(state) => render_state_text("stale", state),
        ServiceStatus::Stopped => "stopped\n".to_owned(),
        ServiceStatus::Unresponsive(Some(state)) => render_state_text("unresponsive", state),
        ServiceStatus::Unresponsive(None) => "unresponsive\n".to_owned(),
        ServiceStatus::Error(reason) => format!("error {reason}\n"),
    }
}

fn render_state_text(status: &str, state: &ServiceState) -> String {
    let auto_refresh = state
        .auto_refresh_seconds
        .map_or_else(String::new, |seconds| format!(" auto refresh: {seconds}s"));
    format!(
        "{status} endpoint={} pid={} index={}{}\n",
        state.endpoint,
        state.pid,
        state.index_path.display(),
        auto_refresh
    )
}

#[must_use]
pub fn render_status_json(status: &ServiceStatus) -> String {
    let value = match status {
        ServiceStatus::Starting => json!({ "status": "starting" }),
        ServiceStatus::Running(state) => render_state_json("running", state),
        ServiceStatus::Stale(state) => render_state_json("stale", state),
        ServiceStatus::Stopped => json!({ "status": "stopped" }),
        ServiceStatus::Unresponsive(Some(state)) => render_state_json("unresponsive", state),
        ServiceStatus::Unresponsive(None) => json!({ "status": "unresponsive" }),
        ServiceStatus::Error(reason) => json!({ "status": "error", "reason": reason }),
    };

    format!("{value}\n")
}

fn render_state_json(status: &str, state: &ServiceState) -> serde_json::Value {
    let mut value = json!({
        "status": status,
        "endpoint": &state.endpoint,
        "pid": state.pid,
        "index_path": &state.index_path,
        "started_unix_seconds": state.started_unix_seconds,
    });
    if let Some(seconds) = state.auto_refresh_seconds {
        value["auto_refresh_seconds"] = json!(seconds);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_collisions_are_bounded_and_never_cleaned_as_owned_files() {
        let directory = std::env::temp_dir().join(format!(
            "aifs-state-temp-collisions-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("state.json");
        for counter in 0..TEMP_ATTEMPTS {
            let collision = directory.join(format!(
                ".state.json.aifs-tmp-{}-0-{counter}.tmp",
                std::process::id()
            ));
            fs::write(collision, b"foreign").unwrap();
        }
        let sequence = AtomicU64::new(0);
        let error = create_state_temp_at(&path, 0, &sequence)
            .err()
            .expect("32 collisions must fail");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(sequence.load(Ordering::Relaxed), 32);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 32);
        let (temporary, file) = create_state_temp_at(&path, 0, &sequence).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(file);
        drop(temporary);
        for entry in fs::read_dir(&directory).unwrap() {
            assert_eq!(fs::read(entry.unwrap().path()).unwrap(), b"foreign");
        }
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 32);
        fs::remove_dir_all(directory).unwrap();
    }
}
