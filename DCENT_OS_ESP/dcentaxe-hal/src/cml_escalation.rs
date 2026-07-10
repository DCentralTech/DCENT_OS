//! Pure two-strike CML fault-escalation window for the TPS546 power path.
//!
//! Extracted from `power.rs::Tps546::check_fault` so the escalation decision is
//! host-unit-testable without the ESP-IDF toolchain (power.rs links esp-idf and
//! only compiles for the espidf target). No hardware/ESP-IDF deps live here.

/// One poll's classification feeding the CML escalation window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmlEvent {
    /// STATUS_WORD read clean (== 0) this poll.
    Clean,
    /// A recoverable CML-class fault (isolated CML, or phantom VOUT_OV+CML)
    /// was observed and CLEAR_FAULTS'd this poll.
    Cml,
}

/// Two-strike window length (ms). Preserves the original 60_000 constant.
pub const WINDOW_MS: u64 = 60_000;

/// Result of advancing the window by one poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CmlDecision {
    /// Persist into `cml_fault_count`.
    pub new_count: u8,
    /// Persist into `cml_window_start_ms` (0 = no active window).
    pub new_window_start_ms: u64,
    /// 2nd+ strike inside the window — caller returns RegulatorFault.
    pub escalate: bool,
    /// Strike count that tripped escalation (for the log msg); 0 otherwise.
    pub strikes: u8,
}

/// Advance the escalation window. HALPWR-1: the window ages purely by
/// wall-clock; a *clean* poll inside an active window must NOT reset the
/// strike counter (old code zeroed it every clean STATUS_WORD, and the
/// 1st-strike CLEAR_FAULTS guaranteed the next poll read clean -> the 2nd
/// strike could never land). Strikes are forgotten only once the window has
/// fully elapsed. Boundary semantics match the original CML path: a poll is
/// still "in window" when `now - start <= window_ms` (original used the
/// inverse `> window_ms` to mean expired).
pub fn advance_cml_window(
    event: CmlEvent,
    now_ms: u64,
    window_start_ms: u64,
    fault_count: u8,
    window_ms: u64,
) -> CmlDecision {
    let window_active = window_start_ms != 0 && now_ms.saturating_sub(window_start_ms) <= window_ms;
    match event {
        CmlEvent::Clean => {
            if window_active {
                CmlDecision {
                    new_count: fault_count,
                    new_window_start_ms: window_start_ms,
                    escalate: false,
                    strikes: 0,
                }
            } else {
                CmlDecision {
                    new_count: 0,
                    new_window_start_ms: 0,
                    escalate: false,
                    strikes: 0,
                }
            }
        }
        CmlEvent::Cml => {
            let (count, start) = if window_active {
                (fault_count.saturating_add(1), window_start_ms)
            } else {
                (1, now_ms)
            };
            if count >= 2 {
                CmlDecision {
                    new_count: 0,
                    new_window_start_ms: 0,
                    escalate: true,
                    strikes: count,
                }
            } else {
                CmlDecision {
                    new_count: count,
                    new_window_start_ms: start,
                    escalate: false,
                    strikes: 0,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the magic window constant — must stay 60s to match the original
    /// power.rs `const WINDOW_MS: u64 = 60_000;`.
    #[test]
    fn window_ms_is_60s() {
        assert_eq!(WINDOW_MS, 60_000);
    }

    /// First CML opens the window at 1 strike, no escalation yet.
    #[test]
    fn first_cml_opens_window_no_escalate() {
        let dec = advance_cml_window(CmlEvent::Cml, 1000, 0, 0, WINDOW_MS);
        assert_eq!(dec.new_count, 1);
        assert_eq!(dec.new_window_start_ms, 1000);
        assert!(!dec.escalate);
        assert_eq!(dec.strikes, 0);
    }

    /// THE regression guard (HALPWR-1): a clean poll inside the active window
    /// must NOT zero the strike counter. Old code reset it every clean read.
    #[test]
    fn clean_poll_inside_window_keeps_strike() {
        let dec = advance_cml_window(CmlEvent::Clean, 6000, 1000, 1, WINDOW_MS);
        assert_eq!(
            dec.new_count, 1,
            "clean poll inside window must keep the strike"
        );
        assert_eq!(dec.new_window_start_ms, 1000);
        assert!(!dec.escalate);
        assert_eq!(dec.strikes, 0);
    }

    /// Required spec: CML -> clean (strike survives) -> CML within 60s escalates.
    #[test]
    fn cml_clean_cml_within_60s_escalates() {
        // strike 1
        let d1 = advance_cml_window(CmlEvent::Cml, 1000, 0, 0, WINDOW_MS);
        assert_eq!(d1.new_count, 1);
        assert_eq!(d1.new_window_start_ms, 1000);
        // clean poll inside window keeps the strike alive
        let d2 = advance_cml_window(
            CmlEvent::Clean,
            10_000,
            d1.new_window_start_ms,
            d1.new_count,
            WINDOW_MS,
        );
        assert_eq!(d2.new_count, 1);
        assert_eq!(d2.new_window_start_ms, 1000);
        // strike 2 inside window -> escalate
        let d3 = advance_cml_window(
            CmlEvent::Cml,
            30_000,
            d2.new_window_start_ms,
            d2.new_count,
            WINDOW_MS,
        );
        assert!(d3.escalate);
        assert_eq!(d3.strikes, 2);
        assert_eq!(d3.new_count, 0);
        assert_eq!(d3.new_window_start_ms, 0);
    }

    /// Required spec: a CML gap over 60s forgets the strike and does NOT escalate.
    #[test]
    fn cml_gap_over_60s_no_escalate() {
        // strike 1 at t=1000
        let d1 = advance_cml_window(CmlEvent::Cml, 1000, 0, 0, WINDOW_MS);
        // clean poll at t=70000: now - start = 69000 > 60000 -> window expired
        let d2 = advance_cml_window(
            CmlEvent::Clean,
            70_000,
            d1.new_window_start_ms,
            d1.new_count,
            WINDOW_MS,
        );
        assert_eq!(d2.new_count, 0);
        assert_eq!(d2.new_window_start_ms, 0);
        // CML at t=71000 opens a fresh window at 1 strike, no escalation
        let d3 = advance_cml_window(
            CmlEvent::Cml,
            71_000,
            d2.new_window_start_ms,
            d2.new_count,
            WINDOW_MS,
        );
        assert!(!d3.escalate);
        assert_eq!(d3.new_count, 1);
        assert_eq!(d3.new_window_start_ms, 71_000);
    }

    /// Two faulting polls back-to-back (no clean between) still escalate —
    /// preserves the original consecutive-poll behaviour.
    #[test]
    fn two_cml_back_to_back_escalates() {
        let d1 = advance_cml_window(CmlEvent::Cml, 1000, 0, 0, WINDOW_MS);
        let d2 = advance_cml_window(
            CmlEvent::Cml,
            2000,
            d1.new_window_start_ms,
            d1.new_count,
            WINDOW_MS,
        );
        assert!(d2.escalate);
        assert_eq!(d2.strikes, 2);
    }

    /// Boundary: exactly 60s after the window start is still "in window"
    /// (matches the original `> WINDOW_MS` == expired semantics).
    #[test]
    fn boundary_exactly_60s_still_in_window() {
        // now - start == 60000 == WINDOW_MS -> NOT expired -> increment
        let dec = advance_cml_window(CmlEvent::Cml, 61_000, 1000, 1, WINDOW_MS);
        assert!(dec.escalate);
        assert_eq!(dec.strikes, 2);
    }

    /// A clean poll with no active window is a no-op (stays 0/0).
    #[test]
    fn clean_with_no_window_is_noop() {
        let dec = advance_cml_window(CmlEvent::Clean, 5000, 0, 0, WINDOW_MS);
        assert_eq!(dec.new_count, 0);
        assert_eq!(dec.new_window_start_ms, 0);
        assert!(!dec.escalate);
    }
}
