//! Error values and protocol-safe error payloads produced by the worker.

use serde::Serialize;
use thiserror::Error;

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
}

impl WorkerError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
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

    pub fn payload(&self) -> ErrorPayload<'_> {
        ErrorPayload {
            code: self.code,
            message: &self.message,
        }
    }
}

#[derive(Serialize)]
/// Borrowed representation of an error for JSON protocol responses.
pub struct ErrorPayload<'a> {
    /// Stable machine-readable code borrowed from the source error.
    pub code: &'a str,
    /// Human-readable message borrowed from the source error.
    pub message: &'a str,
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
