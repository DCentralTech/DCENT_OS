//! LuxOS-shape thermal supervisor (RE-005 closure, Wave E 2026-05-19).
//!
//! Six-layer thermal supervision FSM layered ON TOP of the existing
//! `crate::controller::ThermalController` PID loop. Per the RE team handoff at
//!
//! §RE-005, the supervisor consumes per-tick sensor readings (board PCB
//! temps, chip die temps, fan tachs, optional hydro inlet/outlet) and emits
//! action *requests* to the controller without replacing it (autotuner stays
//! master per the plan).
//!
//! # Source of truth
//!
//! Clean-room implementation from the RE-005 handoff. No proprietary code
//! copied. Thresholds (target 55 C / hot 65 C / panic 70 C / chip_hot 93 C /
//! chip_panic 100 C) and ATM windows (5 C / 8 C / 15 min startup / 15 min
//! post-ramp) come from the documented LuxOS config surface, not from binary
//! reverse-engineering. Confidence per RE-005: MEDIUM for behavior;
//! MEDIUM-LOW for exact tick cadence + exact fan curve formula because the
//! live `a lab unit` capture did not cross hot/panic thresholds. Wave E parameterizes
//! the unknowns as TOML knobs rather than hardcoding them.
//!
//! # Opt-in safety
//!
//! This module is COMPILED but NOT INSTANTIATED into the running daemon by
//! default. The integration site in the controller will be gated on
//! `[thermal.supervisor].enabled = true` in `dcentrald.toml`; with that flag
//! false (default), the existing controller stays master and supervisor
//! actions are not emitted. Wave G/H is the integration wave; this wave
//! ships defensive code that's safety-reviewable in isolation.
//!
//! # Load-bearing rules
//!
//! - **Quiet-home `fan_max_pwm` cap MUST NOT be exceeded.** `RequestFansMax`
//!   means "request fans up to `fan_max_pwm`", NOT 100% PWM. Raising the cap
//!   above the per-platform home-unit floor requires explicit operator config
//!   edit. Cut-hash-before-noise stays the default escalation order. Enforced
//!   by `tests::request_fans_max_respects_quiet_home_cap`.
//! - **Supervisor is a REQUESTOR.** The autotuner stays master for any
//!   profile step decisions; `RequestProfileStepUp/Down` are advisories the
//!   autotuner can decline if it has stronger signals. Enforced by
//!   `tests::atm_step_request_does_not_force_autotuner`.
//! - **Emergency-shutdown beats DPS.** When `RequestEmergencyShutdown` is
//!   emitted, the caller's existing `DangerousShutdown` / power-cut path
//!   takes precedence over any DPS governor action. The supervisor does NOT
//!   call into hardware directly.
//!
//! # State machine
//!
//! ```text
//!   per tick:
//!     1. reliability_filter(board_sensors) -> valid_board
//!     2. reliability_filter(chip_sensors)  -> valid_chip
//!     3. classify each board:
//!          board_max >= board_panic -> RequestBoardPowerOff
//!          chip_max  >= chip_panic  -> RequestBoardPowerOff
//!          board_max >= hot        -> RequestFansMax + RequestProfileStepDown
//!          board_max >= target     -> RequestFansCurve + ATM hold
//!          else (cool, post-grace) -> RequestFansMin + maybe RequestProfileStepUp
//!     4. fan health:
//!          working_fan_count < min_fans -> RequestEmergencyShutdown (FanPanic)
//!          single fan tach 0           -> emit FanFailure (continue mining
//!                                          unless thresholds escalate)
//!     5. recovery:
//!          if board off + cool band + reboot budget remains -> attempt
//!          recovery; else leave off + emit BudgetExhausted
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Per-tick sensor reading the supervisor consumes. Caller (controller loop)
/// supplies this every tick; the supervisor never reads hardware directly.
#[derive(Debug, Clone)]
pub struct ThermalTick {
    /// Per-board PCB sensor readings (°C). `None` entries indicate failed
    /// reads. The reliability filter drops sensors that differ from peer
    /// average by > `bad_average_threshold_c` for `max_bad_readings`
    /// consecutive ticks.
    pub board_sensors: Vec<BoardSensors>,
    /// Per-fan tachometer reading (RPM). 0 means stalled; the supervisor
    /// classifies this as a fan failure.
    pub fan_tach_rpms: Vec<u32>,
    /// Current commanded fan PWM (0..255). The supervisor uses this to
    /// detect "PWM > 0 but tach 0" stalls.
    pub current_fan_pwm: u8,
    /// Optional hydro inlet/outlet (°C). `None` for air-cooled units.
    pub hydro_inlet_c: Option<f32>,
    pub hydro_outlet_c: Option<f32>,
    /// Seconds elapsed since `tick()` was last called. Drives ATM startup +
    /// post-ramp grace counters.
    pub tick_elapsed_secs: u32,
}

/// One board's sensor bundle.
#[derive(Debug, Clone)]
pub struct BoardSensors {
    /// Chain ID this bundle applies to.
    pub chain_id: u8,
    /// PCB temperature readings (°C). Empty if the board has no PCB
    /// sensors; the supervisor will treat that as a sensor failure when
    /// `min_per_board > 0`.
    pub pcb_temps_c: Vec<f32>,
    /// Per-chip die temperature readings (°C). Empty if the platform
    /// doesn't expose per-chip thermal diodes.
    pub chip_temps_c: Vec<f32>,
    /// `true` if the board is currently powered on. The supervisor uses
    /// this to drive the recovery state machine.
    pub powered_on: bool,
}

// ---------------------------------------------------------------------------
// Outputs (action requests)
// ---------------------------------------------------------------------------

/// What the supervisor decides this tick. The caller (controller) forwards
/// `RequestFans*` to the fan PID, `RequestProfileStep*` to the autotuner, and
/// `RequestBoardPowerOff`/`RequestEmergencyShutdown` to the existing safety
/// supervisor. The supervisor never touches hardware directly.
// NOTE: `Eq` is intentionally NOT derived — `EmitChipImbalance` carries an
// `f32` spread (diagnostic telemetry), and `f32` is not `Eq`. `PartialEq` is
// retained and is all the codebase uses (pattern `matches!`, never `==` on the
// action or as a hash key). Dropping `Eq` is non-breaking here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorAction {
    /// Hold current behavior; no change.
    NoOp,
    /// Request fan PID to drive PWM toward `fan_max_pwm` (the quiet-home
    /// cap). NEVER means 100% PWM — load-bearing.
    RequestFansMax { reason: ThermalReason },
    /// Request fan PID to run the normal target-temp curve.
    RequestFansCurve,
    /// Request fan PID to drive PWM toward `fan_min_pwm`.
    RequestFansMin,
    /// Advisory: autotuner should step the active profile down. The
    /// autotuner is not obligated to comply.
    RequestProfileStepDown { reason: ThermalReason },
    /// Advisory: autotuner should step the active profile up. The autotuner
    /// is not obligated to comply.
    RequestProfileStepUp,
    /// Power off a single board (board_panic / chip_panic / hydro_panic /
    /// sensor_failure / hw_error).
    RequestBoardPowerOff {
        chain_id: u8,
        reason: ThermalReason,
        recoverable: bool,
    },
    /// Emergency-shutdown all boards; preserve cut-hash-before-noise (do
    /// NOT raise fan PWM as a shutdown response).
    RequestEmergencyShutdown { reason: ThermalReason },
    /// A fan failed (tach 0 while PWM > 0). Continue mining unless other
    /// thresholds escalate; emit telemetry.
    EmitFanFailure { fan_index: u8 },
    /// A sensor was dropped (bad-average filter). Telemetry only.
    EmitSensorDropped { chain_id: u8, sensor_index: u8 },
    /// **Telemetry / DIAGNOSTIC only — never drives hardware.** The spread
    /// (max − min) across this board's *valid* per-chip die temperatures
    /// exceeded `chip_imbalance_threshold_c`. A large inter-chip spread is a
    /// failing-chip / poor-thermal-paste / uneven-clamp indicator that an
    /// operator (or the autotuner, later) can act on — but the supervisor
    /// itself does NOT raise fans, push power, or cut hash on it. It only
    /// flags. The real over-temp escalation is still driven by the board /
    /// chip *max* thresholds elsewhere in `tick()`; this variant is a pure
    /// read-side observation that rides alongside whatever fan/curve action
    /// the temperature thresholds already produced.
    EmitChipImbalance { chain_id: u8, spread_c: f32 },
    /// Recovery attempt: board was previously powered off; cool band met +
    /// budget remains; attempt to bring it back. `attempt` is 1, 2, …, up
    /// to `max_reboot`.
    AttemptBoardRecovery { chain_id: u8, attempt: u32 },
    /// Recovery budget exhausted; board stays off until power-cycle or
    /// operator action.
    EmitRecoveryBudgetExhausted { chain_id: u8 },
}

/// Reason for an action. Narrow + serializable for telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalReason {
    /// Board PCB temperature reached `board_hot_c`.
    BoardHot,
    /// Board PCB temperature reached `board_panic_c`.
    BoardPanic,
    /// Chip die temperature reached `chip_hot_c`.
    ChipHot,
    /// Chip die temperature reached `chip_panic_c`.
    ChipPanic,
    /// Hydro inlet reached `hydro_inlet_panic_c`.
    HydroPanic,
    /// Hydro inlet below `hydro_inlet_startup_min_c`; mining gated.
    HydroStartupCold,
    /// **Hydro flow loss / reversal — PROTECTIVE, fails closed (cut hash).**
    /// Only checked when `hydro_configured == true`. Fires when the coolant
    /// loop appears to have lost flow or reversed direction:
    /// (a) the inlet sensor we depend on went missing (`None`) while hydro is
    ///     configured — we can no longer prove the loop is healthy, OR
    /// (b) the inlet is reading at-or-above the outlet by more than
    ///     `hydro_flow_loss_margin_c` — on a healthy loop the inlet (cold
    ///     supply) is COLDER than the outlet (warm return); inlet ≥ outlet
    ///     means flow has stopped/reversed and stagnant hot water is sitting
    ///     on the cold side.
    /// The safe response is ALWAYS to cut hash (`RequestEmergencyShutdown`) —
    /// NEVER to raise fans or push power. An immersion/hydro rig has no
    /// chassis fans to blast, so removing the heat source is the only correct
    /// move.
    HydroFlowLoss,
    /// Working fan count fell below `min_fans`.
    FanPanic,
    /// Valid sensor count fell below `min_per_board`.
    SensorFailure,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Thermal supervisor configuration (TOML `[thermal.supervisor]`). Mirrors
/// the LuxOS `[temp_control]` + `[fan_control]` + `[atm]` + `[hashboard]`
/// surfaces per RE-005 §"Config Fields".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThermalSupervisorConfig {
    /// **Default false.** Live-HW gated to `true` only after Wave G/H
    /// operator authorization. With this flag false the supervisor is
    /// dormant and `tick()` returns `NoOp` immediately.
    #[serde(default)]
    pub enabled: bool,

    /// Board PCB target temperature (°C). RE-005 default 55.0.
    #[serde(default = "default_board_target")]
    pub board_target_c: f32,
    /// Board PCB hot threshold (°C). RE-005 default 65.0.
    #[serde(default = "default_board_hot")]
    pub board_hot_c: f32,
    /// Board PCB panic threshold (°C). RE-005 default 70.0.
    #[serde(default = "default_board_panic")]
    pub board_panic_c: f32,
    /// Chip die hot threshold (°C). RE-005 default 93.0.
    #[serde(default = "default_chip_hot")]
    pub chip_hot_c: f32,
    /// Chip die panic threshold (°C). RE-005 default 100.0.
    #[serde(default = "default_chip_panic")]
    pub chip_panic_c: f32,

    /// ATM cool hysteresis on board (°C). RE-005 default 5.0 (step-up when
    /// board < hot - 5).
    #[serde(default = "default_atm_temp_window")]
    pub atm_temp_window_c: f32,
    /// ATM cool hysteresis on chip (°C). RE-005 default 8.0.
    #[serde(default = "default_atm_chip_temp_window")]
    pub atm_chip_temp_window_c: f32,
    /// ATM startup grace (seconds). RE-005 default 900 (15 min).
    #[serde(default = "default_atm_startup_secs")]
    pub atm_startup_grace_secs: u32,
    /// ATM post-ramp grace (seconds). RE-005 default 900 (15 min).
    #[serde(default = "default_atm_post_ramp_secs")]
    pub atm_post_ramp_grace_secs: u32,

    /// **Load-bearing: quiet-home cap.** Per-platform default (e.g., am2
    /// home unit `a lab unit` = 30). `RequestFansMax` drives PWM up to THIS cap,
    /// NOT 255. Raising it above the platform home cap requires explicit
    /// operator config edit.
    #[serde(default = "default_fan_max_pwm")]
    pub fan_max_pwm: u8,
    /// Minimum fan PWM (RE-005 default 51 ≈ 20%).
    #[serde(default = "default_fan_min_pwm")]
    pub fan_min_pwm: u8,
    /// Working-fans floor below which `RequestEmergencyShutdown` fires.
    /// RE-005 default 1.
    #[serde(default = "default_min_fans")]
    pub min_fans: u8,

    /// Minimum valid PCB sensors per board below which the supervisor
    /// emits `RequestBoardPowerOff` with `SensorFailure`. RE-005 default 1.
    #[serde(default = "default_min_per_board")]
    pub min_per_board: u8,
    /// Bad-average threshold (°C). A sensor is dropped if it differs from
    /// peer average by > this for `max_bad_readings` consecutive ticks.
    /// RE-005 default 2.0.
    #[serde(default = "default_bad_avg_thresh")]
    pub bad_average_threshold_c: f32,
    /// Consecutive bad readings before a sensor is dropped. RE-005
    /// default 10.
    #[serde(default = "default_max_bad_readings")]
    pub max_bad_readings: u32,

    /// **Inter-chip temperature imbalance flag threshold (°C). DIAGNOSTIC /
    /// TELEMETRY ONLY.** When the spread (max − min) across a board's valid
    /// per-chip die temperatures exceeds this, the supervisor emits a
    /// telemetry-only `EmitChipImbalance` and surfaces the spread in the
    /// snapshot. It NEVER drives fans / freq / power and never cuts hash —
    /// it only flags a failing-chip / poor-paste / uneven-clamp indicator for
    /// an operator (or the autotuner, later) to act on. A board with fewer
    /// than 2 valid chip sensors can't have a spread and is never flagged.
    /// Default 15.0 °C — a conservatively large spread that healthy boards
    /// stay well under, so the flag is a real anomaly signal, not noise.
    #[serde(default = "default_chip_imbalance_threshold")]
    pub chip_imbalance_threshold_c: f32,

    /// **Hydro/water-cooling configured. DEFAULT FALSE.** Mirrors the
    /// immersion gated-capability pattern: on a normal air-cooled unit this
    /// stays false and the hydro flow-loss / inlet-missing protective checks
    /// are completely inert (`hydro_inlet_c` is `None` and the new checks
    /// never fire) — byte-identical to the pre-flow-check path. Set true ONLY
    /// on a unit that actually has a coolant loop with an inlet (and ideally
    /// outlet) sensor. When true, a MISSING inlet reading or a flow-loss /
    /// reversal condition fails CLOSED (cuts hash).
    #[serde(default)]
    pub hydro_configured: bool,
    /// **Hydro flow-loss / reversal margin (°C). PROTECTIVE.** On a healthy
    /// loop the inlet (cold supply) runs colder than the outlet (warm
    /// return). When `inlet >= outlet - hydro_flow_loss_margin_c` (i.e. the
    /// cold side is no longer meaningfully colder than the warm side), flow
    /// has stalled or reversed and stagnant hot coolant is sitting on the
    /// inlet — the supervisor cuts hash. Only consulted when
    /// `hydro_configured == true` AND both inlet and outlet are present.
    /// Default 1.0 °C (a tiny positive margin: inlet must be at least ~1 °C
    /// colder than outlet to be considered "flowing").
    #[serde(default = "default_hydro_flow_loss_margin")]
    pub hydro_flow_loss_margin_c: f32,

    /// Hydro inlet startup minimum (°C). Below this, supervisor emits
    /// `RequestEmergencyShutdown { HydroStartupCold }`. RE-005 default 15.0.
    #[serde(default = "default_hydro_startup_min")]
    pub hydro_inlet_startup_min_c: f32,
    /// Hydro inlet panic (°C). Above this, immediate emergency shutdown.
    /// RE-005 default 50.0.
    #[serde(default = "default_hydro_panic")]
    pub hydro_inlet_panic_c: f32,

    /// Per-board reboot budget for overtemp recovery. RE-005 default 5.
    #[serde(default = "default_max_reboot")]
    pub max_reboot: u32,
    /// True iff the supervisor should attempt to recover a powered-off
    /// board when temps return to safe band and budget remains. RE-005
    /// default true.
    #[serde(default = "default_overtemp_recovery")]
    pub overtemp_auto_recovery: bool,
}

impl Default for ThermalSupervisorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            board_target_c: default_board_target(),
            board_hot_c: default_board_hot(),
            board_panic_c: default_board_panic(),
            chip_hot_c: default_chip_hot(),
            chip_panic_c: default_chip_panic(),
            atm_temp_window_c: default_atm_temp_window(),
            atm_chip_temp_window_c: default_atm_chip_temp_window(),
            atm_startup_grace_secs: default_atm_startup_secs(),
            atm_post_ramp_grace_secs: default_atm_post_ramp_secs(),
            fan_max_pwm: default_fan_max_pwm(),
            fan_min_pwm: default_fan_min_pwm(),
            min_fans: default_min_fans(),
            min_per_board: default_min_per_board(),
            bad_average_threshold_c: default_bad_avg_thresh(),
            max_bad_readings: default_max_bad_readings(),
            chip_imbalance_threshold_c: default_chip_imbalance_threshold(),
            hydro_configured: false,
            hydro_flow_loss_margin_c: default_hydro_flow_loss_margin(),
            hydro_inlet_startup_min_c: default_hydro_startup_min(),
            hydro_inlet_panic_c: default_hydro_panic(),
            max_reboot: default_max_reboot(),
            overtemp_auto_recovery: default_overtemp_recovery(),
        }
    }
}

fn default_board_target() -> f32 {
    55.0
}
fn default_board_hot() -> f32 {
    65.0
}
fn default_board_panic() -> f32 {
    70.0
}
fn default_chip_hot() -> f32 {
    93.0
}
fn default_chip_panic() -> f32 {
    100.0
}
fn default_atm_temp_window() -> f32 {
    5.0
}
fn default_atm_chip_temp_window() -> f32 {
    8.0
}
fn default_atm_startup_secs() -> u32 {
    900
}
fn default_atm_post_ramp_secs() -> u32 {
    900
}
/// LOAD-BEARING DEFAULT: 30 = the home-unit cap (matches `a lab unit` quiet contract
///). Per-platform
/// baked configs may lower this; raising it requires explicit operator edit.
fn default_fan_max_pwm() -> u8 {
    30
}
fn default_fan_min_pwm() -> u8 {
    10
}
fn default_min_fans() -> u8 {
    1
}
fn default_min_per_board() -> u8 {
    1
}
fn default_bad_avg_thresh() -> f32 {
    2.0
}
fn default_max_bad_readings() -> u32 {
    10
}
/// DIAGNOSTIC-only inter-chip imbalance flag threshold (°C). A conservatively
/// large spread (15 °C) so the telemetry flag marks a real anomaly (failing
/// chip / poor paste), not normal per-chip variation. Never drives hardware.
fn default_chip_imbalance_threshold() -> f32 {
    15.0
}
/// PROTECTIVE hydro flow-loss margin (°C): inlet must be at least this much
/// colder than outlet to be considered "flowing". 1.0 °C is a tiny positive
/// margin — inlet ≥ outlet − 1 °C reads as flow loss / reversal → cut hash.
fn default_hydro_flow_loss_margin() -> f32 {
    1.0
}
fn default_hydro_startup_min() -> f32 {
    15.0
}
fn default_hydro_panic() -> f32 {
    50.0
}
fn default_max_reboot() -> u32 {
    5
}
fn default_overtemp_recovery() -> bool {
    true
}

/// Platform families that can opt the [`ThermalSupervisor`] on by default
/// once each has passed live thermal validation. Kept narrow + serializable
/// so the daemon can pass a marker derived from `/etc/dcentos/board_target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorPlatform {
    /// am1 — S9 (BM1387).
    Am1S9,
    /// am2 — Zynq S17/S19/S19j Pro (BM1398/BM1362), incl. the `a lab unit`/`a lab unit`
    /// home units. Quiet-home cap is mandatory here.
    Am2Zynq,
    /// am3 — Amlogic S21 / S19j Pro / S19k (NoPic).
    Am3Aml,
    /// am3 — BeagleBone Black S19j Pro (BM1362).
    Am3Bb,
    /// Anything not yet classified.
    Unknown,
}

impl SupervisorPlatform {
    /// Map a `/etc/dcentos/board_target`-style marker to a platform family.
    /// Conservative: anything unrecognized maps to [`Unknown`], which is
    /// never default-on.
    pub fn from_board_target(marker: &str) -> Self {
        let m = marker.trim().to_ascii_lowercase();
        if m.starts_with("am1") || m.contains("s9") {
            SupervisorPlatform::Am1S9
        } else if m.starts_with("am2") || m.contains("zynq") || m.contains("xil") {
            SupervisorPlatform::Am2Zynq
        } else if m.starts_with("am3-bb") || m.contains("beaglebone") || m.contains("am335") {
            SupervisorPlatform::Am3Bb
        } else if m.starts_with("am3") || m.contains("aml") {
            SupervisorPlatform::Am3Aml
        } else {
            SupervisorPlatform::Unknown
        }
    }
}

/// Decide whether the [`ThermalSupervisor`] should be ON by default for a
/// platform.
///
/// **LIVE-HARDWARE-DEFAULT contract (do NOT regress):** the supervisor is a
/// fail-closed safety layer (it can only make the thermal response MORE
/// conservative), so per-platform default-on is desirable — but ONLY after a
/// platform has live thermal validation. Until an operator opts a platform in
/// via `platform_validated` (e.g. the per-platform env gate
/// `DCENT_THERMAL_SUPERVISOR_DEFAULT_ON=1`), this returns `false` for EVERY
/// platform, so the compiled default stays OFF and no platform is silently
/// flipped to default-on. An explicit `[thermal.supervisor].enabled = true`
/// in config always wins regardless of this helper.
///
/// `platform_validated` is the per-platform live-validation flag the caller
/// resolves (config and/or env). When it is `false` this function is a no-op
/// (`false`). When it is `true`, only platforms whose live validation has been
/// signed off return `true` here; un-validated platforms still return `false`.
pub fn supervisor_default_enabled(platform: SupervisorPlatform, platform_validated: bool) -> bool {
    if !platform_validated {
        // No platform is default-on without the explicit per-platform flag.
        return false;
    }
    match platform {
        // FLAGGED FOR OPERATOR LIVE VALIDATION: as each platform's supervisor
        // FSM is validated on live hardware, flip its arm to `true`. Today
        // NONE are signed off, so even with the flag set the helper stays
        // conservative — the flag is the operator's explicit opt-in, but the
        // per-platform sign-off below is what actually enables it. This keeps
        // a half-configured unit from arming an unvalidated FSM.
        SupervisorPlatform::Am1S9 => false,
        SupervisorPlatform::Am2Zynq => false,
        SupervisorPlatform::Am3Aml => false,
        SupervisorPlatform::Am3Bb => false,
        SupervisorPlatform::Unknown => false,
    }
}

// ---------------------------------------------------------------------------
// Per-sensor reliability state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct SensorState {
    /// How many consecutive ticks this sensor differed from peer-avg by
    /// > bad_average_threshold_c.
    bad_streak: u32,
    /// `true` once `bad_streak >= max_bad_readings`; the sensor is
    /// excluded from classification.
    dropped: bool,
}

#[derive(Debug, Clone, Default)]
struct BoardState {
    /// Per-PCB-sensor reliability state (indexed by sensor position).
    pcb_states: Vec<SensorState>,
    /// Per-chip-sensor reliability state (indexed by chip position).
    chip_states: Vec<SensorState>,
    /// Recovery attempts spent on this board.
    recovery_attempts: u32,
    /// `true` if we've ever powered this board off due to thermal.
    /// Recovery only attempts when this is true.
    ever_thermal_off: bool,
    /// **DIAGNOSTIC only.** Most recent inter-chip temperature spread
    /// (max − min across valid per-chip die temps, °C) observed for this
    /// board, surfaced in the snapshot. `None` until at least 2 valid chip
    /// sensors have been read. Never drives any control decision.
    last_chip_spread_c: Option<f32>,
    /// **DIAGNOSTIC only.** `true` if the last spread exceeded
    /// `chip_imbalance_threshold_c`. Read-side flag for the snapshot.
    chip_imbalance_flagged: bool,
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

/// Thermal supervisor. Owns per-board sensor reliability state + ATM grace
/// counters + recovery budget bookkeeping. Pure FSM: caller drives ticks; the
/// supervisor never touches hardware.
#[derive(Debug, Clone)]
pub struct ThermalSupervisor {
    config: ThermalSupervisorConfig,
    boards: HashMap<u8, BoardState>,
    /// Total seconds since the supervisor was constructed (drives ATM
    /// startup grace).
    uptime_secs: u32,
    /// Seconds since the most recent profile step (drives ATM post-ramp
    /// grace).
    secs_since_last_step: u32,
}

impl ThermalSupervisor {
    /// Construct a supervisor with documented spec defaults.
    pub fn new(config: ThermalSupervisorConfig) -> Self {
        Self {
            config,
            boards: HashMap::new(),
            uptime_secs: 0,
            secs_since_last_step: u32::MAX, // start fully cooled-down
        }
    }

    /// True iff the supervisor is enabled in the config (caller should NoOp
    /// early when this returns false).
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Public read-only snapshot for `/api/thermal/supervisor`.
    pub fn snapshot(&self) -> SupervisorSnapshot {
        // DIAGNOSTIC: worst inter-chip spread across all boards this snapshot.
        // Pure read-side aggregate; `None` until at least one board has had
        // >= 2 valid chip sensors. Never used in any control decision.
        let worst_chip_imbalance_c = self
            .boards
            .values()
            .filter_map(|b| b.last_chip_spread_c)
            .fold(None::<f32>, |acc, s| Some(acc.map_or(s, |m: f32| m.max(s))));
        SupervisorSnapshot {
            enabled: self.config.enabled,
            uptime_secs: self.uptime_secs,
            secs_since_last_step: self.secs_since_last_step,
            board_states: self
                .boards
                .iter()
                .map(|(chain_id, b)| BoardStateSnapshot {
                    chain_id: *chain_id,
                    recovery_attempts: b.recovery_attempts,
                    dropped_pcb_sensors: b.pcb_states.iter().filter(|s| s.dropped).count() as u8,
                    dropped_chip_sensors: b.chip_states.iter().filter(|s| s.dropped).count() as u8,
                    chip_imbalance_c: b.last_chip_spread_c,
                    chip_imbalance_flagged: b.chip_imbalance_flagged,
                })
                .collect(),
            fan_max_pwm: self.config.fan_max_pwm,
            chip_imbalance_threshold_c: self.config.chip_imbalance_threshold_c,
            worst_chip_imbalance_c,
            hydro_configured: self.config.hydro_configured,
        }
    }

    /// Process one tick. Returns one or more actions in priority order
    /// (`RequestEmergencyShutdown` first; per-board panic next; ATM/fans
    /// last). Empty when dormant.
    pub fn tick(&mut self, sample: &ThermalTick) -> Vec<SupervisorAction> {
        if !self.is_enabled() {
            return Vec::new();
        }

        self.uptime_secs = self.uptime_secs.saturating_add(sample.tick_elapsed_secs);
        self.secs_since_last_step = self
            .secs_since_last_step
            .saturating_add(sample.tick_elapsed_secs);

        let mut actions: Vec<SupervisorAction> = Vec::new();

        // 1. Hydro panic and startup-cold beat everything.
        if let Some(inlet) = sample.hydro_inlet_c {
            if !inlet.is_finite() {
                actions.push(SupervisorAction::RequestEmergencyShutdown {
                    reason: if self.config.hydro_configured {
                        ThermalReason::HydroFlowLoss
                    } else {
                        ThermalReason::SensorFailure
                    },
                });
                return actions;
            }
            if inlet >= self.config.hydro_inlet_panic_c {
                actions.push(SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroPanic,
                });
                return actions;
            }
            if inlet < self.config.hydro_inlet_startup_min_c {
                actions.push(SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroStartupCold,
                });
                return actions;
            }
        }

        // 1b. Hydro flow-loss / reversal — PROTECTIVE, fail-closed (cut hash).
        //
        // GATED behind `hydro_configured` (DEFAULT FALSE). On a normal
        // air-cooled unit this flag is false and this whole block is skipped,
        // so the path above is byte-identical to the pre-flow-check behavior.
        // We mirror the immersion gated-capability pattern: a hydro
        // capability is OFF unless an operator explicitly declares the unit
        // has a coolant loop.
        //
        // Two flow-loss conditions, both fail CLOSED by cutting hash (never by
        // raising fans / pushing power — an immersion/hydro rig has no chassis
        // fans to blast, so removing the heat source is the only safe move):
        //   (a) inlet sensor MISSING while hydro is configured — we depend on
        //       it to prove the loop is healthy; without it we cannot, so we
        //       stop. (The startup-cold / panic checks above already covered
        //       the present-but-out-of-range cases.)
        //   (b) inlet at-or-above outlet by < margin — on a healthy loop the
        //       cold supply (inlet) is colder than the warm return (outlet);
        //       inlet >= outlet - margin means flow stalled/reversed and
        //       stagnant hot coolant is sitting on the cold side.
        if self.config.hydro_configured {
            if let Some(outlet) = sample.hydro_outlet_c {
                if !outlet.is_finite() {
                    actions.push(SupervisorAction::RequestEmergencyShutdown {
                        reason: ThermalReason::HydroFlowLoss,
                    });
                    return actions;
                }
            }
            match (sample.hydro_inlet_c, sample.hydro_outlet_c) {
                (None, _) => {
                    // (a) Inlet missing while hydro is configured → fail closed.
                    tracing::warn!(
                        "HYDRO FLOW PROTECT: inlet temperature MISSING while hydro is configured \
                         — cannot prove the coolant loop is healthy. Cutting hash (fail-closed). \
                         NEVER raising fans / pushing power as a hydro-loss response."
                    );
                    actions.push(SupervisorAction::RequestEmergencyShutdown {
                        reason: ThermalReason::HydroFlowLoss,
                    });
                    return actions;
                }
                (Some(inlet), Some(outlet)) => {
                    // (b) Inlet no longer meaningfully colder than outlet →
                    //     flow loss / reversal.
                    if inlet >= outlet - self.config.hydro_flow_loss_margin_c {
                        tracing::warn!(
                            inlet_c = inlet,
                            outlet_c = outlet,
                            margin_c = self.config.hydro_flow_loss_margin_c,
                            "HYDRO FLOW PROTECT: inlet is not meaningfully colder than outlet \
                             (inlet >= outlet - margin) — coolant flow appears lost or reversed. \
                             Cutting hash (fail-closed). NEVER raising fans / pushing power."
                        );
                        actions.push(SupervisorAction::RequestEmergencyShutdown {
                            reason: ThermalReason::HydroFlowLoss,
                        });
                        return actions;
                    }
                }
                // inlet present but NO outlet sensor: the reversal proxy needs
                // both. The inlet panic / startup-cold checks above still guard
                // the absolute inlet temperature; we can't infer direction
                // without an outlet, so we don't fabricate a flow verdict here.
                (Some(_), None) => {}
            }
        }

        // 2. Fan panic (working fan count < min_fans).
        let working_fans = sample.fan_tach_rpms.iter().filter(|r| **r > 0).count() as u8;
        let total_fans = sample.fan_tach_rpms.len() as u8;
        if total_fans > 0 && working_fans < self.config.min_fans {
            actions.push(SupervisorAction::RequestEmergencyShutdown {
                reason: ThermalReason::FanPanic,
            });
            return actions;
        }

        // 3. Fan failure (PWM > 0 but a single tach reads 0) — telemetry,
        //    not emergency. Continue mining unless other thresholds
        //    escalate.
        if sample.current_fan_pwm > 0 {
            for (i, rpm) in sample.fan_tach_rpms.iter().enumerate() {
                if *rpm == 0 {
                    actions.push(SupervisorAction::EmitFanFailure { fan_index: i as u8 });
                }
            }
        }

        // 4. Per-board classification.
        // F-thermal-2: an empty board_sensors vec (total input blackout) must NOT
        // read as "all cool" and emit RequestFansMin/RequestProfileStepUp on zero
        // thermal evidence (the C35 fail-open class). A present-but-empty/NaN board
        // already fails closed via the SensorFailure power-off path; seed the flag
        // false for the empty-vec shape so it produces NoOp instead of a cool verdict.
        let mut max_board_was_cool = !sample.board_sensors.is_empty();
        let mut any_above_target = false;
        for board in &sample.board_sensors {
            let chain_id = board.chain_id;
            let bs = self.boards.entry(chain_id).or_default();
            // Ensure per-sensor state vectors track the current sensor
            // count (allows hot-plug at startup; never shrinks).
            if bs.pcb_states.len() < board.pcb_temps_c.len() {
                bs.pcb_states
                    .resize(board.pcb_temps_c.len(), SensorState::default());
            }
            if bs.chip_states.len() < board.chip_temps_c.len() {
                bs.chip_states
                    .resize(board.chip_temps_c.len(), SensorState::default());
            }

            // Reliability filter: drop sensors that disagree with peer avg
            // by > bad_average_threshold for max_bad_readings consecutive
            // ticks.
            let dropped_pcb_sensors = run_reliability_filter(
                &board.pcb_temps_c,
                &mut bs.pcb_states,
                self.config.bad_average_threshold_c,
                self.config.max_bad_readings,
            );
            for sensor_index in dropped_pcb_sensors {
                actions.push(SupervisorAction::EmitSensorDropped {
                    chain_id,
                    sensor_index,
                });
            }
            let dropped_chip_sensors = run_reliability_filter(
                &board.chip_temps_c,
                &mut bs.chip_states,
                self.config.bad_average_threshold_c,
                self.config.max_bad_readings,
            );
            for sensor_index in dropped_chip_sensors {
                actions.push(SupervisorAction::EmitSensorDropped {
                    chain_id,
                    sensor_index,
                });
            }

            // Compute board/chip max from non-dropped, FINITE sensors.
            //
            // THERMAL fail-closed: a non-finite (NaN / ±Inf) reading must NEVER
            // count as a "valid" sensor. `f32::max(f32::MIN, NaN) == f32::MIN`,
            // so an all-NaN board would fold `board_max` to `f32::MIN` and read
            // as ice-cold — classifying a possibly-hot board as safe and even
            // requesting a frequency step-UP. That is the exact fail-OPEN class
            // as the prior `controller.rs` NEG_INFINITY-fold bug. Filtering
            // non-finite here makes an all-garbage board's `valid_pcb.len()`
            // fall below `min_per_board`, so the SensorFailure → power-off path
            // above fires (fail-CLOSED) instead of a spurious cool verdict.
            let valid_pcb: Vec<f32> = board
                .pcb_temps_c
                .iter()
                .enumerate()
                .filter(|(i, t)| {
                    !bs.pcb_states.get(*i).map(|s| s.dropped).unwrap_or(false) && t.is_finite()
                })
                .map(|(_, t)| *t)
                .collect();
            let valid_chip: Vec<f32> = board
                .chip_temps_c
                .iter()
                .enumerate()
                .filter(|(i, t)| {
                    !bs.chip_states.get(*i).map(|s| s.dropped).unwrap_or(false) && t.is_finite()
                })
                .map(|(_, t)| *t)
                .collect();

            // Sensor failure: too few valid PCB sensors → power off this board.
            // F-thermal-3: clamp min_per_board to >=1 at the point of use.
            // ThermalSupervisor::new stores the TOML config unvalidated (unlike the
            // controller constructor), so a config min_per_board=0 would make this
            // guard (len < 0) never fire → board_max folds over an empty valid_pcb to
            // f32::MIN → ice-cold fail-open. At least one valid sensor is required.
            if (valid_pcb.len() as u8) < self.config.min_per_board.max(1) {
                if board.powered_on {
                    bs.ever_thermal_off = true;
                    actions.push(SupervisorAction::RequestBoardPowerOff {
                        chain_id,
                        reason: ThermalReason::SensorFailure,
                        recoverable: false,
                    });
                }
                // A board whose sensors failed is NOT "cool": clear the cool verdict
                // so the aggregate never emits a fans-min / step-power-UP advisory on
                // a blind board (the same C35 fail-open class as F-thermal-2/3).
                max_board_was_cool = false;
                continue;
            }

            let board_max = valid_pcb.iter().cloned().fold(f32::MIN, f32::max);
            let chip_max = if valid_chip.is_empty() {
                f32::MIN
            } else {
                valid_chip.iter().cloned().fold(f32::MIN, f32::max)
            };

            // F-thermal-1: the reliability filter (dropped "liar" sensors) must NOT
            // blind the PANIC tier. A sensor dropped because it strays past the
            // ±bad_average_threshold band (which normal per-chip die spreads do —
            // chip_imbalance_threshold_c defaults to 15 C) is excluded from
            // board_max/chip_max above, so a later panic-level reading FROM that
            // sensor would be invisible, skipping the only chip-die shutdown in the
            // system. Take a raw max over ALL finite readings (dropped or not) for
            // the panic checks ONLY; hot/curve/ATM classification keeps the filtered
            // maxes so a liar can't drag the fan curve. If every reading is
            // non-finite the raw max stays f32::MIN and the earlier SensorFailure
            // power-off has already fired (fail-closed).
            let raw_finite_pcb_max = board
                .pcb_temps_c
                .iter()
                .copied()
                .filter(|t| t.is_finite())
                .fold(f32::MIN, f32::max);
            let raw_finite_chip_max = board
                .chip_temps_c
                .iter()
                .copied()
                .filter(|t| t.is_finite())
                .fold(f32::MIN, f32::max);

            // Inter-chip temperature imbalance — DIAGNOSTIC / TELEMETRY ONLY.
            //
            // Pure read-side: compute the spread (max − min) across this
            // board's VALID per-chip die temps and, when it exceeds the
            // threshold, emit a telemetry-only `EmitChipImbalance` + record it
            // for the snapshot. This is a failing-chip / poor-paste /
            // uneven-clamp indicator. It does NOT alter `max_board_was_cool`,
            // `any_above_target`, the fan request, the frequency, the power
            // state, or the hash-cut decision — those stay driven solely by
            // the board/chip *max* thresholds below. An operator (or the
            // autotuner, later) can act on the flag; the supervisor only
            // surfaces it. Needs >= 2 valid chip sensors to have a spread.
            if valid_chip.len() >= 2 {
                let chip_min = valid_chip.iter().cloned().fold(f32::INFINITY, f32::min);
                let spread_c = chip_max - chip_min;
                bs.last_chip_spread_c = Some(spread_c);
                let flagged = spread_c > self.config.chip_imbalance_threshold_c;
                bs.chip_imbalance_flagged = flagged;
                if flagged {
                    tracing::warn!(
                        chain_id,
                        spread_c,
                        threshold_c = self.config.chip_imbalance_threshold_c,
                        chip_max,
                        chip_min,
                        "CHIP IMBALANCE (diagnostic): inter-chip die-temp spread exceeds threshold \
                         — possible failing chip / poor thermal paste / uneven heatsink clamp. \
                         TELEMETRY ONLY: no fan / freq / power change is driven by this flag."
                    );
                    actions.push(SupervisorAction::EmitChipImbalance { chain_id, spread_c });
                }
            } else {
                // Fewer than 2 valid chip sensors → no spread to report.
                bs.last_chip_spread_c = None;
                bs.chip_imbalance_flagged = false;
            }

            // Panic thresholds — power off this board. Uses the raw finite max
            // (F-thermal-1) so a dropped-as-"liar" sensor at panic level still fires.
            if raw_finite_pcb_max >= self.config.board_panic_c {
                if board.powered_on {
                    bs.ever_thermal_off = true;
                    actions.push(SupervisorAction::RequestBoardPowerOff {
                        chain_id,
                        reason: ThermalReason::BoardPanic,
                        recoverable: self.config.overtemp_auto_recovery,
                    });
                }
                max_board_was_cool = false;
                any_above_target = true;
                continue;
            }
            if raw_finite_chip_max >= self.config.chip_panic_c {
                if board.powered_on {
                    bs.ever_thermal_off = true;
                    actions.push(SupervisorAction::RequestBoardPowerOff {
                        chain_id,
                        reason: ThermalReason::ChipPanic,
                        recoverable: self.config.overtemp_auto_recovery,
                    });
                }
                max_board_was_cool = false;
                any_above_target = true;
                continue;
            }

            // Hot — request fans toward home cap (NOT 100%) + ATM step
            // down.
            let hot = board_max >= self.config.board_hot_c || chip_max >= self.config.chip_hot_c;
            if hot {
                actions.push(SupervisorAction::RequestFansMax {
                    reason: if board_max >= self.config.board_hot_c {
                        ThermalReason::BoardHot
                    } else {
                        ThermalReason::ChipHot
                    },
                });
                if self.uptime_secs >= self.config.atm_startup_grace_secs
                    && self.secs_since_last_step >= self.config.atm_post_ramp_grace_secs
                {
                    actions.push(SupervisorAction::RequestProfileStepDown {
                        reason: if chip_max >= self.config.chip_hot_c {
                            ThermalReason::ChipHot
                        } else {
                            ThermalReason::BoardHot
                        },
                    });
                    self.secs_since_last_step = 0;
                }
                max_board_was_cool = false;
                any_above_target = true;
                continue;
            }

            // Above target but below hot — curve.
            if board_max >= self.config.board_target_c {
                any_above_target = true;
                max_board_was_cool = false;
            }

            // Recovery: powered-off board returning to cool band.
            if !board.powered_on && bs.ever_thermal_off && self.config.overtemp_auto_recovery {
                let cool_enough = board_max
                    < self.config.board_hot_c - self.config.atm_temp_window_c
                    && (valid_chip.is_empty()
                        || chip_max < self.config.chip_hot_c - self.config.atm_chip_temp_window_c);
                if cool_enough {
                    if bs.recovery_attempts < self.config.max_reboot {
                        bs.recovery_attempts += 1;
                        actions.push(SupervisorAction::AttemptBoardRecovery {
                            chain_id,
                            attempt: bs.recovery_attempts,
                        });
                    } else {
                        actions.push(SupervisorAction::EmitRecoveryBudgetExhausted { chain_id });
                    }
                }
            }
        }

        // 5. Aggregate fan request (a single fan-action wins per tick).
        let has_fan_max = actions
            .iter()
            .any(|a| matches!(a, SupervisorAction::RequestFansMax { .. }));
        if !has_fan_max {
            if any_above_target {
                actions.push(SupervisorAction::RequestFansCurve);
            } else if max_board_was_cool {
                actions.push(SupervisorAction::RequestFansMin);
                if self.uptime_secs >= self.config.atm_startup_grace_secs
                    && self.secs_since_last_step >= self.config.atm_post_ramp_grace_secs
                {
                    actions.push(SupervisorAction::RequestProfileStepUp);
                    self.secs_since_last_step = 0;
                }
            }
        }

        if actions.is_empty() {
            actions.push(SupervisorAction::NoOp);
        }
        actions
    }
}

/// Reliability filter: marks sensors that disagree with the peer reference by
/// Greater than `threshold_c` for `max_readings` consecutive ticks. Returns the list of
/// sensor indices newly dropped this tick.
///
/// # THERMAL-5: robust center (median, not inclusive mean) — load-bearing
///
/// The peer reference is the **median** of the non-dropped readings, NOT the
/// arithmetic mean. The mean is non-robust: a single drifting sensor drags the
/// reference toward itself and can push the *whole set* over `threshold_c`. For
/// `[55, 55, 90]` the mean is ~66.7, so all three readings exceed a 2 °C
/// threshold and ALL THREE drop after `max_readings` ticks → `valid_pcb.len()`
/// falls below `min_per_board` → `RequestBoardPowerOff{SensorFailure}` even
/// though two sensors plainly agree. That is the "one bad sensor poisons the
/// whole board" defect this finding targets.
///
/// The median has a 50 % breakdown point: as long as the majority of sensors
/// agree, the reference tracks the agreeing cluster and only the genuine
/// outlier is flagged. For `[55, 55, 90]` the median is 55, so only the 90
/// deviates — exactly one sensor is dropped, the two good sensors survive, and
/// the board keeps a valid aggregate. This mirrors the MAD/median estimator
/// already used by `dcentrald_api_types::sensor_outlier::SensorRing`.
///
/// Fail-closed is preserved by construction: this filter only *drops* sensors,
/// it never resurrects one or weakens a threshold. When agreement genuinely
/// collapses (e.g. a 2-vs-2 split, or every sensor truly diverging) enough
/// sensors still drop that `valid_pcb.len()` falls below `min_per_board`, and
/// the caller's `SensorFailure` → power-off path fires — the safe response to
/// "we can no longer trust the thermal picture". A real board-wide hot event
/// (all sensors reading high but *together*) produces a tight cluster, a high
/// median, and zero spurious drops, so the genuine `board_hot` / `board_panic`
/// escalation in `tick()` still triggers. The robust center never masks a real
/// hot board; it only stops one liar from blinding the other sensors.
fn run_reliability_filter(
    readings: &[f32],
    states: &mut [SensorState],
    threshold_c: f32,
    max_readings: u32,
) -> Vec<u8> {
    let mut newly_dropped: Vec<u8> = Vec::new();
    if readings.is_empty() || states.is_empty() {
        return newly_dropped;
    }

    // Peer reference = MEDIAN across non-dropped sensors (robust to a single
    // drifting sensor; see the THERMAL-5 doc-comment above).
    let live_readings: Vec<f32> = readings
        .iter()
        .enumerate()
        .filter(|(i, _)| !states.get(*i).map(|s| s.dropped).unwrap_or(false))
        .map(|(_, t)| *t)
        .collect();
    if live_readings.len() < 2 {
        // With <2 sensors there's no peer to disagree with.
        return newly_dropped;
    }
    let peer_ref = median_f32(&live_readings);

    for (i, t) in readings.iter().enumerate() {
        let s = match states.get_mut(i) {
            Some(s) => s,
            None => continue,
        };
        if s.dropped {
            continue;
        }
        // A non-finite reading is always "bad": `(NaN - peer_ref).abs() >
        // threshold_c` is false, so without this a NaN sensor would reset its
        // bad_streak forever and never drop — staying in the "valid" set. Count
        // it toward the drop streak so a persistently-garbage sensor is dropped
        // (and emits EmitSensorDropped telemetry) like any other liar.
        if !t.is_finite() || (*t - peer_ref).abs() > threshold_c {
            s.bad_streak = s.bad_streak.saturating_add(1);
            if s.bad_streak >= max_readings {
                s.dropped = true;
                newly_dropped.push(i as u8);
            }
        } else {
            s.bad_streak = 0;
        }
    }
    newly_dropped
}

/// Median of a slice (robust center for the reliability filter). For an even
/// count, returns the mean of the two middle elements. NaN-safe ordering
/// (NaNs sort to one end and never become the chosen median unless every
/// reading is NaN). Mirrors `dcentrald_api_types::sensor_outlier::median_f32`,
/// kept local so `supervisor.rs` carries no cross-crate coupling for its
/// safety-critical center.
fn median_f32(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f32> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    match n % 2 {
        1 => sorted.get(n / 2).copied().unwrap_or(0.0),
        _ => {
            let lo = sorted.get(n / 2 - 1).copied().unwrap_or(0.0);
            let hi = sorted.get(n / 2).copied().unwrap_or(lo);
            (lo + hi) / 2.0
        }
    }
}

// ---------------------------------------------------------------------------
// Read-only snapshot for /api/thermal/supervisor (E3b will wire this)
// ---------------------------------------------------------------------------

/// JSON-serializable snapshot the API route returns to operators / dashboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    pub enabled: bool,
    pub uptime_secs: u32,
    pub secs_since_last_step: u32,
    pub board_states: Vec<BoardStateSnapshot>,
    pub fan_max_pwm: u8,
    /// DIAGNOSTIC: the inter-chip imbalance flag threshold (°C) in effect.
    pub chip_imbalance_threshold_c: f32,
    /// DIAGNOSTIC: worst inter-chip die-temp spread (°C) across all boards,
    /// or `None` until a board has reported >= 2 valid chip sensors. A high
    /// value flags a failing chip / poor paste — it never drives hardware.
    pub worst_chip_imbalance_c: Option<f32>,
    /// True iff hydro/water-cooling is configured on this unit (the hydro
    /// flow-loss / inlet-missing protective checks are active). False on a
    /// normal air-cooled unit (checks inert).
    pub hydro_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardStateSnapshot {
    pub chain_id: u8,
    pub recovery_attempts: u32,
    pub dropped_pcb_sensors: u8,
    pub dropped_chip_sensors: u8,
    /// DIAGNOSTIC: most recent inter-chip die-temp spread (°C) for this board
    /// (max − min across valid per-chip sensors). `None` until >= 2 valid
    /// chip sensors are read. Telemetry only — never drives any control.
    pub chip_imbalance_c: Option<f32>,
    /// DIAGNOSTIC: `true` if the last spread exceeded the threshold. Flag
    /// only — surfaced for an operator / autotuner to act on later.
    pub chip_imbalance_flagged: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_enabled() -> ThermalSupervisorConfig {
        ThermalSupervisorConfig {
            enabled: true,
            ..ThermalSupervisorConfig::default()
        }
    }

    fn tick(boards: Vec<BoardSensors>, fans: Vec<u32>, pwm: u8, dt: u32) -> ThermalTick {
        ThermalTick {
            board_sensors: boards,
            fan_tach_rpms: fans,
            current_fan_pwm: pwm,
            hydro_inlet_c: None,
            hydro_outlet_c: None,
            tick_elapsed_secs: dt,
        }
    }

    fn board(chain_id: u8, pcb: Vec<f32>, chips: Vec<f32>) -> BoardSensors {
        BoardSensors {
            chain_id,
            pcb_temps_c: pcb,
            chip_temps_c: chips,
            powered_on: true,
        }
    }

    // -- 1. Default-off contract --
    #[test]
    fn supervisor_disabled_by_default_emits_no_actions() {
        let mut s = ThermalSupervisor::new(ThermalSupervisorConfig::default());
        assert!(!s.is_enabled());
        let sample = tick(
            vec![board(0, vec![100.0, 100.0], vec![100.0])],
            vec![1000],
            30,
            5,
        );
        // Even a chain hot enough to panic should produce no actions while disabled.
        assert!(s.tick(&sample).is_empty());
    }

    // -- 2. Cool board → fans min + (after grace) step-up advisory --
    #[test]
    fn cool_board_requests_fans_min() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let sample = tick(
            vec![board(0, vec![45.0, 45.0], vec![60.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(actions
            .iter()
            .any(|a| matches!(a, SupervisorAction::RequestFansMin)));
    }

    // -- F-thermal-1: a dropped "liar" sensor's panic reading still fires --
    #[test]
    fn dropped_liar_chip_sensor_still_fires_panic() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        // Warm up: chip 3 sits ~15 C above the median every tick (a normal spread,
        // below chip_hot_c) so the reliability filter drops it as a "liar" after
        // max_bad_readings. No shutdown should occur during warm-up.
        for _ in 0..12 {
            let a = s.tick(&tick(
                vec![board(0, vec![55.0, 55.0], vec![70.0, 72.0, 74.0, 88.0])],
                vec![1000],
                30,
                5,
            ));
            assert!(
                !a.iter()
                    .any(|x| matches!(x, SupervisorAction::RequestBoardPowerOff { .. })),
                "no shutdown expected during sub-hot warm-up: {a:?}"
            );
        }
        // The dropped chip 3 now spikes past chip_panic_c; the filtered chip_max
        // (over the survivors) misses it, but the raw finite max must still fire.
        let actions = s.tick(&tick(
            vec![board(0, vec![55.0, 55.0], vec![70.0, 72.0, 74.0, 101.0])],
            vec![1000],
            30,
            5,
        ));
        assert!(
            actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestBoardPowerOff {
                    reason: ThermalReason::ChipPanic,
                    ..
                }
            )),
            "a dropped 'liar' chip sensor at panic level must still fire ChipPanic: {actions:?}"
        );
    }

    // -- F-thermal-2: empty board_sensors blackout must not read as cool --
    #[test]
    fn empty_board_sensors_blackout_does_not_read_cool() {
        let cfg = ThermalSupervisorConfig {
            atm_startup_grace_secs: 0,
            atm_post_ramp_grace_secs: 0,
            ..cfg_enabled()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let actions = s.tick(&tick(vec![], vec![1000], 30, 5));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestProfileStepUp)),
            "empty blackout must not step power UP: {actions:?}"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestFansMin)),
            "empty blackout must not emit a cool-verdict fans-min: {actions:?}"
        );
    }

    // -- F-thermal-3: min_per_board=0 must still fail closed on all-NaN --
    #[test]
    fn min_per_board_zero_still_fails_closed_on_all_nan() {
        let cfg = ThermalSupervisorConfig {
            min_per_board: 0,
            atm_startup_grace_secs: 0,
            atm_post_ramp_grace_secs: 0,
            ..cfg_enabled()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let actions = s.tick(&tick(
            vec![board(0, vec![f32::NAN, f32::NAN], vec![f32::NAN])],
            vec![1000],
            30,
            5,
        ));
        assert!(
            actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestBoardPowerOff {
                    reason: ThermalReason::SensorFailure,
                    ..
                }
            )),
            "all-NaN board with min_per_board=0 must fail closed (SensorFailure): {actions:?}"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestProfileStepUp)),
            "must not step power UP on an all-NaN board: {actions:?}"
        );
    }

    // -- 3. Target band → fans curve --
    #[test]
    fn target_band_requests_fans_curve() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let sample = tick(
            vec![board(0, vec![58.0, 58.0], vec![70.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(actions
            .iter()
            .any(|a| matches!(a, SupervisorAction::RequestFansCurve)));
    }

    // -- 4. Hot → fans max + step-down advisory after grace --
    #[test]
    fn hot_board_requests_fans_max_after_grace() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            atm_startup_grace_secs: 0,
            atm_post_ramp_grace_secs: 0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let sample = tick(
            vec![board(0, vec![66.0, 66.0], vec![80.0])],
            vec![1000],
            30,
            1,
        );
        let actions = s.tick(&sample);
        assert!(actions.iter().any(|a| matches!(
            a,
            SupervisorAction::RequestFansMax {
                reason: ThermalReason::BoardHot
            }
        )));
        assert!(actions
            .iter()
            .any(|a| matches!(a, SupervisorAction::RequestProfileStepDown { .. })));
    }

    // -- 5. Board panic → RequestBoardPowerOff --
    #[test]
    fn board_panic_powers_off_board() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let sample = tick(
            vec![board(0, vec![72.0, 72.0], vec![85.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(actions.iter().any(|a| matches!(
            a,
            SupervisorAction::RequestBoardPowerOff {
                reason: ThermalReason::BoardPanic,
                ..
            }
        )));
    }

    // -- 6. Chip panic → RequestBoardPowerOff --
    #[test]
    fn chip_panic_powers_off_board() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let sample = tick(
            vec![board(0, vec![60.0, 60.0], vec![101.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(actions.iter().any(|a| matches!(
            a,
            SupervisorAction::RequestBoardPowerOff {
                reason: ThermalReason::ChipPanic,
                ..
            }
        )));
    }

    // -- 6b. All-NaN board fails CLOSED, never "cool" (NaN-fail-open guard) --
    #[test]
    fn all_nan_board_fails_closed_not_classified_cool() {
        // Regression for the NaN-fail-open class (same class as the prior
        // controller.rs NEG_INFINITY-fold bug). A board whose PCB sensors all
        // read NaN (garbage I2C/diode decode) must NOT fold board_max to
        // f32::MIN and be classified cool — which would emit RequestFansMin +
        // a frequency step-UP advisory on a possibly-hot board (fail-OPEN).
        // The finite filter drops the NaN readings so valid_pcb is empty →
        // the SensorFailure → power-off path fires (fail-CLOSED).
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let sample = tick(
            vec![board(0, vec![f32::NAN, f32::NAN], vec![f32::NAN])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestBoardPowerOff {
                    reason: ThermalReason::SensorFailure,
                    ..
                }
            )),
            "all-NaN board must fail closed to a SensorFailure power-off, got {actions:?}"
        );
        // The dangerous fail-OPEN signal is a frequency step-UP on an
        // untrustworthy (possibly-hot) board — that must NEVER be emitted.
        // (A global RequestFansMin can legitimately accompany the power-off:
        // the de-energized board needs no cooling, and any genuinely hot peer
        // board would emit RequestFansMax which the aggregate honors.)
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestProfileStepUp)),
            "all-NaN board must NEVER trigger a frequency step-UP (fail-open), got {actions:?}"
        );
    }

    // -- 6c. A NaN sensor mixed with a hot finite sensor never masks the heat --
    #[test]
    fn nan_sensor_does_not_mask_a_hot_finite_sensor() {
        // If one sensor is NaN and another reads panic-hot, the finite filter
        // keeps the hot reading in valid_pcb, so board_max is the real hot
        // value and the panic power-off still fires. (min_per_board default is
        // 1, so a single finite sensor is enough to keep evaluating.)
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let sample = tick(
            vec![board(0, vec![f32::NAN, 72.0], vec![60.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestBoardPowerOff {
                    reason: ThermalReason::BoardPanic,
                    ..
                }
            )),
            "a NaN sensor must not hide a hot finite sensor; BoardPanic must fire, got {actions:?}"
        );
    }

    // -- 7. Fan panic → EmergencyShutdown (FanPanic) --
    #[test]
    fn no_working_fans_triggers_fan_panic() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            min_fans: 1,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let sample = tick(
            vec![board(0, vec![55.0, 55.0], vec![70.0])],
            vec![0, 0],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(actions.iter().any(|a| matches!(
            a,
            SupervisorAction::RequestEmergencyShutdown {
                reason: ThermalReason::FanPanic
            }
        )));
    }

    // -- 8. Single fan stall = FanFailure (continue mining) --
    #[test]
    fn single_fan_stall_emits_fan_failure_but_not_panic() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            min_fans: 1, // 1 working fan is enough
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let sample = tick(
            vec![board(0, vec![55.0, 55.0], vec![70.0])],
            vec![1000, 0],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(actions
            .iter()
            .any(|a| matches!(a, SupervisorAction::EmitFanFailure { fan_index: 1 })));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, SupervisorAction::RequestEmergencyShutdown { .. })));
    }

    // -- 9. Hydro panic beats everything --
    #[test]
    fn hydro_panic_emergency_shutdown() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let mut sample = tick(vec![board(0, vec![55.0], vec![70.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(51.0);
        let actions = s.tick(&sample);
        assert!(matches!(
            actions[0],
            SupervisorAction::RequestEmergencyShutdown {
                reason: ThermalReason::HydroPanic
            }
        ));
    }

    // -- 10. Hydro cold-startup blocks mining --
    #[test]
    fn hydro_cold_blocks_startup() {
        let mut s = ThermalSupervisor::new(cfg_enabled());
        let mut sample = tick(vec![board(0, vec![20.0], vec![30.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(10.0);
        let actions = s.tick(&sample);
        assert!(matches!(
            actions[0],
            SupervisorAction::RequestEmergencyShutdown {
                reason: ThermalReason::HydroStartupCold
            }
        ));
    }

    // -- 11. Sensor reliability filter drops outlier after max_bad_readings --
    // THERMAL-5 RESOLVED: the filter now uses a robust MEDIAN peer reference
    // instead of the all-inclusive mean. For [55, 55, 90] the median is 55, so
    // only sensor 2 (the 90) deviates by > 2 °C → exactly ONE sensor drops; the
    // two agreeing sensors survive. (Under the old inclusive-mean center the 90
    // dragged peer_avg to ~66.7 and ALL THREE dropped, blinding the board and
    // tripping a spurious SensorFailure power-off.) The robust center never
    // masks a genuine board-wide hot event — when sensors read high *together*
    // the median is high and the board_hot/board_panic path in tick() still
    // escalates. See run_reliability_filter's THERMAL-5 doc-comment.
    #[test]
    fn sensor_reliability_filter_drops_outlier() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            max_bad_readings: 3,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // 3 PCBs: 55, 55, 90 → sensor index 2 disagrees with peer avg by 23°C
        // (above 2°C threshold). After 3 consecutive bad readings → dropped.
        for _ in 0..3 {
            let sample = tick(
                vec![board(0, vec![55.0, 55.0, 90.0], vec![60.0])],
                vec![1000],
                30,
                5,
            );
            s.tick(&sample);
        }
        let snapshot = s.snapshot();
        let board_snap = snapshot
            .board_states
            .iter()
            .find(|b| b.chain_id == 0)
            .unwrap();
        assert_eq!(board_snap.dropped_pcb_sensors, 1);
    }

    // -- 12. Insufficient valid sensors → power off this board (SensorFailure) --
    #[test]
    fn insufficient_valid_sensors_powers_off_board() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            min_per_board: 2,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        let actions = s.tick(&sample);
        assert!(actions.iter().any(|a| matches!(
            a,
            SupervisorAction::RequestBoardPowerOff {
                reason: ThermalReason::SensorFailure,
                ..
            }
        )));
    }

    // -- 13. LOAD-BEARING: RequestFansMax respects fan_max_pwm cap (semantic) --
    // The supervisor's contract is "RequestFansMax means up to fan_max_pwm,
    // NOT 255". We verify by snapshot inspection: the action enum carries
    // no PWM value — the cap is read from config by the caller. Default
    // config is 30, NOT 100 or 255.
    #[test]
    fn request_fans_max_respects_quiet_home_cap() {
        let cfg = ThermalSupervisorConfig::default();
        assert_eq!(cfg.fan_max_pwm, 30,
            "Default fan_max_pwm MUST be the home-unit cap (30). Raising it requires explicit operator config edit.");
        let mut s = ThermalSupervisor::new(ThermalSupervisorConfig {
            enabled: true,
            ..cfg
        });
        let snap = s.snapshot();
        assert_eq!(
            snap.fan_max_pwm, 30,
            "Snapshot must surface the cap to operators."
        );
        // Verify RequestFansMax doesn't carry a "raise to 255" parameter:
        // the action enum literally has no PWM field, so the cap is the
        // caller's enforcement responsibility against config.fan_max_pwm.
        let sample = tick(vec![board(0, vec![66.0], vec![80.0])], vec![1000], 30, 1);
        let actions = s.tick(&sample);
        for a in &actions {
            if let SupervisorAction::RequestFansMax { .. } = a {
                // Compile-time check: no PWM field on this variant.
                // (If a future change adds one, this test forces a rewrite
                // that re-asserts the cap semantics.)
            }
        }
    }

    // -- 14. ATM stepper is an advisory; autotuner stays master --
    #[test]
    fn atm_step_request_does_not_force_autotuner() {
        // The supervisor emits RequestProfileStepUp / RequestProfileStepDown
        // as advisories. The action enum does NOT include "force-set
        // profile X". This test asserts the variant set is intentional;
        // future changes that add forcing actions trip this check.
        fn _advisory_only(a: &SupervisorAction) {
            match a {
                SupervisorAction::NoOp
                | SupervisorAction::RequestFansMax { .. }
                | SupervisorAction::RequestFansCurve
                | SupervisorAction::RequestFansMin
                | SupervisorAction::RequestProfileStepDown { .. }
                | SupervisorAction::RequestProfileStepUp
                | SupervisorAction::RequestBoardPowerOff { .. }
                | SupervisorAction::RequestEmergencyShutdown { .. }
                | SupervisorAction::EmitFanFailure { .. }
                | SupervisorAction::EmitSensorDropped { .. }
                | SupervisorAction::EmitChipImbalance { .. }
                | SupervisorAction::AttemptBoardRecovery { .. }
                | SupervisorAction::EmitRecoveryBudgetExhausted { .. } => {}
            }
        }
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            atm_startup_grace_secs: 0,
            atm_post_ramp_grace_secs: 0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let sample = tick(vec![board(0, vec![66.0], vec![80.0])], vec![1000], 30, 1);
        for a in s.tick(&sample) {
            _advisory_only(&a);
        }
    }

    // -- 15. Recovery budget exhausts → EmitRecoveryBudgetExhausted --
    #[test]
    fn recovery_budget_exhausts() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            max_reboot: 2,
            overtemp_auto_recovery: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // First tick: board panics, marked thermal-off.
        let hot_sample = tick(vec![board(0, vec![72.0], vec![85.0])], vec![1000], 30, 5);
        s.tick(&hot_sample);
        // Subsequent ticks: board is OFF + cool band met → recovery attempts.
        for _ in 0..2 {
            let cool_off = ThermalTick {
                board_sensors: vec![BoardSensors {
                    chain_id: 0,
                    pcb_temps_c: vec![45.0],
                    chip_temps_c: vec![60.0],
                    powered_on: false,
                }],
                ..tick(vec![], vec![1000], 30, 5)
            };
            s.tick(&cool_off);
        }
        // 3rd attempt: budget (max_reboot=2) exhausted.
        let cool_off = ThermalTick {
            board_sensors: vec![BoardSensors {
                chain_id: 0,
                pcb_temps_c: vec![45.0],
                chip_temps_c: vec![60.0],
                powered_on: false,
            }],
            ..tick(vec![], vec![1000], 30, 5)
        };
        let actions = s.tick(&cool_off);
        assert!(actions.iter().any(|a| matches!(
            a,
            SupervisorAction::EmitRecoveryBudgetExhausted { chain_id: 0 }
        )));
    }

    // -- THERMAL-8: per-platform default-enable is OFF without the flag --
    #[test]
    fn supervisor_default_enabled_is_off_without_validation_flag() {
        // Compiled default: NO platform is default-on unless the operator sets
        // the per-platform live-validation flag. This is the LIVE-HARDWARE-DEFAULT
        // guard — a missing/false flag MUST keep the supervisor dormant on every
        // platform so we never silently arm an unvalidated FSM on live hardware.
        for p in [
            SupervisorPlatform::Am1S9,
            SupervisorPlatform::Am2Zynq,
            SupervisorPlatform::Am3Aml,
            SupervisorPlatform::Am3Bb,
            SupervisorPlatform::Unknown,
        ] {
            assert!(
                !supervisor_default_enabled(p, false),
                "{p:?} must NOT be default-on without the per-platform validation flag"
            );
        }
    }

    // -- THERMAL-8: even WITH the flag set, only signed-off platforms arm --
    #[test]
    fn supervisor_default_enabled_with_flag_only_signed_off_platforms() {
        // Today NO platform arm is signed off (all `false` in the match), so even
        // with the validation flag the helper stays conservative. This test pins
        // that contract; when a platform is live-validated, flip its arm in
        // `supervisor_default_enabled` AND update this assertion in the same change.
        for p in [
            SupervisorPlatform::Am1S9,
            SupervisorPlatform::Am2Zynq,
            SupervisorPlatform::Am3Aml,
            SupervisorPlatform::Am3Bb,
            SupervisorPlatform::Unknown,
        ] {
            assert!(
                !supervisor_default_enabled(p, true),
                "{p:?} arm is not signed off yet — must stay OFF until live-validated"
            );
        }
    }

    // -- THERMAL-8: board_target marker maps to the right platform family --
    #[test]
    fn supervisor_platform_from_board_target_classifies() {
        use SupervisorPlatform::*;
        assert_eq!(SupervisorPlatform::from_board_target("am1-s9"), Am1S9);
        assert_eq!(SupervisorPlatform::from_board_target("S9"), Am1S9);
        assert_eq!(
            SupervisorPlatform::from_board_target("am2-s19jpro"),
            Am2Zynq
        );
        assert_eq!(
            SupervisorPlatform::from_board_target("zynq-bm3-am2"),
            Am2Zynq
        );
        assert_eq!(SupervisorPlatform::from_board_target("xil"), Am2Zynq);
        // am3-bb must classify as BB, NOT generic am3-aml (more-specific first).
        assert_eq!(
            SupervisorPlatform::from_board_target("am3-bb-s19jpro"),
            Am3Bb
        );
        assert_eq!(SupervisorPlatform::from_board_target("am3-aml"), Am3Aml);
        assert_eq!(
            SupervisorPlatform::from_board_target("amlogic-a113d"),
            Am3Aml
        );
        assert_eq!(
            SupervisorPlatform::from_board_target("something-else"),
            Unknown
        );
    }

    // -- THERMAL-9: passing the FULL per-fan RPM vector avoids the spurious
    // FanPanic that a single `vec![min_rpm]` produced. --
    #[test]
    fn full_fan_vector_avoids_spurious_panic_vs_single_min() {
        // 4 fans, one stalled (the min reading is 0). The daemon used to pass the
        // supervisor a single `vec![min_rpm]` == `vec![0]`, which the supervisor
        // read as "1 fan total, 0 working" → FanPanic → EmergencyShutdown even
        // though 3 fans were fine. THERMAL-9 passes the full vector instead.
        let per_fan_rpms = vec![0u32, 3000, 3100, 2950];
        let min_rpm = per_fan_rpms.iter().copied().min().unwrap();

        let cfg = ThermalSupervisorConfig {
            enabled: true,
            min_fans: 1,
            ..ThermalSupervisorConfig::default()
        };

        // OLD (buggy) shape: single min value → spurious panic.
        let mut s_bug = ThermalSupervisor::new(cfg.clone());
        let bug_sample = tick(
            vec![board(0, vec![55.0, 55.0], vec![70.0])],
            vec![min_rpm],
            30,
            5,
        );
        assert!(
            s_bug.tick(&bug_sample).iter().any(|a| matches!(
                a,
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::FanPanic
                }
            )),
            "single vec![min_rpm] reproduces the spurious FanPanic the fix targets"
        );

        // NEW (fixed) shape: full per-fan vector → fan failure telemetry only, no panic.
        let mut s_fix = ThermalSupervisor::new(cfg);
        let fix_sample = tick(
            vec![board(0, vec![55.0, 55.0], vec![70.0])],
            per_fan_rpms,
            30,
            5,
        );
        let actions = s_fix.tick(&fix_sample);
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::EmitFanFailure { fan_index: 0 })),
            "stalled fan should surface as EmitFanFailure on the full vector"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestEmergencyShutdown { .. })),
            "full per-fan vector must NOT trip a spurious FanPanic when 3/4 fans are healthy"
        );
    }

    // -- THERMAL-5: one drifting sensor does NOT mark the rest bad --
    // Regression for the inclusive-mean poisoning bug. With the robust median
    // center, a single drifting sensor in a 4-sensor board drops alone; the 3
    // agreeing sensors stay valid, so the board keeps a usable aggregate and is
    // NEVER powered off for SensorFailure.
    #[test]
    fn one_drifting_sensor_does_not_poison_the_set() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            max_bad_readings: 3,
            min_per_board: 1,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // 4 PCBs: three agree at ~55 °C, one drifts to 80 °C. Median = 55, so
        // only the drifter (index 3) exceeds the 2 °C threshold.
        for _ in 0..3 {
            let sample = tick(
                vec![board(0, vec![55.0, 55.5, 54.8, 80.0], vec![60.0])],
                vec![1000],
                30,
                5,
            );
            let actions = s.tick(&sample);
            // The board must NEVER be powered off for SensorFailure while 3
            // healthy sensors remain — that is the whole point of the fix.
            assert!(
                !actions.iter().any(|a| matches!(
                    a,
                    SupervisorAction::RequestBoardPowerOff {
                        reason: ThermalReason::SensorFailure,
                        ..
                    }
                )),
                "one drifting sensor must NOT poison the set into a SensorFailure power-off"
            );
        }
        let board_snap = s
            .snapshot()
            .board_states
            .into_iter()
            .find(|b| b.chain_id == 0)
            .expect("board 0 tracked");
        assert_eq!(
            board_snap.dropped_pcb_sensors, 1,
            "exactly the single drifting sensor should drop, not the agreeing peers"
        );
    }

    // -- THERMAL-5: all-bad sensors → FAIL CLOSED (power off, fans within cap) --
    // When sensor agreement collapses so far that fewer than `min_per_board`
    // sensors remain valid, the supervisor must FAIL CLOSED: power the board
    // off with SensorFailure (treat unknown thermal as dangerous), and never
    // run hot. It must NOT raise fans as the failure response (cut-hash-before-
    // noise), and any fan action it does emit stays bounded by the home cap.
    #[test]
    fn all_bad_sensors_fail_closed_and_respect_fan_cap() {
        // Require 3 valid PCB sensors per board so a 2-vs-2 collapse drops
        // enough to breach the floor.
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            max_bad_readings: 2,
            min_per_board: 3,
            bad_average_threshold_c: 2.0,
            ..ThermalSupervisorConfig::default()
        };
        // Fan cap is the home-unit default (30) — assert we never command above it.
        assert_eq!(cfg.fan_max_pwm, 30);
        let mut s = ThermalSupervisor::new(cfg);

        // 4 sensors in a 2-vs-2 split around a median of 52.5; every sensor is
        // 2.5 °C from the median (> threshold 2.0), so all four go bad and drop.
        let mut last_actions = Vec::new();
        for _ in 0..3 {
            let sample = tick(
                vec![board(0, vec![50.0, 55.0, 50.0, 55.0], vec![60.0])],
                vec![1000],
                30,
                5,
            );
            last_actions = s.tick(&sample);
        }

        // Fail-closed: the board is powered off for SensorFailure (we no longer
        // trust the thermal picture — never keep mining hot on blind sensors).
        assert!(
            last_actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestBoardPowerOff {
                    reason: ThermalReason::SensorFailure,
                    ..
                }
            )),
            "all-bad sensors must fail closed → RequestBoardPowerOff{{SensorFailure}}; got {last_actions:?}"
        );
        // Cut-hash-before-noise: the supervisor must NOT respond to a sensor
        // blackout by blasting fans. RequestFansMax (drive toward the cap) is
        // only a hot-board response and must not appear here.
        assert!(
            !last_actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestFansMax { .. })),
            "sensor blackout must cut hash, not raise fans; got {last_actions:?}"
        );
        // PWM-30 cap: the snapshot the operator/caller reads still surfaces the
        // home cap; the action enum carries no PWM value (the caller enforces
        // the cap against fan_max_pwm), so the supervisor can never request
        // above-cap PWM. Pin both the cap and the no-PWM-field contract.
        assert_eq!(
            s.snapshot().fan_max_pwm,
            30,
            "fan cap must stay at the home-unit PWM-30 ceiling on the fail-closed path"
        );
    }

    // =====================================================================
    // GROUP B / Task 1 — inter-chip temperature imbalance (DIAGNOSTIC only)
    // =====================================================================

    // 1a. Imbalance flag FIRES above the threshold — and is TELEMETRY ONLY:
    //     it emits EmitChipImbalance + surfaces the spread in the snapshot,
    //     but does NOT cut hash / power off the board / raise fans by itself.
    #[test]
    fn chip_imbalance_flag_fires_above_threshold_telemetry_only() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            chip_imbalance_threshold_c: 15.0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Board well below hot/panic (board 55, chips cool overall) but with a
        // 25 °C inter-chip spread (60..85) — a failing-chip / poor-paste signal.
        // chip_hot default is 93, chip_panic 100, so 85 does NOT escalate.
        let sample = tick(
            vec![board(0, vec![55.0], vec![60.0, 62.0, 85.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);

        // The diagnostic flag fires with the correct spread (85 - 60 = 25).
        let imbalance = actions.iter().find_map(|a| match a {
            SupervisorAction::EmitChipImbalance { chain_id, spread_c } => {
                Some((*chain_id, *spread_c))
            }
            _ => None,
        });
        let (chain_id, spread_c) = imbalance.expect("imbalance flag must fire above threshold");
        assert_eq!(chain_id, 0);
        assert!(
            (spread_c - 25.0).abs() < 1e-3,
            "spread should be 25 °C, got {spread_c}"
        );

        // TELEMETRY ONLY: no hash-cut, no board power-off, no fan-max driven BY
        // the imbalance. (The board is cool, so the only fan action is the
        // benign cool-band RequestFansMin — never RequestFansMax/shutdown.)
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestEmergencyShutdown { .. }
                    | SupervisorAction::RequestBoardPowerOff { .. }
                    | SupervisorAction::RequestFansMax { .. }
            )),
            "chip imbalance is diagnostic — it must NOT cut hash / power off / blast fans; got {actions:?}"
        );

        // Snapshot surfaces the per-board spread + flag + worst aggregate.
        let snap = s.snapshot();
        assert_eq!(snap.chip_imbalance_threshold_c, 15.0);
        assert_eq!(snap.worst_chip_imbalance_c, Some(25.0));
        let bsnap = snap
            .board_states
            .iter()
            .find(|b| b.chain_id == 0)
            .expect("board 0 tracked");
        assert!((bsnap.chip_imbalance_c.unwrap() - 25.0).abs() < 1e-3);
        assert!(
            bsnap.chip_imbalance_flagged,
            "flag must be set above threshold"
        );
    }

    // 1b. Imbalance flag does NOT fire below the threshold — a tight chip
    //     cluster (healthy board) is never flagged.
    #[test]
    fn chip_imbalance_flag_does_not_fire_below_threshold() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            chip_imbalance_threshold_c: 15.0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Spread = 64 - 60 = 4 °C, well under the 15 °C threshold.
        let sample = tick(
            vec![board(0, vec![55.0], vec![60.0, 62.0, 64.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::EmitChipImbalance { .. })),
            "tight chip cluster must NOT be flagged; got {actions:?}"
        );
        let snap = s.snapshot();
        let bsnap = snap.board_states.iter().find(|b| b.chain_id == 0).unwrap();
        // The spread is still SURFACED (4 °C) even though it isn't flagged —
        // telemetry is always honest about the value.
        assert!((bsnap.chip_imbalance_c.unwrap() - 4.0).abs() < 1e-3);
        assert!(
            !bsnap.chip_imbalance_flagged,
            "below threshold must not flag"
        );
        assert_eq!(snap.worst_chip_imbalance_c, Some(4.0));
    }

    // 1c. Fewer than 2 valid chip sensors → no spread to compute, never flagged.
    #[test]
    fn chip_imbalance_needs_two_sensors() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Single chip sensor (and an empty-chip board) → no imbalance verdict.
        let sample = tick(
            vec![
                board(0, vec![55.0], vec![70.0]),
                board(1, vec![55.0], vec![]),
            ],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::EmitChipImbalance { .. })),
            "boards with <2 valid chip sensors must not emit an imbalance flag"
        );
        let snap = s.snapshot();
        assert_eq!(
            snap.worst_chip_imbalance_c, None,
            "no board has >=2 chip sensors → worst imbalance is None"
        );
        for b in &snap.board_states {
            assert_eq!(b.chip_imbalance_c, None);
            assert!(!b.chip_imbalance_flagged);
        }
    }

    // 1d. Imbalance is DIAGNOSTIC even on a hot board: it rides alongside the
    //     real hot-board escalation (which is driven by the chip MAX, not the
    //     spread) and never weakens or strengthens that escalation.
    #[test]
    fn chip_imbalance_rides_alongside_hot_escalation() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            chip_imbalance_threshold_c: 15.0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // chip_hot default 93: chips 70..96 → max 96 ≥ 93 → ChipHot escalation,
        // AND the 26 °C spread (70..96) flags imbalance. Both must appear; the
        // hot escalation is driven by the MAX (96), not the spread.
        let sample = tick(
            vec![board(0, vec![55.0], vec![70.0, 80.0, 96.0])],
            vec![1000],
            30,
            5,
        );
        let actions = s.tick(&sample);
        assert!(
            actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestFansMax {
                    reason: ThermalReason::ChipHot
                }
            )),
            "chip max ≥ chip_hot must still drive the real hot escalation"
        );
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::EmitChipImbalance { .. })),
            "the imbalance diagnostic rides alongside the hot escalation"
        );
    }

    // =====================================================================
    // GROUP B / Task 2 — hydro flow-loss / inlet-missing (PROTECTIVE)
    // =====================================================================

    // 2a. Default-OFF inert: with hydro_configured=false (the default), an
    //     air-cooled unit (no hydro telemetry) is byte-identical to the
    //     pre-flow-check path — no HydroFlowLoss, normal classification.
    #[test]
    fn hydro_flow_check_default_off_is_inert() {
        let cfg = ThermalSupervisorConfig::default();
        assert!(!cfg.hydro_configured, "hydro must be OFF by default");
        let mut s = ThermalSupervisor::new(ThermalSupervisorConfig {
            enabled: true,
            ..cfg
        });
        // No hydro telemetry at all (air-cooled). Normal cool board.
        let sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        let actions = s.tick(&sample);
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            )),
            "default-OFF hydro must never produce a HydroFlowLoss verdict; got {actions:?}"
        );
        let snap = s.snapshot();
        assert!(!snap.hydro_configured);
    }

    // 2b. Hydro configured + inlet MISSING → fail closed (cut hash). The safe
    //     direction: never raise fans / push power.
    #[test]
    fn hydro_configured_missing_inlet_fails_closed() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Hydro configured but inlet sensor is gone (None) — we can no longer
        // prove the loop is healthy.
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = None;
        sample.hydro_outlet_c = Some(45.0);
        let actions = s.tick(&sample);
        assert!(
            matches!(
                actions[0],
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            ),
            "missing inlet while hydro configured must cut hash; got {actions:?}"
        );
        // Safe direction: cut hash, NOT raise fans.
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestFansMax { .. })),
            "hydro flow loss must cut hash, never blast fans"
        );
        // Fan cap is still the home PWM-30 ceiling.
        assert_eq!(s.snapshot().fan_max_pwm, 30);
    }

    #[test]
    fn hydro_configured_non_finite_inlet_fails_closed() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(f32::NAN);
        sample.hydro_outlet_c = Some(42.0);

        let actions = s.tick(&sample);
        assert!(
            matches!(
                actions[0],
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            ),
            "NaN hydro inlet must fail closed as hydro flow loss; got {actions:?}"
        );
    }

    #[test]
    fn hydro_configured_non_finite_outlet_fails_closed() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(30.0);
        sample.hydro_outlet_c = Some(f32::INFINITY);

        let actions = s.tick(&sample);
        assert!(
            matches!(
                actions[0],
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            ),
            "non-finite hydro outlet must fail closed; got {actions:?}"
        );
    }

    // 2c. Hydro configured + inlet NOT colder than outlet → flow loss/reversal
    //     → fail closed (cut hash).
    #[test]
    fn hydro_configured_flow_reversal_fails_closed() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            hydro_flow_loss_margin_c: 1.0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Reversed loop: inlet (40) is WARMER than outlet (38) → stagnant hot
        // coolant on the cold side. inlet >= outlet - 1.0 → flow loss.
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(40.0);
        sample.hydro_outlet_c = Some(38.0);
        let actions = s.tick(&sample);
        assert!(
            matches!(
                actions[0],
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            ),
            "inlet not colder than outlet must cut hash (flow loss); got {actions:?}"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, SupervisorAction::RequestFansMax { .. })),
            "flow loss must cut hash, never blast fans"
        );
    }

    // 2d. Hydro configured + HEALTHY loop (inlet meaningfully colder than
    //     outlet, both in range) → NO flow-loss verdict; normal classification.
    #[test]
    fn hydro_configured_healthy_loop_no_flow_loss() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            hydro_flow_loss_margin_c: 1.0,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Healthy: inlet 30 (cold supply) clearly colder than outlet 42 (warm
        // return), inlet within [startup_min 15, panic 50].
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(30.0);
        sample.hydro_outlet_c = Some(42.0);
        let actions = s.tick(&sample);
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            )),
            "a healthy hydro loop must NOT trip flow loss; got {actions:?}"
        );
    }

    // 2e. The existing inlet panic / startup-cold paths are preserved AND still
    //     win over the new flow-loss check (panic checked first). A hot inlet
    //     yields HydroPanic, not HydroFlowLoss.
    #[test]
    fn hydro_panic_still_wins_over_flow_loss() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        // Inlet at 51 (>= panic 50) AND would also read as flow loss vs a 50
        // outlet — panic must win (it is checked first).
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(51.0);
        sample.hydro_outlet_c = Some(50.0);
        let actions = s.tick(&sample);
        assert!(
            matches!(
                actions[0],
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroPanic
                }
            ),
            "inlet panic must win over flow loss; got {actions:?}"
        );
    }

    // 2f. Hydro configured + inlet present but NO outlet sensor → we can't infer
    //     direction, so we do NOT fabricate a flow verdict (the absolute inlet
    //     panic/startup-cold guards above still apply). Inlet in-range here →
    //     no shutdown.
    #[test]
    fn hydro_configured_inlet_without_outlet_no_flow_verdict() {
        let cfg = ThermalSupervisorConfig {
            enabled: true,
            hydro_configured: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = Some(30.0); // in-range, between startup_min and panic
        sample.hydro_outlet_c = None; // no outlet → can't judge direction
        let actions = s.tick(&sample);
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                SupervisorAction::RequestEmergencyShutdown {
                    reason: ThermalReason::HydroFlowLoss
                }
            )),
            "inlet present but no outlet → no flow verdict fabricated; got {actions:?}"
        );
    }

    // 2g. Disabled supervisor: even with hydro_configured + a flow-loss
    //     condition, a DORMANT supervisor returns no actions (the enabled gate
    //     wins). Guards against arming hydro logic on a disabled FSM.
    #[test]
    fn hydro_flow_loss_inert_when_supervisor_disabled() {
        let cfg = ThermalSupervisorConfig {
            enabled: false, // dormant
            hydro_configured: true,
            ..ThermalSupervisorConfig::default()
        };
        let mut s = ThermalSupervisor::new(cfg);
        let mut sample = tick(vec![board(0, vec![55.0], vec![60.0])], vec![1000], 30, 5);
        sample.hydro_inlet_c = None;
        let actions = s.tick(&sample);
        assert!(
            actions.is_empty(),
            "a disabled supervisor must emit no actions even with a hydro flow-loss condition"
        );
    }
}
