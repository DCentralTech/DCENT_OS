//! Hardware watchdog driver.
//!
//! Wraps the /dev/watchdog interface for the Cadence WDT at 0xF8005000.
//! The watchdog reboots the system if not "kicked" within the configured
//! timeout period. This ensures the miner recovers from daemon crashes.
//!
//! CONFIG_WATCHDOG_NOWAYOUT is NOT set on the S9 kernel, meaning the
//! watchdog CAN be stopped (by closing the fd after writing "V").

use std::fs;

use crate::{HalError, Result};

/// Default watchdog device path.
pub const WATCHDOG_DEV: &str = "/dev/watchdog";

/// The magic close character. Writing "V" before closing disables the watchdog.
pub const WATCHDOG_MAGIC_CLOSE: u8 = b'V';

#[cfg(target_env = "musl")]
type WatchdogIoctlRequest = libc::c_int;

#[cfg(not(target_env = "musl"))]
type WatchdogIoctlRequest = libc::c_ulong;

fn watchdog_ioctl_request(req: libc::c_ulong) -> WatchdogIoctlRequest {
    req as WatchdogIoctlRequest
}

#[cfg(test)]
mod tests {
    use super::WATCHDOG_MAGIC_CLOSE;

    const SOURCE: &str = include_str!("watchdog.rs");

    fn source_after(marker: &str) -> &'static str {
        let start = SOURCE
            .rfind(marker)
            .expect("watchdog source marker missing");
        &SOURCE[start..]
    }

    #[test]
    fn watchdog_magic_close_byte_is_v() {
        assert_eq!(
            WATCHDOG_MAGIC_CLOSE, b'V',
            "Linux watchdog magic-close must write ASCII 'V' before close"
        );
    }

    #[test]
    fn close_magic_is_the_only_magic_close_path() {
        let close_body = source_after("pub fn close_magic(self)");
        let drop_body = source_after("impl Drop for Watchdog");

        assert!(
            close_body.contains("nix::unistd::write(&self.file, &[WATCHDOG_MAGIC_CLOSE])"),
            "close_magic must write WATCHDOG_MAGIC_CLOSE before dropping the fd"
        );
        assert!(
            !drop_body.contains("WATCHDOG_MAGIC_CLOSE")
                && !drop_body.contains("close_magic(")
                && !drop_body.contains("nix::unistd::write("),
            "Watchdog::Drop must remain fail-closed: dropping without an explicit \
             close_magic leaves /dev/watchdog armed so a crashed daemon reboots"
        );
        assert!(
            drop_body.contains("WITHOUT magic close"),
            "Watchdog::Drop must keep an explicit warning that the watchdog remains armed"
        );
    }
}

/// Hardware watchdog wrapper.
pub struct Watchdog {
    /// Owned file handle for /dev/watchdog.
    file: fs::File,
}

impl Watchdog {
    /// Open the watchdog device.
    ///
    /// The watchdog starts counting immediately upon open.
    pub fn open() -> Result<Self> {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(WATCHDOG_DEV)
            .map_err(|e| HalError::DeviceOpen {
                path: WATCHDOG_DEV.to_string(),
                source: e,
            })?;

        tracing::info!("Watchdog opened — countdown started");

        Ok(Self { file })
    }

    /// Kick (pet) the watchdog to reset the countdown timer.
    ///
    /// Must be called at regular intervals (typically every 5 seconds)
    /// to prevent a system reboot.
    pub fn kick(&self) -> Result<()> {
        nix::unistd::write(&self.file, &[0])
            .map_err(|e| HalError::Watchdog(format!("kick failed: {}", e)))?;
        Ok(())
    }

    /// Set the watchdog timeout in seconds.
    ///
    /// Uses the standard Linux WDIOC_SETTIMEOUT ioctl to configure how long
    /// the watchdog waits before rebooting if not kicked.
    /// BUG FIX (2026-04-11): timeout_s was parsed from config but never applied.
    #[cfg(unix)]
    pub fn set_timeout(&self, seconds: u32) -> Result<()> {
        use std::os::fd::AsRawFd;
        // WDIOC_SETTIMEOUT = _IOWR('W', 6, int) = 0xC0045706
        const WDIOC_SETTIMEOUT: libc::c_ulong = 0xC004_5706u32 as libc::c_ulong;
        let mut secs = seconds as libc::c_int;
        let ret = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                watchdog_ioctl_request(WDIOC_SETTIMEOUT),
                &mut secs,
            )
        };
        if ret < 0 {
            return Err(HalError::Watchdog(format!(
                "WDIOC_SETTIMEOUT({}) ioctl failed: {}",
                seconds,
                std::io::Error::last_os_error(),
            )));
        }
        tracing::info!(
            requested = seconds,
            actual = secs,
            "Watchdog timeout set to {}s",
            secs
        );
        Ok(())
    }

    /// Close the watchdog with the magic close character.
    ///
    /// Writing "V" before closing tells the watchdog driver to disable
    /// the timer (only works when CONFIG_WATCHDOG_NOWAYOUT is not set).
    pub fn close_magic(self) -> Result<()> {
        nix::unistd::write(&self.file, &[WATCHDOG_MAGIC_CLOSE])
            .map_err(|e| HalError::Watchdog(format!("magic close failed: {}", e)))?;
        tracing::info!("Watchdog disabled via magic close");
        // File is closed when `self.file` is dropped
        Ok(())
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        // WARNING: Dropping without writing "V" leaves the watchdog running.
        // The system will reboot if nothing else opens /dev/watchdog.
        // This is intentional -- if dcentrald crashes, we WANT the reboot.
        tracing::warn!("Watchdog dropped WITHOUT magic close — system will reboot if not reopened");
    }
}
