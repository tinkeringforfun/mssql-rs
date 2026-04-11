//! Windows WSL2 wrapper for running the container on Windows via WSL.
//!
//! This module is a stub — the full implementation would detect WSL2, copy the
//! Linux binary into the WSL filesystem, run it, and forward the SQL Server
//! port back to the Windows host.
//!
//! # How it would work
//!
//! 1. Check `wsl.exe --status` to verify WSL2 is installed and running.
//! 2. Copy the compiled Linux binary to a known path inside the default WSL distro.
//! 3. Invoke the binary via `wsl.exe -- /path/to/binary <args>`.
//! 4. SQL Server binds inside WSL on the configured port. Because WSL2 uses a
//!    virtual network, we need `netsh interface portproxy` or rely on WSL2's
//!    automatic localhost forwarding (Windows 11+).
//! 5. Return the connection info pointing to `127.0.0.1:<port>`.
//!
//! # Why this is a stub
//!
//! The namespace-based container approach is Linux-only by nature. On Windows,
//! WSL2 _is_ a Linux VM, so we delegate to it. The implementation is non-trivial
//! (binary distribution, port forwarding edge cases, WSL distro management) and
//! is deferred to a future release.

use crate::error::{Error, Result};

/// Check whether WSL2 is available on this Windows host.
///
/// # Returns
///
/// `Ok(true)` if WSL2 is detected, `Ok(false)` if WSL is not installed,
/// or `Err` if detection itself fails.
pub fn is_wsl2_available() -> Result<bool> {
    #[cfg(target_os = "windows")]
    {
        // TODO: Run `wsl.exe --status` and parse output to confirm WSL2.
        // For now, just check if wsl.exe exists on PATH.
        let output = std::process::Command::new("wsl.exe")
            .arg("--status")
            .output();
        match output {
            Ok(o) => Ok(o.status.success()),
            Err(_) => Ok(false),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(false)
    }
}

/// Start SQL Server inside WSL2.
///
/// # TODO
///
/// - Copy the Linux binary into WSL
/// - Invoke it with the right arguments
/// - Set up port forwarding
/// - Return connection info
pub async fn start_in_wsl(
    _sa_password: &str,
    _port: u16,
) -> Result<()> {
    if !is_wsl2_available()? {
        return Err(Error::WslNotAvailable);
    }

    // TODO: Implement WSL2 container launch.
    // Steps:
    // 1. Determine WSL distro to use (default or specific)
    // 2. Copy/install the mssql-container binary inside WSL
    // 3. Run: wsl.exe -- sudo /path/to/mssql-container --internal-run <args>
    // 4. Wait for port to be reachable from Windows side
    // 5. Return connection info

    Err(Error::Container(
        "WSL2 support is not yet implemented".to_string(),
    ))
}
