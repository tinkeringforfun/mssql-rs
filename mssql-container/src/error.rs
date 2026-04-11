use thiserror::Error;

/// Errors that can occur in the mssql-container crate.
#[derive(Debug, Error)]
pub enum Error {
    /// HTTP request failed (registry communication).
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// I/O error (file system, process, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Digest verification failed.
    #[error("Digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        expected: String,
        actual: String,
    },

    /// Registry returned an unexpected status code.
    #[error("Registry error: HTTP {status} from {url}")]
    Registry {
        status: u16,
        url: String,
    },

    /// The container process failed to start or exited unexpectedly.
    #[error("Container error: {0}")]
    Container(String),

    /// Health check timed out waiting for SQL Server to accept connections.
    #[error("Health check timed out after {timeout_secs}s on port {port}")]
    HealthCheckTimeout {
        port: u16,
        timeout_secs: u64,
    },

    /// Operation requires root / CAP_SYS_ADMIN.
    #[error("Insufficient privileges: must run as root or with CAP_SYS_ADMIN")]
    InsufficientPrivileges,

    /// Nix (syscall) error.
    #[error("Syscall error: {0}")]
    Nix(#[from] nix::Error),

    /// WSL2 is not available on this Windows host.
    #[error("WSL2 is not available")]
    WslNotAvailable,
}

/// Crate-level Result alias.
pub type Result<T> = std::result::Result<T, Error>;
