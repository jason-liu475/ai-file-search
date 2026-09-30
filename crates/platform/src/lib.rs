//! Audited native operations that cannot live in the unsafe-free application crates.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{create_private_pipe, current_user_sid, verify_private_pipe_client};
