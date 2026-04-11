//! Global singleton container manager.
//!
//! Ensures only one SQL Server container runs per process (and coordinates
//! across processes via flock + a JSON state file).

use crate::error::Result;
use crate::SqlServerContainer;
use serde::{Deserialize, Serialize};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info};

/// A descriptor for a running SQL Server container instance.
#[derive(Debug, Clone)]
pub struct RunningInstance {
    /// Host address (always "127.0.0.1").
    pub host: String,
    /// TCP port SQL Server is listening on.
    pub port: u16,
    /// The SA password.
    pub sa_password: String,
}

/// Persisted state written to `~/.mssql-container/instance.json`.
#[derive(Debug, Serialize, Deserialize)]
struct InstanceState {
    pid: u32,
    port: u16,
    password: String,
}

/// In-process singleton: holds the running container (which stops on drop)
/// and the cached instance info.
struct SingletonInner {
    instance: RunningInstance,
    /// Keep the container alive for the lifetime of the process.
    _container: SqlServerContainer,
}

/// Process-wide singleton.
static SINGLETON: OnceLock<Arc<Mutex<Option<SingletonInner>>>> = OnceLock::new();

fn singleton_lock() -> &'static Arc<Mutex<Option<SingletonInner>>> {
    SINGLETON.get_or_init(|| Arc::new(Mutex::new(None)))
}

fn state_dir() -> PathBuf {
    crate::dirs_or_home().join(".mssql-container")
}

fn state_file() -> PathBuf {
    state_dir().join("instance.json")
}

fn lock_file() -> PathBuf {
    state_dir().join("lock")
}

/// Probe whether something is listening on `127.0.0.1:port`.
fn tcp_probe(port: u16) -> bool {
    let addr = format!("127.0.0.1:{port}");
    TcpStream::connect_timeout(
        &addr.parse().unwrap(),
        Duration::from_millis(500),
    )
    .is_ok()
}

/// Check whether a PID is still alive.
#[cfg(target_os = "linux")]
fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) checks existence without sending a signal.
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// Acquire a cross-process flock on `~/.mssql-container/lock`.
///
/// Returns the open file (must be kept alive for the duration of the critical section).
#[cfg(target_os = "linux")]
fn acquire_flock() -> Result<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    std::fs::create_dir_all(state_dir())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_file())?;
    #[allow(deprecated)]
    nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusive)?;
    Ok(file)
}

#[cfg(not(target_os = "linux"))]
fn acquire_flock() -> Result<std::fs::File> {
    std::fs::create_dir_all(state_dir())?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_file())?;
    Ok(file)
}

/// Try to recover a running instance from a previous process.
fn try_recover_existing() -> Option<RunningInstance> {
    let data = std::fs::read_to_string(state_file()).ok()?;
    let state: InstanceState = serde_json::from_str(&data).ok()?;

    if pid_alive(state.pid) && tcp_probe(state.port) {
        info!(
            pid = state.pid,
            port = state.port,
            "Reusing existing container from previous process"
        );
        Some(RunningInstance {
            host: "127.0.0.1".to_string(),
            port: state.port,
            sa_password: state.password,
        })
    } else {
        debug!("Previous instance not responding, will start fresh");
        // Clean up stale state file.
        let _ = std::fs::remove_file(state_file());
        None
    }
}

/// Write instance state to disk so other/future processes can find it.
fn persist_state(pid: u32, port: u16, password: &str) -> Result<()> {
    std::fs::create_dir_all(state_dir())?;
    let state = InstanceState {
        pid,
        port,
        password: password.to_string(),
    };
    std::fs::write(state_file(), serde_json::to_string_pretty(&state)?)?;
    Ok(())
}

/// Returns a running SQL Server container instance, starting one if needed.
///
/// Thread-safe, process-wide singleton. Multiple calls return the same instance.
/// Uses a lock file (`~/.mssql-container/lock`) for cross-process coordination.
pub async fn get_or_start() -> Result<Arc<RunningInstance>> {
    let mtx = singleton_lock().clone();
    let mut guard = mtx.lock().await;

    // Fast path: already running in this process.
    if let Some(inner) = guard.as_ref() {
        return Ok(Arc::new(inner.instance.clone()));
    }

    // Acquire cross-process lock.
    let _flock = acquire_flock()?;

    // Check for a container from a previous process.
    if let Some(instance) = try_recover_existing() {
        // We found a live container but we don't own it — we can't hold a
        // SqlServerContainer handle (that would kill it on drop). Just cache
        // the connection info. If the other process dies, a future call will
        // detect the dead TCP and start fresh.
        //
        // We store None for _container and return the info. But our
        // SingletonInner requires a _container field... Let's handle this by
        // returning early without storing in the singleton (so each call
        // re-checks, which is fine since the probe is cheap).
        return Ok(Arc::new(instance));
    }

    // Start a new container.
    info!("Starting new SQL Server container");
    let container = SqlServerContainer::start().await?;

    let instance = RunningInstance {
        host: container.host().to_string(),
        port: container.port(),
        sa_password: container.sa_password().to_string(),
    };

    // Persist state for cross-process discovery.
    // Get the PID from the container process.
    #[cfg(target_os = "linux")]
    {
        if let Some(ref process) = container.process {
            persist_state(process.pid.as_raw() as u32, instance.port, &instance.sa_password)?;
        }
    }

    let result = Arc::new(instance.clone());

    *guard = Some(SingletonInner {
        instance,
        _container: container,
    });

    Ok(result)
}
