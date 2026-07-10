//! The long-running `bridge_client_task`.
//!
//! Lifecycle (cancellation-aware throughout):
//!   1. Gateway-watch — poll `ip -4 route show default`; fire only when the
//!      default gateway is the bridge subnet IP (`10.77.0.1`) or the operator's
//!      `gateway_override`.
//!   2. Discover — `probe_health` (product == "dcent-pack"), telemetry-regex
//!      fallback if `/api/v1/health` 404s.
//!   3. Pair — `pair_with_retry` (spec §2.5 policy).
//!   4. Serve — concurrent heartbeat (60 s) + telemetry (10 s heating / 60 s
//!      idle). On a USABLE external temperature AND the bridge reporting
//!      `accessories.temperature_feedback.enabled == true` AND `cfg.feed_thermal`,
//!      write `(temp_c*10) as u32` (clamped to [0,80] *10) into the EXISTING
//!      `room_temp_c10` atomic via the [`RoomTempSink`] port. On staleness /
//!      loss, STOP writing (the heater falls back to internal sensors).
//!
//! The crate stays no-HAL / no-`dcentrald-api`: the daemon supplies the
//! `room_temp_c10` write path through the tiny [`RoomTempSink`] trait + a
//! [`BridgeRuntime`] context for the identity / mode / secret it needs.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::client::{BridgeClient, HeartbeatOutcome};
use crate::config::BridgeConfig;
use crate::crypto::UnitSecret;
use crate::error::BridgeError;
use crate::protocol::HeartbeatRequest;

/// The bridge subnet gateway IP that identifies a candidate bridge (spec §1.2).
pub const BRIDGE_GATEWAY_IP: &str = "10.77.0.1";

/// Port through which the bridge task writes the external temperature into the
/// daemon's shared state. Implemented by the daemon for `AppState` (writes the
/// existing `room_temp_c10` atomic) so this leaf crate need not depend on
/// `dcentrald-api`.
pub trait RoomTempSink: Send + Sync {
    /// Store room temperature as Celsius*10 (the existing atomic encoding).
    fn set_room_temp_c10(&self, value: u32);
}

/// Runtime context the daemon supplies: stable identity + a live snapshot of
/// the values the heartbeat/pair calls need. Kept tiny + cloneable so the
/// daemon can build it from `AppState` / config without a circular dep.
#[derive(Clone)]
pub struct BridgeRuntime {
    /// Stable miner UUID (spec §2.2 `device_id`).
    pub device_id: String,
    /// `eth0` MAC, uppercase colons (spec §2.2 `miner_mac`).
    pub miner_mac: String,
    /// Free-form model id (spec §2.2 `model`).
    pub model: String,
    /// mDNS-safe hostname (spec §2.2 `hostname`).
    pub hostname: String,
    /// Miner-side HTTP port for the reverse proxy (spec §2.2 `api_port`).
    pub api_port: u16,
    /// 32-byte unit secret from QR provisioning. `None` ⇒ unprovisioned; the
    /// task logs and exits (dev pairing is out of scope for the daemon path).
    pub unit_secret: Option<UnitSecret>,
}

/// A provider of live miner telemetry for the heartbeat body + thermal gating.
///
/// The V0.2 Change-B expanded fields all have `{ None }` default bodies so every
/// existing impl / test mock keeps compiling; the daemon's adapter overrides the
/// ones it can source (see `bridge_glue::MinerStatusAdapter`). Field names/units
/// are frozen in `dcent-expansion-pack/docs/MESH_MODULE.md`.
pub trait MinerStatusProvider: Send + Sync {
    /// Miner uptime in seconds.
    fn uptime_s(&self) -> u64;
    /// Current operating mode string (`"mining_heating"`, `"idle"`, ...).
    fn mode(&self) -> String;
    /// Whether the miner is actively heating (drives 10 s vs 60 s telemetry).
    fn is_heating(&self) -> bool;
    /// Miner-reported chip temperature, if available.
    fn miner_temperature_c(&self) -> Option<f32>;
    /// Wall-plug power draw, if available.
    fn power_w(&self) -> Option<u16>;

    // --- V0.2 Change-B expanded heartbeat telemetry (default None) ----------

    /// Pool-reported hashrate in TH/s.
    fn hashrate_ths(&self) -> Option<f64> {
        None
    }
    /// Accepted / rejected share counts as `(accepted, rejected)`.
    fn shares(&self) -> Option<(u64, u64)> {
        None
    }
    /// Best difficulty, free-form pre-formatted string (e.g. `"184.2M"`).
    fn best_difficulty(&self) -> Option<String> {
        None
    }
    /// Best-block height context for the mesh.
    fn block_height(&self) -> Option<u64> {
        None
    }
    /// Fan speed in RPM.
    fn fan_speed_rpm(&self) -> Option<u32> {
        None
    }
    /// COMMANDED ASIC frequency in MHz (target, not a measured silicon value).
    fn asic_frequency_mhz(&self) -> Option<f32> {
        None
    }
    /// Measured ASIC voltage in V — `None` unless a real measured source exists.
    fn asic_voltage_v(&self) -> Option<f32> {
        None
    }
    /// Height of a block this miner found (drives the mesh BLK beacon).
    fn block_found_height(&self) -> Option<u64> {
        None
    }
}

/// Read the default-route gateway IP via `ip -4 route show default`.
///
/// Returns the gateway IP string (e.g. `"10.77.0.1"`) or `None` if there is no
/// default route / the command fails. Parses the `default via <ip> dev ...` line.
pub fn read_default_gateway() -> Option<String> {
    let out = std::process::Command::new("ip")
        .args(["-4", "route", "show", "default"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_default_gateway(&text)
}

/// Pure parser for the `ip -4 route show default` output (unit-testable).
pub fn parse_default_gateway(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut toks = line.split_whitespace();
        // Expect: default via <gw> dev <iface> ...
        if toks.next() == Some("default") && toks.next() == Some("via") {
            if let Some(gw) = toks.next() {
                return Some(gw.to_string());
            }
        }
    }
    None
}

/// Whether `gateway` indicates a candidate bridge: equals `10.77.0.1` or the
/// operator's `gateway_override` (spec §1.2 + non-negotiable override).
pub fn is_bridge_gateway(gateway: &str, gateway_override: Option<&str>) -> bool {
    if let Some(ovr) = gateway_override {
        return gateway == ovr;
    }
    gateway == BRIDGE_GATEWAY_IP
}

/// The bridge base URL for a given gateway IP.
fn bridge_base_url(gateway: &str) -> String {
    format!("http://{}", gateway)
}

/// The DCENT Expansion Pack bridge client task.
///
/// Cancellation-aware: every wait point selects on `shutdown.cancelled()`.
/// Returns `Ok(())` on clean shutdown; surfaces fatal pairing errors (auth /
/// enrollment-locked) by logging and returning — the daemon keeps running and
/// the heater falls back to internal sensors.
pub async fn bridge_client_task(
    cfg: BridgeConfig,
    shutdown: CancellationToken,
    runtime: BridgeRuntime,
    status: Arc<dyn MinerStatusProvider>,
    sink: Arc<dyn RoomTempSink>,
) {
    if !cfg.enabled {
        return;
    }

    let secret = match runtime.unit_secret.clone() {
        Some(s) => s,
        None => {
            tracing::info!("bridge client enabled but no unit secret provisioned; not pairing");
            return;
        }
    };

    let gw_override = cfg.gateway_override.clone();

    loop {
        // --- 1. Gateway watch -------------------------------------------------
        let gateway = loop {
            if shutdown.is_cancelled() {
                return;
            }
            match read_default_gateway() {
                Some(gw) if is_bridge_gateway(&gw, gw_override.as_deref()) => break gw,
                _ => {}
            }
            // Not on a bridge subnet; poll again in 30 s (cancellation-aware).
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        };

        let base = bridge_base_url(&gateway);
        let mut client = match BridgeClient::new(&base) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "failed to build bridge HTTP client; retrying");
                if sleep_or_cancel(&shutdown, Duration::from_secs(30)).await {
                    return;
                }
                continue;
            }
        };

        // --- 2. Discovery -----------------------------------------------------
        if !discover(&client, &shutdown).await {
            // Either cancelled or gateway is not a real bridge: re-watch.
            if shutdown.is_cancelled() {
                return;
            }
            if sleep_or_cancel(&shutdown, Duration::from_secs(30)).await {
                return;
            }
            continue;
        }

        // --- 3. Pair ----------------------------------------------------------
        let pair_result = tokio::select! {
            _ = shutdown.cancelled() => return,
            r = client.pair_with_retry(
                &secret,
                &runtime.device_id,
                &runtime.miner_mac,
                &runtime.model,
                &runtime.hostname,
                runtime.api_port,
            ) => r,
        };

        match pair_result {
            Ok(resp) => {
                tracing::info!(
                    bridge_name = %resp.bridge_name,
                    proxy_url = %resp.proxy_url,
                    "paired with dcent-pack bridge"
                );
            }
            Err(e) => {
                // Auth / enrollment-locked / bad-request are fatal for this
                // pairing cycle; log + fall back. Re-watch the gateway after a
                // pause so a later operator action (setup-button) can recover.
                tracing::warn!(error = %e, "bridge pairing failed; falling back to internal sensors");
                if sleep_or_cancel(&shutdown, Duration::from_secs(60)).await {
                    return;
                }
                continue;
            }
        }

        // --- 4. Serve (heartbeat + telemetry) --------------------------------
        let needs_repair = serve_loop(
            &mut client,
            &cfg,
            &runtime,
            &secret,
            &status,
            &sink,
            &shutdown,
        )
        .await;
        if shutdown.is_cancelled() {
            return;
        }
        if needs_repair {
            tracing::info!("bridge requested re-pair (paired:false); restarting pairing cycle");
            client.reset_staleness();
            // loop back to discovery + pair
            continue;
        }
        // serve_loop returned without re-pair and without cancel ⇒ transient
        // error path already handled; loop to re-watch.
    }
}

/// Run discovery (health then telemetry fallback). Returns true if a genuine
/// dcent-pack bridge was confirmed.
async fn discover(client: &BridgeClient, shutdown: &CancellationToken) -> bool {
    let health = tokio::select! {
        _ = shutdown.cancelled() => return false,
        h = client.probe_health() => h,
    };
    match health {
        Ok(Some(_h)) => return true,
        Ok(None) => { /* maybe 404 ⇒ try telemetry fallback */ }
        Err(e) => {
            tracing::debug!(error = %e, "health probe failed; trying telemetry fallback");
        }
    }
    let tel = tokio::select! {
        _ = shutdown.cancelled() => return false,
        t = client.probe_telemetry_fallback() => t,
    };
    matches!(tel, Ok(Some(_)))
}

/// Sanitize an external bridge temperature (°C) into the daemon's
/// `room_temp_c10` atomic (°C × 10, as `u32`). This is the ONE place an external
/// sensor value crosses into the heater thermal-control input, so the clamp is a
/// load-bearing input sanitizer — a garbage reading must never drive fan/throttle
/// decisions. Bounds [0, 80]°C → [0, 800]. NaN maps to 0 (`f32::clamp` returns
/// NaN for NaN, and `NaN as u32` saturates to 0 in Rust), i.e. a NaN reading
/// reads as "0°C" — never a panic or an unbounded value. (gap-swarm no-HAL hunt
/// #7: extracted from serve_loop so the clamp is host-testable.)
pub(crate) fn clamp_room_temp_c10(c: f32) -> u32 {
    let clamped = c.clamp(0.0, 80.0);
    (clamped * 10.0) as u32
}

/// The concurrent heartbeat + telemetry serving loop. Returns `true` if the
/// bridge signalled a re-pair (`paired:false`); `false` on cancellation or a
/// fatal heartbeat error.
async fn serve_loop(
    client: &mut BridgeClient,
    cfg: &BridgeConfig,
    runtime: &BridgeRuntime,
    secret: &UnitSecret,
    status: &Arc<dyn MinerStatusProvider>,
    sink: &Arc<dyn RoomTempSink>,
    shutdown: &CancellationToken,
) -> bool {
    let mut hb = tokio::time::interval(Duration::from_secs(cfg.heartbeat_interval_s.max(1)));
    let mut tel = tokio::time::interval(Duration::from_secs(cfg.telemetry_poll_idle_s.max(1)));
    // Re-tune the telemetry cadence each tick based on heating state.

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return false,

            _ = hb.tick() => {
                // Build the full (Change-B expanded) heartbeat from the provider,
                // then sign + POST it with the per-unit secret (Change A).
                let shares = status.shares();
                let req = HeartbeatRequest {
                    device_id: runtime.device_id.clone(),
                    uptime_s: status.uptime_s(),
                    mode: status.mode(),
                    miner_temperature_c: status.miner_temperature_c(),
                    power_w: status.power_w(),
                    hashrate_ths: status.hashrate_ths(),
                    shares_accepted: shares.map(|(a, _)| a),
                    shares_rejected: shares.map(|(_, r)| r),
                    best_difficulty: status.best_difficulty(),
                    block_height: status.block_height(),
                    fan_speed_rpm: status.fan_speed_rpm(),
                    asic_frequency_mhz: status.asic_frequency_mhz(),
                    asic_voltage_v: status.asic_voltage_v(),
                    block_found_height: status.block_found_height(),
                };
                let outcome = client.heartbeat(&req, Some(secret)).await;
                match outcome {
                    Ok(HeartbeatOutcome::Ok) => {}
                    Ok(HeartbeatOutcome::NeedsRepair) => return true,
                    Err(BridgeError::WrongMiner) => {
                        tracing::warn!("bridge paired to a different miner; stopping (operator action required)");
                        return false;
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "heartbeat error; continuing");
                    }
                }
            }

            _ = tel.tick() => {
                // Re-derive cadence: heating ⇒ fast poll, else idle poll.
                let want = if status.is_heating() {
                    cfg.telemetry_poll_heating_s.max(1)
                } else {
                    cfg.telemetry_poll_idle_s.max(1)
                };
                tel = tokio::time::interval(Duration::from_secs(want));
                // Skip the immediate tick the new interval fires at t=0.
                tel.tick().await;

                match client.poll_telemetry().await {
                    Ok(t) => {
                        let feedback_enabled = t.accessories.temperature_feedback.enabled;
                        let temp = client.record_and_extract_temp(&t);
                        if cfg.feed_thermal && feedback_enabled {
                            if let Some(c) = temp {
                                let c10 = clamp_room_temp_c10(c);
                                sink.set_room_temp_c10(c10);
                                tracing::trace!(temp_c10 = c10, "fed bridge external temp into room_temp_c10");
                            }
                            // On unusable/stale temp we simply STOP writing;
                            // the existing atomic decays naturally and the
                            // heater PID falls back to internal sensors.
                        }
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "telemetry poll error; external sensor unavailable");
                        client.reset_staleness();
                    }
                }
            }
        }
    }
}

/// Sleep for `dur` unless cancelled. Returns `true` if cancelled.
async fn sleep_or_cancel(shutdown: &CancellationToken, dur: Duration) -> bool {
    tokio::select! {
        _ = shutdown.cancelled() => true,
        _ = tokio::time::sleep(dur) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn parse_default_gateway_never_panics_on_arbitrary_text(route_output in ".{0,2048}") {
            let _ = parse_default_gateway(&route_output);
        }
    }

    #[test]
    fn clamp_room_temp_c10_bounds_and_quantization() {
        // Normal reading → °C × 10.
        assert_eq!(clamp_room_temp_c10(23.4), 234);
        // Sub-zero (cold-room/outdoor sensor) floors at 0°C — documents the floor.
        assert_eq!(clamp_room_temp_c10(-5.0), 0);
        // Above 80°C ceilings at 80°C (= 800) — never feeds an unbounded value
        // into the heater PID.
        assert_eq!(clamp_room_temp_c10(120.0), 800);
        assert_eq!(clamp_room_temp_c10(80.0), 800);
        // NaN reads as 0°C (no panic, no garbage) — pins the f32::clamp(NaN)=NaN
        // + `NaN as u32`=0 saturation contract.
        assert_eq!(clamp_room_temp_c10(f32::NAN), 0);
    }

    #[test]
    fn parse_gateway_basic() {
        let out = "default via 10.77.0.1 dev eth0 proto dhcp metric 100";
        assert_eq!(parse_default_gateway(out).as_deref(), Some("10.77.0.1"));
    }

    #[test]
    fn parse_gateway_customer_router() {
        let out = "default via 203.0.113.1 dev eth0 proto static";
        assert_eq!(parse_default_gateway(out).as_deref(), Some("203.0.113.1"));
    }

    #[test]
    fn parse_gateway_none() {
        assert_eq!(parse_default_gateway(""), None);
        assert_eq!(
            parse_default_gateway("203.0.113.0/24 dev eth0 scope link"),
            None
        );
    }

    #[test]
    fn discovery_predicate_default() {
        assert!(is_bridge_gateway("10.77.0.1", None));
        assert!(!is_bridge_gateway("203.0.113.1", None));
    }

    #[test]
    fn discovery_predicate_override() {
        // Override replaces the default match entirely.
        assert!(is_bridge_gateway("10.99.0.1", Some("10.99.0.1")));
        assert!(!is_bridge_gateway("10.77.0.1", Some("10.99.0.1")));
    }
}
