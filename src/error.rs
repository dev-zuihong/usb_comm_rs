//! 保留底层信息的统一错误类型与平台错误分类。

use std::{io, sync::Arc};

pub type Result<T> = std::result::Result<T, Error>;

/// 统一错误类型；后端无法给出明确错误分类时，保留原始错误信息。
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("device is busy: {0}")]
    Busy(String),
    #[error("device disconnected: {0}")]
    Disconnected(String),
    #[error("operation timed out; previously transmitted bytes cannot be recalled")]
    Timeout,
    #[error("session is closed")]
    Closed,
    #[error("unsupported operation or setting: {0}")]
    Unsupported(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("use the async API inside a Tokio runtime")]
    InvalidContext,
    #[error("{backend}: {message}")]
    Backend {
        backend: &'static str,
        message: String,
    },
    #[error("I/O error: {0}")]
    Io(#[source] Arc<io::Error>),
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        // 将 Windows 共享冲突和 Unix EBUSY 归类为占用，同时保留原始说明。
        if matches!(error.raw_os_error(), Some(32)) && cfg!(windows)
            || matches!(error.raw_os_error(), Some(16)) && cfg!(unix)
        {
            return Self::Busy(error.to_string());
        }
        match error.kind() {
            io::ErrorKind::PermissionDenied => Self::PermissionDenied(error.to_string()),
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::NotFound
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof => Self::Disconnected(error.to_string()),
            io::ErrorKind::Unsupported => Self::Unsupported(error.to_string()),
            _ => Self::Io(Arc::new(error)),
        }
    }
}

impl From<serialport::Error> for Error {
    fn from(error: serialport::Error) -> Self {
        match error.kind() {
            serialport::ErrorKind::NoDevice => Self::Disconnected(error.to_string()),
            serialport::ErrorKind::InvalidInput => Self::InvalidConfig(error.to_string()),
            serialport::ErrorKind::Io(kind) => io::Error::new(kind, error.to_string()).into(),
            _ => Self::Backend {
                backend: "serialport",
                message: error.to_string(),
            },
        }
    }
}

impl From<hidapi::HidError> for Error {
    fn from(error: hidapi::HidError) -> Self {
        match error {
            hidapi::HidError::IoError { error } => error.into(),
            error => Self::Backend {
                backend: "hidapi",
                message: error.to_string(),
            },
        }
    }
}
