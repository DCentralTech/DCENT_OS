// SPDX-License-Identifier: GPL-3.0-or-later
// BAP subscription manager. Tracks which params the accessory is subscribed
// to, how often to push them, and when to retire stale subscriptions.
// Matches ESP-Miner `main/bap/bap_subscription.c` semantics.

use crate::protocol::{BapCommand, BapFrame};
use crate::AppSnapshot;

/// After this long without any accessory traffic we drop all subscriptions.
/// Upstream uses 5 minutes; we stay strict so a powered-off / disconnected
/// screen doesn't leave the mining side blindly emitting frames forever.
pub const IDLE_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// Default cadence for a subscribed param (1 Hz). Upstream publishes most
/// params roughly every second; the accessory can request a tighter cadence
/// via `SUB,<param>,<interval_ms>` but we clamp to 250 ms lower bound.
pub const DEFAULT_INTERVAL_MS: u64 = 1000;
pub const MIN_INTERVAL_MS: u64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscribableParam {
    Hashrate,
    Temperature,
    Power,
    Voltage,
    Current,
    Shares,
    Frequency,
    AsicVoltage,
    FanSpeed,
    AutoFan,
    BestDifficulty,
    BlockHeight,
    Wifi,
    SystemInfo,
    FoundBlock,
}

impl SubscribableParam {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hashrate => "hashrate",
            Self::Temperature => "temperature",
            Self::Power => "power",
            Self::Voltage => "voltage",
            Self::Current => "current",
            Self::Shares => "shares",
            Self::Frequency => "frequency",
            Self::AsicVoltage => "asic_voltage",
            Self::FanSpeed => "fan_speed",
            Self::AutoFan => "auto_fan",
            Self::BestDifficulty => "best_difficulty",
            Self::BlockHeight => "block_height",
            Self::Wifi => "wifi",
            Self::SystemInfo => "systemInfo",
            Self::FoundBlock => "found_block",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hashrate" => Self::Hashrate,
            "temperature" => Self::Temperature,
            "power" => Self::Power,
            "voltage" => Self::Voltage,
            "current" => Self::Current,
            "shares" => Self::Shares,
            "frequency" => Self::Frequency,
            "asic_voltage" => Self::AsicVoltage,
            "fan_speed" => Self::FanSpeed,
            "auto_fan" => Self::AutoFan,
            "best_difficulty" => Self::BestDifficulty,
            "block_height" => Self::BlockHeight,
            "wifi" => Self::Wifi,
            "systemInfo" => Self::SystemInfo,
            "found_block" => Self::FoundBlock,
            _ => return None,
        })
    }

    /// Render this param's current value from the host snapshot.
    pub fn render(&self, snap: &AppSnapshot) -> String {
        match self {
            Self::Hashrate => format!("{:.2}", snap.hashrate_ghs),
            Self::Temperature => format!("{:.1}", snap.temperature_c),
            Self::Power => format!("{:.2}", snap.power_w),
            Self::Voltage => format!("{:.0}", snap.voltage_mv),
            Self::Current => format!("{:.0}", snap.current_ma),
            Self::Shares => format!("{},{}", snap.shares_accepted, snap.shares_rejected),
            Self::Frequency => format!("{:.1}", snap.frequency_mhz),
            Self::AsicVoltage => snap.asic_voltage_mv.to_string(),
            Self::FanSpeed => snap.fan_speed_pct.to_string(),
            Self::AutoFan => snap.auto_fan.to_string(),
            Self::BestDifficulty => format!("{:.3}", snap.best_difficulty),
            Self::BlockHeight => snap.block_height.to_string(),
            Self::Wifi => format!(
                "{},{},{}",
                if snap.wifi_connected { 1 } else { 0 },
                snap.wifi_rssi_dbm,
                snap.wifi_ssid
            ),
            Self::SystemInfo => serde_json::json!({
                "hashRate": snap.hashrate_ghs,
                "hashRate_1m": snap.hashrate_1m_ghs,
                "temp": snap.temperature_c,
                "power": snap.power_w,
                "voltage": snap.voltage_mv,
                "current": snap.current_ma,
                "sharesAccepted": snap.shares_accepted,
                "sharesRejected": snap.shares_rejected,
                "frequency": snap.frequency_mhz,
                "coreVoltageActual": snap.asic_voltage_mv,
                "fanSpeedPercent": snap.fan_speed_pct,
                "autoFanSpeed": snap.auto_fan,
                "bestDiff": snap.best_difficulty,
                "bestSessionDiff": snap.best_difficulty,
                "blockHeight": snap.block_height,
                "ssid": snap.wifi_ssid,
                "wifiStatus": if snap.wifi_connected { "Connected" } else { "Disconnected" },
                "wifiRSSI": snap.wifi_rssi_dbm,
                "deviceModel": snap.device_model,
                "version": snap.firmware_version,
            })
            .to_string(),
            Self::FoundBlock => String::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct Subscription {
    param: SubscribableParam,
    interval_ms: u64,
    /// `None` means "never emitted yet"; first emit always fires regardless of
    /// interval. We can't sentinel on `0` because the very first `emit_due`
    /// call frequently happens at `now_ms == 0`, which would collide and
    /// re-trigger below the `MIN_INTERVAL_MS` clamp.
    last_sent_ms: Option<u64>,
}

/// Per-accessory subscription state. Upstream allows only a single accessory
/// at a time (one UART = one client); we match that assumption.
#[derive(Debug, Default)]
pub struct SubscriptionManager {
    subs: Vec<Subscription>,
    /// `None` means "no accessory activity recorded yet" — the idle timeout is
    /// not armed until the first keepalive. We can't sentinel on `0` because the
    /// espidf monotonic `now_ms` is boot-relative and a `SUB` arriving at the
    /// exact `now_ms == 0` instant would otherwise leave the idle drop disabled
    /// forever (AUX-9). Mirrors `Subscription.last_sent_ms`.
    last_activity_ms: Option<u64>,
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn refresh_keepalive(&mut self, now_ms: u64) {
        self.last_activity_ms = Some(now_ms);
    }

    pub fn add(&mut self, param: SubscribableParam, interval_ms: Option<u64>, now_ms: u64) {
        self.refresh_keepalive(now_ms);
        let interval = interval_ms
            .unwrap_or(DEFAULT_INTERVAL_MS)
            .max(MIN_INTERVAL_MS);
        if let Some(existing) = self.subs.iter_mut().find(|s| s.param == param) {
            existing.interval_ms = interval;
            existing.last_sent_ms = None;
        } else {
            self.subs.push(Subscription {
                param,
                interval_ms: interval,
                last_sent_ms: None,
            });
        }
    }

    pub fn remove(&mut self, param: SubscribableParam, now_ms: u64) {
        self.refresh_keepalive(now_ms);
        self.subs.retain(|s| s.param != param);
    }

    pub fn clear(&mut self) {
        self.subs.clear();
    }

    /// Emit every subscription whose interval has elapsed, subject to the
    /// idle timeout. Call once per server tick.
    pub fn emit_due(&mut self, now_ms: u64, snap: &AppSnapshot) -> Vec<BapFrame> {
        // AUX-9: the idle timeout is armed from the first keepalive regardless
        // of the absolute `now_ms` value (including `now_ms == 0`). `None` means
        // no activity recorded yet, so nothing to time out.
        if let Some(last_activity) = self.last_activity_ms {
            if now_ms.saturating_sub(last_activity) > IDLE_TIMEOUT_MS {
                log::info!(
                    "BAP: accessory idle > {} ms, dropping subscriptions",
                    IDLE_TIMEOUT_MS
                );
                self.clear();
                return Vec::new();
            }
        }
        let mut out = Vec::new();
        for sub in self.subs.iter_mut() {
            let due = match sub.last_sent_ms {
                None => true,
                Some(last) => now_ms.saturating_sub(last) >= sub.interval_ms,
            };
            if due {
                sub.last_sent_ms = Some(now_ms);
                out.push(BapFrame::new(
                    BapCommand::Res,
                    sub.param.as_str(),
                    sub.param.render(snap),
                ));
            }
        }
        out
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.subs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_remove() {
        let mut mgr = SubscriptionManager::new();
        mgr.add(SubscribableParam::Hashrate, None, 100);
        assert_eq!(mgr.len(), 1);
        mgr.remove(SubscribableParam::Hashrate, 200);
        assert_eq!(mgr.len(), 0);
    }

    #[test]
    fn emit_respects_interval() {
        let mut mgr = SubscriptionManager::new();
        mgr.add(SubscribableParam::Hashrate, Some(1000), 0);
        let snap = AppSnapshot::default();
        let first = mgr.emit_due(500, &snap);
        assert_eq!(first.len(), 1, "first tick always emits");
        let second = mgr.emit_due(800, &snap);
        assert_eq!(second.len(), 0, "before interval elapses");
        let third = mgr.emit_due(1600, &snap);
        assert_eq!(third.len(), 1, "after interval");
    }

    #[test]
    fn idle_timeout_clears() {
        let mut mgr = SubscriptionManager::new();
        mgr.add(SubscribableParam::Hashrate, None, 1);
        mgr.refresh_keepalive(1);
        let snap = AppSnapshot::default();
        let _ = mgr.emit_due(IDLE_TIMEOUT_MS + 2, &snap);
        assert_eq!(mgr.len(), 0);
    }

    #[test]
    fn idle_timeout_armed_when_first_activity_at_now_zero() {
        // AUX-9: a SUB whose keepalive lands at now_ms == 0 must STILL arm the
        // idle timeout. With the old `u64`-0-means-unset sentinel the drop never
        // fired.
        let mut mgr = SubscriptionManager::new();
        mgr.add(SubscribableParam::Hashrate, None, 0); // keepalive at now=0
        assert_eq!(mgr.len(), 1);
        let snap = AppSnapshot::default();
        // Well past the idle window with no further activity → must drop.
        let out = mgr.emit_due(IDLE_TIMEOUT_MS + 1, &snap);
        assert!(out.is_empty(), "no frames after idle drop");
        assert_eq!(
            mgr.len(),
            0,
            "idle timeout must fire even when first activity was at now_ms==0"
        );
    }

    #[test]
    fn no_idle_drop_before_first_activity() {
        // With no activity recorded (last_activity_ms == None) emit_due must not
        // treat the manager as idle (there's nothing subscribed anyway, but the
        // guard must not panic / mis-fire).
        let mut mgr = SubscriptionManager::new();
        let snap = AppSnapshot::default();
        let out = mgr.emit_due(IDLE_TIMEOUT_MS * 10, &snap);
        assert!(out.is_empty());
        assert_eq!(mgr.len(), 0);
    }

    #[test]
    fn min_interval_clamp() {
        let mut mgr = SubscriptionManager::new();
        mgr.add(SubscribableParam::Hashrate, Some(10), 0);
        let snap = AppSnapshot::default();
        let first = mgr.emit_due(0, &snap);
        assert_eq!(first.len(), 1);
        // 100 ms later — below MIN_INTERVAL_MS (250 ms), should NOT emit.
        let second = mgr.emit_due(100, &snap);
        assert_eq!(second.len(), 0);
    }
}
