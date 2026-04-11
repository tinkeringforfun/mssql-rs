//! Linux container setup using namespaces, pivot_root, and process management.

use crate::error::{Error, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::signal::{kill, Signal};
use nix::unistd::{chdir, execve, fork, getuid, pivot_root, sethostname, ForkResult, Pid};
use std::ffi::CString;
use std::net::TcpStream;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info, warn};

/// Configuration for spawning a containerized SQL Server process.
pub struct ContainerConfig {
    pub rootfs: std::path::PathBuf,
    pub sa_password: String,
    pub port: u16,
    pub hostname: String,
}

/// A running container process.
pub struct ContainerProcess {
    pub pid: Pid,
}

impl ContainerProcess {
    /// Send SIGTERM and wait up to `timeout` for the process to exit, then SIGKILL.
    pub fn stop(&self, timeout: Duration) {
        info!(pid = self.pid.as_raw(), "Stopping container");

        if let Err(e) = kill(self.pid, Signal::SIGTERM) {
            warn!(error = %e, "Failed to send SIGTERM");
            return;
        }

        let start = Instant::now();
        loop {
            match nix::sys::wait::waitpid(self.pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)) {
                Ok(nix::sys::wait::WaitStatus::StillAlive) => {
                    if start.elapsed() > timeout {
                        warn!("Timeout waiting for SIGTERM, sending SIGKILL");
                        let _ = kill(self.pid, Signal::SIGKILL);
                        let _ = nix::sys::wait::waitpid(self.pid, None);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => return,
            }
        }
    }
}

/// Check that we have sufficient privileges to create namespaces.
pub fn check_privileges() -> Result<()> {
    if !getuid().is_root() {
        return Err(Error::InsufficientPrivileges);
    }
    Ok(())
}

/// Spawn the SQL Server process inside a new set of namespaces.
///
/// # Safety
///
/// This function calls `fork()` and `execve()`. The child process sets up
/// namespaces, mounts, and pivots into the rootfs before exec-ing sqlservr.
/// Must be called as root.
pub fn spawn_container(config: &ContainerConfig) -> Result<ContainerProcess> {
    check_privileges()?;

    let rootfs = config.rootfs.clone();
    let sa_password = config.sa_password.clone();
    let port = config.port;
    let hostname = config.hostname.clone();

    // Fork first, then unshare in the child.
    match unsafe { fork() }? {
        ForkResult::Parent { child } => {
            info!(pid = child.as_raw(), "Container process forked");
            Ok(ContainerProcess { pid: child })
        }
        ForkResult::Child => {
            // This is the child — set up namespaces and exec.
            // Any error here is fatal for the child.
            let result = setup_and_exec(&rootfs, &sa_password, port, &hostname);
            if let Err(e) = result {
                eprintln!("Container setup failed: {e}");
                std::process::exit(1);
            }
            unreachable!()
        }
    }
}

/// Child process: unshare namespaces, set up mounts, pivot_root, exec sqlservr.
fn setup_and_exec(rootfs: &Path, sa_password: &str, port: u16, hostname: &str) -> Result<()> {
    // Create new PID, mount, and UTS namespaces.
    unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNS | CloneFlags::CLONE_NEWUTS)?;

    // Set hostname.
    sethostname(hostname)?;

    // Make all mounts private so our changes don't propagate to the host.
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_PRIVATE | MsFlags::MS_REC,
        None::<&str>,
    )?;

    // Bind-mount the rootfs onto itself (required for pivot_root).
    mount(
        Some(rootfs),
        rootfs,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    )?;

    // Create necessary directories inside the rootfs.
    let proc_dir = rootfs.join("proc");
    let sys_dir = rootfs.join("sys");
    let dev_dir = rootfs.join("dev");
    let old_root = rootfs.join("oldroot");

    std::fs::create_dir_all(&proc_dir)?;
    std::fs::create_dir_all(&sys_dir)?;
    std::fs::create_dir_all(&dev_dir)?;
    std::fs::create_dir_all(&old_root)?;

    // Create minimal /dev nodes.
    create_dev_nodes(&dev_dir)?;

    // pivot_root: swap root to our rootfs.
    pivot_root(rootfs, &old_root)?;
    chdir("/")?;

    // Mount proc and sys.
    mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )?;
    mount(
        Some("sysfs"),
        "/sys",
        Some("sysfs"),
        MsFlags::MS_RDONLY,
        None::<&str>,
    )?;

    // Unmount old root.
    umount2("/oldroot", MntFlags::MNT_DETACH)?;
    std::fs::remove_dir("/oldroot").ok();

    // Exec sqlservr.
    let sqlservr = CString::new("/opt/mssql/bin/sqlservr").unwrap();
    let args: Vec<CString> = vec![sqlservr.clone()];
    let env: Vec<CString> = vec![
        CString::new("ACCEPT_EULA=Y").unwrap(),
        CString::new(format!("MSSQL_SA_PASSWORD={sa_password}")).unwrap(),
        CString::new(format!("MSSQL_TCP_PORT={port}")).unwrap(),
        CString::new("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin").unwrap(),
        CString::new("HOME=/root").unwrap(),
    ];

    debug!("Exec-ing sqlservr");
    execve(&sqlservr, &args, &env)?;
    unreachable!()
}

/// Create minimal device nodes needed by SQL Server.
fn create_dev_nodes(dev_dir: &Path) -> Result<()> {
    use nix::sys::stat::{makedev, mknod, Mode, SFlag};

    // /dev/null (1, 3)
    let null_path = dev_dir.join("null");
    if !null_path.exists() {
        mknod(
            &null_path,
            SFlag::S_IFCHR,
            Mode::from_bits_truncate(0o666),
            makedev(1, 3),
        )?;
    }

    // /dev/zero (1, 5)
    let zero_path = dev_dir.join("zero");
    if !zero_path.exists() {
        mknod(
            &zero_path,
            SFlag::S_IFCHR,
            Mode::from_bits_truncate(0o666),
            makedev(1, 5),
        )?;
    }

    // /dev/urandom (1, 9)
    let urandom_path = dev_dir.join("urandom");
    if !urandom_path.exists() {
        mknod(
            &urandom_path,
            SFlag::S_IFCHR,
            Mode::from_bits_truncate(0o666),
            makedev(1, 9),
        )?;
    }

    // /dev/random (1, 8)
    let random_path = dev_dir.join("random");
    if !random_path.exists() {
        mknod(
            &random_path,
            SFlag::S_IFCHR,
            Mode::from_bits_truncate(0o666),
            makedev(1, 8),
        )?;
    }

    Ok(())
}

/// Poll the TCP port until SQL Server accepts connections, or timeout.
pub async fn wait_for_ready(port: u16, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    let addr = format!("127.0.0.1:{port}");

    info!(addr, timeout_secs = timeout.as_secs(), "Waiting for SQL Server");

    loop {
        if start.elapsed() > timeout {
            return Err(Error::HealthCheckTimeout {
                port,
                timeout_secs: timeout.as_secs(),
            });
        }

        match TcpStream::connect_timeout(
            &addr.parse().unwrap(),
            Duration::from_millis(500),
        ) {
            Ok(_) => {
                info!("SQL Server is ready");
                return Ok(());
            }
            Err(_) => {
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
