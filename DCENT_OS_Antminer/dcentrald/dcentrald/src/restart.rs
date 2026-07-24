use std::time::Duration;

/// Automatic process replacement is suspended until a daemon can publish a
/// durable, typed disposition receipt for every hardware resource it owns.
/// Returning `false` preserves the current call-site contract while ensuring
/// recovery code cannot create an unresolved session and then falsely claim a
/// restart was scheduled.
pub(crate) fn schedule_daemon_restart(reason: &str, delay: Duration) -> bool {
    tracing::error!(
        reason,
        requested_delay_ms = delay.as_millis(),
        "Automatic daemon restart refused: no typed hardware disposition receipt is available"
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_restart_is_refused_without_typed_disposition() {
        assert!(!schedule_daemon_restart("test", Duration::from_millis(1)));
    }
}
