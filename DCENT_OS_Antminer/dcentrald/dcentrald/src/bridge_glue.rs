//! Glue between the daemon and the no-HAL `dcentrald-bridge` crate.
//!
//! `dcentrald-bridge` deliberately does not depend on `dcentrald-api` (so it
//! stays a light, cross-friendly leaf crate). The daemon supplies the two
//! ports the bridge task needs:
//!
//! - [`RoomTempSinkAdapter`] — writes the bridge's external temperature into
//!   the EXISTING `AppState.room_temp_c10` atomic (the same slot the thermal
//!   PID and `POST /api/home/room-temp` use). No new sensor abstraction.
//! - [`MinerStatusAdapter`] — snapshots live miner state (uptime / mode /
//!   heating / chip temp / wall watts) for the heartbeat body + telemetry
//!   cadence, from the daemon's existing `watch` channels.
//!
//! Identity (`device_id` / `miner_mac` / `hostname` / `model`) and the unit
//! secret are assembled in [`build_runtime`].

use std::sync::atomic::Ordering;
use std::sync::Arc;

use dcentrald_api::{AppState, MinerState, OperatingMode};
use dcentrald_autotuner::{LivePowerEstimate, PowerAuthorityKind};
use dcentrald_bridge::{BridgeRuntime, MinerStatusProvider, RoomTempSink, UnitSecret};
use tokio::sync::watch;

/// Env var carrying the bridge unit secret as base32 (RFC 4648, no pad) for
/// v0.1 bring-up. Once DCENT_OS grows a QR-provisioning NVS path this is where
/// the secret would instead be loaded. Absent ⇒ the task logs and exits, and
/// the heater falls back to internal sensors.
const UNIT_SECRET_ENV: &str = "DCENT_BRIDGE_UNIT_SECRET";

/// Writes into the existing `room_temp_c10` atomic on `AppState`.
pub struct RoomTempSinkAdapter {
    state: Arc<AppState>,
}

impl RoomTempSinkAdapter {
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

impl RoomTempSink for RoomTempSinkAdapter {
    fn set_room_temp_c10(&self, value: u32) {
        // Same encoding + ordering the REST handler and thermal PID use.
        self.state.room_temp_c10.store(value, Ordering::Relaxed);
    }
}

/// Snapshots live miner state for the bridge heartbeat + telemetry cadence.
pub struct MinerStatusAdapter {
    state_rx: watch::Receiver<MinerState>,
    power_rx: watch::Receiver<LivePowerEstimate>,
}

impl MinerStatusAdapter {
    pub fn new(
        state_rx: watch::Receiver<MinerState>,
        power_rx: watch::Receiver<LivePowerEstimate>,
    ) -> Self {
        Self { state_rx, power_rx }
    }

    fn mode(&self) -> OperatingMode {
        self.state_rx.borrow().mode
    }
}

/// Wall power to report over the ExpansionPack bridge heartbeat, or `None`.
///
/// The heartbeat carries power as a bare `uint16` watt with no provenance field,
/// so unlike the WebSocket scalar (`power_modeled:true`) and the Home Assistant
/// sensor (gated on `live_power_available`), the bridge cannot label a value as
/// modeled-vs-measured. To uphold the load-bearing "never present a modeled
/// estimate as measured power" invariant, we only pass values whose source is a
/// real sensor (PMBus/ADC) and suppress everything else to `None`.
///
/// NOTE — intentionally dark today: the `LivePowerEstimate` watch channel that
/// feeds the bridge currently only ever carries modeled sources ("estimated" /
/// "curtailment"), because no production publisher wires PMBus/ADC telemetry
/// into it yet. So this returns `None` on every platform until measured power
/// is plumbed into that channel — at which point the measured pass-through below
/// lights up with no further change. Suppressing to `None` is safe for every
/// bridge consumer (absent power_w is treated as "not reported", not 0 W).
fn measured_bridge_power_w(power: &LivePowerEstimate) -> Option<u16> {
    if !power.wall_watts.is_finite() || power.wall_watts <= 0.0 {
        return None;
    }
    let authority = PowerAuthorityKind::from_source(&power.source, power.calibrated);
    if !authority.is_measured() {
        return None;
    }

    // Bridge clamps to uint16; clamp here too so we never wrap.
    Some(power.wall_watts.clamp(0.0, u16::MAX as f64) as u16)
}

impl MinerStatusProvider for MinerStatusAdapter {
    fn uptime_s(&self) -> u64 {
        self.state_rx.borrow().uptime_s
    }

    fn mode(&self) -> String {
        // Map the daemon's OperatingMode to the bridge's free-form `mode`
        // string (the bridge stores it verbatim and surfaces it in telemetry).
        match self.mode() {
            OperatingMode::Home => "mining_heating".to_string(),
            OperatingMode::Standard => "mining".to_string(),
            OperatingMode::Hacker => "mining".to_string(),
        }
    }

    fn is_heating(&self) -> bool {
        // Home (space-heater) mode is the heating posture that warrants the
        // 10 s fast telemetry poll.
        matches!(self.mode(), OperatingMode::Home)
    }

    fn miner_temperature_c(&self) -> Option<f32> {
        // Hottest reporting chain temperature, if any chain is reporting.
        let st = self.state_rx.borrow();
        st.chains
            .iter()
            .map(|c| c.temp_c)
            .filter(|t| t.is_finite() && *t > 0.0)
            .fold(None, |acc, t| match acc {
                Some(m) if m >= t => Some(m),
                _ => Some(t),
            })
    }

    fn power_w(&self) -> Option<u16> {
        measured_bridge_power_w(&self.power_rx.borrow())
    }

    // --- V0.2 Change-B expanded telemetry (MESH_MODULE.md §2 field contract) --

    fn hashrate_ths(&self) -> Option<f64> {
        // The heartbeat carries hashrate in TH/s; MinerState tracks GH/s. Only
        // report a real, positive, finite rate (suppress the pre-first-share 0).
        let ghs = self.state_rx.borrow().hashrate_ghs;
        if ghs.is_finite() && ghs > 0.0 {
            Some(ghs / 1000.0)
        } else {
            None
        }
    }

    fn shares(&self) -> Option<(u64, u64)> {
        let st = self.state_rx.borrow();
        Some((st.accepted, st.rejected))
    }

    fn fan_speed_rpm(&self) -> Option<u32> {
        // Report only a spinning tach; 0 RPM means "no reading / not connected",
        // which the bridge treats as absent (keeps its placeholder) rather than
        // "fans stopped".
        let rpm = self.state_rx.borrow().fans.rpm;
        if rpm > 0 {
            Some(rpm)
        } else {
            None
        }
    }

    /// COMMANDED chain frequency in MHz — the frequency DCENT_OS is DRIVING the
    /// chain at (the autotuner/config target from `ChainState::frequency_mhz`),
    /// NOT a measured silicon frequency. The bridge surfaces it as a SET/RES
    /// frequency echo. Reports the first chain that is commanded above 0 MHz.
    fn asic_frequency_mhz(&self) -> Option<f32> {
        let st = self.state_rx.borrow();
        st.chains
            .iter()
            .map(|c| c.frequency_mhz)
            .find(|f| *f > 0)
            .map(|f| f as f32)
    }

    /// Measured ASIC voltage — intentionally `None`. `ChainState::voltage_mv`
    /// is a COMMANDED rail setpoint, not a measured reading; presenting it as a
    /// measured `RES voltage` would violate the same "never label a modeled
    /// value as measured" provenance discipline as `measured_bridge_power_w`
    /// (and MESH_MODULE.md §2: dcentrald keeps voltage `None` until a measured
    /// source exists). Stays suppressed until real ADC/PMBus voltage is plumbed.
    fn asic_voltage_v(&self) -> Option<f32> {
        None
    }

    // block_height / best_difficulty / block_found_height are left at the trait
    // default `None`.
    // TODO(W4.5): derive block_height (BIP34 coinbase height), best_difficulty
    // (best-share tracker), and block_found_height (block-found detector) — out
    // of scope for W4.4; sourced as `None` until those derivations land.
}

/// Assemble the [`BridgeRuntime`] identity from config + system facts.
///
/// - `miner_mac`  : `/sys/class/net/eth0/address`, uppercased (spec §2.2).
/// - `device_id`  : `dcentos-<model>-<mac-no-colons-lowercase>` (stable per unit).
/// - `hostname`   : `/etc/hostname` (mDNS-safe display name).
/// - `model`      : `[mining].model` if set, else `"dcentos-miner"`.
/// - `unit_secret`: base32 from `DCENT_BRIDGE_UNIT_SECRET`, else `None`.
pub fn build_runtime(model: Option<&str>, api_port: u16) -> BridgeRuntime {
    let miner_mac = std::fs::read_to_string("/sys/class/net/eth0/address")
        .map(|s| s.trim().to_uppercase())
        .unwrap_or_else(|_| "00:00:00:00:00:00".to_string());

    let hostname = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dcentos".to_string());

    let model = model
        .map(|m| m.to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "dcentos-miner".to_string());

    let mac_compact = miner_mac.replace(':', "").to_lowercase();
    let model_slug = model
        .to_lowercase()
        .replace([' ', '/'], "-")
        .replace("antminer-", "");
    let device_id = format!("dcentos-{}-{}", model_slug, mac_compact);

    let unit_secret = load_unit_secret();

    BridgeRuntime {
        device_id,
        miner_mac,
        model,
        hostname,
        api_port,
        unit_secret,
    }
}

/// Load the unit secret from the env var, decoding base32-no-pad to 32 bytes.
/// Returns `None` (with a debug log) on absence or a malformed value rather
/// than panicking — an unprovisioned unit simply does not pair.
fn load_unit_secret() -> Option<UnitSecret> {
    let raw = std::env::var(UNIT_SECRET_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match dcentrald_bridge::unit_secret_from_base32(trimmed) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!(error = %e, "DCENT_BRIDGE_UNIT_SECRET set but not valid base32-32B; ignoring");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_stable_and_sane() {
        // build_runtime reads /sys + /etc which won't exist on the dev host;
        // it must still produce a well-formed device_id without panicking.
        let rt = build_runtime(Some("Antminer S19j Pro"), 80);
        assert!(rt.device_id.starts_with("dcentos-"));
        assert_eq!(rt.api_port, 80);
        // model slug strips the "antminer-" prefix and lowercases.
        assert!(rt.device_id.contains("s19j-pro"));
    }

    #[test]
    fn missing_model_falls_back() {
        let rt = build_runtime(None, 8080);
        assert!(rt.device_id.starts_with("dcentos-dcentos-miner-"));
        assert_eq!(rt.model, "dcentos-miner");
    }

    fn adapter_with_power(power: LivePowerEstimate) -> MinerStatusAdapter {
        let (_state_tx, state_rx) = watch::channel(MinerState::empty(OperatingMode::Home));
        let (_power_tx, power_rx) = watch::channel(power);
        MinerStatusAdapter::new(state_rx, power_rx)
    }

    fn adapter_with_state(state: MinerState) -> MinerStatusAdapter {
        let (_state_tx, state_rx) = watch::channel(state);
        let (_power_tx, power_rx) = watch::channel(LivePowerEstimate::default());
        MinerStatusAdapter::new(state_rx, power_rx)
    }

    fn chain_with(frequency_mhz: u16, voltage_mv: u16) -> dcentrald_api::ChainState {
        dcentrald_api::ChainState {
            id: 6,
            chips: 63,
            frequency_mhz,
            voltage_mv,
            temp_c: 50.0,
            temp_source: None,
            hashrate_ghs: 1_000.0,
            errors: 0,
            status: "mining".to_string(),
        }
    }

    #[test]
    fn bridge_hashrate_ths_converts_ghs_to_ths() {
        let mut st = MinerState::empty(OperatingMode::Home);
        st.hashrate_ghs = 25_000.0;
        assert_eq!(adapter_with_state(st).hashrate_ths(), Some(25.0));
    }

    #[test]
    fn bridge_hashrate_ths_none_when_not_positive_or_finite() {
        // Fresh miner (0 GH/s) and NaN both suppress to None.
        assert_eq!(
            adapter_with_state(MinerState::empty(OperatingMode::Home)).hashrate_ths(),
            None
        );
        let mut nan = MinerState::empty(OperatingMode::Home);
        nan.hashrate_ghs = f64::NAN;
        assert_eq!(adapter_with_state(nan).hashrate_ths(), None);
    }

    #[test]
    fn bridge_shares_reports_accepted_and_rejected() {
        let mut st = MinerState::empty(OperatingMode::Home);
        st.accepted = 12_044;
        st.rejected = 7;
        assert_eq!(adapter_with_state(st).shares(), Some((12_044, 7)));
    }

    #[test]
    fn bridge_fan_speed_rpm_gated_above_zero() {
        let mut spinning = MinerState::empty(OperatingMode::Home);
        spinning.fans.rpm = 5_400;
        assert_eq!(adapter_with_state(spinning).fan_speed_rpm(), Some(5_400));
        // 0 RPM (no reading) → absent, not "stopped".
        assert_eq!(
            adapter_with_state(MinerState::empty(OperatingMode::Home)).fan_speed_rpm(),
            None
        );
    }

    #[test]
    fn bridge_asic_frequency_mhz_reports_first_commanded_chain() {
        let mut st = MinerState::empty(OperatingMode::Home);
        st.chains.push(chain_with(525, 1_150));
        assert_eq!(adapter_with_state(st).asic_frequency_mhz(), Some(525.0));
        // No chains ⇒ nothing commanded ⇒ None.
        assert_eq!(
            adapter_with_state(MinerState::empty(OperatingMode::Home)).asic_frequency_mhz(),
            None
        );
    }

    #[test]
    fn bridge_asic_voltage_v_omits_runtime_modeled_voltage() {
        // The chain carries a commanded voltage_mv (1150 mV), but the bridge must
        // NOT present it as a measured ASIC voltage — mirror the measured-power
        // provenance discipline (bridge_power_w_omits_runtime_modeled_power).
        let mut st = MinerState::empty(OperatingMode::Home);
        st.chains.push(chain_with(525, 1_150));
        assert_eq!(adapter_with_state(st).asic_voltage_v(), None);
    }

    #[test]
    fn bridge_w4_5_fields_stay_none_until_derived() {
        // block_height / best_difficulty / block_found_height are out of scope
        // for W4.4 and sourced as None until the W4.5 derivations land.
        let mut st = MinerState::empty(OperatingMode::Home);
        st.chains.push(chain_with(525, 1_150));
        let adapter = adapter_with_state(st);
        assert_eq!(adapter.block_height(), None);
        assert_eq!(adapter.best_difficulty(), None);
        assert_eq!(adapter.block_found_height(), None);
    }

    #[test]
    fn bridge_power_w_omits_runtime_modeled_power() {
        let adapter = adapter_with_power(LivePowerEstimate {
            wall_watts: 1_250.0,
            source: "estimated".to_string(),
            ..LivePowerEstimate::default()
        });

        assert_eq!(adapter.power_w(), None);
    }

    #[test]
    fn bridge_power_w_omits_wall_calibrated_estimate() {
        let adapter = adapter_with_power(LivePowerEstimate {
            wall_watts: 1_250.0,
            source: "estimated".to_string(),
            calibrated: true,
            ..LivePowerEstimate::default()
        });

        assert_eq!(adapter.power_w(), None);
    }

    #[test]
    fn bridge_power_w_reports_measured_wall_power() {
        // Forward-looking contract: once a real PMBus/ADC source is wired into
        // the LivePowerEstimate channel, the bridge passes it through. No
        // production publisher emits a measured source today (see
        // measured_bridge_power_w), so this pins the intended behavior for when
        // that telemetry lands rather than any current runtime path.
        let adapter = adapter_with_power(LivePowerEstimate {
            wall_watts: 1_234.6,
            source: "pmbus".to_string(),
            ..LivePowerEstimate::default()
        });

        assert_eq!(adapter.power_w(), Some(1_234));
    }
}
