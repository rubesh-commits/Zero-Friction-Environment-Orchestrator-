use std::path::PathBuf;

/// Errors surfaced by the orchestration pipeline. Messages are written for a
/// human reading a terminal, not for a programmatic API consumer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Message(String),

    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("path escapes its sandbox: {0}")]
    PathEscape(PathBuf),

    #[error("no compatible entrypoint was found for {0}")]
    NoEntrypoint(String),

    #[error("required runtime is unavailable: {0}")]
    MissingRuntime(String),

    #[error("operation was cancelled by the user")]
    Cancelled,

    #[error("network download failed: {0}")]
    Download(String),

    #[error("checksum verification failed for {0}")]
    Checksum(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Adds path context to a `std::io::Error`, since Rust's bare I/O errors never
/// say which file failed.
pub trait IoContext<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T>;
    fn context(self, message: impl Into<String>) -> Result<T>;
}

impl<T> IoContext<T> for std::result::Result<T, std::io::Error> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T> {
        let path = path.into();
        self.map_err(|source| Error::Io {
            context: format!(" while accessing {}", path.display()),
            source,
        })
    }

    fn context(self, message: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::Io {
            context: format!(": {}", message.into()),
            source,
        })
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            context: String::new(),
            source,
        }
    }
}

/// Convenience constructor for a plain human-readable error.
pub fn msg(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}