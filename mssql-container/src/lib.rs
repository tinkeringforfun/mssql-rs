//! # mssql-container
//!
//! Pull and run SQL Server Linux containers **without Docker** or any container
//! runtime. Uses OCI image pulling + Linux namespaces directly.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use mssql_container::SqlServerContainer;
//!
//! # async fn example() -> mssql_container::error::Result<()> {
//! let container = SqlServerContainer::start().await?;
//! println!("Connect at: {}", container.connection_string());
//! // Use it...
//! drop(container); // auto-stops
//! # Ok(())
//! # }
//! ```
//!
//! ## Requirements
//!
//! - **Linux** with root privileges (or CAP_SYS_ADMIN)
//! - Internet access to pull from `mcr.microsoft.com`
//! - On Windows, WSL2 is required (stub — not yet implemented)

pub mod container;
pub mod error;
pub mod image;
pub mod registry;
pub mod singleton;
pub mod wsl;

pub use singleton::{RunningInstance, get_or_start};

use error::Result;
#[cfg(target_os = "windows")]
use error::Error;
use registry::RegistryClient;
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

/// Default SA password for SQL Server.
const DEFAULT_SA_PASSWORD: &str = "StrongP@ss1";
/// Default TCP port.
const DEFAULT_PORT: u16 = 1433;
/// Default image registry.
const DEFAULT_REGISTRY: &str = "mcr.microsoft.com";
/// Default image repository.
const DEFAULT_REPOSITORY: &str = "mssql/server";
/// Default image tag.
const DEFAULT_TAG: &str = "2022-latest";
/// Default health check timeout.
const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(120);

/// Builder for configuring a [`SqlServerContainer`].
pub struct SqlServerContainerBuilder {
    registry: String,
    repository: String,
    tag: String,
    sa_password: String,
    port: u16,
    cache_dir: Option<PathBuf>,
    health_timeout: Duration,
}

impl Default for SqlServerContainerBuilder {
    fn default() -> Self {
        Self {
            registry: DEFAULT_REGISTRY.to_string(),
            repository: DEFAULT_REPOSITORY.to_string(),
            tag: DEFAULT_TAG.to_string(),
            sa_password: DEFAULT_SA_PASSWORD.to_string(),
            port: DEFAULT_PORT,
            cache_dir: None,
            health_timeout: DEFAULT_HEALTH_TIMEOUT,
        }
    }
}

impl SqlServerContainerBuilder {
    /// Set the full image name (e.g. `"mcr.microsoft.com/mssql/server"`).
    ///
    /// This sets both the registry and repository. If the image contains a `/`
    /// after the first component, the first component is treated as the registry.
    pub fn image(mut self, image: &str) -> Self {
        if let Some(idx) = image.find('/') {
            self.registry = image[..idx].to_string();
            self.repository = image[idx + 1..].to_string();
        } else {
            self.repository = image.to_string();
        }
        self
    }

    /// Set the image tag (default: `"2022-latest"`).
    pub fn tag(mut self, tag: &str) -> Self {
        self.tag = tag.to_string();
        self
    }

    /// Set the SA password (default: `"StrongP@ss1"`).
    pub fn sa_password(mut self, password: &str) -> Self {
        self.sa_password = password.to_string();
        self
    }

    /// Set the TCP port SQL Server listens on (default: `1433`).
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Set the cache directory for OCI layers (default: `~/.mssql-container/cache/`).
    pub fn cache_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cache_dir = Some(dir.into());
        self
    }

    /// Set the health check timeout (default: 120 seconds).
    pub fn health_timeout(mut self, timeout: Duration) -> Self {
        self.health_timeout = timeout;
        self
    }

    /// Build the configuration. Call `.start().await` on the result to launch.
    pub fn build(self) -> SqlServerContainerConfig {
        let cache_dir = self.cache_dir.unwrap_or_else(|| {
            dirs_or_home().join(".mssql-container").join("cache")
        });
        let base_dir = cache_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| {
            dirs_or_home().join(".mssql-container")
        });

        SqlServerContainerConfig {
            registry: self.registry,
            repository: self.repository,
            tag: self.tag,
            sa_password: self.sa_password,
            port: self.port,
            cache_dir,
            rootfs_base: base_dir.join("rootfs"),
            health_timeout: self.health_timeout,
        }
    }
}

/// Resolved configuration, ready to start.
pub struct SqlServerContainerConfig {
    registry: String,
    repository: String,
    tag: String,
    sa_password: String,
    port: u16,
    cache_dir: PathBuf,
    rootfs_base: PathBuf,
    health_timeout: Duration,
}

impl SqlServerContainerConfig {
    /// Pull the image, assemble the rootfs, and start SQL Server.
    pub async fn start(self) -> Result<SqlServerContainer> {
        // On Windows, try WSL2.
        #[cfg(target_os = "windows")]
        {
            wsl::start_in_wsl(&self.sa_password, self.port).await?;
            // If WSL succeeded, we'd return here. For now it always errors.
        }

        // Check privileges on Linux.
        #[cfg(target_os = "linux")]
        container::check_privileges()?;

        // 1. Pull manifest.
        let client = RegistryClient::new(&self.registry, &self.repository);
        let manifest = client.pull_manifest(&self.tag).await?;

        // 2. Pull all layer blobs.
        let mut layer_paths = Vec::new();
        let layer_digests: Vec<String> = manifest.layers.iter().map(|l| l.digest.clone()).collect();

        for layer in &manifest.layers {
            let path = client.pull_blob(&layer.digest, &self.cache_dir).await?;
            layer_paths.push(path);
        }

        // 3. Assemble rootfs.
        let hash = image::rootfs_hash(&layer_digests);
        let rootfs_dir = self.rootfs_base.join(&hash);

        if !rootfs_dir.join("opt").exists() {
            info!("Assembling rootfs");
            image::assemble_rootfs(&layer_paths, &rootfs_dir)?;
        } else {
            info!("Rootfs already assembled, reusing");
        }

        // 4. Start the container.
        #[cfg(target_os = "linux")]
        let process = container::spawn_container(&container::ContainerConfig {
            rootfs: rootfs_dir,
            sa_password: self.sa_password.clone(),
            port: self.port,
            hostname: "mssql-container".to_string(),
        })?;

        #[cfg(not(target_os = "linux"))]
        return Err(Error::Container(
            "Linux namespaces are only available on Linux".to_string(),
        ));

        // 5. Wait for SQL Server to be ready.
        #[cfg(target_os = "linux")]
        container::wait_for_ready(self.port, self.health_timeout).await?;

        #[cfg(target_os = "linux")]
        {
            Ok(SqlServerContainer {
                process: Some(process),
                port: self.port,
                sa_password: self.sa_password,
            })
        }
    }
}

/// A running SQL Server container.
///
/// When dropped, the container is automatically stopped (SIGTERM, then SIGKILL
/// after a timeout).
pub struct SqlServerContainer {
    #[cfg(target_os = "linux")]
    pub(crate) process: Option<container::ContainerProcess>,
    #[cfg(not(target_os = "linux"))]
    pub(crate) process: Option<()>,
    port: u16,
    sa_password: String,
}

impl SqlServerContainer {
    /// Start a SQL Server container with default settings.
    ///
    /// Equivalent to `SqlServerContainer::builder().build().start().await`.
    pub async fn start() -> Result<Self> {
        Self::builder().build().start().await
    }

    /// Create a builder for configuring the container.
    pub fn builder() -> SqlServerContainerBuilder {
        SqlServerContainerBuilder::default()
    }

    /// The host address (always `127.0.0.1` — we use host networking).
    pub fn host(&self) -> &str {
        "127.0.0.1"
    }

    /// The TCP port SQL Server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The SA password.
    pub fn sa_password(&self) -> &str {
        &self.sa_password
    }

    /// An ADO.NET-style connection string.
    pub fn connection_string(&self) -> String {
        format!(
            "Server=127.0.0.1,{};User Id=sa;Password={};TrustServerCertificate=true;",
            self.port, self.sa_password
        )
    }

    /// A `mssql://` URL for the connection.
    pub fn url(&self) -> String {
        format!(
            "mssql://sa:{}@127.0.0.1:{}/master",
            self.sa_password, self.port
        )
    }

    /// Explicitly stop the container. Also called automatically on drop.
    pub fn stop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(process) = self.process.take() {
            process.stop(Duration::from_secs(10));
        }
    }
}

impl Drop for SqlServerContainer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Get the user's home directory.
fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}
