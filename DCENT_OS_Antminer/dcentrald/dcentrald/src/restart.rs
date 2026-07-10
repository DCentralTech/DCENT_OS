use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static RESTART_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// The installed init-script name. Every Buildroot overlay ships
/// `/etc/init.d/S82dcentrald` (verified across zynq / amlogic / beaglebone /
/// cvitek / am2-*); there is NO `/etc/init.d/dcentrald` symlink anywhere. The
/// prior hardcoded `/etc/init.d/dcentrald` therefore failed with "No such file
/// or directory", so the daemon-initiated restart — the SOLE auto-recovery path
/// for the NoPic (S21 / S19j Pro Amlogic / S19K Pro) thermal cooldown, which
/// does `disable_psu(); schedule_daemon_restart(); break;` and exits Ok(0) — was
/// silently dead on every shipped image (a unit powered down safely after a
/// thermal event but stayed down forever). (gap-swarm daemon-startup #5)
const RESTART_INIT_SCRIPT: &str = "/etc/init.d/S82dcentrald";

/// Build the detached restart shell command. Pure + unit-testable so the
/// init-script path can never silently drift from the installed name again (#7).
///
/// Runs `<script> restart`; if the script is somehow absent it falls back to
/// `kill -TERM <self_pid>` so procd / the S82dcentrald crash-wrapper can respawn
/// (matches the proven rest.rs recovery fallback). `RESTART_SCHEDULED` guards
/// idempotency at the caller.
///
/// WAVE 0 STABILIZE (2026-06-05): the API control plane has a MIRROR of this
/// logic in `dcentrald-api::rest::build_daemon_restart_command` /
/// `DAEMON_RESTART_INIT_SCRIPT` (the API crate cannot depend on this binary
/// crate — the dependency edge runs the other way). Both must keep targeting
/// `/etc/init.d/S82dcentrald`; the old API path spawned the nonexistent
/// `/etc/init.d/dcentrald`, which always failed → clean SIGTERM exit → the
/// supervisor stopped respawning → the "Restart" button left the unit dead.
/// If you change the init-script name here, change it there too.
fn build_restart_command(delay_s: u64, self_pid: u32) -> String {
    let core = format!(
        "{RESTART_INIT_SCRIPT} restart >/tmp/dcentrald_restart_cmd.log 2>&1 || kill -TERM {self_pid}"
    );
    if delay_s == 0 {
        core
    } else {
        format!("sleep {delay_s}; {core}")
    }
}

pub(crate) fn schedule_daemon_restart(reason: &str, delay: Duration) -> bool {
    if RESTART_SCHEDULED.swap(true, Ordering::AcqRel) {
        tracing::info!(reason, "Daemon restart already scheduled");
        return false;
    }

    let delay_s = delay.as_secs().max(u64::from(delay.subsec_nanos() > 0));
    let _ = std::fs::write("/tmp/dcentrald_restart", reason);

    let command = build_restart_command(delay_s, std::process::id());

    match Command::new("/bin/sh").args(["-c", &command]).spawn() {
        Ok(_) => {
            tracing::warn!(reason, delay_s, "Scheduled daemon restart via init.d");
            true
        }
        Err(e) => {
            RESTART_SCHEDULED.store(false, Ordering::Release);
            tracing::error!(reason, error = %e, "Failed to schedule daemon restart via init.d");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_command_targets_installed_init_script_not_the_dead_path() {
        let cmd = build_restart_command(5, 1234);
        // MUST target the INSTALLED script (S82dcentrald), not the nonexistent
        // /etc/init.d/dcentrald that silently disabled thermal auto-recovery.
        assert!(
            cmd.contains("/etc/init.d/S82dcentrald restart"),
            "restart must target the installed init script: {cmd}"
        );
        assert!(
            !cmd.contains("/etc/init.d/dcentrald "),
            "must NOT target the nonexistent /etc/init.d/dcentrald: {cmd}"
        );
        // SIGTERM respawn fallback if the script is somehow absent.
        assert!(
            cmd.contains("kill -TERM 1234"),
            "must have the SIGTERM fallback: {cmd}"
        );
        // Delay prefixes a sleep; zero delay does not.
        assert!(
            cmd.starts_with("sleep 5; "),
            "delay must prefix a sleep: {cmd}"
        );
        assert!(
            !build_restart_command(0, 1).starts_with("sleep"),
            "zero delay must not sleep"
        );
    }
}
