// SPDX-License-Identifier: GPL-3.0-or-later
//! Region airtime governor for the `$DCM` mesh — the firmware-policy duty-cycle /
//! dwell clamp the SX1262 driver deliberately does not enforce itself.
//!
//! Every transmit path (beacon, relay, ack, MCP-triggered send) MUST pass its
//! estimated airtime through a [`DutyCycle`] before keying the radio, so a busy
//! mesh cannot bust the EU 1% duty budget or the NA per-transmission dwell limit.
//!
//! Airtime is estimated with the standard Semtech LoRa formula (AN1200.13) as a
//! pure function ([`ModulationParams::airtime_ms`]) so the governor is fully
//! host-testable with no radio. The governor is a clock-free token bucket: the
//! caller passes a monotonic `now_ms` tick into [`DutyCycle::try_acquire`].

use crate::sx1262::Region;

/// EU 863–870 MHz SRD duty-cycle fraction (ETSI EN 300 220, 1%). Also applied as
/// a conservative courtesy budget on NA (which has no hard duty cap, only dwell).
pub const EU_DUTY_FRACTION: f64 = 0.01;

/// NA 902–928 MHz FCC 15.247 digital-modulation dwell limit per transmission, ms.
pub const NA_MAX_DWELL_MS: f64 = 400.0;

/// Initial + maximum accumulated airtime budget, ms. Bounds how much a node that
/// has been quiet can burst before the steady-state 1% refill governs it.
pub const DEFAULT_BUDGET_CAP_MS: f64 = 5_000.0;

/// LoRa modulation parameters needed to estimate on-air time. Physical values
/// (real spreading factor / bandwidth / coding rate), decoupled from the SX1262
/// register encoding so this stays a pure, testable calculation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModulationParams {
    /// Spreading factor, 5..=12.
    pub spreading_factor: u8,
    /// Bandwidth in Hz (e.g. 125_000).
    pub bandwidth_hz: u32,
    /// Coding-rate denominator, 5..=8 (i.e. 4/5 .. 4/8).
    pub coding_rate_denom: u8,
    /// Explicit header (`true`) vs implicit header (`false`).
    pub explicit_header: bool,
    /// Payload CRC appended.
    pub crc_on: bool,
    /// Preamble length in symbols.
    pub preamble_syms: u16,
}

impl Default for ModulationParams {
    /// The crate's default air profile: SF7 / BW 125 kHz / CR 4/5 / explicit
    /// header / CRC on / 8-symbol preamble (matches `sx1262::configure_lora`).
    fn default() -> Self {
        Self {
            spreading_factor: 7,
            bandwidth_hz: 125_000,
            coding_rate_denom: 5,
            explicit_header: true,
            crc_on: true,
            preamble_syms: 8,
        }
    }
}

impl ModulationParams {
    /// Symbol time (ms): `2^SF / BW`.
    pub fn symbol_time_ms(&self) -> f64 {
        (2f64.powi(self.spreading_factor as i32) / self.bandwidth_hz as f64) * 1000.0
    }

    /// Whether the low-data-rate optimization applies (symbol time > 16 ms — the
    /// SX1262 requirement at SF11/SF12 on 125 kHz). Affects airtime.
    pub fn low_data_rate_optimize(&self) -> bool {
        self.symbol_time_ms() > 16.0
    }

    /// Estimated on-air time in milliseconds for a `payload_len`-byte payload
    /// (Semtech AN1200.13). Clock-free and exact for the governor's needs.
    pub fn airtime_ms(&self, payload_len: usize) -> f64 {
        let sf = self.spreading_factor as f64;
        let tsym = self.symbol_time_ms();
        let de = if self.low_data_rate_optimize() {
            1.0
        } else {
            0.0
        };
        let cr = (self.coding_rate_denom as f64) - 4.0; // 4/5 -> 1 .. 4/8 -> 4
        let crc = if self.crc_on { 1.0 } else { 0.0 };
        let ih = if self.explicit_header { 0.0 } else { 1.0 };

        let numerator = 8.0 * payload_len as f64 - 4.0 * sf + 28.0 + 16.0 * crc - 20.0 * ih;
        let denom = 4.0 * (sf - 2.0 * de);
        let payload_sym = 8.0 + (numerator / denom).ceil().max(0.0) * (cr + 4.0);

        let t_preamble = (self.preamble_syms as f64 + 4.25) * tsym;
        let t_payload = payload_sym * tsym;
        t_preamble + t_payload
    }
}

/// A clock-free token-bucket airtime governor. Budget accrues at the region's
/// duty fraction of elapsed time (capped at [`DEFAULT_BUDGET_CAP_MS`]); a
/// transmission is admitted only if its airtime fits the budget AND does not
/// exceed the region's per-transmission dwell clamp.
#[derive(Debug, Clone)]
pub struct DutyCycle {
    region: Region,
    budget_ms: f64,
    cap_ms: f64,
    refill_fraction: f64,
    max_dwell_ms: f64,
    last_ms: Option<u64>,
    granted: u64,
    denied: u64,
}

impl DutyCycle {
    /// A governor for `region`, starting with a full burst budget so a freshly
    /// booted node can beacon immediately, then settling to the 1% refill.
    pub fn for_region(region: Region) -> Self {
        let max_dwell_ms = match region {
            Region::Na915 => NA_MAX_DWELL_MS,
            Region::Eu868 => f64::INFINITY, // EU has no dwell limit, only duty.
        };
        Self {
            region,
            budget_ms: DEFAULT_BUDGET_CAP_MS,
            cap_ms: DEFAULT_BUDGET_CAP_MS,
            refill_fraction: EU_DUTY_FRACTION,
            max_dwell_ms,
            last_ms: None,
            granted: 0,
            denied: 0,
        }
    }

    /// The region this governor enforces.
    pub fn region(&self) -> Region {
        self.region
    }

    /// Current spendable airtime budget, ms.
    pub fn remaining_ms(&self) -> f64 {
        self.budget_ms
    }

    /// Count of admitted / refused transmissions (telemetry for the dashboard).
    pub fn granted(&self) -> u64 {
        self.granted
    }
    pub fn denied(&self) -> u64 {
        self.denied
    }

    fn refill(&mut self, now_ms: u64) {
        if let Some(last) = self.last_ms {
            // saturating_sub tolerates a backward clock (skew) as zero elapsed.
            let dt = now_ms.saturating_sub(last) as f64;
            self.budget_ms = (self.budget_ms + dt * self.refill_fraction).min(self.cap_ms);
        }
        self.last_ms = Some(now_ms);
    }

    /// Try to admit a transmission of `airtime_ms` at time `now_ms`. Refuses when
    /// the airtime exceeds the region dwell clamp or the remaining budget; on
    /// success, spends the airtime from the budget. Returns whether it was
    /// admitted.
    pub fn try_acquire(&mut self, airtime_ms: f64, now_ms: u64) -> bool {
        self.refill(now_ms);
        if airtime_ms > self.max_dwell_ms || airtime_ms > self.budget_ms {
            self.denied += 1;
            return false;
        }
        self.budget_ms -= airtime_ms;
        self.granted += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    // ---- airtime estimator (hand-computed Semtech AN1200.13 reference points) ----

    #[test]
    fn airtime_sf7_125k_matches_reference() {
        // SF7 / BW125k / CR4:5 / explicit / CRC / 8-preamble / PL=20 -> 56.576 ms.
        let m = ModulationParams::default();
        assert!(
            approx(m.airtime_ms(20), 56.576, 0.05),
            "got {}",
            m.airtime_ms(20)
        );
    }

    #[test]
    fn airtime_sf9_125k_matches_reference() {
        // Same profile at SF9, PL=20 -> 185.344 ms.
        let m = ModulationParams {
            spreading_factor: 9,
            ..Default::default()
        };
        assert!(
            approx(m.airtime_ms(20), 185.344, 0.05),
            "got {}",
            m.airtime_ms(20)
        );
    }

    #[test]
    fn airtime_is_monotonic_in_payload_and_sf() {
        let m = ModulationParams::default();
        assert!(m.airtime_ms(50) > m.airtime_ms(10));
        let big_sf = ModulationParams {
            spreading_factor: 12,
            ..Default::default()
        };
        assert!(big_sf.airtime_ms(20) > m.airtime_ms(20));
    }

    #[test]
    fn low_data_rate_optimize_kicks_in_at_high_sf() {
        // 125 kHz: SF11 (16.384 ms symbol) enables LDRO; SF7 does not.
        assert!(ModulationParams {
            spreading_factor: 11,
            ..Default::default()
        }
        .low_data_rate_optimize());
        assert!(!ModulationParams::default().low_data_rate_optimize());
    }

    // ---- DutyCycle governor ----

    #[test]
    fn na_dwell_clamp_rejects_long_single_tx() {
        let mut na = DutyCycle::for_region(Region::Na915);
        // 500 ms single TX exceeds the 400 ms NA dwell limit even with budget.
        assert!(!na.try_acquire(500.0, 0));
        assert!(na.try_acquire(400.0, 0), "at the dwell limit is allowed");
        // EU has no dwell clamp: the same 500 ms fits the burst budget.
        let mut eu = DutyCycle::for_region(Region::Eu868);
        assert!(eu.try_acquire(500.0, 0));
    }

    #[test]
    fn budget_bounds_airtime_near_one_percent_over_an_hour() {
        let mut eu = DutyCycle::for_region(Region::Eu868);
        let mut total = 0.0;
        // Spam a 185 ms beacon every 100 ms tick for one hour.
        let mut now = 0u64;
        while now <= 3_600_000 {
            if eu.try_acquire(185.0, now) {
                total += 185.0;
            }
            now += 100;
        }
        // ~1% of an hour (36 s) plus the one-time burst cap (5 s) = ~41 s ceiling.
        assert!(total <= 42_000.0, "airtime {total} exceeded ~1% + burst");
        assert!(
            total >= 34_000.0,
            "governor over-throttled: only {total} ms"
        );
    }

    #[test]
    fn budget_starvation_recovers_after_idle() {
        let mut eu = DutyCycle::for_region(Region::Eu868);
        // Drain the burst budget.
        while eu.try_acquire(185.0, 0) {}
        assert!(!eu.try_acquire(185.0, 0), "budget exhausted");
        // After idle time the budget refills at 1% and a beacon fits again.
        assert!(
            eu.try_acquire(185.0, 60_000),
            "18.5 s at 1% > one 185 ms frame"
        );
    }

    #[test]
    fn backward_clock_does_not_credit_budget_or_panic() {
        let mut eu = DutyCycle::for_region(Region::Eu868);
        assert!(eu.try_acquire(185.0, 1_000_000));
        let before = eu.remaining_ms();
        // now goes backward → treated as zero elapsed, no phantom credit.
        assert!(eu.try_acquire(10.0, 500_000));
        assert!(eu.remaining_ms() <= before);
    }

    #[test]
    fn granted_denied_counters_track() {
        let mut na = DutyCycle::for_region(Region::Na915);
        assert!(na.try_acquire(100.0, 0));
        assert!(!na.try_acquire(500.0, 0)); // dwell reject
        assert_eq!((na.granted(), na.denied()), (1, 1));
    }
}
