use thiserror::Error;

/// Errors that can be returned by this crate.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SisError {
    /// An underlying I/O operation failed.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// The file is not a well-formed arivis SIS (`.sis`) file.
    #[error("invalid SIS file: {0}")]
    Format(String),
    /// The file is valid, but uses a layout this crate has not been verified against.
    #[error("unsupported SIS file: {0}")]
    Unsupported(String),
    /// Writing the Zarr store failed with the given diagnostic message.
    #[error("Zarr write error: {0}")]
    ZarrWrite(String),
}

impl SisError {
    /// The same error, prefixed with the scope it is about: its position (which is also
    /// its output group) and its name.
    pub(crate) fn in_scope(self, index: usize, name: &str) -> Self {
        let prefix = format!("scope {index} \"{name}\"");
        match self {
            SisError::Format(m) => SisError::Format(format!("{prefix}: {m}")),
            SisError::Unsupported(m) => SisError::Unsupported(format!("{prefix}: {m}")),
            other => other,
        }
    }
}

pub(crate) fn format_err(msg: impl Into<String>) -> SisError {
    SisError::Format(msg.into())
}
