//! Error values and protocol-safe error payloads produced by the worker.

use thiserror::Error;

use crate::protocol::View;

/// Standard result type used by worker operations.
pub type Result<T> = std::result::Result<T, WorkerError>;

#[derive(Debug, Error)]
#[error("{message}")]
/// A stable error code paired with a human-readable diagnostic message.
pub struct WorkerError {
    /// Stable machine-readable code returned to protocol clients.
    pub code: &'static str,
    /// Contextual message intended for users and diagnostics.
    pub message: String,
    /// Named views available for recovery from a failed query.
    pub available_views: Option<Vec<View>>,
}

impl WorkerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            available_views: None,
        }
    }

    /// Attach the view catalog parsed from the same query source.
    pub fn with_available_views(mut self, available_views: Vec<View>) -> Self {
        self.available_views = Some(available_views);
        self
    }

    pub fn io(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => "ENOENT",
            std::io::ErrorKind::PermissionDenied => "EACCES",
            std::io::ErrorKind::AlreadyExists => "EEXIST",
            std::io::ErrorKind::BrokenPipe => "EPIPE",
            std::io::ErrorKind::InvalidInput => "EINVAL",
            std::io::ErrorKind::IsADirectory => "EISDIR",
            _ => "io_error",
        };
        Self::new(code, error.to_string())
    }

    pub fn json(error: serde_json::Error) -> Self {
        Self::new("invalid_json", error.to_string())
    }
}

impl From<std::io::Error> for WorkerError {
    fn from(value: std::io::Error) -> Self {
        Self::io(value)
    }
}

impl From<serde_json::Error> for WorkerError {
    fn from(value: serde_json::Error) -> Self {
        Self::json(value)
    }
}
