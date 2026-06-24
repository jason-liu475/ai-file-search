use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

pub const DEFAULT_ENDPOINT: &str = "aifs-service";
pub const SERVICE_STATE_ENV: &str = "AIFS_SERVICE_STATE";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceState {
    pub endpoint: String,
    pub pid: u32,
    pub index_path: PathBuf,
    pub started_unix_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    Running(ServiceState),
    Stale(ServiceState),
    Stopped,
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
/// Returns an I/O error when the file cannot be read, or `InvalidData` when
/// the file exists but does not contain a valid service state.
pub fn read_state(path: &Path) -> io::Result<Option<ServiceState>> {
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes the advisory service state file.
///
/// # Errors
///
/// Returns an I/O error when the parent directory or file cannot be written.
pub fn write_state(path: &Path, state: &ServiceState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(state)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(path, format!("{contents}\n"))
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

#[must_use]
pub fn render_status_text(status: &ServiceStatus) -> String {
    match status {
        ServiceStatus::Running(state) => format!(
            "running endpoint={} pid={} index={}\n",
            state.endpoint,
            state.pid,
            state.index_path.display()
        ),
        ServiceStatus::Stale(state) => format!(
            "stale endpoint={} pid={} index={}\n",
            state.endpoint,
            state.pid,
            state.index_path.display()
        ),
        ServiceStatus::Stopped => "stopped\n".to_owned(),
    }
}

#[must_use]
pub fn render_status_json(status: &ServiceStatus) -> String {
    let value = match status {
        ServiceStatus::Running(state) => json!({
            "status": "running",
            "endpoint": &state.endpoint,
            "pid": state.pid,
            "index_path": &state.index_path,
            "started_unix_seconds": state.started_unix_seconds,
        }),
        ServiceStatus::Stale(state) => json!({
            "status": "stale",
            "endpoint": &state.endpoint,
            "pid": state.pid,
            "index_path": &state.index_path,
            "started_unix_seconds": state.started_unix_seconds,
        }),
        ServiceStatus::Stopped => json!({ "status": "stopped" }),
    };

    format!("{value}\n")
}
