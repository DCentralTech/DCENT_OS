//! W13.D1: live cold-boot phase emitter (host-safe API surface).
//!
//! Holds the `tokio::sync::watch` channel that the cold-boot orchestrators
//! (`dcentrald-hal::platform::cvitek::cvitek_cold_boot`,
//! `dcentrald-hal::platform::beaglebone::beaglebone_cold_boot`, etc.)
//! publish into. The `/api/boot/phase` handler reads the current value;
//! `/api/boot/timeline` reads the bounded ring of recent transitions.
//!
//! # Wiring status
//! W13.D1 ships the API surface ONLY. The cold-boot orchestrators do
//! NOT yet publish into this watch channel — that is wired in W14 once
//! the platform-dispatch refactor lands. Today the tracker exposes the
//! sender so a future wave can drop in `tracker.publish(phase)` from the
//! cold-boot fast paths without changing the API handler shape.
//!
//! # Cross-references
//! - See `~/
//! - See `~/
//! -

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::watch;

use dcentrald_api_types::boot_phase::{BootPhase, BootTimelineEntry, GenericBootPhase};

/// Bounded ring depth for the boot-timeline. 32 entries is enough for
/// the 6 CV1835 substates × ~2 cold-boot cycles + headroom — bigger
/// rings just hide bugs.
pub const BOOT_TIMELINE_CAPACITY: usize = 32;

/// Tracker for the live cold-boot phase + bounded transition ring.
pub struct BootPhaseTracker {
    /// Watch sender — orchestrators call `publish(phase)` to broadcast.
    tx: watch::Sender<BootPhase>,
    /// Watch receiver — exposed to the API handler via `subscribe()`.
    rx: watch::Receiver<BootPhase>,
    /// Wall-clock unix-ms when the current phase was entered.
    started_at_unix_ms: Mutex<Option<u64>>,
    /// Bounded ring of recent transitions (oldest-first).
    timeline: Mutex<Vec<BootTimelineEntry>>,
}

impl BootPhaseTracker {
    pub fn new() -> Self {
        let initial = BootPhase::Generic(GenericBootPhase::Booting);
        let (tx, rx) = watch::channel(initial);
        Self {
            tx,
            rx,
            started_at_unix_ms: Mutex::new(None),
            timeline: Mutex::new(Vec::with_capacity(BOOT_TIMELINE_CAPACITY)),
        }
    }

    /// Returns a watch receiver for the live phase. The API handler
    /// reads the current value via `rx.borrow()`.
    pub fn subscribe(&self) -> watch::Receiver<BootPhase> {
        self.rx.clone()
    }

    /// Publish a new phase. Idempotent — sending the same phase twice
    /// in a row is a no-op (no spurious timeline entry, no cache churn).
    /// Used by cold-boot orchestrators to emit phase transitions.
    pub fn publish(&self, phase: BootPhase) {
        let now_ms = now_unix_ms();
        let prev = *self.tx.borrow();
        if prev == phase {
            return;
        }
        // Close out the previous timeline entry, then push the new one.
        let mut timeline = self.timeline.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(last) = timeline.last_mut() {
            if last.ended_at_unix_ms.is_none() {
                last.ended_at_unix_ms = Some(now_ms);
            }
        }
        // Bound the ring.
        if timeline.len() >= BOOT_TIMELINE_CAPACITY {
            timeline.remove(0);
        }
        timeline.push(BootTimelineEntry {
            phase,
            started_at_unix_ms: now_ms,
            ended_at_unix_ms: None,
        });
        drop(timeline);

        *self
            .started_at_unix_ms
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(now_ms);
        // Best-effort send — we don't care if there are no subscribers
        // (the API handler may not have spun up yet on early cold boot).
        let _ = self.tx.send(phase);
    }

    /// Read the current phase + entry timestamp. Used by `/api/boot/phase`.
    pub fn current(&self) -> (BootPhase, Option<u64>) {
        let phase = *self.rx.borrow();
        let started_at = *self
            .started_at_unix_ms
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        (phase, started_at)
    }

    /// Snapshot the bounded timeline ring. Used by `/api/boot/timeline`.
    pub fn timeline_snapshot(&self) -> Vec<BootTimelineEntry> {
        self.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Default for BootPhaseTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_api_types::boot_phase::Cv1835BootPhase;

    #[test]
    fn default_phase_is_generic_booting() {
        let t = BootPhaseTracker::new();
        let (phase, started) = t.current();
        assert_eq!(phase, BootPhase::Generic(GenericBootPhase::Booting));
        // Default phase isn't a publish — no timestamp recorded.
        assert_eq!(started, None);
    }

    #[test]
    fn publish_records_phase_and_timestamp() {
        let t = BootPhaseTracker::new();
        t.publish(BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit));
        let (phase, started) = t.current();
        assert_eq!(phase, BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit));
        assert!(started.is_some());
    }

    #[test]
    fn duplicate_publish_is_idempotent() {
        let t = BootPhaseTracker::new();
        t.publish(BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit));
        t.publish(BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit));
        let timeline = t.timeline_snapshot();
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn publish_closes_previous_timeline_entry() {
        let t = BootPhaseTracker::new();
        t.publish(BootPhase::Cv1835(Cv1835BootPhase::BootPsuInit));
        t.publish(BootPhase::Cv1835(Cv1835BootPhase::BootPicDcDcEnable));
        let timeline = t.timeline_snapshot();
        assert_eq!(timeline.len(), 2);
        assert!(timeline[0].ended_at_unix_ms.is_some());
        assert_eq!(timeline[1].ended_at_unix_ms, None);
    }

    #[test]
    fn timeline_ring_is_bounded() {
        let t = BootPhaseTracker::new();
        for i in 0..(BOOT_TIMELINE_CAPACITY + 5) {
            // Alternate between two distinct generic phases so each
            // publish creates a fresh timeline entry.
            let g = if i % 2 == 0 {
                GenericBootPhase::Booting
            } else {
                GenericBootPhase::Mining
            };
            t.publish(BootPhase::Generic(g));
        }
        let timeline = t.timeline_snapshot();
        assert_eq!(timeline.len(), BOOT_TIMELINE_CAPACITY);
    }
}
