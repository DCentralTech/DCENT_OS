//!  HIGH-8 (2026-05-24) — recipe-broken runtime guard.
//!
//! Fail-closed startup gate that refuses to launch `dcentrald` on
//! `a lab unit`-class AM2-XIL-Loki hardware when any of the 4 LIVE-FALSIFIED
//! "must-not-set" `DCENT_AM2_*` env vars from the  PROVEN MINING
//! RECIPE are present in the environment.
//!
//! ## Context
//!
//! First DCENT_OS mining on `a lab unit` was achieved 2026-05-24 ():
//! 12 shares accepted by public-pool.io in 165 s via bosminer-handoff.
//! The proven path requires EXACTLY 13 specific env vars set (see
//! `tests/wave54_proven_mining_recipe.rs`), and 4 specific env vars
//! that MUST NOT be set — any of them re-breaks the mining path. The
//! 4 forbidden gates were each independently FALSIFIED on the live
//! `a lab unit` unit:
//!
//! | Env var                                    | Why forbidden            |
//! |--------------------------------------------|--------------------------|
//! | `DCENT_AM2_PIC_RESET_AND_START_APP`        | BARE warmup transitions chip from fw=0x89 → fw=0x82 |
//! | `DCENT_AM2_PIC_RESET_STRACE_DERIVED`       | FRAMED warmup, same fw transition problem |
//! | `DCENT_AM2_PSU_LOKI_REGISTER_POINTER`      | Loki Enable bytes corrupt spoof state for next bosminer |
//! | `DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE`     | Calibration probe corrupts Loki spoof state |
//!
//! ## Decision contract
//!
//! - Fingerprint MUST match on ALL of: `platform == "zynq-bm3-am2"`,
//!   `board_target` ends with `xil` (suffix match), and (if present)
//!   `psu_hardware_variant == "loki"`. If `psu_hardware_variant` is
//!   absent the guard still fires on platform+board_target alone (the
//!   2 forbidden PIC vars affect dsPIC state regardless of PSU, and
//!   `a lab unit` is the only documented XIL-25 unit). The lab-only
//!   `DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE=1` may bridge the deliberate
//!   `am2-s19j` sysupgrade package identity into `a lab unit` diagnostics, but
//!   it still requires the same AM2/Loki-compatible platform fingerprint.
//! - If fingerprint does NOT match (S9, `a lab unit`, `a lab unit`, `a lab unit`, `a lab unit`,
//!   any non-`a lab unit`-class), the guard is a no-op pass.
//! - If fingerprint matches AND any of the 4 forbidden gates is set
//!   (via the canonical env-helper convention — `1`/`true`/`yes`/`on`
//!   case-insensitive), the guard REFUSES to start (returns
//!   `GuardDecision::Refuse` with the offending names).
//! - Operator override: `DCENT_BYPASS_WAVE54_GUARD=1` flips a
//!   forced refuse → a `Bypass` decision so the caller logs a loud
//!   warning and continues. Lab-only / recovery escape hatch.
//!
//! ## Wiring
//!
//! Called from `main.rs::run_main` AFTER config load + logging init +
//! platform-stamp read, BEFORE any I²C/UIO/PSU/PIC/chain hardware
//! touch. See the call site in `run_main()` just after the
//! `auto-enabled s19j-hybrid` info log, before the silicon-profile
//! registry load.
//!
//! ## Test guarantees
//!
//! See `tests/wave55a_recipe_broken_guard_refuses_forbidden_env_on_xil_25.rs`
//! for the host-runnable regression pins. The pure decision function
//! `evaluate_guard()` is the unit under test (no HAL deps; takes string
//! slices + an env list; returns a pure enum).
//!
//! ## Operator override hint
//!
//! Refusal error message must include both the runbook
//!  and the
//! load-bearing memory rule
//!  so a future
//! agent / operator can find the canonical recipe.
//!
//! ## Exit code
//!
//! On refusal the daemon exits with `EX_CONFIG = 78` (BSD sysexits
//! convention — "configuration error"), distinct from the strict-reject
//! unknown-flag exit code (2) and from generic runtime failures (1).

use tracing::{error, warn};

/// The 4 env vars that re-break the  PROVEN MINING RECIPE
/// on `a lab unit`-class XIL S19j Pro hardware. Setting ANY of these
/// regresses the proven path (each was independently live-falsified
/// before the recipe was locked in).
///
/// Keep this list in sync with:
/// - `tests/wave54_proven_mining_recipe.rs::FORBIDDEN_ENV_VARS`
///
/// - `DCENT_OS_Antminer/scripts/run_wave54_25_PROVEN_MINING.sh`
///
/// ##  standalone-mode carve-out (2026-05-25)
///
/// `DCENT_AM2_PIC_RESET_AND_START_APP` is **conditionally** forbidden.
/// On cold-cold DCENT_OS-from-NAND boot ( standalone path),
/// the chip starts in dsPIC fw=0x82 BOOTLOADER and requires the BARE
/// warmup to transition to fw=0x82 APP MODE (per  evidence).
/// The  classification (always-forbidden) held only for the
/// bosminer-handoff recipe, where the chip starts at fw=0x89 and
/// BARE warmup re-breaks rail engagement.
///
/// When `DCENT_AM2_STANDALONE_RE_FIX=1` is present, the guard
/// removes `DCENT_AM2_PIC_RESET_AND_START_APP` from the offender
/// scan (the other 3 stay forbidden — they corrupt Loki spoof state
/// regardless of standalone vs handoff mode). See
/// `evaluate_guard()` for the filter logic.
///
/// ##  standalone-mode FRAMED warmup carve-out (2026-05-25)
///
/// `DCENT_AM2_PIC_RESET_STRACE_DERIVED` is **also** conditionally
/// forbidden under the same `DCENT_AM2_STANDALONE_RE_FIX=1` umbrella.
/// On cold-cold AC-cycle, /55i/55j LIVE evidence proved that
/// the BARE warmup (`DCENT_AM2_PIC_RESET_AND_START_APP`) does NOT
/// transition the chip from `fw=0x82 BOOTLOADER` → `fw=0x89` APP MODE;
/// the FRAMED chain (byte-exact replay of bosminer's
/// `[55 AA 04 07 00 0B]` RESET + `[AA 04 00 0A]` JUMP + `[AA 04 17
/// 00 1B]` GET_VERSION) is the proven byte sequence that engages
/// chip rail to 13.7 V on cold-cold standalone.
///
/// The  classification of FRAMED-as-forbidden held only for
/// the bosminer-handoff recipe (where the chip is already at fw=0x89
/// and the FRAMED RESET would pull it back to fw=0x82). Under
/// standalone cold-cold, the chip is at fw=0x82 BOOTLOADER and the
/// FRAMED chain is the only known path to fw=0x89.
///
///  extends the existing  carve-out to drop BOTH
/// `DCENT_AM2_PIC_RESET_AND_START_APP` and
/// `DCENT_AM2_PIC_RESET_STRACE_DERIVED` from the offender scan when
/// `DCENT_AM2_STANDALONE_RE_FIX=1`. The other 2 gates
/// (`PSU_LOKI_REGISTER_POINTER`, `PSU_CALIBRATION_PROBE_WAKE`) stay
/// forbidden — they corrupt Loki spoof state regardless of mode.
pub const WAVE54_FORBIDDEN_ENV_VARS: &[&str] = &[
    "DCENT_AM2_PIC_RESET_AND_START_APP",
    "DCENT_AM2_PIC_RESET_STRACE_DERIVED",
    "DCENT_AM2_PSU_LOKI_REGISTER_POINTER",
    "DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE",
];

/// VOLT-PIC-1 (2026-06-21) — lab-only SAFETY overrides that must NEVER be set on
/// the `a lab unit` home unit, in **every** mode (no standalone carve-out).
///
/// Unlike [`WAVE54_FORBIDDEN_ENV_VARS`] (which break the *mining recipe* and are
/// conditionally carved out for the standalone cold-boot path), these two env
/// vars loosen the **safety envelope** and are never part of any proven `a lab unit`
/// path — they exist only for bench/lab work on sacrificial hardware:
///
/// | Env var                          | What it loosens (forbidden on `a lab unit`) |
/// |----------------------------------|--------------------------------------|
/// | `DCENT_AM2_ALLOW_LAB_OVERVOLT`   | lifts the over-volt clamp ceiling    |
/// | `DCENT_AM2_TRUST_DEGRADED_FW`    | trusts a fw=0x86 (post-RESET degraded) dsPIC for voltage commands |
///
/// `a lab unit` is the operator's home-study unit (PWM-30 quiet cap, home posture), so
/// the guard fails closed with a loud refuse if either is present — STRENGTHENING
/// the `a lab unit` envelope. They are scanned regardless of `DCENT_AM2_STANDALONE_RE_FIX`
/// because neither is ever needed to bring `a lab unit` up. The fw=0x86 refusal and the
/// over-volt clamp themselves still live (and remain the primary guard) in
/// `dcentrald-asic`/`s19j_hybrid_mining`; this is defense-in-depth for `a lab unit`.
pub const XIL_25_FORBIDDEN_SAFETY_OVERRIDE_ENV_VARS: &[&str] = &[
    "DCENT_AM2_ALLOW_LAB_OVERVOLT",
    "DCENT_AM2_TRUST_DEGRADED_FW",
];

///  (2026-05-25) — Standalone-mode umbrella env var. When
/// truthy in the environment, drops `DCENT_AM2_PIC_RESET_AND_START_APP`
/// from the forbidden-list scan (the BARE warmup is the proven
/// cold-cold path for dsPIC fw=0x82 bootloader → app-mode transition
/// per  evidence). The other 3 forbidden gates remain
/// forbidden — they corrupt Loki spoof state regardless of cold-cold
/// vs post-bosminer chip state.
///
///
/// for the dual-state (fw=0x82 cold / fw=0x89 post-bosminer) model.
pub const STANDALONE_RE_FIX_ENV_VAR: &str = "DCENT_AM2_STANDALONE_RE_FIX";

/// Operator override — turns a refusal into a loud warning + continue.
/// Lab/recovery only; production launchers MUST NOT set this.
pub const BYPASS_ENV_VAR: &str = "DCENT_BYPASS_WAVE54_GUARD";

/// EX_CONFIG from BSD sysexits.h. Used as the daemon exit code when the
/// guard refuses to start (distinct from strict-reject's exit 2 and from
/// generic anyhow runtime failures).
pub const EX_CONFIG_EXIT_CODE: i32 = 78;

/// `/etc/dcentos/*` paths the guard reads to fingerprint the platform.
/// Held as constants so the test file can name them in error messages
/// and so the `read_*` helpers stay byte-flat with the existing
/// `auto_detect_s19j_hybrid` / `tokio_pool_for_board_target` readers.
pub const PLATFORM_FILE: &str = "/etc/dcentos/platform";
pub const BOARD_TARGET_FILE: &str = "/etc/dcentos/board_target";
pub const PSU_HARDWARE_VARIANT_FILE: &str = "/etc/dcentos/psu_hardware_variant";

/// Canonical platform string for the Antminer S19j Pro on the AM2 Zynq
/// control board (XIL `a lab unit`, `a lab unit`, `a lab unit` all match this stamp).
pub const ZYNQ_BM3_AM2_PLATFORM: &str = "zynq-bm3-am2";

/// Suffix that uniquely identifies `a lab unit`-class XIL units in
/// `/etc/dcentos/board_target`. Allows both `am2-xil` and
/// `am2-s19jpro-xil` forms (the toolbox has historically written both
/// shapes; the suffix match keeps us tolerant without false-matching
/// `a lab unit` or `a lab unit` which use a different board_target string).
pub const XIL_25_BOARD_TARGET_SUFFIX: &str = "xil";

/// Lab-only explicit `a lab unit` fingerprint override.
///
/// The current am2-s19jpro sysupgrade package must keep
/// `/etc/dcentos/board_target = am2-s19j` so the package prefix and install
/// gates keep matching `sysupgrade-am2-s19j/`.  standalone diagnostics
/// on the known home `a lab unit` unit need the `a lab unit`-gated code paths anyway, so the
/// launcher may set this env var. When set, the guard also treats the run as a
/// `a lab unit` fingerprint so any forbidden  gates still fail closed.
pub const XIL_25_FINGERPRINT_OVERRIDE_ENV: &str = "DCENT_AM2_XIL25_FINGERPRINT_OVERRIDE";

/// Default-off reachability gate for applying the AM2-Zynq/BM1362 `a lab unit`
/// recipe family to sibling units after their own operator A/B.
pub const AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV: &str = "DCENT_AM2_ZYNQ_BM1362_CLASS_RECIPE";

/// Lab-only explicit `a lab unit` per-unit proof override. Launchers must set this
/// only after validating the unit's durable identity (for example MAC +
/// platform files); the daemon deliberately does not infer `a lab unit` from IP.
pub const XIL_109_FINGERPRINT_OVERRIDE_ENV: &str = "DCENT_AM2_XIL109_FINGERPRINT_OVERRIDE";

/// Canonical `psu_hardware_variant` for the Loki spoof board. When
/// present in `/etc/dcentos/psu_hardware_variant` this is the strongest
/// possible `a lab unit`-class fingerprint (the only documented Loki-spoof unit
/// on the active fleet is `a lab unit`). Absence is tolerated — fingerprint
/// passes on platform+board_target alone.
pub const LOKI_PSU_HARDWARE_VARIANT: &str = "loki";

/// Guard decision returned by [`evaluate_guard`] (the pure host-testable
/// algorithm). The caller in `main.rs` translates this into an
/// `error!` + `process::exit(78)` / `warn!` + `continue` / no-op pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardDecision {
    /// Fingerprint did not match (non-`a lab unit` unit OR `a lab unit` unit with
    /// clean env). No log, no warning — guard is a silent no-op. Holds
    /// the brief reason for trace-level diagnostics only.
    Pass {
        /// Short human-readable reason, e.g. "platform=zynq-bm3-am1"
        /// or "no forbidden env vars set". Not surfaced to operator;
        /// kept for `tracing::trace!` if a debug investigation needs it.
        reason: String,
    },
    /// Fingerprint matched AND at least one forbidden env var is set.
    /// Caller MUST log an `error!` listing `offenders`, mention the
    /// runbook + memory rule, and exit with `EX_CONFIG_EXIT_CODE`.
    Refuse {
        /// The env var names that are set (in `WAVE54_FORBIDDEN_ENV_VARS`
        /// order, deduplicated). Always non-empty when `Refuse` is
        /// returned.
        offenders: Vec<String>,
        /// Detected fingerprint values for the log line.
        platform: String,
        board_target: String,
        psu_hardware_variant: Option<String>,
    },
    /// Fingerprint matched AND forbidden gates set BUT the operator
    /// override `DCENT_BYPASS_WAVE54_GUARD=1` is present. Caller MUST
    /// log a `warn!` (loud, naming each offender), then proceed. This
    /// is the lab-only / recovery escape hatch.
    Bypass {
        offenders: Vec<String>,
        platform: String,
        board_target: String,
        psu_hardware_variant: Option<String>,
    },
}

/// Returns `true` if `value` matches the canonical DCENT_AM2_* env-flag
/// truth convention (mirrors `s19j_hybrid_mining::am2_env_flag` and the
/// `main::env_flag` helper). Accepts `1`, `true`/`TRUE`, `yes`/`YES`,
/// `on`/`ON`. Anything else (including empty string, `0`, `false`) is
/// false.
///
/// Kept as a free function so the test can call it directly without
/// re-importing the binary-crate helpers.
pub fn env_value_is_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
}

/// Returns `true` iff the platform/board_target/psu_hardware_variant
/// triple identifies a `a lab unit`-class AM2-XIL unit.
///
/// Match rules:
/// - `platform` MUST equal `zynq-bm3-am2`.
/// - `board_target` MUST end with `xil` (suffix match — tolerates
///   both `am2-xil` and `am2-s19jpro-xil` historical shapes).
/// - `psu_hardware_variant` MAY be absent. If present and `!= "loki"`,
///   the fingerprint does NOT match (caller likely declared a different
///   PSU variant → not the `a lab unit` Loki-spoof topology). If present and
///   `== "loki"` (or absent), match passes on platform + board_target.
pub fn fingerprint_matches_xil_25(
    platform: &str,
    board_target: &str,
    psu_hardware_variant: Option<&str>,
) -> bool {
    if platform.trim() != ZYNQ_BM3_AM2_PLATFORM {
        return false;
    }
    if !board_target.trim().ends_with(XIL_25_BOARD_TARGET_SUFFIX) {
        return false;
    }
    // psu_hardware_variant is optional. If explicitly declared as
    // something OTHER than "loki", treat it as a not-`a lab unit` declaration
    // and let the run proceed (operator has actively said this is not
    // the Loki topology). Absence or explicit "loki" → match.
    match psu_hardware_variant.map(str::trim) {
        Some("") | None => true,
        Some(v) if v.eq_ignore_ascii_case(LOKI_PSU_HARDWARE_VARIANT) => true,
        Some(_) => false,
    }
}

/// True for the AM2 Zynq + BM1362 class only. This is a class predicate, not an
/// activation gate: callers still need an explicit per-unit proof before using
/// any `a lab unit`-derived recipe on a sibling unit. A present non-BM1362 chip hint is
/// treated as a veto so a contradictory runtime probe fails closed.
pub fn am2_zynq_bm1362_class_matches(
    platform: &str,
    board_target: &str,
    chip_type_hint: Option<&str>,
) -> bool {
    if platform.trim() != ZYNQ_BM3_AM2_PLATFORM {
        return false;
    }

    let board_target = board_target.trim().to_ascii_lowercase();
    let board_target_is_bm1362 = matches!(board_target.as_str(), "am2-s19j" | "am2-s19jpro")
        || board_target.ends_with("s19jpro-xil")
        || board_target.ends_with("xil");

    let chip_hint_is_bm1362 = chip_type_hint
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let value = value.to_ascii_lowercase();
            value == "bm1362" || value == "0x1362" || value == "1362"
        });

    if chip_hint_is_bm1362 == Some(false) {
        return false;
    }

    board_target_is_bm1362 || chip_hint_is_bm1362.unwrap_or(false)
}

/// True only when the AM2-Zynq/BM1362 class matches and the caller supplied an
/// explicit `a lab unit` proof env. This keeps `a lab unit` reachability default-off. The
/// env is an external per-unit proof supplied by a launcher/runbook after
/// durable identity checks; the daemon deliberately does not infer `a lab unit` from
/// IP address or broad board class alone.
pub fn fingerprint_matches_xil_109<F>(
    platform: &str,
    board_target: &str,
    chip_type_hint: Option<&str>,
    env_lookup: F,
) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    am2_zynq_bm1362_class_matches(platform, board_target, chip_type_hint)
        && env_lookup(XIL_109_FINGERPRINT_OVERRIDE_ENV)
            .as_deref()
            .map(env_value_is_truthy)
            .unwrap_or(false)
}

/// Pure host-testable guard algorithm. Takes pre-read string values +
/// the full env table; returns the decision.
///
/// `env_lookup` is a callback so the test can pass an in-memory map
/// without poisoning the real process env. In production the caller
/// passes a closure that wraps `std::env::var(name).ok()`.
pub fn evaluate_guard<F>(
    platform: &str,
    board_target: &str,
    psu_hardware_variant: Option<&str>,
    env_lookup: F,
) -> GuardDecision
where
    F: Fn(&str) -> Option<String>,
{
    let explicit_xil25_override = env_lookup(XIL_25_FINGERPRINT_OVERRIDE_ENV)
        .as_deref()
        .map(env_value_is_truthy)
        .unwrap_or(false)
        && board_target.trim() == "am2-s19j"
        && fingerprint_matches_xil_25(platform, "am2-xil", psu_hardware_variant);

    if !fingerprint_matches_xil_25(platform, board_target, psu_hardware_variant)
        && !explicit_xil25_override
    {
        return GuardDecision::Pass {
            reason: format!(
                "fingerprint not `a lab unit`-class XIL (platform={platform:?} \
                 board_target={board_target:?} psu_hardware_variant={:?})",
                psu_hardware_variant
            ),
        };
    }

    //  (2026-05-25) — Standalone-mode carve-out for the
    // cold-cold DCENT_OS-from-NAND boot path. When the operator has
    // explicitly opted into the  standalone RE-finding path
    // via `DCENT_AM2_STANDALONE_RE_FIX=1`, the BARE warmup is the
    // proven cold-boot transition (per  evidence: 16-byte
    // parser flush + bare `[55 AA 07]` RESET → chip transitions from
    // fw=0x82 BOOTLOADER → fw=0x82 APP MODE). The  forbidden
    // classification of `DCENT_AM2_PIC_RESET_AND_START_APP` was
    // correct for the bosminer-handoff recipe (where the chip starts
    // at fw=0x89 and BARE warmup pulls it back to fw=0x82 → loses
    // chip rail). On the standalone path there is no fw=0x89 state
    // to break — the chip is fresh from AC-cycle.
    //
    //  (2026-05-25) — Standalone-mode FRAMED warmup carve-out.
    // /55i/55j LIVE evidence proved BARE alone does NOT lift
    // the chip from fw=0x82 BOOTLOADER → fw=0x89 APP MODE on cold-cold
    // `a lab unit`; the FRAMED chain (byte-exact replay of bosminer's
    // `[55 AA 04 07 00 0B]` RESET + `[AA 04 00 0A]` JUMP) is the
    // proven byte sequence that engages chip rail to 13.7 V. The
    //  classification of FRAMED-as-forbidden was correct only
    // for the bosminer-handoff recipe (chip already at fw=0x89; FRAMED
    // RESET would pull it back to fw=0x82). Under standalone, the
    // chip is at fw=0x82 BOOTLOADER and the FRAMED chain is the only
    // proven path to fw=0x89. Carve out the same way as BARE.
    //
    // Other 2 gates (`PSU_LOKI_REGISTER_POINTER`,
    // `PSU_CALIBRATION_PROBE_WAKE`) stay forbidden because they
    // corrupt Loki spoof state regardless of standalone vs handoff
    // mode (next bosminer engagement fails until AC-cycle).
    let standalone_re_fix = env_lookup(STANDALONE_RE_FIX_ENV_VAR)
        .map(|v| env_value_is_truthy(&v))
        .unwrap_or(false);

    // Fingerprint matches — scan the forbidden env vars (the  recipe
    // breakers below, then the VOLT-PIC-1 safety overrides).
    let mut offenders: Vec<String> = Vec::new();
    for name in WAVE54_FORBIDDEN_ENV_VARS {
        //  +  standalone-mode carve-out: skip BOTH
        // `DCENT_AM2_PIC_RESET_AND_START_APP` (BARE warmup)
        // AND `DCENT_AM2_PIC_RESET_STRACE_DERIVED` (FRAMED
        // warmup) when the operator has opted into the standalone
        // cold-boot path. The remaining 2 gates
        // (`PSU_LOKI_REGISTER_POINTER`, `PSU_CALIBRATION_PROBE_WAKE`)
        // stay forbidden — they corrupt Loki spoof state regardless
        // of cold-cold vs post-bosminer chip state.
        if standalone_re_fix
            && (*name == "DCENT_AM2_PIC_RESET_AND_START_APP"
                || *name == "DCENT_AM2_PIC_RESET_STRACE_DERIVED")
        {
            continue;
        }
        if let Some(value) = env_lookup(name) {
            if env_value_is_truthy(&value) {
                offenders.push((*name).to_string());
            }
        }
    }

    // VOLT-PIC-1: the lab over-volt / trust-degraded-fw safety overrides are
    // ALSO forbidden on `a lab unit`, in EVERY mode (no standalone carve-out — they are
    // never part of any proven `a lab unit` path; they only loosen the safety envelope
    // for bench/lab work on sacrificial hardware). This STRENGTHENS the `a lab unit`
    // home-unit guard: the over-volt clamp and the fw=0x86 refusal must never be
    // loosened on the operator's home study unit.
    for name in XIL_25_FORBIDDEN_SAFETY_OVERRIDE_ENV_VARS {
        if let Some(value) = env_lookup(name) {
            if env_value_is_truthy(&value) {
                offenders.push((*name).to_string());
            }
        }
    }

    if offenders.is_empty() {
        return GuardDecision::Pass {
            reason: "fingerprint matches `a lab unit`-class XIL but no forbidden \
                     env vars are set (recipe-compatible)"
                .to_string(),
        };
    }

    // Operator override: turn refusal into a loud warning + continue.
    let bypass = env_lookup(BYPASS_ENV_VAR)
        .map(|v| env_value_is_truthy(&v))
        .unwrap_or(false);

    let p = platform.trim().to_string();
    let b = board_target.trim().to_string();
    let psu_hv = psu_hardware_variant
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if bypass {
        GuardDecision::Bypass {
            offenders,
            platform: p,
            board_target: b,
            psu_hardware_variant: psu_hv,
        }
    } else {
        GuardDecision::Refuse {
            offenders,
            platform: p,
            board_target: b,
            psu_hardware_variant: psu_hv,
        }
    }
}

/// Read `/etc/dcentos/*` fingerprint files + the live process env and
/// run [`evaluate_guard`]. Returns `Ok(())` for `Pass` / `Bypass`
/// (after logging in the `Bypass` case), or `Err(anyhow::Error)` with
/// `EX_CONFIG_EXIT_CODE` exit-code metadata for `Refuse`. The caller
/// in `main.rs` should match on the result and `process::exit(78)` on
/// the `Err` branch (rather than `?`-bubbling) so the operator gets a
/// clean exit code instead of an anyhow stack trace.
///
/// On `Refuse`, logs a full `tracing::error!` with the offending env
/// vars, the fingerprint, the runbook path, and the load-bearing
/// memory rule name — so a future agent / operator can find the
/// canonical recipe without needing to read this source file.
///
/// On `Bypass`, logs a `tracing::warn!` that the lab-override is
/// active and the run is proceeding despite the recipe-broken env.
///
/// On `Pass`, returns silently (no log — non-`a lab unit` units must not see
/// any  noise).
pub fn enforce() -> std::result::Result<(), GuardRefusal> {
    let platform = std::fs::read_to_string(PLATFORM_FILE)
        .or_else(|_| std::fs::read_to_string("/etc/bos_platform"))
        .unwrap_or_default();
    let board_target = std::fs::read_to_string(BOARD_TARGET_FILE).unwrap_or_default();
    let psu_hardware_variant = std::fs::read_to_string(PSU_HARDWARE_VARIANT_FILE).ok();

    let decision = evaluate_guard(
        platform.trim(),
        board_target.trim(),
        psu_hardware_variant.as_deref().map(str::trim),
        |name| std::env::var(name).ok(),
    );

    match decision {
        GuardDecision::Pass { .. } => Ok(()),
        GuardDecision::Bypass {
            offenders,
            platform,
            board_target,
            psu_hardware_variant,
        } => {
            warn!(
                offenders = ?offenders,
                platform = %platform,
                board_target = %board_target,
                psu_hardware_variant = ?psu_hardware_variant,
                bypass_env = BYPASS_ENV_VAR,
                "Wave-55a guard BYPASSED via {} — proceeding despite recipe-broken env. \
                 This is a LAB-ONLY override; production launchers MUST NOT set this. \
.",
                BYPASS_ENV_VAR
            );
            Ok(())
        }
        GuardDecision::Refuse {
            offenders,
            platform,
            board_target,
            psu_hardware_variant,
        } => {
            error!(
                offenders = ?offenders,
                platform = %platform,
                board_target = %board_target,
                psu_hardware_variant = ?psu_hardware_variant,
                "Wave-55a guard REFUSING to start: {} forbidden env var(s) set on \
                 `a lab unit`-class XIL hardware. Each of these env vars has been LIVE-FALSIFIED \
                 on `a lab unit` — setting any one re-breaks the Wave-54 PROVEN MINING RECIPE \
                 (12 shares accepted by public-pool.io in 165s via bosminer-handoff). \
                 Forbidden vars set: {}. Runbook: \
                  \
                 Load-bearing rule: . \
                 To override for lab/recovery only, set {}=1 (loud warning + continue). \
                 Exiting with EX_CONFIG ({}).",
                offenders.len(),
                offenders.join(", "),
                BYPASS_ENV_VAR,
                EX_CONFIG_EXIT_CODE,
            );
            Err(GuardRefusal {
                offenders,
                platform,
                board_target,
                psu_hardware_variant,
            })
        }
    }
}

/// Surfaced refusal — the caller (`main.rs`) is expected to
/// `process::exit(EX_CONFIG_EXIT_CODE)` after observing this. Not an
/// `anyhow::Error` so we get a clean exit code rather than a backtrace.
#[derive(Debug, Clone)]
pub struct GuardRefusal {
    pub offenders: Vec<String>,
    pub platform: String,
    pub board_target: String,
    pub psu_hardware_variant: Option<String>,
}

impl std::fmt::Display for GuardRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Wave-55a guard refused start: forbidden env vars [{}] set on `a lab unit`-class XIL \
             ({}/{})..",
            self.offenders.join(", "),
            self.platform,
            self.board_target,
        )
    }
}

impl std::error::Error for GuardRefusal {}

#[cfg(test)]
mod inline_tests {
    //! Inline unit tests (also pinned by the host-runnable integration
    //! test at `tests/wave55a_recipe_broken_guard_refuses_forbidden_env_on_xil_25.rs`).
    //! These run with `cargo test -p dcentrald --lib` on any host
    //! (no HAL deps in this module).
    use super::*;
    use std::collections::HashMap;

    fn env_from(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |name: &str| owned.get(name).cloned()
    }

    #[test]
    fn fingerprint_matches_canonical_xil_25() {
        assert!(fingerprint_matches_xil_25(
            "zynq-bm3-am2",
            "am2-xil",
            Some("loki")
        ));
        assert!(fingerprint_matches_xil_25(
            "zynq-bm3-am2",
            "am2-s19jpro-xil",
            Some("loki")
        ));
        // psu_hardware_variant absent — still matches (platform + suffix
        // are sufficient).
        assert!(fingerprint_matches_xil_25(
            "zynq-bm3-am2",
            "am2-xil",
            None
        ));
    }

    #[test]
    fn fingerprint_rejects_non_xil_25_units() {
        // S9 / am1
        assert!(!fingerprint_matches_xil_25("zynq-bm1-s9", "am1-s9", None));
        // .109 — XIL but not `a lab unit`
        assert!(!fingerprint_matches_xil_25(
            "zynq-bm3-am2",
            "am2-s19jpro",
            Some("loki")
        ));
        // .135 AML
        assert!(!fingerprint_matches_xil_25(
            "amlogic-a113d",
            "am3-aml-s21",
            None
        ));
        // .79 BB
        assert!(!fingerprint_matches_xil_25(
            "am335x-bb",
            "am3-bb-s19jpro",
            None
        ));
        // .129 S19 Pro
        assert!(!fingerprint_matches_xil_25(
            "zynq-bm3-am2",
            "am2-s19pro",
            None
        ));
    }

    #[test]
    fn fingerprint_rejects_explicit_non_loki_psu_variant() {
        // Operator declared bare-apw3 (not Loki) → don't match, even on
        // a `xil`-suffixed board_target.
        assert!(!fingerprint_matches_xil_25(
            "zynq-bm3-am2",
            "am2-xil",
            Some("bare-apw3")
        ));
    }

    #[test]
    fn passes_when_fingerprint_matches_but_no_forbidden_env() {
        let env = env_from(&[("DCENT_AM2_TRUST_RAIL_FALLBACK", "1")]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
        assert!(matches!(decision, GuardDecision::Pass { .. }));
    }

    #[test]
    fn refuses_for_each_forbidden_env_var_individually() {
        for forbidden in WAVE54_FORBIDDEN_ENV_VARS {
            let env = env_from(&[(*forbidden, "1")]);
            let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
            match decision {
                GuardDecision::Refuse { offenders, .. } => {
                    assert_eq!(offenders, vec![(*forbidden).to_string()]);
                }
                other => panic!("expected Refuse for {forbidden}, got {other:?}"),
            }
        }
    }

    #[test]
    fn fpga_uart_relay_cold_env_is_not_forbidden() {
        // 2026-06-11 (LIVE-PINNED): DCENT_AM2_FPGA_UART_RELAY_COLD drives the
        // observed bosminer low-bit GPIO state at 0x41220000 before enum. The
        // v+2 live run proved this is necessary to match bosminer state but not
        // sufficient to fix standalone enum=0. It touches NO dsPIC, so it must
        // not be added to the forbidden list. Pin both: absence from the list,
        // and that setting it on a `a lab unit` fingerprint does not trip the guard.
        assert!(
            !WAVE54_FORBIDDEN_ENV_VARS.contains(&"DCENT_AM2_FPGA_UART_RELAY_COLD"),
            "DCENT_AM2_FPGA_UART_RELAY_COLD must NOT be forbidden — it matches \
             bosminer GPIO state and touches no dsPIC"
        );
        let env = env_from(&[("DCENT_AM2_FPGA_UART_RELAY_COLD", "1")]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
        assert!(
            matches!(decision, GuardDecision::Pass { .. }),
            "relay-enable env alone must not trigger a guard refusal, got {decision:?}"
        );
    }

    #[test]
    fn override_extends_guard_to_am2_s19j_package_identity() {
        let env = env_from(&[
            (XIL_25_FINGERPRINT_OVERRIDE_ENV, "1"),
            ("DCENT_AM2_PSU_LOKI_REGISTER_POINTER", "1"),
        ]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-s19j", Some("loki"), env);
        match decision {
            GuardDecision::Refuse { offenders, .. } => {
                assert_eq!(
                    offenders,
                    vec!["DCENT_AM2_PSU_LOKI_REGISTER_POINTER".to_string()]
                );
            }
            other => panic!("expected Refuse with override on am2-s19j, got {other:?}"),
        }
    }

    #[test]
    fn override_alone_passes_on_am2_s19j_package_identity() {
        let env = env_from(&[(XIL_25_FINGERPRINT_OVERRIDE_ENV, "1")]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-s19j", Some("loki"), env);
        assert!(matches!(decision, GuardDecision::Pass { .. }));
    }

    #[test]
    fn override_requires_exact_am2_s19j_package_identity() {
        for board_target in ["am2-s19jpro", "am2-s19pro", "am2-s19"] {
            let env = env_from(&[
                (XIL_25_FINGERPRINT_OVERRIDE_ENV, "1"),
                ("DCENT_AM2_PSU_LOKI_REGISTER_POINTER", "1"),
            ]);
            let decision = evaluate_guard("zynq-bm3-am2", board_target, Some("loki"), env);
            assert!(
                matches!(decision, GuardDecision::Pass { .. }),
                "override must not promote {board_target} into the .25 guard"
            );
        }
    }

    #[test]
    fn override_does_not_match_non_loki_psu_variant() {
        let env = env_from(&[
            (XIL_25_FINGERPRINT_OVERRIDE_ENV, "1"),
            ("DCENT_AM2_PSU_LOKI_REGISTER_POINTER", "1"),
        ]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-s19j", Some("bare-apw3"), env);
        assert!(matches!(decision, GuardDecision::Pass { .. }));
    }

    #[test]
    fn bypass_env_converts_refuse_to_bypass() {
        let env = env_from(&[
            ("DCENT_AM2_PIC_RESET_STRACE_DERIVED", "1"),
            (BYPASS_ENV_VAR, "1"),
        ]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
        match decision {
            GuardDecision::Bypass { offenders, .. } => {
                assert_eq!(
                    offenders,
                    vec!["DCENT_AM2_PIC_RESET_STRACE_DERIVED".to_string()]
                );
            }
            other => panic!("expected Bypass, got {other:?}"),
        }
    }

    #[test]
    fn truthy_env_value_recognition_matches_am2_env_flag_convention() {
        for v in &["1", "true", "TRUE", "yes", "YES", "on", "ON"] {
            assert!(env_value_is_truthy(v), "{v:?} must be truthy");
        }
        for v in &["", "0", "false", "FALSE", "no", "off", "2", "1 "] {
            assert!(!env_value_is_truthy(v), "{v:?} must NOT be truthy");
        }
    }

    #[test]
    fn non_xil_25_with_all_forbidden_env_set_still_passes() {
        // Critical no-regression: on S9 / `a lab unit` / `a lab unit` / `a lab unit` /
        // `a lab unit`, even if every forbidden env var is set, the guard
        // MUST pass (it would be a behavior change for non-`a lab unit` units
        // otherwise — those gates have legitimate uses elsewhere).
        let env = env_from(&[
            ("DCENT_AM2_PIC_RESET_AND_START_APP", "1"),
            ("DCENT_AM2_PIC_RESET_STRACE_DERIVED", "1"),
            ("DCENT_AM2_PSU_LOKI_REGISTER_POINTER", "1"),
            ("DCENT_AM2_PSU_CALIBRATION_PROBE_WAKE", "1"),
        ]);
        // `a lab unit` (XIL but not `a lab unit`)
        let decision = evaluate_guard("zynq-bm3-am2", "am2-s19jpro", Some("loki"), env);
        assert!(matches!(decision, GuardDecision::Pass { .. }));
    }

    // ----- VOLT-PIC-1: lab over-volt / trust-degraded-fw forbidden on .25 -----

    #[test]
    fn refuses_lab_overvolt_on_xil_25_fingerprint() {
        let env = env_from(&[("DCENT_AM2_ALLOW_LAB_OVERVOLT", "1")]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
        match decision {
            GuardDecision::Refuse { offenders, .. } => {
                assert_eq!(offenders, vec!["DCENT_AM2_ALLOW_LAB_OVERVOLT".to_string()]);
            }
            other => {
                panic!("expected Refuse for DCENT_AM2_ALLOW_LAB_OVERVOLT on .25, got {other:?}")
            }
        }
    }

    #[test]
    fn refuses_trust_degraded_fw_on_xil_25_fingerprint() {
        let env = env_from(&[("DCENT_AM2_TRUST_DEGRADED_FW", "1")]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
        match decision {
            GuardDecision::Refuse { offenders, .. } => {
                assert_eq!(offenders, vec!["DCENT_AM2_TRUST_DEGRADED_FW".to_string()]);
            }
            other => {
                panic!("expected Refuse for DCENT_AM2_TRUST_DEGRADED_FW on .25, got {other:?}")
            }
        }
    }

    #[test]
    fn refuses_each_safety_override_env_var_individually() {
        for forbidden in XIL_25_FORBIDDEN_SAFETY_OVERRIDE_ENV_VARS {
            let env = env_from(&[(*forbidden, "1")]);
            let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
            match decision {
                GuardDecision::Refuse { offenders, .. } => {
                    assert_eq!(offenders, vec![(*forbidden).to_string()]);
                }
                other => panic!("expected Refuse for {forbidden} on .25, got {other:?}"),
            }
        }
    }

    #[test]
    fn safety_overrides_are_not_carved_out_by_standalone_mode() {
        // VOLT-PIC-1 vars are forbidden in EVERY mode — unlike the two PIC-reset
        // vars, the standalone cold-boot carve-out must NOT exempt them.
        let env = env_from(&[
            (STANDALONE_RE_FIX_ENV_VAR, "1"),
            ("DCENT_AM2_TRUST_DEGRADED_FW", "1"),
        ]);
        let decision = evaluate_guard("zynq-bm3-am2", "am2-xil", Some("loki"), env);
        match decision {
            GuardDecision::Refuse { offenders, .. } => {
                assert_eq!(offenders, vec!["DCENT_AM2_TRUST_DEGRADED_FW".to_string()]);
            }
            other => panic!("expected Refuse even in standalone mode, got {other:?}"),
        }
    }

    #[test]
    fn safety_overrides_pass_on_non_xil_25_units() {
        // No regression: on non-`a lab unit` units these vars retain their legitimate
        // lab use, so the guard is a no-op pass even with both set.
        let env = env_from(&[
            ("DCENT_AM2_ALLOW_LAB_OVERVOLT", "1"),
            ("DCENT_AM2_TRUST_DEGRADED_FW", "1"),
        ]);
        // `a lab unit` (XIL but not `a lab unit`)
        let decision = evaluate_guard("zynq-bm3-am2", "am2-s19jpro", Some("loki"), env);
        assert!(matches!(decision, GuardDecision::Pass { .. }));
        // S9 / am1
        let env_s9 = env_from(&[
            ("DCENT_AM2_ALLOW_LAB_OVERVOLT", "1"),
            ("DCENT_AM2_TRUST_DEGRADED_FW", "1"),
        ]);
        let decision_s9 = evaluate_guard("zynq-bm1-s9", "am1-s9", None, env_s9);
        assert!(matches!(decision_s9, GuardDecision::Pass { .. }));
    }
}
