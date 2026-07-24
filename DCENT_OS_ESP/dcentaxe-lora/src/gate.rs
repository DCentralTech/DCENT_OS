// SPDX-License-Identifier: GPL-3.0-or-later
//! Inbound owner-command gate — the single decision point that turns an
//! air-received `$DCM` [`Command`](crate::mesh::MeshKind::Command) frame into a
//! **safe, bounded, applied action** or a typed rejection.
//!
//! This closes the "the security layer is built but disconnected" gap the 2026-07
//! mesh maturity audit flagged: [`auth::MeshAuthenticator`](crate::auth::MeshAuthenticator)
//! (HMAC owner-auth + anti-replay) is fully implemented and RFC-4231-tested, but
//! nothing on the live path calls it and inbound `Command` frames were never
//! dispatched — they were blindly reflooded. [`CommandGate`] is that dispatcher's
//! pure core: the esp-idf `lora_task` hands it every decoded frame and, on
//! [`GateOutcome::Apply`], performs exactly the returned hardware write.
//!
//! Three layers, fail-closed and in this order:
//!   1. **Kind** — only a `Command` frame is a control; anything else is
//!      [`GateReject::NotACommand`] (and is still relayed/observed elsewhere).
//!   2. **Authenticity + freshness** — [`MeshAuthenticator::authorize_command`]:
//!      a valid owner HMAC over `(src, seq, verb, param, value)` AND a fresh
//!      sequence number. No key ⇒ everything refused. A forged tag never advances
//!      the replay window.
//!   3. **Parse + clamp** — the command is mapped to a small, deliberately-safe
//!      [`MeshControl`] subset and every setpoint is clamped to the caller-supplied
//!      [`ControlLimits`] (the SAME envelope the local autotuner enforces). The
//!      dangerous raw knobs — core voltage, arbitrary PLL frequency — are **not
//!      representable over the air at all.**
//!
//! Pure and host-testable: no radio, no clock, no hardware. The caller owns the
//! owner key (from NVS) and the per-board limits (from its hardware profile).

use crate::auth::{MeshAuthenticator, OWNER_KEY_LEN};
use crate::mesh::{MeshCommand, MeshFrame, MeshKind};

/// The autotuner mode an owner may select over the air. Names mirror the four
/// canonical DCENT design-language modes (Max Hashrate / Best Efficiency /
/// Target Watts / Target Temp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutotunerMode {
    MaxHashrate,
    BestEfficiency,
    TargetWatts,
    TargetTemp,
}

impl AutotunerMode {
    /// Parse a compact wire token (case-insensitive; `_`, `-`, or space between
    /// words all accepted).
    pub fn from_token(s: &str) -> Option<Self> {
        match normalize(s).as_str() {
            "max_hashrate" | "maxhashrate" | "max" => Some(AutotunerMode::MaxHashrate),
            "best_efficiency" | "bestefficiency" | "efficiency" | "eff" => {
                Some(AutotunerMode::BestEfficiency)
            }
            "target_watts" | "targetwatts" | "watts" => Some(AutotunerMode::TargetWatts),
            "target_temp" | "targettemp" | "temp" => Some(AutotunerMode::TargetTemp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AutotunerMode::MaxHashrate => "max_hashrate",
            AutotunerMode::BestEfficiency => "best_efficiency",
            AutotunerMode::TargetWatts => "target_watts",
            AutotunerMode::TargetTemp => "target_temp",
        }
    }
}

/// A typed, authenticated, **already-clamped** control action ready to apply.
///
/// This is the whole over-the-air control vocabulary — a small safe subset. Note
/// what is absent by design: there is NO `SetCoreVoltage` and NO raw
/// `SetFrequency`, because a mistyped or hostile value on those knobs is a
/// hardware-damage / instant-zero-hashrate risk. Owners tune power/thermal via
/// the clamped setpoints below (mirrors the default-OFF MQTT command entities).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshControl {
    /// Set the autotuner power budget (watts), already clamped to [`ControlLimits`].
    SetTargetWatts(u32),
    /// Set the target chip temperature (°C), already clamped.
    SetTargetTempC(u8),
    /// Select an autotuner mode.
    SetAutotunerMode(AutotunerMode),
    /// Blink the identify LED (harmless, but still owner-gated to stop nuisance).
    Identify,
    /// Restart the mining task.
    RestartMining,
}

/// Why the gate did not produce an applied action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateReject {
    /// The frame was not a `Command`.
    NotACommand,
    /// Owner-auth or anti-replay failed (fail-closed — the default).
    Unauthorized,
    /// The verb/param is not a recognized safe control.
    UnknownControl,
    /// The value did not parse as the control's expected type.
    BadValue,
}

/// The gate's decision for one frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// Apply this safe, clamped action to the hardware.
    Apply(MeshControl),
    /// Do nothing; see the reason.
    Reject(GateReject),
}

/// The bounded safety envelope an over-the-air setpoint is clamped to. Supplied
/// by the caller (the binary's per-board hardware profile) so this crate stays
/// hardware-agnostic and reuses the SAME limits the local autotuner enforces.
#[derive(Debug, Clone, Copy)]
pub struct ControlLimits {
    pub min_watts: u32,
    pub max_watts: u32,
    pub min_target_temp_c: u8,
    pub max_target_temp_c: u8,
}

impl ControlLimits {
    /// A conservative default envelope for a single-chip BitAxe-class board. The
    /// caller SHOULD override with its real per-board profile.
    pub const DEFAULT: ControlLimits = ControlLimits {
        min_watts: 1,
        max_watts: 100,
        min_target_temp_c: 40,
        max_target_temp_c: 70,
    };
}

impl Default for ControlLimits {
    fn default() -> Self {
        ControlLimits::DEFAULT
    }
}

/// Normalize a token: trim, lowercase, and collapse `-`/space to `_`.
fn normalize(s: &str) -> String {
    s.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c == '-' || c == ' ' { '_' } else { c })
        .collect()
}

/// Clamp `v` into `[a, b]` regardless of the argument order — never panics even
/// if a misconfigured profile passes `a > b` (guards the `Ord::clamp` min>max
/// panic; see the mujina cross-check regression rule).
fn clamp_u32(v: u32, a: u32, b: u32) -> u32 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    v.max(lo).min(hi)
}

fn clamp_u8(v: u8, a: u8, b: u8) -> u8 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    v.max(lo).min(hi)
}

/// Parse an authenticated command into a safe, clamped [`MeshControl`]. Pure — no
/// auth here (the caller gates auth first); this is the parse+clamp layer only.
pub fn parse_and_clamp(
    cmd: &MeshCommand,
    limits: &ControlLimits,
) -> Result<MeshControl, GateReject> {
    let verb = normalize(&cmd.verb);
    let param = normalize(&cmd.param);
    match verb.as_str() {
        "set" => match param.as_str() {
            "target_watts" | "watts" | "target_power" => {
                let w: u32 = cmd.value.trim().parse().map_err(|_| GateReject::BadValue)?;
                Ok(MeshControl::SetTargetWatts(clamp_u32(
                    w,
                    limits.min_watts,
                    limits.max_watts,
                )))
            }
            "target_temp_c" | "target_temp" | "temp" => {
                let t: u8 = cmd.value.trim().parse().map_err(|_| GateReject::BadValue)?;
                Ok(MeshControl::SetTargetTempC(clamp_u8(
                    t,
                    limits.min_target_temp_c,
                    limits.max_target_temp_c,
                )))
            }
            "mode" | "autotuner_mode" | "autotuner" => AutotunerMode::from_token(&cmd.value)
                .map(MeshControl::SetAutotunerMode)
                .ok_or(GateReject::BadValue),
            _ => Err(GateReject::UnknownControl),
        },
        "cmd" => match param.as_str() {
            "identify" | "blink" => Ok(MeshControl::Identify),
            "restart" | "restart_mining" | "reboot_mining" => Ok(MeshControl::RestartMining),
            _ => Err(GateReject::UnknownControl),
        },
        _ => Err(GateReject::UnknownControl),
    }
}

/// The pure inbound-command gate: owner-auth + anti-replay + parse + clamp.
#[derive(Debug, Clone)]
pub struct CommandGate {
    auth: MeshAuthenticator,
    limits: ControlLimits,
}

impl CommandGate {
    /// Build a gate with the configured owner `key` (`None` ⇒ every command is
    /// refused until provisioned) and the per-board `limits`.
    pub fn new(key: Option<[u8; OWNER_KEY_LEN]>, limits: ControlLimits) -> Self {
        Self {
            auth: MeshAuthenticator::new(key),
            limits,
        }
    }

    /// Replace the owner key (e.g. after NVS provisioning).
    pub fn set_key(&mut self, key: Option<[u8; OWNER_KEY_LEN]>) {
        self.auth.set_key(key);
    }

    /// Replace the per-board safety limits.
    pub fn set_limits(&mut self, limits: ControlLimits) {
        self.limits = limits;
    }

    /// `true` once an owner key is configured.
    pub fn is_provisioned(&self) -> bool {
        self.auth.is_provisioned()
    }

    /// Decide what to do with a decoded frame. Only a `Command` is a control;
    /// auth + replay run before any parse, and every setpoint is clamped.
    pub fn admit(&mut self, frame: &MeshFrame) -> GateOutcome {
        let cmd: &MeshCommand = match &frame.kind {
            MeshKind::Command(c) => c,
            _ => return GateOutcome::Reject(GateReject::NotACommand),
        };
        if self
            .auth
            .authorize_command(cmd, frame.src, frame.seq)
            .is_err()
        {
            return GateOutcome::Reject(GateReject::Unauthorized);
        }
        match parse_and_clamp(cmd, &self.limits) {
            Ok(ctrl) => GateOutcome::Apply(ctrl),
            Err(r) => GateOutcome::Reject(r),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{command_mac, tag_to_hex};
    use crate::mesh::{MeshCommand, MeshFrame, MeshKind, NodeId, Telemetry, DEFAULT_TTL};

    const KEY: [u8; 32] = [0x5a; 32];
    const SRC: NodeId = NodeId(0x0000_beef);

    fn signed_frame(seq: u8, verb: &str, param: &str, value: &str) -> MeshFrame {
        let auth = Some(tag_to_hex(&command_mac(
            &KEY, SRC, 0, seq, verb, param, value,
        )));
        MeshFrame {
            src: SRC,
            seq,
            ttl: DEFAULT_TTL,
            kind: MeshKind::Command(MeshCommand {
                verb: verb.into(),
                param: param.into(),
                value: value.into(),
                epoch: 0,
                auth,
            }),
        }
    }

    fn gate() -> CommandGate {
        CommandGate::new(Some(KEY), ControlLimits::DEFAULT)
    }

    #[test]
    fn applies_signed_watts_within_range() {
        let mut g = gate();
        let f = signed_frame(1, "set", "target_watts", "45");
        assert_eq!(
            g.admit(&f),
            GateOutcome::Apply(MeshControl::SetTargetWatts(45))
        );
    }

    #[test]
    fn clamps_out_of_range_setpoints() {
        let mut g = gate();
        // Above max_watts (100) → clamped to 100.
        assert_eq!(
            g.admit(&signed_frame(1, "set", "target_watts", "9999")),
            GateOutcome::Apply(MeshControl::SetTargetWatts(100))
        );
        // Below min_target_temp_c (40) → clamped to 40.
        assert_eq!(
            g.admit(&signed_frame(2, "set", "target_temp_c", "10")),
            GateOutcome::Apply(MeshControl::SetTargetTempC(40))
        );
    }

    #[test]
    fn rejects_unauthenticated_frame() {
        // Gate with a DIFFERENT key than the frame was signed with.
        let mut g = CommandGate::new(Some([0x11; 32]), ControlLimits::DEFAULT);
        assert_eq!(
            g.admit(&signed_frame(1, "set", "target_watts", "45")),
            GateOutcome::Reject(GateReject::Unauthorized)
        );
        // And an unprovisioned gate refuses a validly-signed frame.
        let mut unprov = CommandGate::new(None, ControlLimits::DEFAULT);
        assert_eq!(
            unprov.admit(&signed_frame(1, "set", "target_watts", "45")),
            GateOutcome::Reject(GateReject::Unauthorized)
        );
        assert!(!unprov.is_provisioned());
    }

    #[test]
    fn rejects_replayed_frame() {
        let mut g = gate();
        let f = signed_frame(7, "cmd", "identify", "");
        assert_eq!(g.admit(&f), GateOutcome::Apply(MeshControl::Identify));
        // Exact same authenticated frame again → refused by the replay window.
        assert_eq!(g.admit(&f), GateOutcome::Reject(GateReject::Unauthorized));
    }

    #[test]
    fn rejects_non_command_frame() {
        let mut g = gate();
        let tlm = MeshFrame {
            src: SRC,
            seq: 1,
            ttl: DEFAULT_TTL,
            kind: MeshKind::Telemetry(Telemetry {
                hashrate_ghs: 1.0,
                chip_temp_c: 50.0,
                power_w: 15.0,
                shares_accepted: 1,
                shares_rejected: 0,
                best_diff: "1k".into(),
                block_height: 1,
            }),
        };
        assert_eq!(g.admit(&tlm), GateOutcome::Reject(GateReject::NotACommand));
    }

    #[test]
    fn rejects_unknown_control_and_bad_value() {
        let mut g = gate();
        assert_eq!(
            g.admit(&signed_frame(1, "set", "core_voltage", "1200")),
            GateOutcome::Reject(GateReject::UnknownControl)
        );
        assert_eq!(
            g.admit(&signed_frame(2, "set", "target_watts", "notanumber")),
            GateOutcome::Reject(GateReject::BadValue)
        );
    }

    #[test]
    fn parses_all_autotuner_modes() {
        let mut g = gate();
        let cases = [
            ("Max Hashrate", AutotunerMode::MaxHashrate),
            ("best_efficiency", AutotunerMode::BestEfficiency),
            ("target-watts", AutotunerMode::TargetWatts),
            ("temp", AutotunerMode::TargetTemp),
        ];
        for (i, (tok, mode)) in cases.iter().enumerate() {
            assert_eq!(
                g.admit(&signed_frame(i as u8 + 1, "set", "mode", tok)),
                GateOutcome::Apply(MeshControl::SetAutotunerMode(*mode)),
                "mode token {tok:?}"
            );
        }
    }

    #[test]
    fn identify_and_restart_are_owner_gated_actions() {
        let mut g = gate();
        assert_eq!(
            g.admit(&signed_frame(1, "cmd", "identify", "")),
            GateOutcome::Apply(MeshControl::Identify)
        );
        assert_eq!(
            g.admit(&signed_frame(2, "cmd", "restart_mining", "")),
            GateOutcome::Apply(MeshControl::RestartMining)
        );
    }

    #[test]
    fn clamp_helpers_never_panic_on_inverted_limits() {
        // Misconfigured profile (min > max) must not panic; value is bounded.
        assert_eq!(clamp_u32(50, 100, 10), 50); // 50 already inside [10,100]
        assert_eq!(clamp_u32(5, 100, 10), 10);
        assert_eq!(clamp_u32(500, 100, 10), 100);
        assert_eq!(clamp_u8(200, 70, 40), 70);
    }

    #[test]
    fn tampered_value_fails_auth_before_parse() {
        let mut g = gate();
        // Sign for value "45", then tamper the value to "9999" in the frame.
        let mut f = signed_frame(1, "set", "target_watts", "45");
        if let MeshKind::Command(c) = &mut f.kind {
            c.value = "9999".into();
        }
        assert_eq!(
            g.admit(&f),
            GateOutcome::Reject(GateReject::Unauthorized),
            "tampering the value invalidates the owner MAC"
        );
    }
}
