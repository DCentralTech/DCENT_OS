// SPDX-License-Identifier: GPL-3.0-or-later
// BAP command dispatch. Maps incoming frames to host app-state calls,
// returns either a `RES` / `ACK` reply to send or `None` when the frame
// starts a background operation (subscription push is handled separately).

use crate::protocol::{BapCommand, BapError, BapFrame};
use crate::subscription::{SubscribableParam, SubscriptionManager};
use crate::BapAppState;
// `AppSnapshot` is only constructed by the `#[cfg(test)]` mock app-state below;
// importing it unconditionally is an unused-import warning in a firmware build
// (and trips the CI clippy `-D warnings` gate).
#[cfg(test)]
use crate::AppSnapshot;

/// Dispatch one frame. Returns the reply frame to send (if any).
pub fn dispatch<S: BapAppState>(
    app: &S,
    subs: &mut SubscriptionManager,
    frame: &BapFrame,
) -> Result<Option<BapFrame>, BapError> {
    let now_ms = now_ms();
    subs.refresh_keepalive(now_ms);

    match frame.cmd {
        BapCommand::Req => {
            let param = SubscribableParam::parse(&frame.param).ok_or(BapError::UnknownParam)?;
            let snap = app.snapshot();
            Ok(Some(BapFrame::res(param.as_str(), param.render(&snap))))
        }
        BapCommand::Sub => {
            let param = SubscribableParam::parse(&frame.param).ok_or(BapError::UnknownParam)?;
            let interval = if frame.value.is_empty() {
                None
            } else {
                Some(
                    frame
                        .value
                        .parse::<u64>()
                        .map_err(|_| BapError::InvalidValue)?,
                )
            };
            subs.add(param, interval, now_ms);
            Ok(Some(BapFrame::ack(param.as_str())))
        }
        BapCommand::Unsub => {
            let param = SubscribableParam::parse(&frame.param).ok_or(BapError::UnknownParam)?;
            subs.remove(param, now_ms);
            Ok(Some(BapFrame::ack(param.as_str())))
        }
        BapCommand::Set => apply_set(app, &frame.param, &frame.value),
        BapCommand::Cmd => apply_cmd(app, &frame.param, &frame.value),
        // Responses and status/log from the accessory are informational.
        BapCommand::Ack | BapCommand::Err | BapCommand::Res | BapCommand::Sta | BapCommand::Log => {
            Ok(None)
        }
    }
}

fn apply_set<S: BapAppState>(
    app: &S,
    param: &str,
    value: &str,
) -> Result<Option<BapFrame>, BapError> {
    match param {
        "frequency" => {
            let mhz: f32 = value.parse().map_err(|_| BapError::InvalidValue)?;
            // BAP-2: reject an out-of-envelope frequency at the protocol layer
            // before it reaches the host. The host's qualify_operating_point()
            // still applies the tighter per-board clamp.
            let mhz =
                crate::protocol::bounds::check_frequency_mhz(mhz).ok_or(BapError::InvalidValue)?;
            app.set_frequency(mhz).map_err(BapError::HandlerFailed)?;
        }
        "asic_voltage" => {
            let mv: u16 = value.parse().map_err(|_| BapError::InvalidValue)?;
            // BAP-2: hard over-volt reject at the protocol layer (defence in
            // depth; the host still clamps per board).
            let mv =
                crate::protocol::bounds::check_asic_voltage_mv(mv).ok_or(BapError::InvalidValue)?;
            app.set_asic_voltage(mv).map_err(BapError::HandlerFailed)?;
        }
        "fan_speed" => {
            let pct: u8 = value.parse().map_err(|_| BapError::InvalidValue)?;
            // BAP-2: a fan duty above 100% is invalid; reject before the host
            // handler (a zero/low value is permitted — thermal safety is
            // enforced by the host's fail-closed fan policy, not BAP).
            let pct = crate::protocol::bounds::check_fan_pct(pct).ok_or(BapError::InvalidValue)?;
            app.set_fan_speed(pct).map_err(BapError::HandlerFailed)?;
        }
        "auto_fan" => {
            let enabled = match value {
                "1" | "true" | "TRUE" | "on" => true,
                "0" | "false" | "FALSE" | "off" => false,
                _ => return Err(BapError::InvalidValue),
            };
            app.set_auto_fan(enabled).map_err(BapError::HandlerFailed)?;
        }
        "ssid" => {
            app.set_wifi_ssid(value).map_err(BapError::HandlerFailed)?;
        }
        "password" => {
            app.set_wifi_password(value)
                .map_err(BapError::HandlerFailed)?;
        }
        _ => return Err(BapError::UnknownParam),
    }
    Ok(Some(BapFrame::ack(param)))
}

fn apply_cmd<S: BapAppState>(
    app: &S,
    cmd: &str,
    _value: &str,
) -> Result<Option<BapFrame>, BapError> {
    match cmd {
        "restart_mining" => app.restart_mining().map_err(BapError::HandlerFailed)?,
        "identify" => app.identify().map_err(BapError::HandlerFailed)?,
        _ => return Err(BapError::UnknownParam),
    }
    Ok(Some(BapFrame::ack(cmd)))
}

#[cfg(target_os = "espidf")]
fn now_ms() -> u64 {
    unsafe { (esp_idf_hal::sys::esp_timer_get_time() / 1000) as u64 }
}

#[cfg(not(target_os = "espidf"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct MockApp {
        freq: Cell<f32>,
        fan: Cell<u8>,
        restart_count: Cell<u32>,
    }

    impl Default for MockApp {
        fn default() -> Self {
            Self {
                freq: Cell::new(500.0),
                fan: Cell::new(50),
                restart_count: Cell::new(0),
            }
        }
    }

    unsafe impl Send for MockApp {}

    impl BapAppState for MockApp {
        fn snapshot(&self) -> AppSnapshot {
            AppSnapshot {
                frequency_mhz: self.freq.get(),
                fan_speed_pct: self.fan.get(),
                ..Default::default()
            }
        }
        fn set_frequency(&self, mhz: f32) -> Result<(), String> {
            self.freq.set(mhz);
            Ok(())
        }
        fn set_asic_voltage(&self, _: u16) -> Result<(), String> {
            Ok(())
        }
        fn set_fan_speed(&self, pct: u8) -> Result<(), String> {
            self.fan.set(pct);
            Ok(())
        }
        fn set_auto_fan(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi_ssid(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn set_wifi_password(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn restart_mining(&self) -> Result<(), String> {
            self.restart_count.set(self.restart_count.get() + 1);
            Ok(())
        }
        fn identify(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn req_hashrate_returns_res() {
        let app = MockApp::default();
        let mut subs = SubscriptionManager::new();
        let frame = BapFrame::new(BapCommand::Req, "hashrate", "");
        let reply = dispatch(&app, &mut subs, &frame).unwrap().unwrap();
        assert_eq!(reply.cmd, BapCommand::Res);
        assert_eq!(reply.param, "hashrate");
    }

    #[test]
    fn set_frequency_applies() {
        let app = MockApp::default();
        let mut subs = SubscriptionManager::new();
        let frame = BapFrame::new(BapCommand::Set, "frequency", "600");
        let reply = dispatch(&app, &mut subs, &frame).unwrap().unwrap();
        assert_eq!(reply.cmd, BapCommand::Ack);
        assert_eq!(app.freq.get(), 600.0);
    }

    #[test]
    fn cmd_restart_mining_runs() {
        let app = MockApp::default();
        let mut subs = SubscriptionManager::new();
        let frame = BapFrame::new(BapCommand::Cmd, "restart_mining", "");
        let reply = dispatch(&app, &mut subs, &frame).unwrap().unwrap();
        assert_eq!(reply.cmd, BapCommand::Ack);
        assert_eq!(app.restart_count.get(), 1);
    }

    #[test]
    fn sub_unsub_manages_state() {
        let app = MockApp::default();
        let mut subs = SubscriptionManager::new();
        dispatch(
            &app,
            &mut subs,
            &BapFrame::new(BapCommand::Sub, "hashrate", "500"),
        )
        .unwrap();
        assert_eq!(subs.len(), 1);
        dispatch(
            &app,
            &mut subs,
            &BapFrame::new(BapCommand::Unsub, "hashrate", ""),
        )
        .unwrap();
        assert_eq!(subs.len(), 0);
    }

    #[test]
    fn set_overvolt_rejected_at_protocol_layer() {
        // BAP-2: an over-volt SET must be rejected with InvalidValue BEFORE the
        // host handler is called.
        struct GuardApp {
            applied_voltage: Cell<Option<u16>>,
        }
        unsafe impl Send for GuardApp {}
        impl BapAppState for GuardApp {
            fn snapshot(&self) -> AppSnapshot {
                AppSnapshot::default()
            }
            fn set_frequency(&self, _: f32) -> Result<(), String> {
                Ok(())
            }
            fn set_asic_voltage(&self, mv: u16) -> Result<(), String> {
                self.applied_voltage.set(Some(mv));
                Ok(())
            }
            fn set_fan_speed(&self, _: u8) -> Result<(), String> {
                Ok(())
            }
            fn set_auto_fan(&self, _: bool) -> Result<(), String> {
                Ok(())
            }
            fn set_wifi_ssid(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn set_wifi_password(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
            fn restart_mining(&self) -> Result<(), String> {
                Ok(())
            }
            fn identify(&self) -> Result<(), String> {
                Ok(())
            }
        }

        let app = GuardApp {
            applied_voltage: Cell::new(None),
        };
        let mut subs = SubscriptionManager::new();

        // 2000 mV is above MAX_ASIC_VOLTAGE_MV (1600) → rejected, handler untouched.
        let over = BapFrame::new(BapCommand::Set, "asic_voltage", "2000");
        assert!(matches!(
            dispatch(&app, &mut subs, &over),
            Err(BapError::InvalidValue)
        ));
        assert_eq!(
            app.applied_voltage.get(),
            None,
            "host handler must not be called for over-volt"
        );

        // A legal voltage passes through to the host handler.
        let ok = BapFrame::new(BapCommand::Set, "asic_voltage", "1200");
        let reply = dispatch(&app, &mut subs, &ok).unwrap().unwrap();
        assert_eq!(reply.cmd, BapCommand::Ack);
        assert_eq!(app.applied_voltage.get(), Some(1200));

        // An over-100% fan and a NaN/garbage frequency are also rejected.
        let bad_fan = BapFrame::new(BapCommand::Set, "fan_speed", "200");
        assert!(matches!(
            dispatch(&app, &mut subs, &bad_fan),
            Err(BapError::InvalidValue)
        ));
        let bad_freq = BapFrame::new(BapCommand::Set, "frequency", "5000");
        assert!(matches!(
            dispatch(&app, &mut subs, &bad_freq),
            Err(BapError::InvalidValue)
        ));
    }

    #[test]
    fn unknown_param_errors() {
        let app = MockApp::default();
        let mut subs = SubscriptionManager::new();
        let frame = BapFrame::new(BapCommand::Req, "nonsense", "");
        assert!(matches!(
            dispatch(&app, &mut subs, &frame),
            Err(BapError::UnknownParam)
        ));
    }
}
