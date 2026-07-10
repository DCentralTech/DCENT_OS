//!  pwr-A — Cold-boot power state machine (HAL-free).
//!
//! Source RE evidence:
//!
//! §2-3 (lines 15-158) and the cross-cutting rules in §error-transitions
//! (lines 161-188).
//!
//! Maps the canonical bosminer cold-boot sequence into a HAL-free state
//! machine. The runtime adapter inside `dcentrald-asic::dspic` /
//! `dcentrald-hal::psu` drives this state machine by feeding observations
//! (I²C ACK/NACK, GPIO read-back, heartbeat tick count, FW byte) and
//! reading back the next target state.
//!
//! The state machine deliberately mirrors the  `gdtuner` and
//!  `atm_stepper` shape — `feed(observation) -> PowerState` plus
//! explicit `request_advance(target)` for adapter-driven transitions
//! (e.g. once the runtime confirms a 7 s warm-boot wait has elapsed, it
//! calls `request_advance(PicJump)` rather than the state machine
//! reading the clock itself).
//!
//! **Hard non-regression rules pinned by tests** (per `feedback_*` rules):
//! - 5-tick PIC heartbeat stability gate before SET_VOLTAGE
//!.
//! - PIC RESET banned on dsPIC fw=0x89.
//! - dsPIC fw=0x86 = corruption state — voltage commands refused
//!.
//! - PSU disarm requires 3-write triple sequence with 1 s gaps
//!   (per RE doc Phase B3 lines 137-139).

use serde::{Deserialize, Serialize};

/// Canonical cold-boot states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    /// AC just applied; kernel up.
    PowerOn,
    // Phase A — GPIO assertion.
    GpioPwrAssert,
    GpioHbReset,
    // Phase B — PSU bring-up.
    PsuOpenBus,
    PsuGetVersion,
    /// Triple-write disarm; n in 1..=3.
    PsuDisarm {
        attempt: u8,
    },
    PsuSetInitV,
    PsuArmWd,
    PsuHbtLoopStart,
    PsuSettle,
    // Phase C — dsPIC bring-up.
    PicFlushParser,
    PicGetVersion,
    /// Variant decision based on observed FW byte.
    PicVariantDecide,
    PicReset,
    PicResetWait,
    PicJump,
    PicJumpWait,
    PicGetVersion2,
    /// Phase D — 5-tick heartbeat stability gate. n in 1..=5.
    PicHbtGateTick {
        count: u8,
    },
    PicSetVoltage,
    PicEnable,
    PicDcDcRamp,
    PicHbtLoopStart,
    /// Cold boot complete; cgminer-core takes over.
    Ready,
    /// Hard fault — operator must intervene. Cleared only by `reset()`.
    Fault,
}

/// Observation fed by the runtime adapter on each tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerObservation {
    /// I²C bus is reachable + last command got an ACK.
    pub i2c_ack: bool,
    /// PSU/PIC last reply parsed cleanly (preamble + CKSUM ok).
    pub frame_ok: bool,
    /// PIC firmware byte from latest GET_VERSION reply.
    /// `None` if no reply has been observed yet.
    pub pic_fw: Option<u8>,
    /// Whether the runtime adapter has finished the timed-wait portion
    /// of the current state (caller's responsibility — pwr-A is clock-free).
    pub timed_wait_done: bool,
}

impl PowerObservation {
    /// Convenience: a fresh, all-default observation.
    pub fn empty() -> Self {
        Self {
            i2c_ack: false,
            frame_ok: false,
            pic_fw: None,
            timed_wait_done: false,
        }
    }
}

/// Configuration for the state machine. Defaults match bosminer canon.
#[derive(Debug, Clone, Copy)]
pub struct PowerStateConfig {
    /// Mandatory minimum stable PIC heartbeat ticks before SET_VOLTAGE.
    /// HARD-MANDATED at 5.
    pub stable_tick_gate: u8,
    /// Number of consecutive PSU disarm writes (per RE doc Phase B3).
    pub psu_disarm_triple_write_count: u8,
    /// Whether to fail-safe (return Fault) when PIC fw=0x86 is observed
    ///. Override only with
    /// `DCENT_AM2_TRUST_DEGRADED_FW=1` lab flag.
    pub refuse_fw_86: bool,
}

impl Default for PowerStateConfig {
    fn default() -> Self {
        Self {
            stable_tick_gate: 5,
            psu_disarm_triple_write_count: 3,
            refuse_fw_86: true,
        }
    }
}

/// State machine. One per chain.
#[derive(Debug, Clone)]
pub struct PowerStateMachine {
    state: PowerState,
    config: PowerStateConfig,
    observed_fw: Option<u8>,
    last_observation: PowerObservation,
}

impl PowerStateMachine {
    pub fn new(config: PowerStateConfig) -> Self {
        Self {
            state: PowerState::PowerOn,
            config,
            observed_fw: None,
            last_observation: PowerObservation::empty(),
        }
    }

    pub fn fresh() -> Self {
        Self::new(PowerStateConfig::default())
    }

    pub fn state(&self) -> PowerState {
        self.state
    }

    pub fn observed_fw(&self) -> Option<u8> {
        self.observed_fw
    }

    pub fn config(&self) -> &PowerStateConfig {
        &self.config
    }

    /// Reset back to PowerOn. Drops any observed FW byte.
    pub fn reset(&mut self) {
        self.state = PowerState::PowerOn;
        self.observed_fw = None;
        self.last_observation = PowerObservation::empty();
    }

    /// Mark the chain Faulted. Operator intervention required to clear.
    pub fn mark_fault(&mut self) {
        self.state = PowerState::Fault;
    }

    /// Feed one observation. Returns the new state.
    pub fn feed(&mut self, obs: PowerObservation) -> PowerState {
        self.last_observation = obs;
        if let Some(fw) = obs.pic_fw {
            self.observed_fw = Some(fw);
        }

        // Hard-fault on fw=0x86 if refuse_fw_86 is set, anywhere after
        // we've observed it.
        if self.config.refuse_fw_86 {
            if let Some(0x86) = self.observed_fw {
                if matches!(
                    self.state,
                    PowerState::PicVariantDecide
                        | PowerState::PicSetVoltage
                        | PowerState::PicEnable
                ) {
                    self.state = PowerState::Fault;
                    return self.state;
                }
            }
        }

        match self.state {
            PowerState::PowerOn => {
                self.state = PowerState::GpioPwrAssert;
            }
            PowerState::GpioPwrAssert => {
                if obs.i2c_ack && obs.timed_wait_done {
                    self.state = PowerState::GpioHbReset;
                }
            }
            PowerState::GpioHbReset => {
                if obs.timed_wait_done {
                    self.state = PowerState::PsuOpenBus;
                }
            }
            PowerState::PsuOpenBus => {
                if obs.i2c_ack {
                    self.state = PowerState::PsuGetVersion;
                }
            }
            PowerState::PsuGetVersion => {
                if obs.frame_ok {
                    self.state = PowerState::PsuDisarm { attempt: 1 };
                }
            }
            PowerState::PsuDisarm { attempt } => {
                if obs.frame_ok && obs.timed_wait_done {
                    if attempt >= self.config.psu_disarm_triple_write_count {
                        self.state = PowerState::PsuSetInitV;
                    } else {
                        self.state = PowerState::PsuDisarm {
                            attempt: attempt + 1,
                        };
                    }
                }
            }
            PowerState::PsuSetInitV => {
                if obs.frame_ok {
                    self.state = PowerState::PsuArmWd;
                }
            }
            PowerState::PsuArmWd => {
                if obs.frame_ok {
                    self.state = PowerState::PsuHbtLoopStart;
                }
            }
            PowerState::PsuHbtLoopStart => {
                self.state = PowerState::PsuSettle;
            }
            PowerState::PsuSettle => {
                if obs.timed_wait_done {
                    self.state = PowerState::PicFlushParser;
                }
            }
            PowerState::PicFlushParser => {
                self.state = PowerState::PicGetVersion;
            }
            PowerState::PicGetVersion => {
                if obs.frame_ok && obs.pic_fw.is_some() {
                    self.state = PowerState::PicVariantDecide;
                }
            }
            PowerState::PicVariantDecide => {
                // Variant routing per RE doc + memory rules.
                match self.observed_fw {
                    // RESET BANNED on fw=0x89 — skip directly to gate ticks.
                    Some(0x89) => {
                        self.state = PowerState::PicHbtGateTick { count: 1 };
                    }
                    // RESET-allowed firmwares fall through to the reset path.
                    Some(_) => {
                        self.state = PowerState::PicReset;
                    }
                    // No fw observed — should be impossible, fault.
                    None => {
                        self.state = PowerState::Fault;
                    }
                }
            }
            PowerState::PicReset => {
                if obs.timed_wait_done {
                    self.state = PowerState::PicResetWait;
                }
            }
            PowerState::PicResetWait => {
                if obs.timed_wait_done {
                    self.state = PowerState::PicJump;
                }
            }
            PowerState::PicJump => {
                if obs.timed_wait_done {
                    self.state = PowerState::PicJumpWait;
                }
            }
            PowerState::PicJumpWait => {
                if obs.timed_wait_done {
                    self.state = PowerState::PicGetVersion2;
                }
            }
            PowerState::PicGetVersion2 => {
                if obs.frame_ok {
                    self.state = PowerState::PicHbtGateTick { count: 1 };
                }
            }
            PowerState::PicHbtGateTick { count } => {
                if obs.frame_ok && obs.timed_wait_done {
                    if count >= self.config.stable_tick_gate {
                        self.state = PowerState::PicSetVoltage;
                    } else {
                        self.state = PowerState::PicHbtGateTick { count: count + 1 };
                    }
                } else if !obs.frame_ok {
                    // Stable-tick reset (HBT NACK mid-gate): reset count
                    // to 1, continue Phase D.
                    self.state = PowerState::PicHbtGateTick { count: 1 };
                }
            }
            PowerState::PicSetVoltage => {
                if obs.frame_ok {
                    self.state = PowerState::PicEnable;
                }
            }
            PowerState::PicEnable => {
                if obs.frame_ok {
                    self.state = PowerState::PicDcDcRamp;
                }
            }
            PowerState::PicDcDcRamp => {
                if obs.timed_wait_done {
                    self.state = PowerState::PicHbtLoopStart;
                }
            }
            PowerState::PicHbtLoopStart => {
                self.state = PowerState::Ready;
            }
            PowerState::Ready | PowerState::Fault => {
                // Terminal until reset().
            }
        }
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_obs() -> PowerObservation {
        PowerObservation {
            i2c_ack: true,
            frame_ok: true,
            pic_fw: None,
            timed_wait_done: true,
        }
    }

    #[test]
    fn fresh_starts_at_power_on() {
        let m = PowerStateMachine::fresh();
        assert_eq!(m.state(), PowerState::PowerOn);
        assert_eq!(m.observed_fw(), None);
    }

    #[test]
    fn happy_path_traversal_to_ready_through_reset_path() {
        let mut m = PowerStateMachine::fresh();
        // PowerOn -> GpioPwrAssert (no observation needed; auto).
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::GpioPwrAssert);
        // GpioPwrAssert -> GpioHbReset.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::GpioHbReset);
        // GpioHbReset -> PsuOpenBus.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuOpenBus);
        // PsuOpenBus -> PsuGetVersion.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuGetVersion);
        // PsuGetVersion -> PsuDisarm{1}.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuDisarm { attempt: 1 });
        // 3-write disarm sequence.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuDisarm { attempt: 2 });
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuDisarm { attempt: 3 });
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuSetInitV);
        // ... continue to PSU complete.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuArmWd);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuHbtLoopStart);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuSettle);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicFlushParser);
        // PIC bring-up; fw byte observed at GetVersion.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicGetVersion);
        m.feed(PowerObservation {
            pic_fw: Some(0x82),
            ..ok_obs()
        });
        assert_eq!(m.state(), PowerState::PicVariantDecide);
        // 0x82 is RESET-allowed, so we go through the reset path.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicReset);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicResetWait);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicJump);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicJumpWait);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicGetVersion2);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicHbtGateTick { count: 1 });
        // 5-tick gate.
        for n in 2..=5u8 {
            m.feed(ok_obs());
            assert_eq!(m.state(), PowerState::PicHbtGateTick { count: n });
        }
        // 5th tick promotes to PicSetVoltage.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicSetVoltage);
        // SET_VOLTAGE -> Enable -> DC/DC ramp -> HBT loop -> Ready.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicEnable);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicDcDcRamp);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicHbtLoopStart);
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::Ready);
    }

    #[test]
    fn fw_0x89_skips_reset_path() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::PicVariantDecide;
        m.observed_fw = Some(0x89);
        // RESET-banned variant; jumps straight to Phase D.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicHbtGateTick { count: 1 });
    }

    #[test]
    fn fw_0x86_refuses_voltage_by_default() {
        let mut m = PowerStateMachine::fresh();
        // Synthesize: jump straight to PicVariantDecide with fw=0x86.
        m.state = PowerState::PicGetVersion;
        m.feed(PowerObservation {
            pic_fw: Some(0x86),
            ..ok_obs()
        });
        // PicVariantDecide is reached, then the fw_86 refusal kicks in.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::Fault);
    }

    #[test]
    fn fw_0x86_can_be_overridden() {
        let mut m = PowerStateMachine::new(PowerStateConfig {
            refuse_fw_86: false,
            ..Default::default()
        });
        m.state = PowerState::PicGetVersion;
        m.feed(PowerObservation {
            pic_fw: Some(0x86),
            ..ok_obs()
        });
        m.feed(ok_obs());
        // 0x86 with refuse_fw_86=false -> takes RESET path (some lab
        // recovery scenarios; never use in production without fw byte
        // promotion to 0x82/0x89).
        assert_eq!(m.state(), PowerState::PicReset);
    }

    #[test]
    fn five_tick_gate_is_mandatory() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::PicHbtGateTick { count: 4 };
        // 4 ticks alone is NOT enough — must reach 5.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicHbtGateTick { count: 5 });
        // The 5th tick promotes to PicSetVoltage.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PicSetVoltage);
    }

    #[test]
    fn hbt_nack_mid_gate_resets_count() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::PicHbtGateTick { count: 3 };
        // Bad frame mid-gate -> reset to 1.
        m.feed(PowerObservation {
            frame_ok: false,
            ..ok_obs()
        });
        assert_eq!(m.state(), PowerState::PicHbtGateTick { count: 1 });
    }

    #[test]
    fn psu_disarm_requires_three_writes() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::PsuDisarm { attempt: 1 };
        // 1 -> 2.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuDisarm { attempt: 2 });
        // 2 -> 3.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuDisarm { attempt: 3 });
        // 3 -> next phase (only AFTER the 3rd write itself succeeds).
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuSetInitV);
    }

    #[test]
    fn fault_is_terminal_until_reset() {
        let mut m = PowerStateMachine::fresh();
        m.mark_fault();
        for _ in 0..10 {
            m.feed(ok_obs());
        }
        assert_eq!(m.state(), PowerState::Fault);
        m.reset();
        assert_eq!(m.state(), PowerState::PowerOn);
        assert_eq!(m.observed_fw(), None);
    }

    #[test]
    fn ready_is_terminal_until_reset() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::Ready;
        for _ in 0..10 {
            m.feed(ok_obs());
        }
        assert_eq!(m.state(), PowerState::Ready);
    }

    #[test]
    fn no_progress_when_observation_blocks_advance() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::PsuOpenBus;
        // i2c_ack=false: no progress.
        let no_ack = PowerObservation {
            i2c_ack: false,
            ..ok_obs()
        };
        m.feed(no_ack);
        assert_eq!(m.state(), PowerState::PsuOpenBus);
        // Now ack: advance.
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::PsuGetVersion);
    }

    #[test]
    fn variant_decide_without_fw_observed_faults() {
        let mut m = PowerStateMachine::fresh();
        m.state = PowerState::PicVariantDecide;
        // observed_fw is still None -> fault (impossible state in real
        // life, but the guard is here for safety).
        m.feed(ok_obs());
        assert_eq!(m.state(), PowerState::Fault);
    }

    #[test]
    fn config_default_locks_in_canonical_constants() {
        let cfg = PowerStateConfig::default();
        assert_eq!(cfg.stable_tick_gate, 5);
        assert_eq!(cfg.psu_disarm_triple_write_count, 3);
        assert!(cfg.refuse_fw_86);
    }

    #[test]
    fn observation_serializes_as_round_trippable_struct() {
        let m = PowerStateMachine::fresh();
        let json = serde_json::to_string(&m.state()).unwrap();
        let back: PowerState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, PowerState::PowerOn);
    }

    #[test]
    fn psu_disarm_attempt_field_serializes_with_serde() {
        let s = PowerState::PsuDisarm { attempt: 2 };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("psu_disarm"));
        assert!(json.contains("\"attempt\":2"));
        let back: PowerState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
