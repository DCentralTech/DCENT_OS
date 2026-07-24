//! Host-testable pure-logic core for DCENT_axe.
//!
//! `config.rs` and `ota_signature.rs` physically live in the `dcentaxe` binary
//! crate and are compiled there for firmware via `mod config;` /
//! `mod ota_signature;`. This crate re-includes the SAME source files via
//! `#[path]` purely so the pure logic can be host-compiled and unit-tested in
//! CI without the ESP-IDF toolchain.
//!
//! There is exactly ONE source of truth for each module — do NOT fork the
//! logic. Edits belong in `dcentaxe/src/{config,ota_signature}.rs`; this crate
//! merely exposes them for `cargo test` on a host target. The `#[cfg(test)]`
//! modules inside those files run here and are excluded from every firmware
//! build (where `cfg(test)` is never set).
//!
//! `temp_decode` follows the same convention but physically lives in
//! `dcentaxe-hal/src/temp_decode.rs` (it is consumed by the esp-idf-gated
//! `temp` module, not the `dcentaxe` binary). It is re-included here only so
//! its `#[cfg(test)]` unit tests host-run under `cargo test -p dcentaxe-core`.
//! This `#[path]` copy is a distinct crate root from the `dcentaxe_hal::temp_decode`
//! reachable through the `dcentaxe-hal` dependency; the copy is the one that
//! gets `cfg(test)`.

// ─────────────────────────────────────────────────────────────────────────────
// Test-harness lint posture (do NOT copy this into the real firmware build).
//
// `dcentaxe-core` is a TEST-HARNESS crate: it re-includes partial source files
// from the `dcentaxe` binary and `dcentaxe-hal` via `#[path]` ONLY so their
// `#[cfg(test)]` unit tests host-run in CI without the ESP-IDF toolchain. Because
// each file is compiled here OUT of its real crate context, several lints fire
// here that are FALSE POSITIVES for this harness:
//
//   * `dead_code` / `unused_imports` / `unused_variables` — items that the
//     `dcentaxe` BINARY (main.rs) uses but this harness does not reach read as
//     unused HERE only.
//   * `unexpected_cfgs` — the board-variant features (`bitaxe-gamma`, `nerdnos`,
//     …) that `config.rs` branches on are declared in the `dcentaxe` crate's
//     Cargo.toml, not in this harness crate's, so each `cfg!(feature = …)` looks
//     "unexpected" here.
//   * `unused_mut` — a `mut` binding that the firmware path reassigns but the
//     host-reachable branch does not.
//
// We allow them at the CRATE ROOT so the clippy `-D warnings` CI gate stays green
// for this harness WITHOUT editing the single-source-of-truth files (whose real
// firmware build, where the code IS used, is unaffected — this allow does not
// follow the source into the `dcentaxe`/`dcentaxe-hal` builds).
#![allow(
    dead_code,
    unused_imports,
    unused_variables,
    unused_mut,
    unexpected_cfgs
)]

#[path = "../../dcentaxe/src/config.rs"]
pub mod config;

// PLAN-E W5500 LAN: the pure Wi-Fi⇄Ethernet failover FSM. Single source of
// truth is `dcentaxe/src/net.rs` (feature-gated `eth-w5500` in the binary);
// re-included here UNCONDITIONALLY so its decision-matrix / flap-telemetry
// tests always host-run in the default gate (it only needs `crate::config`,
// which is re-included above).
#[path = "../../dcentaxe/src/net.rs"]
pub mod net;

#[path = "../../dcentaxe/src/notifications.rs"]
pub mod notifications;

// Same single-source-of-truth pattern: capabilities.rs physically lives in the
// `dcentaxe` binary crate and builds the additive `/api/v1/capabilities`
// descriptor from host-pure config/board metadata plus runtime bounds. Re-include
// it here so its shared-contract pin tests run under `cargo test -p dcentaxe-core`
// without an ESP-IDF toolchain.
#[path = "../../dcentaxe/src/capabilities.rs"]
pub mod capabilities;

#[cfg(test)]
mod capability_api_route_guards {
    const API_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/api.rs"
    ));
    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));

    #[test]
    fn capability_endpoint_is_additive_and_registered_after_system_info() {
        let system_info = API_RS
            .find("register_system_info(server, state.clone());")
            .expect("register_api must keep the AxeOS /api/system/info route");
        let capabilities = API_RS
            .find("register_capabilities(server, state.clone());")
            .expect("register_api must add the shared capability endpoint");
        let system_asic = API_RS
            .find("register_system_asic(server, state.clone());")
            .expect("register_api must keep /api/system/asic");

        assert!(
            system_info < capabilities && capabilities < system_asic,
            "/api/v1/capabilities must be additive beside system info, not folded into it"
        );
        assert!(API_RS.contains("\"/api/v1/capabilities\""));
        assert!(API_RS.contains("build_esp_capability_descriptor("));
        assert!(API_RS.contains("authorize_rest_read(&req, &state)"));
        assert!(API_RS.contains("Do not fold this into `/api/system/info`"));
    }

    #[test]
    fn handler_count_includes_capability_endpoint() {
        assert!(
            MAIN_RS.contains("api.rs              54"),
            "main.rs handler accounting must include /api/v1/capabilities"
        );
        assert!(
            MAIN_RS.contains("const REGISTERED_HANDLER_ESTIMATE: usize = 73;"),
            "REGISTERED_HANDLER_ESTIMATE must include /api/v1/capabilities"
        );
    }
}

#[path = "../../dcentaxe/src/ota_signature.rs"]
pub mod ota_signature;

#[path = "../../dcentaxe-hal/src/temp_decode.rs"]
pub mod temp_decode;

// Same single-source-of-truth pattern: tps546_guard.rs physically lives in
// `dcentaxe-hal` (consumed by the espidf-only `i2c` write path). Re-included here
// only so its `#[cfg(test)]` host tests run under `cargo test -p dcentaxe-core`
// without the ESP-IDF toolchain. The module is dependency-free pure logic
// (XPSAFE-2 write-protect policy), so it host-compiles unchanged. This `#[path]`
// copy is a distinct crate root from `dcentaxe_hal::tps546_guard`; the copy is
// the one that gets `cfg(test)`.
#[path = "../../dcentaxe-hal/src/tps546_guard.rs"]
pub mod tps546_guard;

// Same single-source-of-truth pattern: emc2103.rs physically lives in
// `dcentaxe-hal` (its I2C-backed `Emc2103` struct is consumed by the espidf-only
// firmware build). Re-included here only so the pure `decode` submodule's
// `#[cfg(test)]` host tests run under `cargo test -p dcentaxe-core`. On host the
// `espidf_impl` module (and its `crate::i2c` / `log` deps) is cfg-gated out, so
// only the dependency-free decode math compiles.
#[path = "../../dcentaxe-hal/src/emc2103.rs"]
pub mod emc2103;

// Same single-source-of-truth pattern: fan_pid.rs physically lives in
// `dcentaxe-hal` (its control loop is consumed by the espidf-only firmware
// build, so the module is `#[cfg(target_os = "espidf")]`-gated there). It is
// dependency-free pure PID math (HALT-9: derivative EMA filter + retuned gains),
// so it is re-included here only to host-run its `#[cfg(test)]` unit tests under
// `cargo test -p dcentaxe-core`.
#[path = "../../dcentaxe-hal/src/fan_pid.rs"]
pub mod fan_pid;

// Same single-source-of-truth pattern: chip_profiles_bitaxe.rs physically lives
// in the `dcentaxe` binary crate (consumed by the espidf-only autotuner). It is
// dependency-free pure datasheet/V-F-envelope tables, so it is re-included here
// only to host-run its `#[cfg(test)]` tests under `cargo test -p dcentaxe-core`.
// This pins the per-chip V/F caps the autotuner clamps against (AUTOTUNE-4) so a
// future band-filter/dead-band regression is caught in CI.
#[path = "../../dcentaxe/src/chip_profiles_bitaxe.rs"]
pub mod chip_profiles_bitaxe;

// Same single-source-of-truth pattern: mqtt_ha.rs physically lives in the
// `dcentaxe` binary crate (compiled into firmware via `mod mqtt_ha;`, consumed by
// the esp-idf-only `mqtt` transport module). It is dependency-light pure logic
// (the host-pure Home Assistant MQTT auto-discovery + state-payload builder), so
// it is re-included here only to host-run its `#[cfg(test)]` unit tests under
// `cargo test -p dcentaxe-core`. This pins the discovery topic/payload schema +
// the state-key contract so the firmware transport can never publish a payload
// that drifts from the templates HA consumes.
#[path = "../../dcentaxe/src/mqtt_ha.rs"]
pub mod mqtt_ha;

// Same single-source-of-truth pattern: metrics_render.rs physically lives in the
// `dcentaxe` binary crate (declared `mod metrics_render;` in api.rs, consumed by
// the esp-idf-only `register_prometheus` HTTP handler). It is dependency-free pure
// logic (the host-pure Prometheus `/metrics` body builder + label escaper), so it
// is re-included here only to host-run its `#[cfg(test)]` unit tests under
// `cargo test -p dcentaxe-core`. This pins the exposition contract: every existing
// metric family renders byte-identical, the new per-reason reject + share-freshness
// families are well-formed, and every label value is escaped (no broken lines / no
// worker/URL leak).
#[path = "../../dcentaxe/src/metrics_render.rs"]
pub mod metrics_render;

// Same single-source-of-truth pattern: derived_metrics.rs physically lives in the
// `dcentaxe` binary crate (declared `mod derived_metrics;` in main.rs, consumed by
// the esp-idf-only `register_system_info` HTTP handler). It is dependency-free pure
// math (the `/api/system/info` derivations: acceptance-rate, J/TH efficiency,
// expected hashrate), so it is re-included here only to host-run its `#[cfg(test)]`
// unit tests under `cargo test -p dcentaxe-core`. This pins the data-honesty
// contract: `acceptance_rate_pct` returns None at zero confirmed shares so the
// handler can never re-fabricate a 100.0 accept rate on a freshly-booted miner.
#[path = "../../dcentaxe/src/derived_metrics.rs"]
pub mod derived_metrics;

// Same single-source-of-truth pattern: thermal_safety.rs physically lives in the
// `dcentaxe` binary crate (declared `mod thermal_safety;` in main.rs, consumed by
// the esp-idf-only supervisor loop). It is dependency-free pure logic (the
// temperature fold + sensor-adequacy decision), so it is re-included here only to
// host-run its `#[cfg(test)]` unit tests under `cargo test -p dcentaxe-core`. This
// pins the ES-2 fail-closed contract: a die-equipped board that loses every
// ASIC-die reading while only cooler proxies remain is flagged `die_reading_blind`
// (must escalate to THERMAL-BLIND), while a board with no chip diode by design is
// never false-killed, and the all-None case stays handled by `any_temp_valid`.
#[path = "../../dcentaxe/src/thermal_safety.rs"]
pub mod thermal_safety;

/// XPSAFE-1 / COMP-1 host guards.
///
/// These run under the existing CI line
/// `cargo test -p dcentaxe-core --lib --locked` (no ESP-IDF toolchain needed).
/// The espidf-only panic hook + arming code in `dcentaxe/src/main.rs` cannot be
/// host-built on this target, so these pin (a) the pure decision logic the hook
/// depends on (`dcentaxe_hal::safety`), and (b) structural invariants over the
/// `main.rs` / `shared.rs` / workspace `Cargo.toml` source text.
#[cfg(test)]
mod safety_guards {
    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));
    const SHARED_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/shared.rs"
    ));
    const WORKSPACE_CARGO_TOML: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml"));
    const AUTOTUNER_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/autotuner.rs"
    ));

    // ── XPSAFE-1: pure buck-cut polarity (load-bearing) ──────────────────────
    // Guards against an inversion that would make the panic hook ENERGIZE the
    // rail instead of cutting it.
    #[test]
    fn panic_hook_buck_off_level_cuts_rail_both_polarities() {
        use dcentaxe_hal::safety::{buck_level_high, buck_off_level};
        // active-low board: OFF == drive HIGH(1); active-high: OFF == drive LOW(0).
        assert_eq!(buck_off_level(true), 1);
        assert_eq!(buck_off_level(false), 0);
        // Full (active_low, on) truth table.
        assert!(buck_level_high(true, false));
        assert!(!buck_level_high(true, true));
        assert!(!buck_level_high(false, false));
        assert!(buck_level_high(false, true));
    }

    // ── XPSAFE-1: panic fan duty is full-scale, never reduced ────────────────
    #[test]
    fn panic_fan_duty_is_full_scale() {
        use dcentaxe_hal::safety::{emc2101_panic_duty, fan_safe_panic_duty, pwm_byte_for_pct};
        assert_eq!(pwm_byte_for_pct(0), 0);
        assert_eq!(pwm_byte_for_pct(100), 255);
        // Above 100 clamps to full scale.
        assert_eq!(pwm_byte_for_pct(200), 255);
        assert_eq!(fan_safe_panic_duty(), 255);
        assert_eq!(fan_safe_panic_duty(), pwm_byte_for_pct(100));
        // EMC2101 uses a 6-bit duty; 63 == 100% full scale.
        assert_eq!(emc2101_panic_duty(), 63);
        // The panic duty must exceed any mining-floor (20%) command.
        assert!(fan_safe_panic_duty() > pwm_byte_for_pct(20));
    }

    // ── XPSAFE-1: hook installed BEFORE any rail enable ──────────────────────
    // Substitute for the un-host-runnable main(): assert the install CALL's byte
    // offset precedes the first real `enable_buck(true)` call. We match the call
    // sites, not the banner comments: the install call is the only
    // `install_fail_closed_panic_hook();` (the fn def ends `() {`), and the rail
    // enable is `.enable_buck(true)` (doc comments write `` `enable_buck(true)` ``
    // with no leading dot).
    #[test]
    fn panic_hook_installed_before_buck_enable() {
        let install = MAIN_RS
            .find("install_fail_closed_panic_hook();")
            .expect("main.rs must CALL install_fail_closed_panic_hook() (XPSAFE-1)");
        let enable = MAIN_RS
            .find(".enable_buck(true)")
            .expect("main.rs must call enable_buck(true) to bring up the rail");
        assert!(
            install < enable,
            "panic hook install (byte {install}) must precede the first \
             enable_buck(true) call (byte {enable})"
        );
    }

    // ── COMP-1: no poison-prone `.lock().unwrap()` in shared runtime paths ────
    #[test]
    fn no_lock_unwrap_in_main_or_shared() {
        assert_eq!(
            MAIN_RS.matches(".lock().unwrap()").count(),
            0,
            "main.rs must use .lock().unwrap_or_else(|e| e.into_inner()) — a panic \
             must not poison a Mutex another thread then unwraps (COMP-1)"
        );
        assert_eq!(
            SHARED_RS.matches(".lock().unwrap()").count(),
            0,
            "shared.rs must not use .lock().unwrap() (COMP-1)"
        );
    }

    // ── COMP-1: panic=abort pinned at the workspace root ─────────────────────
    #[test]
    fn release_profile_pins_panic_abort() {
        let profile_idx = WORKSPACE_CARGO_TOML
            .find("[profile.release]")
            .expect("workspace Cargo.toml must declare [profile.release]");
        assert!(
            WORKSPACE_CARGO_TOML[profile_idx..].contains("panic = \"abort\""),
            "[profile.release] must set panic = \"abort\" (COMP-1)"
        );
    }

    // ── XPAUTO-2: chip-health backoff decision logic (host-tested) ───────────
    // The retreat condition lives ONLY in `dcentaxe_hal::safety` so the
    // espidf-only autotuner wiring (which can't host-compile) can never disagree
    // with the host-tested decision. Pin the load-bearing semantics here too.
    #[test]
    fn hw_error_backoff_only_retreats_after_consecutive_debounce() {
        use dcentaxe_hal::safety::{hw_error_backoff_should_retreat, hw_error_streak_next};
        const CEIL: f64 = 0.02; // == MAX_ERROR_RATE in the autotuner
        const REQ: u8 = 3; // the call-site debounce

        // Healthy telemetry never retreats.
        assert!(!hw_error_backoff_should_retreat(0.0, CEIL, 0, REQ));
        assert!(!hw_error_backoff_should_retreat(0.01, CEIL, 9, REQ));
        // A single bad tick is debounced (no instant trip on a share-reject burst).
        assert!(!hw_error_backoff_should_retreat(0.10, CEIL, 0, REQ));
        // Three consecutive bad ticks trip exactly once the streak reaches REQ.
        let mut streak = 0u8;
        let mut tripped_on = None;
        for tick in 1u8..=4 {
            streak = hw_error_streak_next(0.10, CEIL, streak);
            if hw_error_backoff_should_retreat(0.10, CEIL, streak.saturating_sub(1), REQ) {
                tripped_on = Some(tick);
                break;
            }
        }
        assert_eq!(tripped_on, Some(3));
        // Garbage telemetry never retreats and resets the streak (fail-benign).
        assert!(!hw_error_backoff_should_retreat(f64::NAN, CEIL, 9, REQ));
        assert_eq!(hw_error_streak_next(f64::INFINITY, CEIL, 7), 0);
    }

    // ── XPAUTO-2: the autotuner wiring is structurally correct + default-OFF ──
    // autotuner.rs is espidf-only and cannot host-compile, so pin the
    // cross-pollination invariants against the source TEXT: the backoff is
    // gated (default-OFF), it RETREATS to last-known-good (never a higher
    // freq/voltage), and it routes through the same pure decision fn that the
    // tests above exercise. A mis-wire (e.g. a backoff that returns a HIGHER
    // freq, or that loses precedence) would not be caught by host compilation.
    #[test]
    fn autotuner_health_backoff_is_gated_and_retreats_to_last_known_good() {
        // The opt-in gate exists and the new branch reads it (default-OFF flag).
        assert!(
            AUTOTUNER_RS.contains("health_backoff_enabled"),
            "autotuner.rs must gate the XPAUTO-2 backoff behind health_backoff_enabled (default-OFF)"
        );
        // The retreat is computed by the host-tested pure fn, never re-derived.
        assert!(
            AUTOTUNER_RS.contains("hw_error_backoff_should_retreat"),
            "autotuner.rs must call the host-tested hw_error_backoff_should_retreat decision fn"
        );
        assert!(
            AUTOTUNER_RS.contains("hw_error_streak_next"),
            "autotuner.rs must advance the streak via hw_error_streak_next"
        );
        // On retreat it returns the existing last-known-good pair — proven down,
        // never a speculative higher point.
        assert!(
            AUTOTUNER_RS.contains("last_good_frequency")
                && AUTOTUNER_RS.contains("last_good_voltage_mv"),
            "the backoff must retreat to the existing last_good_frequency/last_good_voltage_mv"
        );
        // The retreat counter is part of the autotuner struct state.
        assert!(
            AUTOTUNER_RS.contains("hw_err_over_ceiling_streak"),
            "autotuner.rs must persist the consecutive-over-ceiling streak across ticks"
        );
    }
}

/// XPSAFE-2: structural guards over the TPS546 fault-limit write-protect wiring.
///
/// `power.rs` / `i2c.rs` are espidf-only and cannot be host-built, so (like the
/// XPSAFE-1 `safety_guards`) these pin the cross-pollination invariants against
/// the source TEXT: the guard is default-OFF (a Cargo feature, not in `default`),
/// the protected register set stays in sync with `power::pmbus`, and the
/// arm-before-/latch-after-`configure_limits` ordering is intact so a future edit
/// can't silently strip the guard or block normal voltage control.
#[cfg(test)]
mod tps546_guard_wiring {
    use crate::tps546_guard::{is_protected_register, PROTECTED_REGISTERS, TPS546_ADDR};

    const POWER_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe-hal/src/power.rs"
    ));
    const I2C_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe-hal/src/i2c.rs"
    ));
    const HAL_CARGO_TOML: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe-hal/Cargo.toml"
    ));

    // ── XPSAFE-2: the guard is DEFAULT-OFF (opt-in Cargo feature) ────────────
    #[test]
    fn guard_feature_is_declared_and_not_in_default() {
        assert!(
            HAL_CARGO_TOML.contains("tps546-fault-limit-guard = []"),
            "dcentaxe-hal must declare the `tps546-fault-limit-guard` feature"
        );
        // It must NOT be enabled by default — find the `default = [...]` line and
        // assert the feature name does not appear inside it.
        let default_line = HAL_CARGO_TOML
            .lines()
            .find(|l| l.trim_start().starts_with("default ="))
            .expect("HAL Cargo.toml must declare a `default` feature list");
        assert!(
            !default_line.contains("tps546-fault-limit-guard"),
            "the fault-limit guard must stay DEFAULT-OFF (not in `default`): {default_line}"
        );
    }

    // ── XPSAFE-2: power init arms before, and latches after, configure_limits ─
    #[test]
    fn power_init_arms_before_and_latches_after_configure_limits() {
        let arm = POWER_RS
            .find("arm_tps546_fault_limit_guard()")
            .expect("Tps546::new must ARM the guard (XPSAFE-2)");
        let configure = POWER_RS
            .find("tps.configure_limits(i2c, config)")
            .expect("Tps546::new must call configure_limits");
        let latch = POWER_RS
            .find("latch_tps546_fault_limit_guard()")
            .expect("Tps546::new must LATCH the guard after init (XPSAFE-2)");
        assert!(
            arm < configure,
            "guard must be ARMED (byte {arm}) before configure_limits (byte {configure}) \
             so init's legitimate fault-limit writes still go through"
        );
        assert!(
            configure < latch,
            "guard must be LATCHED (byte {latch}) AFTER configure_limits (byte {configure}) \
             so the protection registers are only locked once init's writes are done"
        );
        // Both wiring calls are feature-gated (default-off).
        assert!(
            POWER_RS.contains("#[cfg(feature = \"tps546-fault-limit-guard\")]"),
            "guard arm/latch calls in power.rs must be feature-gated default-off"
        );
    }

    // ── XPSAFE-2: the i2c write path consults the guard ──────────────────────
    #[test]
    fn i2c_write_path_consults_the_guard() {
        assert!(
            I2C_RS.contains("is_write_blocked(addr, register)"),
            "i2c.rs write() must consult GuardState::is_write_blocked (XPSAFE-2)"
        );
        assert!(
            I2C_RS.contains("fn refuse_tps546_write"),
            "i2c.rs must refuse guarded writes via refuse_tps546_write"
        );
        // The guard state must default to disarmed in the constructor.
        assert!(
            I2C_RS.contains("tps546_guard: GuardState::default()"),
            "I2cBus must initialize the guard to the disarmed default"
        );
    }

    // ── XPSAFE-2: protected set stays in sync with the power::pmbus codes ─────
    // The pure module duplicates the PMBus register codes by value (it is
    // dependency-free on purpose). Pin that every protected code is actually the
    // PMBus command code power.rs declares, so the two can never silently drift.
    #[test]
    fn protected_registers_match_power_pmbus_declarations() {
        // (protected register code, the `pub const NAME: u8 =` it must equal)
        let pairs: &[(u8, &str)] = &[
            (0x40, "VOUT_OV_FAULT_LIMIT: u8 = 0x40"),
            (0x42, "VOUT_OV_WARN_LIMIT: u8 = 0x42"),
            (0x43, "VOUT_UV_WARN_LIMIT: u8 = 0x43"),
            (0x44, "VOUT_UV_FAULT_LIMIT: u8 = 0x44"),
            (0x2B, "VOUT_MIN: u8 = 0x2B"),
            (0x24, "VOUT_MAX: u8 = 0x24"),
            (0x55, "VIN_OV_FAULT_LIMIT: u8 = 0x55"),
            (0x58, "VIN_UV_WARN_LIMIT: u8 = 0x58"),
            (0x35, "VIN_ON: u8 = 0x35"),
            (0x36, "VIN_OFF: u8 = 0x36"),
            (0x46, "IOUT_OC_FAULT_LIMIT: u8 = 0x46"),
            (0x4A, "IOUT_OC_WARN_LIMIT: u8 = 0x4A"),
            (0x4F, "OT_FAULT_LIMIT: u8 = 0x4F"),
            (0x51, "OT_WARN_LIMIT: u8 = 0x51"),
            (0x5F, "VIN_OV_FAULT_RESPONSE: u8 = 0x5F"),
            (0x47, "IOUT_OC_FAULT_RESPONSE: u8 = 0x47"),
            (0x50, "OT_FAULT_RESPONSE: u8 = 0x50"),
        ];
        for (code, decl) in pairs {
            assert!(
                is_protected_register(TPS546_ADDR, *code),
                "0x{code:02x} must be in the protected set"
            );
            assert!(
                POWER_RS.contains(decl),
                "power::pmbus must still declare `{decl}` — protected set drifted from power.rs"
            );
        }
        // Every pair above accounts for exactly the protected set (no orphan code
        // in PROTECTED_REGISTERS that power.rs doesn't back).
        assert_eq!(
            PROTECTED_REGISTERS.len(),
            pairs.len(),
            "PROTECTED_REGISTERS changed — update the power::pmbus cross-check table too"
        );
    }
}

/// SV2-activate: the `sv2_authority_pubkey` config field round-trips through the
/// NVS JSON serializer and is `serde(default)` (legacy blobs load `None`), so a
/// firmware that gains this field can still read an old saved config. Lives in
/// the host-only crate root so the `serde_json` dev-dependency never touches a
/// firmware image.
#[cfg(test)]
mod config_sv2_authority {
    use crate::config::DcentAxeConfig;

    #[test]
    fn default_sv2_authority_pubkey_is_none() {
        let cfg = DcentAxeConfig::default();
        assert!(cfg.sv2_authority_pubkey.is_none());
    }

    #[test]
    fn sv2_authority_pubkey_round_trips_none_and_some() {
        // None survives a full serialize → deserialize cycle.
        let mut cfg = DcentAxeConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: DcentAxeConfig = serde_json::from_str(&json).unwrap();
        assert!(back.sv2_authority_pubkey.is_none());

        // Some(token) survives too.
        cfg.sv2_authority_pubkey = Some("some-base58-token".to_string());
        let json = serde_json::to_string(&cfg).unwrap();
        let back: DcentAxeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.sv2_authority_pubkey.as_deref(),
            Some("some-base58-token")
        );
    }

    #[test]
    fn legacy_config_without_field_loads_none() {
        // A pre-feature config blob (no sv2_authority_pubkey key) must still load
        // via #[serde(default)], yielding None — no schema bump, no load failure.
        let full = DcentAxeConfig::default();
        let mut value: serde_json::Value = serde_json::to_value(&full).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("sv2_authority_pubkey");
        let legacy_json = serde_json::to_string(&value).unwrap();
        assert!(
            !legacy_json.contains("sv2_authority_pubkey"),
            "legacy blob must omit the field for this test to be meaningful"
        );
        let loaded: DcentAxeConfig = serde_json::from_str(&legacy_json).unwrap();
        assert!(loaded.sv2_authority_pubkey.is_none());
    }
}

/// MQTT/HA config: default-OFF, safe defaults, and legacy NVS blobs (no `mqtt`
/// key) round-trip via `#[serde(default)]` — so a unit that gains the MQTT
/// feature still reads an old saved config. Same host-only harness as
/// `config_sv2_authority`.
#[cfg(test)]
mod config_mqtt {
    use crate::config::{DcentAxeConfig, MqttConfig};

    #[test]
    fn mqtt_default_is_off_with_safe_defaults() {
        let cfg = DcentAxeConfig::default();
        assert!(!cfg.mqtt.enabled, "MQTT must default OFF");
        assert!(cfg.mqtt.broker_host.is_empty());
        assert_eq!(cfg.mqtt.broker_port, 1883);
        assert!(cfg.mqtt.username.is_empty());
        assert!(cfg.mqtt.password.is_empty());
        assert!(!cfg.mqtt.tls);
        assert_eq!(cfg.mqtt.publish_interval_s, 30);
        // The operator-CONTROL surface must default OFF (no remote write surface
        // unless explicitly enabled).
        assert!(
            !cfg.mqtt.commands_enabled,
            "MQTT command entities must default OFF"
        );
    }

    #[test]
    fn mqtt_config_round_trips() {
        let mut cfg = DcentAxeConfig::default();
        cfg.mqtt = MqttConfig {
            enabled: true,
            broker_host: "203.0.113.10".to_string(),
            broker_port: 1883,
            username: "ha".to_string(),
            password: "secret".to_string(),
            tls: false,
            publish_interval_s: 15,
            commands_enabled: true,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: DcentAxeConfig = serde_json::from_str(&json).unwrap();
        assert!(back.mqtt.enabled);
        assert_eq!(back.mqtt.broker_host, "203.0.113.10");
        assert_eq!(back.mqtt.username, "ha");
        assert_eq!(back.mqtt.password, "secret");
        assert_eq!(back.mqtt.publish_interval_s, 15);
        assert!(
            back.mqtt.commands_enabled,
            "commands_enabled must round-trip"
        );
    }

    #[test]
    fn legacy_config_without_mqtt_loads_default() {
        // A pre-feature blob (no `mqtt` key) must still load via #[serde(default)].
        let full = DcentAxeConfig::default();
        let mut value: serde_json::Value = serde_json::to_value(&full).unwrap();
        value.as_object_mut().unwrap().remove("mqtt");
        let legacy_json = serde_json::to_string(&value).unwrap();
        assert!(
            !legacy_json.contains("\"mqtt\""),
            "legacy blob must omit the field for this test to be meaningful"
        );
        let loaded: DcentAxeConfig = serde_json::from_str(&legacy_json).unwrap();
        assert!(!loaded.mqtt.enabled, "missing mqtt key must default OFF");
        assert_eq!(loaded.mqtt.broker_port, 1883);
    }

    #[test]
    fn legacy_mqtt_blob_without_commands_enabled_loads_off() {
        // A blob that predates the command surface (has `mqtt` but no
        // `commands_enabled` key) must load with the control surface OFF.
        let full = DcentAxeConfig::default();
        let mut value: serde_json::Value = serde_json::to_value(&full).unwrap();
        value
            .get_mut("mqtt")
            .and_then(|m| m.as_object_mut())
            .unwrap()
            .remove("commands_enabled");
        let legacy_json = serde_json::to_string(&value).unwrap();
        let loaded: DcentAxeConfig = serde_json::from_str(&legacy_json).unwrap();
        assert!(
            !loaded.mqtt.commands_enabled,
            "a legacy mqtt blob without commands_enabled must default OFF"
        );
    }
}

/// MQTT/HA command-surface clamp contract: the HA command envelope constants in
/// `mqtt_ha` MUST stay equal to the `chip_profiles_bitaxe::validate_autotune_target`
/// limits the local autotuner enforces, so the advertised HA slider/box range can
/// never drift outside the validated safety envelope. Both modules are re-included
/// in this host crate, so this is a real compile-linked equality (not a text pin).
#[cfg(test)]
mod mqtt_command_clamp_contract {
    use crate::chip_profiles_bitaxe::{
        MAX_AUTOTUNE_TARGET_TEMP_C, MAX_AUTOTUNE_TARGET_WATTS, MIN_AUTOTUNE_TARGET_TEMP_C,
    };
    use crate::mqtt_ha::{
        parse_autotune_mode, AUTOTUNE_MODES, CMD_TARGET_TEMP_MAX_C, CMD_TARGET_TEMP_MIN_C,
        CMD_TARGET_WATTS_MAX,
    };

    #[test]
    fn ha_command_envelope_equals_validator_envelope() {
        // Target-watts ceiling advertised to HA == the validator's board budget.
        assert_eq!(
            CMD_TARGET_WATTS_MAX, MAX_AUTOTUNE_TARGET_WATTS,
            "HA target-watts ceiling must equal validate_autotune_target's max"
        );
        // Target-temp band advertised to HA == the validator's accepted band.
        assert_eq!(CMD_TARGET_TEMP_MIN_C, MIN_AUTOTUNE_TARGET_TEMP_C);
        assert_eq!(CMD_TARGET_TEMP_MAX_C, MAX_AUTOTUNE_TARGET_TEMP_C);
    }

    #[test]
    fn ha_autotune_modes_match_shared_from_api_str() {
        // shared::AutotuneMode is esp-idf-gated and not host-compiled here, so pin
        // the 4 canonical strings against shared.rs source text (the single other
        // source of these strings) + prove parse_autotune_mode round-trips them.
        const SHARED_RS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dcentaxe/src/shared.rs"
        ));
        for m in AUTOTUNE_MODES {
            assert_eq!(parse_autotune_mode(m), Some(m), "mode must round-trip");
            assert!(
                SHARED_RS.contains(&format!("=> \"{m}\"")),
                "shared::AutotuneMode::as_api_str must still emit \"{m}\""
            );
        }
    }
}

/// WF-F main-api lane structural guards.
///
/// `main.rs` / `api.rs` live in the `dcentaxe` BINARY crate and cannot
/// host-compile (esp-idf at module scope), so — exactly like `safety_guards` and
/// `tps546_guard_wiring` — these pin the behavioral invariants of each fix against
/// the source TEXT via `include_str!`. The pure DECISION logic each fix consumes
/// (`power_field_available`, `tach_proof_required`, the fan floor) is already
/// host-tested in `dcentaxe-hal::safety` / `board.rs`; these only pin the
/// main.rs/api.rs WIRING that a host compile cannot reach.
/// Char-boundary-safe slice from `start` for up to `len` bytes (source contains
/// multi-byte chars, so a naive `&s[start..start+len]` can split a UTF-8 code
/// point). Returns the substring from `start` to the largest char boundary at or
/// before `start + len`.
#[cfg(test)]
fn safe_window(s: &str, start: usize, len: usize) -> &str {
    let mut end = (start + len).min(s.len());
    while end > start && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[start..end]
}

#[cfg(test)]
mod thermal_ladder_guards {
    use super::safe_window;

    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));

    fn byte_of(hay: &str, needle: &str) -> usize {
        hay.find(needle)
            .unwrap_or_else(|| panic!("main.rs must contain `{needle}`"))
    }

    // ── XPSAFE-5: cut-hash-before-fan-noise ordering at the WARNING tier ──────
    #[test]
    fn warning_tier_sheds_hash_before_raising_fan() {
        // The WARNING tier (>90 C) must send the hash-shed frequency BEFORE the
        // fan bump (THERMAL_WARN_FAN_CAP_PCT). Find the WARNING comment anchor and
        // assert the freq send precedes the set_speed inside that region.
        let region_start = byte_of(MAIN_RS, "XPSAFE-5 WARNING tier");
        let region = &MAIN_RS[region_start..];
        let freq_send = region
            .find("freq_cmd_tx.send(warn_freq)")
            .expect("WARNING tier must shed hash via freq_cmd_tx.send(warn_freq)");
        let fan_bump = region
            .find("fan_ctrl.set_speed(THERMAL_WARN_FAN_CAP_PCT)")
            .expect("WARNING tier must raise fan to THERMAL_WARN_FAN_CAP_PCT");
        assert!(
            freq_send < fan_bump,
            "XPSAFE-5: WARNING tier must cut hash (byte {freq_send}) BEFORE raising the fan (byte {fan_bump})"
        );
    }

    // ── XPSAFE-5: 95 C tier reduces frequency before pinning fan to 100% ──────
    #[test]
    fn ninety_five_tier_throttles_freq_before_fan_100() {
        let region_start = byte_of(MAIN_RS, "max_temp > 95.0");
        let region = &MAIN_RS[region_start..];
        let freq_send = region
            .find("freq_cmd_tx.send(throttled_freq)")
            .expect("95C tier must throttle via freq_cmd_tx.send(throttled_freq)");
        let fan_100 = region
            .find("fan_ctrl.set_speed(100)")
            .expect("95C tier must still pin fan to 100%");
        assert!(
            freq_send < fan_100,
            "XPSAFE-5: 95C tier must reduce frequency (byte {freq_send}) BEFORE fan=100% (byte {fan_100})"
        );
    }

    // ── XPSAFE-5: the home-cap consts exist and the cap is < 100 ──────────────
    #[test]
    fn warn_fan_cap_is_below_full_scale() {
        assert!(MAIN_RS.contains("const THERMAL_WARN_FREQ_SHED_MHZ: f32 = 50.0;"));
        assert!(MAIN_RS.contains("const THERMAL_WARN_FAN_CAP_PCT: u8 = 70;"));
        // 70 < 100 — the WARNING tier is home-capped, not full blast.
        assert!(
            MAIN_RS.contains("THERMAL_WARN_FAN_CAP_PCT: u8 = 70"),
            "the WARNING-tier fan cap must stay below 100 (home/quiet posture)"
        );
    }

    // ── XPSAFE-5: EMERGENCY tier still cuts hash AND power together ────────────
    #[test]
    fn emergency_tier_still_cuts_hash_and_buck() {
        let region_start = byte_of(MAIN_RS, "max_temp > EMERGENCY_TEMP_C");
        let region = safe_window(MAIN_RS, region_start, 2000);
        assert!(
            region.contains("mining_kill.store(true"),
            "EMERGENCY tier must still set mining_kill"
        );
        assert!(
            region.contains("enable_buck(false)"),
            "EMERGENCY tier must still cut the buck rail"
        );
    }

    // ── XPSAFE-5: thresholds are NEVER lowered (regression guard) ─────────────
    #[test]
    fn thermal_thresholds_unchanged() {
        assert!(MAIN_RS.contains("const EMERGENCY_TEMP_C: f32 = 105.0;"));
        assert!(MAIN_RS.contains("const WARNING_TEMP_C: f32 = 90.0;"));
        assert!(
            MAIN_RS.contains("max_temp > 95.0"),
            "the 95C throttle tier must remain"
        );
    }

    // ── HALT-10: always-on proportional curve, gated + floor-respecting ───────
    #[test]
    fn prop_fan_curve_is_gated_and_respects_floor() {
        assert!(MAIN_RS.contains("const PROP_FAN_LOW_TEMP_C: f32 = 55.0;"));
        assert!(MAIN_RS.contains("const PROP_FAN_HIGH_TEMP_C: f32 = 85.0;"));
        assert!(MAIN_RS.contains("const PROP_FAN_MIN_PCT: u8 = 30;"));
        assert!(MAIN_RS.contains("const PROP_FAN_MAX_PCT: u8 = 70;"));
        // cap < 100 and floor >= 20.
        assert!(MAIN_RS.contains("PROP_FAN_MAX_PCT: u8 = 70"));
        assert!(MAIN_RS.contains("PROP_FAN_MIN_PCT: u8 = 30"));
        // The branch is gated live_fan_target == 0 and max_temp <= WARNING
        // (backstops kept). Anchor on the runtime branch's floor comment, which is
        // unique to the executable curve (the const block above uses a different
        // wording), so we inspect the real branch body and not the doc block.
        let region_start = byte_of(MAIN_RS, "Respect the 20% mining floor");
        let region = safe_window(MAIN_RS, region_start, 600);
        assert!(
            region.contains("prop_pct.max(20)"),
            "HALT-10 curve must clamp to the 20% mining floor"
        );
        assert!(
            region.contains("> fan_ctrl.current_speed()"),
            "HALT-10 curve must only RAISE the fan, never reduce a higher manual command"
        );
        // And the branch is gated on live_fan_target == 0 + max_temp <= WARNING.
        assert!(
            MAIN_RS.contains("} else if live_fan_target == 0"),
            "HALT-10 curve must be gated on live_fan_target == 0 (manual/unset)"
        );
    }
}

#[cfg(test)]
mod fan_tach_guards {
    use super::safe_window;

    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));

    // ── HALT-6 / XPSAFE-7: runtime EMC2101 stall gate consumes tach_proof_required ──
    #[test]
    fn runtime_stall_gate_uses_tach_proof_required() {
        assert!(
            MAIN_RS.contains("board_config.tach_proof_required() || fan1_ever_seen"),
            "the EMC2101 runtime stall gate must OR-gate on tach_proof_required() so an \
             opted-in board fails closed on a never-spinning fan (HALT-6/XPSAFE-7)"
        );
    }

    // ── HALT-6 / XPSAFE-7: EMC2101 boot path adds a tach_proof_required-gated proof ──
    #[test]
    fn emc2101_boot_tach_proof_is_present_and_gated() {
        let start = MAIN_RS
            .find("boot-time tach proof for opted-in EMC2101")
            .expect("EMC2101 boot path must add a tach proof (HALT-6/XPSAFE-7)");
        let region = safe_window(MAIN_RS, start, 1400);
        assert!(
            region.contains("board_config.tach_proof_required()"),
            "boot tach proof must be gated on tach_proof_required()"
        );
        assert!(
            region.contains("read_fan_rpm"),
            "boot tach proof must read RPM via read_fan_rpm"
        );
        assert!(
            region.contains("mining_permitted = false"),
            "a never-spinning opted-in fan must set mining_permitted = false at boot"
        );
    }

    // ── HALT-6 / XPSAFE-7: heuristic-only telemetry is surfaced ───────────────
    #[test]
    fn heuristic_only_fan_proof_is_surfaced() {
        assert!(
            MAIN_RS.contains("no fan proof (heuristic only)"),
            "tachless boards must surface a 'no fan proof (heuristic only)' note"
        );
        assert!(
            MAIN_RS
                .contains("telem.fan_proof_heuristic_only = !board_config.tach_proof_required()"),
            "telemetry must carry fan_proof_heuristic_only derived from tach_proof_required()"
        );
    }
}

#[cfg(test)]
mod mainapi_guards {
    use super::safe_window;

    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));
    const API_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/api.rs"
    ));
    const SHARED_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/shared.rs"
    ));

    // ── XPSAFE-4: lab bypass is compile-time only (no dead std::env arm) ───────
    #[test]
    fn unsafe_lab_bypass_is_compile_time_only() {
        let start = MAIN_RS
            .find("fn unsafe_lab_safety_bypass_enabled")
            .expect("main.rs must define unsafe_lab_safety_bypass_enabled");
        // The fn body ends at the next `}` line after the option_env! check.
        let body = safe_window(MAIN_RS, start, 700);
        assert!(
            body.contains("option_env!(\"DCENTAXE_UNSAFE_LAB_SAFETY_BYPASS\")"),
            "bypass must still read the compile-time env var"
        );
        assert!(
            !body.contains("std::env::var"),
            "XPSAFE-4: the dead runtime std::env::var arm must be gone"
        );
        // Read once at boot + loud warn.
        assert!(
            MAIN_RS.contains("let unsafe_lab_safety_bypass = unsafe_lab_safety_bypass_enabled();")
        );
        assert!(MAIN_RS.contains("UNSAFE LAB SAFETY BYPASS ENABLED"));
    }

    // ── HALPWR-2-wire / COMP-6-wire: per-field NaN guard on power telemetry ───
    #[test]
    fn power_telemetry_is_nan_guarded_per_field() {
        assert!(
            MAIN_RS.contains("if power_field_available(p.power_w)"),
            "telem.power_w must be guarded with power_field_available before assignment"
        );
        // All four power fields are independently guarded.
        for field in ["voltage_mv", "current_ma", "power_w", "input_voltage_mv"] {
            let guard = format!("power_field_available(p.{field})");
            assert!(
                MAIN_RS.contains(&guard),
                "power.{field} must be guarded by {guard} (one blanked field must not blank the others)"
            );
        }
    }

    // ── MAINAPI-1: per-chip loop is bounded (no unchecked index) ──────────────
    #[test]
    fn per_chip_loop_is_bounded() {
        assert!(
            !MAIN_RS.contains("stats.per_chip[i as usize]"),
            "MAINAPI-1: the per-chip telemetry loop must not index per_chip[i as usize] (panic risk)"
        );
        assert!(
            MAIN_RS.contains(".min(dcentaxe_mining::stats::MAX_CHIPS)"),
            "MAINAPI-1: the loop must clamp the chip count to MAX_CHIPS"
        );
        // Belt-and-suspenders: the per_chip read goes through .get(i) (whitespace
        // between `per_chip` and `.get(i)` is collapsed for a robust match).
        let collapsed: String = MAIN_RS.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            collapsed.contains("per_chip .get(i)") || collapsed.contains("per_chip.get(i)"),
            "MAINAPI-1: the loop must use per_chip.get(i) as a degrade"
        );
    }

    // ── HALPWR-8: power-init recovery probe is board-appropriate ──────────────
    #[test]
    fn power_init_recovery_probe_is_board_aware() {
        assert!(
            !MAIN_RS.contains("let _ = i2c.probe(0x24);"),
            "HALPWR-8: the power-init recovery poke must not hardcode i2c.probe(0x24)"
        );
        assert!(
            MAIN_RS.contains("TPS546_ADDR") && MAIN_RS.contains("DS4432U_ADDR"),
            "HALPWR-8: recovery probe must branch on the board regulator address"
        );
        assert!(
            MAIN_RS.contains("board_config.power_controller"),
            "HALPWR-8: recovery probe must branch on board_config.power_controller"
        );
    }

    // ── MAINAPI-5: mining_kill is one-way at runtime (reboot-only recovery) ────
    #[test]
    fn mining_kill_is_one_way_at_runtime() {
        assert_eq!(
            MAIN_RS.matches("mining_kill.store(false").count(),
            0,
            "MAINAPI-5: nothing in the runtime loop may clear mining_kill — recovery is reboot-only"
        );
        let start = MAIN_RS
            .find("Mining resume requested by user")
            .expect("resume branch must exist");
        let region = safe_window(MAIN_RS, start, 200);
        assert!(
            region.contains("esp_restart"),
            "MAINAPI-5: the API resume branch must reboot via esp_restart"
        );
    }

    // ── XPH-4 / MAINAPI-7: named handler cap + compile-time headroom guard ─────
    #[test]
    fn uri_handler_cap_is_named_and_asserted() {
        assert!(MAIN_RS.contains("const MAX_URI_HANDLERS: usize = 96;"));
        assert!(MAIN_RS.contains("const REGISTERED_HANDLER_ESTIMATE: usize ="));
        assert!(
            MAIN_RS
                .contains("const _: () = assert!(REGISTERED_HANDLER_ESTIMATE < MAX_URI_HANDLERS);"),
            "XPH-4: a compile-time floor guard must pin the estimate under the cap"
        );
        assert!(
            MAIN_RS.contains("max_uri_handlers: MAX_URI_HANDLERS,"),
            "HttpConfig must use the named const, not a bare 96"
        );
    }

    // ── XPH-4: the registered-handler estimate matches reality (< cap) ────────
    #[test]
    fn registered_handler_count_under_cap() {
        let api = API_RS.matches(".fn_handler(").count();
        // dashboard/auth/mcp live in other source files; pin the estimate const
        // against the api.rs count we can see here plus the documented constant
        // contributions (auth 5 + mcp 2 + dashboard 12).
        let estimate = api + 5 + 2 + 12;
        assert!(
            estimate < 96,
            "registered handler estimate {estimate} must stay under the 96 cap"
        );
        // The const in main.rs must be >= the true api.rs contribution.
        assert!(
            MAIN_RS.contains("const REGISTERED_HANDLER_ESTIMATE: usize = 73;"),
            "REGISTERED_HANDLER_ESTIMATE should reflect the verified total (73)"
        );
    }

    // ── MAINAPI-6: the dead asic_model_name duplicate is gone ─────────────────
    #[test]
    fn dead_asic_model_name_removed() {
        assert!(
            !API_RS.contains("fn asic_model_name"),
            "MAINAPI-6: the dead board->ASIC duplicate fn asic_model_name must be removed"
        );
    }

    // ── MAINAPI-3: every JSON handler accumulates the full body ───────────────
    #[test]
    fn no_single_read_truncation_in_api() {
        assert_eq!(
            API_RS.matches("req.read(&mut body).unwrap_or(0)").count(),
            0,
            "MAINAPI-3: no handler may single-read a body (truncation on multi-segment POST)"
        );
        // read_full_body fn def + >= 8 call sites = >= 9 occurrences.
        assert!(
            API_RS.matches("read_full_body(").count() >= 9,
            "MAINAPI-3: read_full_body must back at least 8 body-reading handlers"
        );
    }

    // ── MAINAPI-4: explicit target_temp=0 is rejected, manual selectors kept ──
    #[test]
    fn ambiguous_zero_target_temp_is_rejected() {
        assert!(
            API_RS.contains("target_temp=0 is ambiguous"),
            "MAINAPI-4: an explicit target_temp=0 must be rejected with a clear 400 message"
        );
        // The unambiguous manual selectors still set fan_target_temp_c = 0.
        assert!(API_RS.contains("\"manual\" => config.fan_target_temp_c = 0"));
        assert!(API_RS.contains("config.fan_target_temp_c = 0; // manual mode"));
        // apply_config_updates now returns a Result so the 400 can propagate.
        assert!(API_RS.contains(
            "fn apply_config_updates(state: &SharedState, body: &[u8]) -> Result<(), String>"
        ));
    }

    // ── AOTA-6: unsigned OTA surfaces + optionally pins the payload SHA-256 ────
    #[test]
    fn unsigned_ota_surfaces_and_pins_sha() {
        assert!(
            API_RS.contains("X-DCENT-Unsigned-SHA256"),
            "AOTA-6: an optional out-of-band unsigned-SHA pin header must be read"
        );
        assert!(
            API_RS.contains("Unsigned OTA SHA256 mismatch"),
            "AOTA-6: an unsigned-SHA mismatch must abort the OTA"
        );
        // mismatch routes through esp_ota_abort.
        let m = API_RS
            .find("Unsigned OTA SHA256 mismatch")
            .expect("mismatch message present");
        let mut win_start = m.saturating_sub(400);
        while win_start < m && !API_RS.is_char_boundary(win_start) {
            win_start += 1;
        }
        let region = &API_RS[win_start..m];
        assert!(
            region.contains("esp_ota_abort"),
            "AOTA-6: unsigned-SHA mismatch must call esp_ota_abort"
        );
        assert!(
            API_RS.contains("\"payloadSha256\": payload_sha256"),
            "AOTA-6: the success JSON must surface payloadSha256"
        );
        assert!(
            API_RS.contains("OTA accepted in UNSIGNED mode"),
            "AOTA-6: unsigned mode must emit a loud audit warning"
        );
    }

    // ── SV2-3: V2 arm threads a fallback + defines a failover threshold ───────
    #[test]
    fn sv2_fallback_failover_is_wired() {
        assert!(
            MAIN_RS.contains("const SV2_FAILOVER_AFTER_N: u32 = 5;"),
            "SV2-3: a named failover threshold must exist"
        );
        // The V2 arm passes a fallback into run_sv2_client_thread (no longer warn-and-drop).
        // Match the V2 arm in spawn_stratum_thread specifically (the one whose
        // body runs run_sv2_client_thread), not the earlier protocol == V2 checks.
        let arm = MAIN_RS
            .find("run_sv2_client_thread(\n                    config,")
            .expect("spawn_stratum_thread V2 arm must call run_sv2_client_thread");
        let region = safe_window(MAIN_RS, arm, 400);
        assert!(
            region.contains("fallback_pool,"),
            "SV2-3: the V2 arm must thread fallback_pool into run_sv2_client_thread"
        );
        assert!(
            MAIN_RS.contains("fallback_pool: Option<dcentaxe_stratum::StratumConfig>,"),
            "SV2-3: run_sv2_client_thread must accept a fallback_pool param"
        );
        // The failover branch references both the threshold and the fallback.
        assert!(
            MAIN_RS.contains("sv2_consecutive_failures >= SV2_FAILOVER_AFTER_N"),
            "SV2-3: failover must trigger after SV2_FAILOVER_AFTER_N failures"
        );
        assert!(
            MAIN_RS.contains("sv2 failover to fallback"),
            "SV2-3: the failover must surface a status string"
        );
    }

    // ESP-5: SV2 is not live-proven; every API surface that advertises it must
    // also expose the experimental maturity flag.
    #[test]
    fn sv2_experimental_maturity_is_surfaced() {
        assert!(
            API_RS.contains("\"stratumV2Available\": cfg!(feature = \"stratum-v2\")"),
            "/api/system must keep the build-time SV2 availability flag"
        );
        assert!(
            API_RS.contains("\"stratumV2Experimental\": true"),
            "ESP-5: /api/system must surface that SV2 remains experimental"
        );
        assert!(
            API_RS.contains("stratum_v2_experimental: true"),
            "ESP-5: /api/system/info must populate stratumV2Experimental"
        );
    }

    // ── HALT-5: emc_internal_temp boards label chip_temp as ambient proxy ─────
    #[test]
    fn emc_internal_temp_labeled_as_ambient_proxy() {
        assert!(
            MAIN_RS.contains("telem.chip_temp_is_ambient_proxy = board_config.emc_internal_temp"),
            "HALT-5: chip_temp must be labeled a board-ambient proxy on emc_internal_temp boards"
        );
        assert!(
            SHARED_RS.contains("pub chip_temp_is_ambient_proxy: bool"),
            "HALT-5: telemetry must carry the ambient-proxy label field"
        );
        // No threshold was lowered (regression guard).
        assert!(MAIN_RS.contains("const EMERGENCY_TEMP_C: f32 = 105.0;"));
        assert!(MAIN_RS.contains("const WARNING_TEMP_C: f32 = 90.0;"));
    }

    // ── MAINAPI-8: the canonical lock order is documented in shared.rs ─────────
    #[test]
    fn canonical_lock_order_is_documented() {
        assert!(
            SHARED_RS.contains("CANONICAL LOCK ORDER"),
            "MAINAPI-8: shared.rs must document the canonical lock acquisition order"
        );
    }
}

/// DCENT design-language (Phase-2 axe contract) structural guards.
///
/// `dashboard.rs` lives in the `dcentaxe` BINARY crate and embeds the served
/// dashboard as one giant inline HTML/CSS/JS string — it pulls in esp-idf at
/// module scope and CANNOT host-compile, so (exactly like `safety_guards`,
/// `tps546_guard_wiring`, and `mainapi_guards`) these pin the Phase-2 axe
/// design-language CONTRACT against the source TEXT via `include_str!`. This is
/// the host-testable verification + durability mechanism for the
/// un-host-compilable `dashboard.rs`: it is the only place CI can prove the
/// accent / autotuner-mode / truth-ladder / token contract has not regressed.
///
/// Assertions are deliberately whitespace-robust: HTML/CSS/JS minification or a
/// reformat must not flip a guard. Multi-token display strings are matched after
/// collapsing internal whitespace; single-token swatches are matched
/// case-insensitively where the contract is color-value (not label) oriented.
#[cfg(test)]
mod dcent_design_language_guards {
    const DASHBOARD_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard.rs"
    ));
    const TOKENS_CSS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/tokens.css"
    ));
    // The TERM-7 glossary string-table is hosted inline in framework.js (the
    // already-served, handler-free <script src> #1). Pin it from source TEXT —
    // framework.js, like dashboard.rs, embeds in the esp-idf binary crate and
    // cannot host-compile, so this include_str! is the only place CI proves the
    // glossary-equivalent + its canonical spellings + handler-free host have not
    // regressed.
    const FRAMEWORK_JS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/framework.js"
    ));
    // The served (register_static'd + <script src>'d) label-bearing components
    // that the emission wired to pull labels from window.GLOSSARY. stats.js is
    // intentionally EXCLUDED — it is an orphan (not served), so its dormant
    // glossary edit is source-consistency only and is not a contract surface.
    const CORE_JS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/core.js"
    ));
    const ASIC_CHIPS_JS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/asic-chips.js"
    ));
    const BLOCK_TILE_JS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/block-tile.js"
    ));

    /// Collapse every run of ASCII whitespace to a single space so a reflow /
    /// minify of the embedded HTML/CSS/JS cannot break a multi-token match.
    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── (1) accent = DCENT amber #FAA500; #F7931A is opt-in ONLY ──────────────
    // The DCENT primary accent is amber #FAA500. Bitcoin-orange #F7931A is the
    // OPT-IN `--orange-bitcoin` alias / theme-picker swatch ONLY; it must never
    // be the DEFAULT value of `--accent` / `--orange`. A regression that re-set
    // the inline accent slots to #F7931A would not be caught by host compilation.
    #[test]
    fn accent_is_dcent_amber_not_bitcoin_orange_default() {
        let flat = collapse_ws(DASHBOARD_RS);
        // The amber accent is present (case-insensitive: the hex may be authored
        // upper- or lower-case; the contract is the color value, not its casing).
        assert!(
            flat.to_ascii_uppercase().contains("#FAA500"),
            "dashboard.rs must define the DCENT amber accent #FAA500"
        );
        // The inline `--accent` and `--orange` slots default to amber, NOT to
        // Bitcoin-orange. Match the assignment with collapsed whitespace so a
        // reflow of the `:root{...}` block can't dodge the guard.
        let flat_up = flat.to_ascii_uppercase();
        assert!(
            flat_up.contains("--ACCENT:#FAA500") || flat_up.contains("--ACCENT: #FAA500"),
            "the inline --accent slot must default to amber #FAA500"
        );
        assert!(
            flat_up.contains("--ORANGE:#FAA500") || flat_up.contains("--ORANGE: #FAA500"),
            "the inline --orange slot must default to amber #FAA500"
        );
        assert!(
            !flat_up.contains("--ACCENT:#F7931A") && !flat_up.contains("--ACCENT: #F7931A"),
            "regression: the default --accent must NOT be Bitcoin-orange #F7931A"
        );
        assert!(
            !flat_up.contains("--ORANGE:#F7931A") && !flat_up.contains("--ORANGE: #F7931A"),
            "regression: the default --orange must NOT be Bitcoin-orange #F7931A"
        );
        // Every #F7931A occurrence must be the OPT-IN path: either the
        // `--orange-bitcoin` alias or a theme-picker swatch wired to setAccent().
        // Pin that the only legitimate alias exists and that #F7931A is never the
        // accent default (above). We count occurrences case-insensitively and
        // assert each is opt-in by requiring the alias declaration to be present.
        assert!(
            flat_up.contains("--ORANGE-BITCOIN:#F7931A")
                || flat_up.contains("--ORANGE-BITCOIN: #F7931A"),
            "#F7931A must survive only as the opt-in --orange-bitcoin alias"
        );
    }

    // ── (2) the 4 canonical autotuner mode display names are present ──────────
    // The autotuner exposes exactly four user-selectable modes. Their DISPLAY
    // names (the <option> labels) are the contract surface; a rename to a
    // non-canonical label would silently break parity with the OS lane.
    #[test]
    fn four_canonical_autotuner_mode_names_present() {
        let flat = collapse_ws(DASHBOARD_RS);
        for name in [
            "Max Hashrate",
            "Best Efficiency",
            "Target Watts",
            "Target Temp",
        ] {
            assert!(
                flat.contains(name),
                "autotuner mode display name `{name}` missing from dashboard.rs"
            );
        }
        // And the JS mode-description map keys (the machine values behind those
        // labels) stay in lock-step so the labels are not orphaned.
        for key in [
            "max_hashrate",
            "best_efficiency",
            "target_watts",
            "target_temp",
        ] {
            assert!(
                flat.contains(key),
                "autotuner mode value `{key}` missing — display label/value pair drifted"
            );
        }
    }

    // ── (3) truth-ladder: Ready / Standby present, overloaded `Enabled` gone ──
    // The mining state truth-ladder rung-2 was renamed from the OVERLOADED
    // `Enabled` (which RALPH Wave 9D9 used for "permitted but zero hashrate") to
    // the unambiguous `Ready` (mining permitted, awaiting hashrate) /
    // `Standby` (mining disabled) split. Pin that the new state-labels exist and
    // that the old overloaded DISPLAY label is gone. NOTE: we target the
    // displayed state-LABEL strings, not the legacy `data-state="enabled"` CSS
    // class alias (which is a styling hook, not a label) and not the unrelated
    // `Enable Autotuner` checkbox text.
    #[test]
    fn truth_ladder_ready_standby_present_old_enabled_label_gone() {
        let flat = collapse_ws(DASHBOARD_RS);
        // Both casings of the new rung-2 labels appear (hero badge `Ready`/`Standby`
        // + topbar/log pill `READY`/`STANDBY`).
        assert!(
            flat.contains("'Ready'") || flat.contains("\"Ready\"") || flat.contains(">Ready<"),
            "truth-ladder must surface the `Ready` state-label"
        );
        assert!(
            flat.contains("'READY'") || flat.contains("\"READY\"") || flat.contains(">READY<"),
            "truth-ladder must surface the uppercase `READY` pill label"
        );
        assert!(
            flat.contains("'Standby'")
                || flat.contains("\"Standby\"")
                || flat.contains(">Standby<"),
            "truth-ladder must surface the `Standby` state-label"
        );
        assert!(
            flat.contains("'STANDBY'")
                || flat.contains("\"STANDBY\"")
                || flat.contains(">STANDBY<"),
            "truth-ladder must surface the uppercase `STANDBY` pill label"
        );
        // The OLD overloaded rung-2 DISPLAY label must be gone. The state-label is
        // emitted as a quoted JS string literal (e.g. `'ENABLED'`) or rendered
        // text (`>Enabled<`); assert none of those forms survive. The legacy
        // `data-state="enabled"` CSS selector (a class alias) and the
        // `Enable Autotuner` checkbox are intentionally NOT matched by these forms.
        for forbidden in [
            "'ENABLED'",
            "\"ENABLED\"",
            ">ENABLED<",
            "'Enabled'",
            "\"Enabled\"",
            ">Enabled<",
        ] {
            assert!(
                !flat.contains(forbidden),
                "the old overloaded rung-2 state-label `{forbidden}` must be gone (use Ready/Standby)"
            );
        }
    }

    // ── (4) tokens.css: canonical accent + void surface ───────────────────────
    // The design-token source-of-truth must declare the canonical `--accent`
    // amber and the `--bg-void` deepest surface. Robust to whitespace around the
    // `:` and to the alias `var(--s-void)` form (the void value lives on
    // `--s-void: #070710`, with `--bg-void` aliasing it).
    #[test]
    fn tokens_css_has_canonical_accent_and_void() {
        let flat = collapse_ws(TOKENS_CSS);
        let flat_up = flat.to_ascii_uppercase();
        // --accent: #FAA500  (whitespace-robust, case-insensitive on the hex).
        assert!(
            flat_up.contains("--ACCENT:#FAA500") || flat_up.contains("--ACCENT: #FAA500"),
            "tokens.css must declare --accent #FAA500"
        );
        // --bg-void must exist and resolve to #070710 — either declared directly
        // or aliased onto --s-void (whose value is #070710). Accept both forms.
        assert!(
            flat.contains("--bg-void:"),
            "tokens.css must declare the --bg-void surface token"
        );
        let bg_void_direct =
            flat_up.contains("--BG-VOID:#070710") || flat_up.contains("--BG-VOID: #070710");
        let bg_void_aliased = (flat.contains("--bg-void: var(--s-void)")
            || flat.contains("--bg-void:var(--s-void)"))
            && (flat_up.contains("--S-VOID:#070710") || flat_up.contains("--S-VOID: #070710"));
        assert!(
            bg_void_direct || bg_void_aliased,
            "tokens.css --bg-void must resolve to #070710 (direct or via --s-void alias)"
        );
    }

    // ── (5) tokens.css: EVERY [shared] role resolves to its canonical value ────
    // The drift VALIDATOR half of the contract's "author-once / emit-twice /
    // validate" mechanism (token-contract.md §0 / UIVIS-RENDER-1). This parses
    // the axe tokens.css emission and asserts every [shared]-VALUE-converged role
    // equals the canonical value from the contract. It generalizes the by-hand
    // alias-resolution the void test above does into a reusable 1-level resolver.
    //
    // SOURCE OF TRUTH: docs/design-system/DCENT_DESIGN_LANGUAGE/token-contract.md
    // §2/§3/§5/§8. The canonical [shared]-VALUE-converged table on the axe side is:
    //     --accent        #FAA500   (§2 primary brand)
    //     --accent-deep   #FA6700   (§2 ember companion; via --ember alias)
    //     --accent-hover  #FFC94D   (§2 amber lift; via --amber alias)
    //     --orange-bitcoin#F7931A   (§2 opt-in legacy BTC alias — NEVER the default)
    //     --bg-void       #070710   (§3 deepest floor / family glue; via --s-void)
    //     --tgreen        #00FF41   (§5/§8 reserved terminal-green; byte-identical)
    //
    // DELIBERATELY NOT value-validated on axe (the contract-faithful asymmetry):
    // the STATUS hues (--green #34d399 / --yellow #fbbf24 / --red #f87171 /
    // --cyan #22d3ee) are `[shared]-ROLE / per-project-VALUE` (§5 — axe's softer
    // Tailwind-400 family vs OS's saturated table-tuned set), and axe has NO
    // --sphere-mid (§9 forbids it here). Value-asserting those would falsely fail.
    // OS owns the canonical status/sphere VALUES, so the OS validator checks them.

    /// Resolve a `--<role>` value from the whitespace-collapsed UPPERCASED CSS,
    /// following EXACTLY ONE level of `VAR(--OTHER)` indirection (the only depth
    /// present in the axe emission: --accent-deep→--ember, --accent-hover→--amber,
    /// --bg-void→--s-void; --accent/--orange-bitcoin/--tgreen are direct).
    ///
    /// Returns the value token (up to `;`), whitespace already collapsed and
    /// uppercased. 1-LEVEL LIMIT (fail-closed): a future 2-level alias returns the
    /// `VAR(--X)` literal and the equality assert fails loudly — not a silent pass.
    fn css_role_value(flat_up: &str, role_up: &str) -> Option<String> {
        let raw = read_role(flat_up, role_up)?;
        // `VAR(--OTHER)` → resolve one level.
        if let Some(rest) = raw.strip_prefix("VAR(") {
            if let Some(inner) = rest.strip_suffix(')') {
                let inner = inner.trim();
                if inner.starts_with("--") {
                    if let Some(v) = read_role(flat_up, inner) {
                        return Some(v);
                    }
                }
            }
        }
        Some(raw)
    }

    /// Read a single `--ROLE:VALUE;` declaration from the collapsed-uppercase CSS.
    /// `flat_up` has internal whitespace collapsed to single spaces (collapse_ws)
    /// then uppercased; we additionally strip spaces around the value so
    /// `--ACCENT: #FAA500` and `--ACCENT:#FAA500` compare identically.
    fn read_role(flat_up: &str, role_up: &str) -> Option<String> {
        // Find `--ROLE` followed (after optional space) by ':'.
        let needle = role_up;
        let mut search_from = 0usize;
        while let Some(idx) = flat_up[search_from..].find(needle) {
            let start = search_from + idx;
            let after = &flat_up[start + needle.len()..];
            // The char immediately after the role name must be ':' or a space then
            // ':', AND the char BEFORE must not be a `-`/alnum (so `--ACCENT` does
            // not also match inside `--ACCENT-DEEP`). collapse_ws guarantees single
            // spaces; role names use only [A-Z0-9-].
            let prev_ok = start == 0
                || !flat_up.as_bytes()[start - 1].is_ascii_alphanumeric()
                    && flat_up.as_bytes()[start - 1] != b'-';
            let after_trim = after.trim_start();
            if prev_ok && after_trim.starts_with(':') {
                let val = after_trim[1..].trim_start();
                if let Some(end) = val.find(';') {
                    return Some(val[..end].replace(' ', ""));
                }
            }
            search_from = start + needle.len();
        }
        None
    }

    #[test]
    fn tokens_css_shared_roles_match_contract() {
        let flat_up = collapse_ws(TOKENS_CSS).to_ascii_uppercase();
        // Each tuple: (role, canonical value, contract section) — all UPPERCASE.
        let shared: &[(&str, &str, &str)] = &[
            ("--ACCENT", "#FAA500", "§2 primary brand"),
            (
                "--ACCENT-DEEP",
                "#FA6700",
                "§2 ember companion (via --ember)",
            ),
            ("--ACCENT-HOVER", "#FFC94D", "§2 amber lift (via --amber)"),
            (
                "--ORANGE-BITCOIN",
                "#F7931A",
                "§2 opt-in BTC alias (never default)",
            ),
            (
                "--BG-VOID",
                "#070710",
                "§3 floor / family glue (via --s-void)",
            ),
            ("--TGREEN", "#00FF41", "§5/§8 reserved terminal-green"),
        ];
        for (role, want, section) in shared {
            let got = css_role_value(&flat_up, role).unwrap_or_else(|| {
                panic!("tokens.css is missing the [shared] role {role} ({section})")
            });
            assert_eq!(
                &got, want,
                "tokens.css {role} must equal canonical {want} ({section}, \
                 token-contract.md §0/§2/§3/§5/§8)"
            );
        }
        // And the contract-faithful asymmetry holds: the per-project STATUS hues
        // are PRESENT (role names exist) but are NOT the OS-canonical values — so
        // they are correctly [shared]-ROLE / per-project-VALUE, never cross-checked.
        // (Pin the axe Tailwind-400 family stays distinct from OS's saturated set.)
        assert_eq!(
            css_role_value(&flat_up, "--GREEN").as_deref(),
            Some("#34D399"),
            "axe --green stays the per-project Tailwind-400 value (§5 shared-role/per-project-value)"
        );
        assert!(
            css_role_value(&flat_up, "--SPHERE-MID").is_none(),
            "axe must NOT declare --sphere-mid (§9 forbids it; it is OS-only)"
        );
    }

    // ── (6) TERM-7: window.GLOSSARY string-table present + handler-free host ───
    // The axe glossary-equivalent (terminology-lexicon §0/§11) is hosted INLINE
    // in framework.js (the already-served <script src> #1), exposed as
    // `window.GLOSSARY`, so the ~scattered canonical labels are single-sourced
    // and every component IIFE + the inline dashboard.rs JS can resolve them.
    // The host MUST stay handler-free: framework.js is loaded via the existing
    // `register_static(server, "/dashboard/framework.js", ...)` — a regression
    // that split the table into a NEW dashboard/glossary.js would cost +1
    // register_static (73/96) and force bumping REGISTERED_HANDLER_ESTIMATE.
    #[test]
    fn term7_glossary_table_present_and_handler_free() {
        let fw = collapse_ws(FRAMEWORK_JS);
        // The table + resolver are defined and exposed on window.
        assert!(
            fw.contains("const GLOSSARY ="),
            "framework.js must define the inline GLOSSARY string-table (TERM-7)"
        );
        assert!(
            fw.contains("window.GLOSSARY = GLOSSARY"),
            "framework.js must expose GLOSSARY on window so components can resolve labels"
        );
        assert!(
            fw.contains("window.gloss = gloss"),
            "framework.js must expose the gloss() resolver on window"
        );
        // Handler-free host: framework.js is register_static'd in dashboard.rs;
        // a NEW glossary.js handler must NOT have been introduced.
        let dash = collapse_ws(DASHBOARD_RS);
        assert!(
            dash.contains("register_static(server, \"/dashboard/framework.js\""),
            "framework.js must stay the served (register_static) glossary host"
        );
        assert!(
            !dash.contains("/dashboard/glossary.js"),
            "regression: glossary must stay inline in framework.js — no new glossary.js \
             handler (would push REGISTERED_HANDLER_ESTIMATE 73 -> 74/96)"
        );
    }

    // ── (7) TERM-7: GLOSSARY carries the canonical [shared] keys + spellings ───
    // The keys are the stable cross-firmware ids (they must match the OS
    // glossary.ts keys); the strings must be byte-identical to the contract
    // spellings (terminology-lexicon §3.1/§4.x/§5.3/§6.1) or the S1 token+label
    // drift-validators and the truth-ladder guard would diverge from the table.
    #[test]
    fn term7_glossary_has_canonical_shared_keys_and_strings() {
        let fw = collapse_ws(FRAMEWORK_JS);
        // Stable [shared] glossary KEYS (cross-firmware contract surface, §11).
        for key in [
            "window_headline_10m",
            "efficiency_jth",
            "unit_power",
            "btu_per_hour",
            "state_telemetry_pending",
            "state_mining",
            "state_ready",
            "state_standby",
            "state_stopped",
            "telemetry_stale",
            "telemetry_absent",
            "empty_value",
            "share_accepted",
            "best_diff_session",
            "best_diff_all_time",
            "pool_target_difficulty",
            "achieved_difficulty",
            // UINAV-4 shared disclosure-run vocabulary.
            "disclosure_basic",
            "disclosure_standard",
            "advanced_axe_disclosure",
        ] {
            assert!(
                fw.contains(key),
                "GLOSSARY must carry the canonical [shared] key `{key}` (terminology-lexicon §11)"
            );
        }
        // Canonical STRINGS that the served components pull (byte-identical to the
        // contract). 'Hashrate · 10m' is matched in two parts so the central-dot
        // codepoint is not an encoding hazard in the assertion source.
        for s in [
            "Telemetry stale",             // §6.1 telemetry_stale
            "Telemetry pending",           // §6.1 state_telemetry_pending
            "pool accepted",               // §4.4 share_accepted (count-row phrasing)
            "Best Diff (session)",         // §4.2 best_diff_session
            "Best Ever (all-time)",        // §4.2 best_diff_all_time
            "Pool Target Difficulty",      // §4.1 pool_target_difficulty
            "Achieved Difficulty",         // §4.1 achieved_difficulty
            "Lower J/TH = more efficient", // §3.2 efficiency_jth help
        ] {
            assert!(
                fw.contains(s),
                "GLOSSARY must carry the canonical [shared] string `{s}` byte-identical"
            );
        }
        // The 'Hashrate · 10m' headline label (window_headline_10m): match the
        // two literal halves around the middot so the test source stays ASCII.
        assert!(
            fw.contains("Hashrate") && fw.contains("10m") && fw.contains("10m Average"),
            "GLOSSARY window_headline_10m must carry the 'Hashrate · 10m' label + '10m Average' caption"
        );
    }

    // ── (8) TERM-6: 'Telemetry stale' surfaced distinct from 'Offline' ─────────
    // axe was binary online/offline; the canonical lexicon (§6.1) splits
    // held-but-stale telemetry ('Telemetry stale') from nothing-arriving
    // ('Offline'). dashboard.rs's offline path must surface the stale label on
    // the held-_lastInfo branch (truth-contract: label only, never extrapolated).
    #[test]
    fn term6_telemetry_stale_label_surfaced() {
        let dash = collapse_ws(DASHBOARD_RS);
        // The canonical label text and its glossary key are both present.
        assert!(
            dash.contains("Telemetry stale"),
            "dashboard.rs must surface the canonical 'Telemetry stale' label (TERM-6 §6.1)"
        );
        assert!(
            dash.contains("telemetry_stale"),
            "dashboard.rs must reference the telemetry_stale glossary key"
        );
        // The held-vs-absent split is implemented: the banner chooses the stale
        // label only when prior telemetry is held in _lastInfo.
        assert!(
            dash.contains("_telemetryBanner") && dash.contains("_lastInfo?"),
            "the offline banner must split held telemetry (stale) from absent (offline) via _lastInfo"
        );
        // The pre-first-data rung word stays distinct (never collapsed into stale).
        assert!(
            dash.contains("Telemetry pending"),
            "rung-0 'Telemetry pending' must stay distinct from 'Telemetry stale'"
        );
    }

    // ── (9) the served components pull labels from window.GLOSSARY ─────────────
    // core.js / asic-chips.js / block-tile.js were wired to resolve labels via
    // the shared table (with a per-key literal fallback). Pin that they reference
    // it so a future edit cannot silently re-hardcode a scattered label.
    #[test]
    fn served_components_reference_glossary() {
        let core = collapse_ws(CORE_JS);
        assert!(
            core.contains("window.gloss") && core.contains("window_headline_10m"),
            "core.js must resolve its headline/meta labels from window.GLOSSARY"
        );
        for (name, src) in [
            ("asic-chips.js", collapse_ws(ASIC_CHIPS_JS)),
            ("block-tile.js", collapse_ws(BLOCK_TILE_JS)),
        ] {
            assert!(
                src.contains("window.gloss") && src.contains("empty_value"),
                "{name} must resolve the canonical empty-value glyph from window.GLOSSARY"
            );
        }
    }

    // ── (10) M-dash-2: the headline temp tile respects sensor provenance ───────
    // A dead/absent sensor (sensorsOk=false, temp 0) must NOT render as a real
    // "0C / Cool", and an EMC2101 ambient PROXY must be qualified, not shown as the
    // true ASIC die temp. dashboard.rs JS cannot host-execute, so pin from TEXT
    // that the headline `tempQuick` tile is gated on `sensorsOk` and that the
    // `tempSource` ambient-proxy provenance is consulted near the temp rendering.
    #[test]
    fn headline_temp_tile_respects_sensors_ok_and_temp_source() {
        let flat = collapse_ws(DASHBOARD_RS);
        // (a) tempKnown derives from sensorsOk (+ a finite-check mirroring
        //     asic-chips.js) — the sensor-provenance gate.
        assert!(
            flat.contains("tempKnown=(d.sensorsOk!==false)"),
            "the headline temp gate `tempKnown` must derive from d.sensorsOk"
        );
        // (b) the ambient-proxy provenance token is consulted.
        assert!(
            flat.contains("d.dcentaxe.tempSource==='ambient_proxy'"),
            "the temp tile must consult dcentaxe.tempSource for the ambient-proxy qualifier"
        );
        // (c) the headline tempQuick tile renders '--' (not 0C/Cool) when unknown.
        assert!(
            flat.contains("if(!tempKnown){ S('tempQuick','--')"),
            "the headline tempQuick tile must render '--' when the sensor is unknown, \
             not a fabricated 0C/Cool"
        );
        // (d) PROXIMITY: the sensorsOk / tempSource provenance refs sit NEAR the
        //     headline temp rendering (co-located wiring, not an unrelated mention).
        let temp_idx = DASHBOARD_RS
            .find("S('tempQuick'")
            .expect("dashboard.rs must render the headline tempQuick tile");
        let sensor_idx = DASHBOARD_RS
            .find("d.sensorsOk")
            .expect("dashboard.rs must reference d.sensorsOk");
        let source_idx = DASHBOARD_RS
            .find("d.dcentaxe.tempSource")
            .expect("dashboard.rs must reference d.dcentaxe.tempSource");
        let dist = |a: usize, b: usize| if a > b { a - b } else { b - a };
        assert!(
            dist(temp_idx, sensor_idx) < 2000 && dist(temp_idx, source_idx) < 2000,
            "the sensorsOk/tempSource provenance refs must sit NEAR the headline temp \
             rendering (co-located wiring), not an unrelated mention"
        );
    }
}

/// MCP / AUTH CONTRACT structural guards (Phase-3 `mcp-align` lane).
///
/// `mcp.rs` lives in the `dcentaxe` BINARY crate (esp-idf at module scope) and
/// CANNOT host-compile, so — exactly like `safety_guards` and
/// `dcent_design_language_guards` — this pins the cross-firmware MCP/auth
/// contract (`docs/design-system/DCENT_DESIGN_LANGUAGE/mcp-auth-contract.md`)
/// against the `mcp.rs` source TEXT via `include_str!`. It is the only place CI
/// can prove axe's tool-name vocabulary + the read/control auth split has not
/// drifted from the shared `dcent-schema::mcp` registry.
///
/// Matches are deliberately whitespace-robust (substring on a whitespace-
/// collapsed copy), never byte-exact blocks, so a benign reformat of `mcp.rs`
/// cannot flip a guard.
#[cfg(test)]
mod mcp_auth_contract_guards {
    const MCP_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/mcp.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── (1) the 6 cross-firmware-kernel canonical names are present ───────────
    // These are the dcent-schema minimal_profile() names (mcp-auth-contract.md
    // §5.1). axe MUST emit/accept each canonical name. A rename here is a
    // breaking change that host compilation alone cannot catch (binary crate).
    #[test]
    fn kernel_canonical_tool_names_present() {
        let flat = collapse_ws(MCP_RS);
        for name in [
            "get_status",
            "get_device_info",
            "get_swarm_status",
            "identify_device",
            "restart_mining",
            "set_pool",
        ] {
            assert!(
                flat.contains(&format!("\"{name}\"")),
                "mcp.rs must reference the canonical kernel tool name `{name}`"
            );
        }
    }

    // ── (2) legacy aliases are ACCEPTED inbound but NOT emitted standalone ────
    // Contract §2.1 hard rule: "the canonical tool name is the only name a
    // surface EMITS"; a surface MAY still ACCEPT a legacy_alias inbound. axe's
    // dispatch keeps the `get_asic_info`/`get_swarm` match arms (back-compat),
    // but handle_tools_list must no longer list them as standalone tools.
    #[test]
    fn legacy_aliases_accepted_inbound_not_emitted() {
        let flat = collapse_ws(MCP_RS);
        // Inbound dispatch arms for the two aliases MUST stay (back-compat for
        // old AI agents / toolbox callers still using the pre-canonical name).
        assert!(
            flat.contains("\"get_asic_info\" => tool_get_asic_info(state)"),
            "mcp.rs must keep ACCEPTING get_asic_info inbound (legacy alias of get_device_info)"
        );
        assert!(
            flat.contains("\"get_swarm\" => tool_get_swarm(state)"),
            "mcp.rs must keep ACCEPTING get_swarm inbound (legacy alias of get_swarm_status)"
        );
        // …but the two aliases must NOT be emitted as their own tools/list
        // descriptor entries. The emitted form is `"name": "<canonical>"`; an
        // emitted alias would appear as `"name": "get_asic_info"` /
        // `"name": "get_swarm"`. Assert those emission lines are absent.
        assert!(
            !flat.contains("\"name\": \"get_asic_info\""),
            "contract §2.1: get_asic_info is a legacy alias — must NOT be emitted as a standalone \
             tools/list entry (accept inbound only)"
        );
        assert!(
            !flat.contains("\"name\": \"get_swarm\""),
            "contract §2.1: get_swarm is a legacy alias — must NOT be emitted as a standalone \
             tools/list entry (accept inbound only)"
        );
    }

    // ── (3) the 7 CONTROL tools == the shared write semantics ─────────────────
    // The read/control auth split (contract §3) is enforced by
    // mcp_tool_requires_control(). The CONTROL set is the 3 shared-kernel write
    // tools (identify_device/restart_mining/set_pool) + the 4 axe-led tuning
    // tools (set_frequency/set_core_voltage/set_fan_speed/run_autotune). A tool
    // silently dropping out of this matches!() would make it callable on an
    // unauthorized read session — exactly the auth-surface drift this pins.
    #[test]
    fn control_gate_lists_exactly_the_seven_write_tools() {
        let flat = collapse_ws(MCP_RS);
        // The control gate is a matches!(name, ... ) over the 7 CONTROL names.
        for name in [
            "set_frequency",
            "set_core_voltage",
            "set_fan_speed",
            "set_pool",
            "restart_mining",
            "identify_device",
            "run_autotune",
        ] {
            assert!(
                flat.contains(&format!("\"{name}\"")),
                "CONTROL tool `{name}` must appear in mcp.rs (mcp_tool_requires_control gate)"
            );
        }
        // The 3 READ kernel tools must NOT be control-gated. Guard that the
        // control gate function does not list a read tool by checking the
        // matches! arm region around mcp_tool_requires_control.
        let gate = flat
            .find("fn mcp_tool_requires_control")
            .expect("mcp.rs must define the read/control split gate mcp_tool_requires_control");
        // Bound the window at the NEXT sibling `fn ` (not a fixed +400 slice) so a
        // future edit that shrinks the inter-fn doc comment can't let the window
        // spill into `fn mcp_tool_writes_hardware` — whose matches! lists the SAME
        // control names — and mask a mis-class / fail-open regression (the exact
        // AOTA-class defect this guard exists to prevent; completeness-critic L1).
        let end = flat[gate..]
            .match_indices("fn ")
            .nth(1)
            .map(|(i, _)| gate + i)
            .unwrap_or_else(|| (gate + 400).min(flat.len()));
        let region = &flat[gate..end];
        for read_only in [
            "\"get_status\"",
            "\"get_device_info\"",
            "\"get_swarm_status\"",
        ] {
            assert!(
                !region.contains(read_only),
                "READ tool {read_only} must NOT be inside the CONTROL gate (would force owner-auth \
                 on a read tool / mis-class the auth split)"
            );
        }
    }

    // ── (4) the fail-closed-on-OPEN control posture is intact ─────────────────
    // Contract §3.2 + §6: axe's signature posture is CONTROL fail-closed on an
    // OPEN (passwordless) device. The dispatch must refuse a CONTROL tool when
    // !control_authorized regardless of read access. This is the AOTA-class RCE
    // the axe hardening closed; the structure must not regress. (The posture
    // DEFAULT is deliberately the inverse of the OS :3000 server — that
    // asymmetry is the documented, unresolved operator decision; this guard
    // pins only that axe's own fail-closed structure survives, not the cross-
    // product policy.)
    #[test]
    fn control_dispatch_is_fail_closed_on_unauthorized() {
        let flat = collapse_ws(MCP_RS);
        assert!(
            flat.contains("if mcp_tool_requires_control(name) { if !auth.control_authorized {"),
            "handle_tool_call must refuse a CONTROL tool when !control_authorized (fail-closed)"
        );
        // The McpAuth split (read_authorized / control_authorized) is the
        // reference 2-predicate decision (contract §3.1). Pin both predicates.
        assert!(
            flat.contains("read_authorized") && flat.contains("control_authorized"),
            "mcp.rs must keep the two-predicate auth split (read_authorized + control_authorized)"
        );
    }

    // ── (5) cross-firmware byte-alignment: axe mcp.rs ↔ OS :3000 overlay ──────
    // The OS Python `:3000` control server hand-mirrors the SAME dcent-schema
    // minimal_profile() kernel (board/zynq/.../web/mcp_server.py). axe's own
    // mcp.rs and that OS overlay are two independent emissions of the one shared
    // registry — they MUST expose the identical 6 canonical kernel names and the
    // identical 3 shared WRITE names. This is the cross-firmware leg of the
    // author-once / emit-twice / VALIDATE mechanism (token-contract §0,
    // UIVIS-RENDER-1): the dcent-schema Rust drift test
    // (projects/dcent-schema/tests/python_overlay_drift.rs) pins the OS overlay
    // to the registry; THIS test pins axe to the same names the OS overlay
    // emits, so a desync on EITHER firmware surfaces as a red test runnable under
    // `cargo +stable test -p dcentaxe-core --lib`. Text-substring + additive;
    // the OS overlay is read read-only via include_str! (never compiled).
    // Path note: relative to this crate's manifest dir
    // (DCENT_OS_ESP/dcentaxe-core). Two `..` segments reach `projects/`,
    // then into the sibling `dcentos` project.
    const OS_MCP_OVERLAY_PY: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../dcentos/br2_external_dcentos/board/zynq/rootfs-overlay/root/web/mcp_server.py"
    ));

    #[test]
    fn axe_and_os_overlay_share_the_same_kernel_vocabulary() {
        let axe = collapse_ws(MCP_RS);
        let os = collapse_ws(OS_MCP_OVERLAY_PY);

        // The 6 shared-kernel canonical names must be referenced by axe's mcp.rs
        // AND emitted by the OS overlay as the `"name": "<canonical>"` descriptor.
        for name in [
            "get_status",
            "get_device_info",
            "get_swarm_status",
            "identify_device",
            "restart_mining",
            "set_pool",
        ] {
            assert!(
                axe.contains(&format!("\"{name}\"")),
                "axe mcp.rs must reference shared-kernel tool `{name}`"
            );
            assert!(
                os.contains(&format!("\"name\": \"{name}\"")),
                "OS :3000 overlay must EMIT shared-kernel tool `{name}` — cross-firmware desync"
            );
        }

        // The 3 SHARED write tools must be in axe's CONTROL gate AND the OS
        // overlay's WRITE_TOOLS set (both firmwares fail-closed the same names on
        // a release image). axe's own 4 tuning write tools (set_frequency etc.)
        // are axe-only and intentionally NOT asserted against the OS overlay.
        for shared_write in ["identify_device", "restart_mining", "set_pool"] {
            assert!(
                axe.contains(&format!("\"{shared_write}\"")),
                "axe mcp.rs must carry shared write tool `{shared_write}` in its CONTROL gate"
            );
            // The OS WRITE_TOOLS set is a `{ ... }` literal; assert membership by
            // the bare quoted entry appearing after the WRITE_TOOLS marker.
            let wt = os
                .find("WRITE_TOOLS =")
                .expect("OS overlay must define WRITE_TOOLS");
            let region = &os[wt..];
            assert!(
                region.contains(&format!("\"{shared_write}\"")),
                "OS overlay WRITE_TOOLS must contain shared write tool `{shared_write}` — \
                 cross-firmware auth desync (axe gates it, OS must too)"
            );
        }
    }

    // ── MCP-SEC: bitaxe://config masks EVERY pool password, not just primary ──
    // The read resource clones the live config and masks secrets before
    // serializing. The fallback (`fallback_pool`) and split (`split_pool`) pools
    // each carry their OWN StratumConfig.password; a regression that masks only
    // the primary `stratum.password` would leak the backup-pool credentials in
    // cleartext to any read-authorized MCP client. mcp.rs is the espidf-only
    // binary-crate source (cannot host-compile), so pin the masking against the
    // source TEXT (whitespace-robust, additive — same pattern as the other
    // mcp_auth_contract_guards).
    #[test]
    fn config_resource_masks_all_pool_passwords() {
        let flat = collapse_ws(MCP_RS);
        assert!(
            flat.contains("\"bitaxe://config\" =>"),
            "mcp.rs must serve the bitaxe://config read resource"
        );
        // Primary (pre-existing) masking must stay.
        assert!(
            flat.contains("safe_config.stratum.password = \"***\""),
            "bitaxe://config must mask the PRIMARY stratum password"
        );
        // FALLBACK pool password masking (FIX 2).
        assert!(
            flat.contains("safe_config.fallback_pool.as_mut()")
                && flat.contains("fb.password = \"***\""),
            "MCP-SEC: bitaxe://config must mask the FALLBACK pool password"
        );
        // SPLIT (secondary) pool password masking (FIX 2).
        assert!(
            flat.contains("safe_config.split_pool.as_mut()")
                && flat.contains("sp.pool.password = \"***\""),
            "MCP-SEC: bitaxe://config must mask the SPLIT pool password"
        );
    }

    // ── B-ESP-10: pool worker + URL masked on EVERY read surface ─────────────
    // The pool `worker` is the operator's FULL BTC payout address on V1 solo and
    // a pool URL can embed `user:pass@` creds. This mirrors the Antminer
    // load-bearing rule
    // with byte-identical shapes (`mask_wallet` → <first6>…<last4>;
    // `sanitize_pool_url` → strip the authority). api.rs + mcp.rs are the
    // espidf-only binary-crate sources (cannot host-compile), so pin the masking
    // against the source TEXT — additive + whitespace-robust, exactly like
    // `config_resource_masks_all_pool_passwords` above.
    #[test]
    fn pool_worker_and_url_masked_on_read_surfaces() {
        const API_RS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dcentaxe/src/api.rs"
        ));
        let api = collapse_ws(API_RS);
        let mcp = collapse_ws(MCP_RS);

        // /api/system/info struct: primary worker masked + URL sanitized via the
        // owned pre-bound locals (the struct holds &str borrows).
        assert!(
            api.contains(
                "let stratum_user_masked = crate::shared::mask_wallet(&config.stratum.worker_name)"
            ) && api.contains("stratum_user: stratum_user_masked.as_str()"),
            "/api/system/info must mask the primary worker (stratum_user)"
        );
        assert!(
            api.contains(
                "let stratum_url_masked = crate::shared::sanitize_pool_url(&config.stratum.url)"
            ) && api.contains("stratum_url: stratum_url_masked.as_str()"),
            "/api/system/info must sanitize the primary pool URL (stratum_url)"
        );
        assert!(
            api.contains("fallback_user_masked")
                && api.contains("crate::shared::mask_wallet(&fb.worker_name)"),
            "/api/system/info must mask the fallback worker"
        );

        // /api/system GET: the split-pool worker is masked (only split surface).
        assert!(
            api.contains("crate::shared::mask_wallet(&s.pool.worker_name)"),
            "/api/system GET must mask the split-pool worker"
        );

        // /api/pools: both workers masked.
        assert!(
            api.contains("\"worker\": crate::shared::mask_wallet(&config.stratum.worker_name)")
                && api.contains("\"worker\": crate::shared::mask_wallet(&split.pool.worker_name)"),
            "/api/pools must mask the primary + split workers"
        );

        // CGMiner TCP API (pyasic-compat, port 4028): worker masked like Antminer.
        const CGMINER_RS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dcentaxe/src/cgminer_tcp.rs"
        ));
        let cg = collapse_ws(CGMINER_RS);
        assert!(
            cg.contains("\"User\": crate::shared::mask_wallet(&pool.worker_name)")
                && cg.contains("\"User\": crate::shared::mask_wallet(&fallback.worker_name)"),
            "CGMiner API must mask the worker in the `User` field"
        );

        // Whole-file floor: every worker/url read site routes through a helper.
        assert!(
            api.matches("crate::shared::mask_wallet(").count() >= 6,
            "api.rs lost a worker-mask call site (read-surface regression)"
        );
        assert!(
            api.matches("crate::shared::sanitize_pool_url(").count() >= 6,
            "api.rs lost a pool-URL-sanitize call site (read-surface regression)"
        );

        // mcp.rs bitaxe://config masks the worker + URL (not just passwords).
        assert!(
            mcp.contains(
                "safe_config.stratum.worker_name = crate::shared::mask_wallet(&safe_config.stratum.worker_name)"
            ),
            "mcp.rs bitaxe://config must mask the primary worker"
        );
        assert!(
            mcp.contains(
                "safe_config.stratum.url = crate::shared::sanitize_pool_url(&safe_config.stratum.url)"
            ),
            "mcp.rs bitaxe://config must sanitize the primary pool URL"
        );
        assert!(
            mcp.contains("fb.worker_name = crate::shared::mask_wallet(&fb.worker_name)")
                && mcp.contains(
                    "sp.pool.worker_name = crate::shared::mask_wallet(&sp.pool.worker_name)"
                ),
            "mcp.rs bitaxe://config must mask the fallback + split workers"
        );

        // Round-trip WRITE guard exists: a masked read echo must not clobber the
        // stored full worker/url (the dashboard form re-POSTs what it rendered).
        assert!(
            api.contains("crate::shared::is_masked_worker_echo(")
                && api.contains("crate::shared::is_sanitized_url_echo("),
            "apply_config_updates must keep the stored worker/url on a masked echo"
        );

        // ── Newly-closed leaks (this pass) — PINNED so they cannot re-open ───────
        // The original >=6 whole-file floors above gave false confidence: they
        // passed DESPITE these three sites leaking, because OTHER call sites met
        // the count. Pin each newly-fixed site EXACTLY.

        // FIX 1: shared_pool_config (GET + POST /api/config/shared, primary AND
        // fallback) routes the pool URL + worker through the canonical sanitizers.
        assert!(
            api.contains("url: crate::shared::sanitize_pool_url(&pool.url)")
                && api.contains("worker: crate::shared::mask_wallet(&pool.worker_name)"),
            "shared_pool_config (/api/config/shared) must sanitize the URL + mask \
             the worker for the primary AND fallback pool (B-ESP-10 FIX 1)"
        );

        // FIX 2 companion: because GET /api/config/shared now masks, the
        // apply_shared_config_patch WRITE path must guard against a masked re-POST
        // clobbering the stored full worker/url (primary + fallback, url + worker).
        assert!(
            api.matches("crate::shared::is_sanitized_url_echo(").count() >= 2
                && api.matches("crate::shared::is_masked_worker_echo(").count() >= 2,
            "apply_shared_config_patch + apply_config_updates must BOTH echo-guard \
             the worker/url WRITE path (B-ESP-10 FIX 2)"
        );

        // FIX 3: the MCP pool_truth active-pool format string sanitizes the URL.
        // Both the pools[] entry and pool_truth use the identical sanitized form;
        // also assert the raw `format!("{}:{}", status.active_url` is gone.
        assert!(
            mcp.matches(
                "\"active_pool\": format!(\"{}:{}\", crate::shared::sanitize_pool_url(&status.active_url), status.active_port)"
            )
            .count()
                >= 2,
            "mcp pool_truth + pools[] active_pool must BOTH sanitize the active \
             pool URL (B-ESP-10 FIX 3)"
        );
        assert!(
            !mcp.contains("format!(\"{}:{}\", status.active_url"),
            "mcp active_pool must not format the RAW active_url (B-ESP-10 FIX 3 leak)"
        );

        // FIX 4: the CGMiner fallback "Stratum URL" uses the already-sanitized
        // fb_display_url, not the raw fallback.url.
        assert!(
            cg.contains("let fb_display_url = crate::shared::sanitize_pool_url(&fallback.url)")
                && cg.contains("\"Stratum URL\": fb_display_url"),
            "CGMiner fallback \"Stratum URL\" must use the sanitized fb_display_url \
             (B-ESP-10 FIX 4)"
        );
        assert!(
            !cg.contains("\"Stratum URL\": fallback.url"),
            "CGMiner fallback \"Stratum URL\" must not emit the raw fallback.url \
             (B-ESP-10 FIX 4 leak)"
        );
    }

    // ── B-ESP-10 iteration 2: events + console logs + OLED masked at the SOURCE ──
    // The DEEPER channel the iteration-1 HTTP-field masking missed: the stratum
    // EVENT details (`StratumEventRecord.detail`), the client/SV2 console LOGS, and
    // the physical OLED embed the RAW pool URL (and one log printed the RAW worker
    // = BTC payout address). Those event details flow verbatim onto the HTTP/MCP
    // read surfaces (`recent_events`, `primary_failback_detail`,
    // `last_reconnect_cause`). They are now sanitized/masked AT THE SOURCE in
    // `dcentaxe-stratum/src/client.rs` + the binary `main.rs` (+ provisioning.rs),
    // so the api.rs/mcp.rs emission points stay clean automatically. client.rs +
    // main.rs are espidf/foreign-crate sources a host compile cannot reach, so pin
    // the masking against the source TEXT (whitespace-robust, additive). Each fixed
    // site is asserted POSITIVE (uses the helper) AND NEGATIVE (the raw form is gone)
    // so the leak cannot silently re-open.
    #[test]
    fn pool_worker_and_url_masked_in_events_logs_oled() {
        const CLIENT_RS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dcentaxe-stratum/src/client.rs"
        ));
        const MAIN_RS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dcentaxe/src/main.rs"
        ));
        const PROVISIONING_RS: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../dcentaxe/src/provisioning.rs"
        ));
        let client = collapse_ws(CLIENT_RS);
        let main = collapse_ws(MAIN_RS);
        let prov = collapse_ws(PROVISIONING_RS);

        // dcentaxe-stratum imports the canonical helpers (its own byte-identical
        // copy, since the binary's crate::shared is not reachable from this crate).
        assert!(
            client.contains("use crate::mask::{mask_wallet, sanitize_pool_url}"),
            "client.rs must import the sanitize/mask helpers from crate::mask"
        );

        // ── client.rs event details + logs sanitize the pool URL ────────────────
        for needle in [
            // Connect event detail
            "\"connected to {}:{}\", sanitize_pool_url(&self.config.url)",
            // FailoverSkipped / FailoverEntered event details
            "\"fallback unreachable {}:{}\", sanitize_pool_url(&fb.url)",
            "\"switched to fallback {}:{}\", sanitize_pool_url(&self.config.url)",
            // Primary reprobe / failback event details
            "\"probing primary {}:{}\", sanitize_pool_url(&self.primary_config.url)",
            "\"primary {}:{} authorized with job proof\", sanitize_pool_url(&self.primary_config.url)",
            "\"switching back to primary {}:{}\", sanitize_pool_url(&self.config.url)",
            "\"primary {}:{} failed: {}\", sanitize_pool_url(&self.primary_config.url)",
            // PoolRedirect: last_reconnect_cause + ReconnectRequested event detail
            "\"pool redirect {}:{}\", sanitize_pool_url(&self.config.url)",
            "\"redirect to {}:{} in {}s\", sanitize_pool_url(&self.config.url)",
        ] {
            assert!(
                client.contains(needle),
                "client.rs event/log site lost its sanitize_pool_url wrapper: {needle}"
            );
        }
        // The authorize LOG masks the worker (BTC payout address).
        assert!(
            client
                .contains("\"Stratum: authorized as '{}'\", mask_wallet(&self.config.worker_name)"),
            "client.rs authorized log must mask the worker (mask_wallet)"
        );

        // ── client.rs: the RAW forms must be GONE (negative pins) ───────────────
        for raw in [
            "\"connected to {}:{}\", self.config.url",
            "\"switched to fallback {}:{}\", self.config.url",
            "\"fallback unreachable {}:{}\", fb.url",
            "\"pool redirect {}:{}\", self.config.url",
            "\"redirect to {}:{} in {}s\", self.config.url",
            "\"Stratum: authorized as '{}'\", self.config.worker_name",
            "\"primary {}:{} authorized with job proof\", self.primary_config.url",
        ] {
            assert!(
                !client.contains(raw),
                "client.rs still emits a RAW pool URL/worker in an event/log: {raw}"
            );
        }
        // The wire protocol (mining.authorize / mining.submit params) MUST keep the
        // real worker — masking there would break auth. Pin it stays raw.
        assert!(
            client.contains("self.config.worker_name, self.config.password"),
            "client.rs mining.authorize params must keep the REAL worker (write path)"
        );

        // ── main.rs SV2 failover event + OLED sanitize the pool URL ─────────────
        assert!(
            main.contains(
                "\"sv2 failover to fallback {}:{}\", crate::shared::sanitize_pool_url(&fb.url)"
            ),
            "main.rs SV2 failover event/log must sanitize the fallback URL"
        );
        assert!(
            main.contains(
                "let pool_display = format!( \"{}:{}\", crate::shared::sanitize_pool_url(&config.stratum.url)"
            ),
            "main.rs OLED pool_display must sanitize the pool URL"
        );
        // Negative pins: the raw SV2-failover + raw OLED forms are gone.
        assert!(
            !main.contains("\"sv2 failover to fallback {}:{}\", fb.url"),
            "main.rs SV2 failover event still leaks the raw fallback.url"
        );
        assert!(
            !main.contains("format!(\"{}:{}\", config.stratum.url, config.stratum.port)"),
            "main.rs OLED pool_display still leaks the raw config.stratum.url"
        );
        // SV2 wire worker stays raw (handshake auth).
        assert!(
            main.contains("worker: config.worker_name.clone()"),
            "main.rs Sv2Config must keep the REAL worker (write/handshake path)"
        );

        // ── provisioning.rs worker-validation warn masks the BTC payout prefix ──
        assert!(
            prov.contains("crate::shared::mask_wallet(addr_part)"),
            "provisioning.rs worker-validation warn must mask the worker prefix"
        );
        assert!(
            !prov.contains("pool worker format.\", addr_part"),
            "provisioning.rs worker-validation warn still logs the raw worker prefix"
        );
    }
}

/// CAP-OS2AXE-1 / CAP-OS2AXE-3 Phase-3 capability-port structural guards
/// (`axe-ports` lane).
///
/// Phase 3 ported two OS→axe READ-ONLY capability surfaces INLINE into
/// `dashboard.rs` (the autotuner evidence tiles + the Network card's halving
/// countdown + mempool fee radial). `dashboard.rs` is espidf-only and CANNOT
/// host-compile, so — exactly like `dcent_design_language_guards` — these pin
/// the load-bearing invariants of the new inline surfaces against the source
/// TEXT via `include_str!`. This is the host-testable durability mechanism that
/// keeps a future edit from (a) silently stripping the truth-contract honesty
/// copy, or (b) converting the deliberately handler-FREE inline widgets into
/// register_static-spending modular files (the embedded OTA-slot/handler-budget
/// rule from the nav contract).
#[cfg(test)]
mod dashboard_evidence_guards {
    const DASHBOARD_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── CAP-OS2AXE-1: last_good_* tiles carry the "persisted, not live-proven"
    //    honesty label (data-model-fields §7.2). Softening this to imply a live
    //    setpoint would breach the truth-contract. ──────────────────────────────
    #[test]
    fn autotuner_last_good_tiles_are_labeled_persisted_not_live_proven() {
        let flat = collapse_ws(DASHBOARD_RS);
        assert!(
            flat.contains("persisted, not live-proven"),
            "CAP-OS2AXE-1: the last_good_* evidence tiles MUST carry the \
             'persisted, not live-proven' honesty label (data-model-fields §7.2)"
        );
        // The render reads the persisted last-known-good fields (presentation of
        // existing AutotunerView data — no new wire field).
        for field in [
            "lastGoodFrequency",
            "lastGoodVoltageMv",
            "lastGoodJth",
            "lastGoodErrorRate",
        ] {
            assert!(
                flat.contains(field),
                "CAP-OS2AXE-1: the evidence render must read at.{field} (existing AutotunerView field)"
            );
        }
    }

    // ── CAP-OS2AXE-1: the Best Efficiency evidence row is a MEASURED receipt,
    //    distinct from the DERIVED grade and the PERSISTED last_good_* point. It
    //    reads the existing AutotunerView.bestEfficiency field and carries the
    //    "measured … J/TH, lower is better" honesty label so it can't be confused
    //    with a derived/persisted value (data-model-fields §7.x truth split). A
    //    regression dropping this row or its label would not be caught by host
    //    compilation (dashboard.rs is espidf-only). ──────────────────────────────
    #[test]
    fn best_efficiency_row_reads_measured_field_and_is_labeled_measured() {
        let flat = collapse_ws(DASHBOARD_RS);
        // Reads the existing AutotunerView.bestEfficiency wire field (camelCase
        // serde of best_efficiency) — no new field invented.
        assert!(
            flat.contains("at.bestEfficiency"),
            "CAP-OS2AXE-1: the evidence render must read at.bestEfficiency \
             (existing AutotunerView field), not a fabricated one"
        );
        // Carries the MEASURED honesty label — distinct from the DERIVED grade and
        // the PERSISTED last_good_* point — so the three truth classes don't blur.
        assert!(
            flat.contains("measured &mdash; J/TH, lower is better")
                || flat.contains("measured — J/TH, lower is better"),
            "CAP-OS2AXE-1: the Best Efficiency row MUST carry the \
             'measured — J/TH, lower is better' honesty label"
        );
        // The render is wired through the uniquely-named evidence fn (so it can't
        // shadow a dashboard/*.js window.* global per the wiring contract).
        assert!(
            flat.contains("function renderAutotunerEvidence(at)"),
            "CAP-OS2AXE-1: the evidence rows must render through renderAutotunerEvidence(at)"
        );
    }

    // ── CAP-OS2AXE-1: silicon_grade is labeled DERIVED (not a factory bin) and
    //    has an honest "unknown"/em-dash empty state (data-model-fields §7.1). ──
    #[test]
    fn silicon_grade_is_labeled_derived_with_honest_unknown() {
        let flat = collapse_ws(DASHBOARD_RS);
        assert!(
            flat.contains("derived (measured error-rate/nonce), not factory bin"),
            "CAP-OS2AXE-1: silicon_grade MUST be labeled derived (measured \
             error-rate/nonce), NOT a factory bin (data-model-fields §7.1)"
        );
        assert!(
            flat.contains("siliconGrade"),
            "CAP-OS2AXE-1: the evidence render must read at.siliconGrade"
        );
        // Honest empty state: the 'unknown'/'?' grade renders as the em-dash, it
        // is NOT fabricated into a letter.
        assert!(
            flat.contains("g!=='unknown'"),
            "CAP-OS2AXE-1: an 'unknown' silicon_grade must fall back to the honest \
             em-dash empty state, never a fabricated grade"
        );
    }

    // ── CAP-OS2AXE-3: the mempool fee radial uses the mempool.space cross-origin
    //    browser fetch (NOT a new local /api handler) — handler-FREE by contract. ─
    #[test]
    fn mempool_radial_uses_cross_origin_fetch_not_a_local_handler() {
        assert!(
            DASHBOARD_RS.contains("https://mempool.space/api/v1/fees/recommended"),
            "CAP-OS2AXE-3: the mempool radial MUST use the mempool.space cross-origin \
             browser fetch (the block-tile.js pattern), costing NO firmware handler"
        );
        // It must NOT introduce a new local /api fee route (which would imply a
        // firmware register_static/fn_handler cost).
        assert!(
            !DASHBOARD_RS.contains("/api/network/fees") && !DASHBOARD_RS.contains("/api/mempool"),
            "CAP-OS2AXE-3: the mempool radial must NOT add a new local /api fee handler"
        );
        // Fail-silent empty-state honesty: the em-dash render is the default.
        assert!(
            DASHBOARD_RS.contains("fail-silent"),
            "CAP-OS2AXE-3: the mempool fetch must fail SILENTLY to the em-dash empty state"
        );
    }

    // ── CAP-OS2AXE-3: the halving bar reads the ALREADY-CLIENT-SIDE block height
    //    (d.blockHeight) — no fetch, no new wire field, no handler. ─────────────
    #[test]
    fn halving_bar_reads_block_height_no_new_wire_field() {
        let flat = collapse_ws(DASHBOARD_RS);
        // The countdown is driven by the existing client-side block height passed
        // into renderHalvingCountdown(bh) where bh = d.blockHeight.
        assert!(
            flat.contains("renderHalvingCountdown(bh)"),
            "CAP-OS2AXE-3: the halving countdown must be driven by the existing \
             client-side block height (renderHalvingCountdown(bh))"
        );
        assert!(
            flat.contains("var bh=d.blockHeight"),
            "CAP-OS2AXE-3: bh must come from the already-client-side d.blockHeight \
             (no new wire field)"
        );
        // The OS HalvingTimelineBar math is ported faithfully (210000-block epoch).
        assert!(
            flat.contains("_HALVING_INTERVAL=210000"),
            "CAP-OS2AXE-3: the halving math must use the 210000-block interval"
        );
    }

    // ── Budget guardrail: the Phase-3 inline ports add NO new register_static
    //    handler and NO new modular dashboard file. The two landed widgets are
    //    handler-FREE; a regression that introduces a new dashboard/network.js
    //    (the rejected alternative) would bump the handler count toward the cap. ─
    #[test]
    fn phase3_ports_add_no_new_register_static_handler() {
        // No network.js asset was baked + served (the deferred/rejected
        // alternative). The handler-spending forms are an `include_str!` of the
        // file and a `register_static`/served route for it — NOT a mention of the
        // filename in a comment. Match the concrete handler-cost forms only so the
        // explanatory comment ("no new dashboard/network.js") doesn't self-trip.
        assert!(
            !DASHBOARD_RS.contains("include_str!(\"dashboard/network.js\")")
                && !DASHBOARD_RS.contains("include_str!(\"dashboard/network.css\")"),
            "BUDGET: Phase-3 must NOT include_str! a new network dashboard asset \
             (a register_static handler cost) — the network widgets are INLINE"
        );
        assert!(
            !DASHBOARD_RS.contains("register_static(server, \"/dashboard/network.js\"")
                && !DASHBOARD_RS.contains("register_static(server, \"/dashboard/network.css\""),
            "BUDGET: Phase-3 must NOT register_static a /dashboard/network.* route \
             (a new URI handler) — the network widgets are handler-free inline"
        );
        assert!(
            !DASHBOARD_RS.contains("DASH_JS_NETWORK") && !DASHBOARD_RS.contains("DASH_CSS_NETWORK"),
            "BUDGET: Phase-3 must NOT bake a new network dashboard asset const (handler cost)"
        );
    }
}

/// Phase-4 UX-FLOW VOCABULARY structural guards (`axe-flows` lane).
///
/// Phase 4 made axe's flow SURFACES speak the shared flow vocabulary
/// (`flows-and-patterns.md`, `terminology-lexicon.md`, `component-contract.md §7`).
/// Every edit was an additive, INLINE label/string touch-up inside an existing
/// `dashboard.rs` element — NO new register_static handler, NO new modular file,
/// NO new function that could shadow a `dashboard/*.js` window.* global, NO Rust
/// control-flow change (the HTML/JS lives inside Rust raw-string literals, so the
/// edits are syntax-safe by construction). `dashboard.rs` is espidf-only and
/// CANNOT host-compile, so — exactly like `dcent_design_language_guards` and
/// `dashboard_evidence_guards` — these pin the load-bearing flow-vocabulary
/// invariants against the source TEXT via `include_str!`. This is the only CI
/// mechanism that proves the Phase-4 flow vocabulary (and the keep-DISTINCT /
/// keep-axe-floor identity guardrails) has not regressed.
///
/// Matches are deliberately whitespace-robust (substring on a whitespace-
/// collapsed copy), never byte-exact blocks, so a benign HTML/JS reflow or
/// minify cannot flip a guard.
#[cfg(test)]
mod dcent_flow_vocab_guards {
    const DASHBOARD_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── (a) UXFLOW-SAFETY-1: the fan-safety honesty vocabulary is present ──────
    // The "No tach" / "Unproved" RPM-proof readout + the three canonical PWM zone
    // labels + the cut-hash-before-noise posture phrasing (glossary key
    // cut_hash_before_noise). These are LABEL-only — the cut-hash-before-fan
    // BEHAVIOR lives in main.rs (XPSAFE-5 / HALT thermal tiers, host-pinned in
    // `thermal_ladder_guards`), NOT here. A regression that strips the honesty
    // labels (e.g. faking a 0 RPM instead of "No tach") would not be caught by
    // host compilation (dashboard.rs is espidf-only).
    #[test]
    fn fan_safety_honesty_vocabulary_present() {
        let flat = collapse_ws(DASHBOARD_RS);
        // rpm==0 ⇒ literal "No tach" (never a fake 0), and the speed-proof token.
        assert!(
            flat.contains("'No tach'")
                || flat.contains("\"No tach\"")
                || flat.contains(">No tach<"),
            "UXFLOW-SAFETY-1: rpm==0 must render the literal 'No tach' (never a fake 0 RPM)"
        );
        assert!(
            flat.contains("'Unproved'")
                || flat.contains("\"Unproved\"")
                || flat.contains(">Unproved<"),
            "UXFLOW-SAFETY-1: the fan speed-proof token 'Unproved' (rpm==0) must be present"
        );
        // The three canonical PWM zone labels (component-contract §7).
        for zone in ["Home cap", "Loud override", "Thermal override"] {
            assert!(
                flat.contains(zone),
                "UXFLOW-SAFETY-1: the canonical PWM zone label `{zone}` must be present"
            );
        }
        // The cut-hash-before-noise posture phrasing (glossary cut_hash_before_noise).
        assert!(
            flat.contains("cut hash before noise"),
            "UXFLOW-SAFETY-1: the 'cut hash before noise' posture phrasing must surface \
             (LABEL only — the behavior lives in main.rs)"
        );
    }

    // ── (b) UXFLOW-SAFETY-1 identity guard: axe keeps its OWN fan floor (20) ───
    // The shared vocabulary is adopted at axe's floor, NOT OS's 10-30 home clamp.
    // The manual fan slider must stay min=20 max=100; importing OS's numeric clamp
    // is an explicit keep-unique violation (keep-unique-guardrails §8).
    #[test]
    fn fan_slider_keeps_axe_floor_not_os_clamp() {
        let flat = collapse_ws(DASHBOARD_RS);
        // The slider keeps its axe floor (min=20) and ceiling (max=100).
        assert!(
            flat.contains("id=\"fanSlider\" min=20 max=100")
                || flat.contains("type=range id=\"fanSlider\" min=20 max=100"),
            "UXFLOW-SAFETY-1: the manual fan slider must keep axe's own floor (min=20 max=100), \
             NOT be re-clamped to OS's 10-30 home range"
        );
        // The derived zone label is wired through the uniquely-named inline helper
        // (fanZoneLabel) — a NEW name that does NOT collide with any dashboard/*.js
        // window.* global, preserving the wiring contract.
        assert!(
            flat.contains("function fanZoneLabel("),
            "UXFLOW-SAFETY-1: the PWM zone label must render through the uniquely-named \
             fanZoneLabel() helper (wiring-contract-safe)"
        );
        // a11y: the zone is mirrored into aria-valuetext (component-contract §7).
        assert!(
            flat.contains("aria-valuetext"),
            "UXFLOW-SAFETY-1: the fan slider must expose the zone via aria-valuetext (a11y parity)"
        );
    }

    // ── (c) UXFLOW-POOL-1: the 3 canonical pool ROUTING labels are present ─────
    // axe's failbackStatusText already emits the shared routing vocabulary
    // (Primary ready (job proof) / Primary route entered / Fallback active). Pin
    // them so a relabel can't silently break cross-firmware pool-vocabulary
    // parity. Difficulty honesty: the Pool-Target-vs-Achieved phrasing is on the
    // Share Target tooltips.
    #[test]
    fn pool_routing_and_difficulty_vocabulary_present() {
        let flat = collapse_ws(DASHBOARD_RS);
        for label in [
            "Primary ready (job proof)",
            "Primary route entered",
            "Fallback active",
        ] {
            assert!(
                flat.contains(label),
                "UXFLOW-POOL-1: the canonical pool routing label `{label}` must survive \
                 in failbackStatusText"
            );
        }
        // Pool-Target-vs-Achieved difficulty honesty (lexicon §4.1): the Share
        // Target rows carry the 'pool-required minimum, not ... Achieved' tooltip.
        assert!(
            flat.contains("Pool Target Difficulty") && flat.contains("Achieved Difficulty"),
            "UXFLOW-POOL-1: the Share Target honesty tooltip must distinguish Pool Target \
             Difficulty (vardiff minimum) from Achieved Difficulty"
        );
    }

    // ── (d) UXFLOW-OTA-1 (ratify): the Upload≠boot≠rollback proof ladder + the
    //    manifest-preflight vocabulary survive (RALPH 9D8 OTA-UX contract). ─────
    #[test]
    fn ota_proof_ladder_vocabulary_present() {
        let flat = collapse_ws(DASHBOARD_RS);
        // Upload-not-Flash framing + the boot-proof-pending success copy.
        assert!(
            flat.contains("Upload accepted; boot proof pending."),
            "UXFLOW-OTA-1: the OTA success copy must be 'Upload accepted; boot proof pending.'"
        );
        // The operator is told to refresh device status, never to assume reboot.
        assert!(
            flat.contains("refresh device status"),
            "UXFLOW-OTA-1: an unparseable OTA response must tell the operator to \
             'refresh device status' (not infer reboot/success)"
        );
        // The compact manifest-preflight tokens (RALPH 9D8).
        for token in ["slot_fit", "sha_status", "target preflight required"] {
            assert!(
                flat.contains(token),
                "UXFLOW-OTA-1: the OTA manifest-preflight token `{token}` must survive"
            );
        }
    }

    // ── (e) UXFLOW-RESET-1 (keep DISTINCT): the owner-reset keeps-X disclosure
    //    copy survives. axe owner-reset KEEPS WiFi + pool config and is session-
    //    gated; it must NOT converge with OS restore-to-stock (which wipes to
    //    stock). Pin the keeps-X disclosure so the keep-distinct boundary holds. ─
    #[test]
    fn owner_reset_keeps_distinct_disclosure_present() {
        let flat = collapse_ws(DASHBOARD_RS);
        assert!(
            flat.contains("Keeps WiFi + pool config") || flat.contains("keeps WiFi + pool config"),
            "UXFLOW-RESET-1: the owner-reset disclosure must state it KEEPS WiFi + pool config \
             (keep-DISTINCT from OS restore-to-stock, which wipes to stock)"
        );
        // It is session-gated (an active owner session), not a physical-access
        // bypass — the keep-unique auth posture (do not regress).
        assert!(
            flat.contains("Requires an active owner session"),
            "UXFLOW-RESET-1: owner-reset must remain session-gated (active owner session), \
             not a claim_skip-style bypass"
        );
    }

    // ── (f) UXFLOW-TUNE-1: an absent tuner status renders the canonical
    //    'Unavailable' honesty word, never the old bare 'idle' (the OS
    //    valueOrUnavailable convention). The 4 mode names stay canonical (already
    //    pinned in `dcent_design_language_guards`); this pins the run-status
    //    honesty fallback only. ──────────────────────────────────────────────────
    #[test]
    fn tuner_null_status_renders_unavailable_not_bare_idle() {
        let flat = collapse_ws(DASHBOARD_RS);
        // The null/empty fallback is the canonical 'Unavailable'.
        assert!(
            flat.contains("at.status||'Unavailable'")
                || flat.contains("at.status || 'Unavailable'"),
            "UXFLOW-TUNE-1: an absent tuner status must fall back to the canonical \
             'Unavailable', not a bare 'idle'"
        );
        // The default markup span is also 'Unavailable', not the old 'idle'.
        assert!(
            flat.contains("id=\"atStatus\">Unavailable<"),
            "UXFLOW-TUNE-1: the tuner Status span must default to 'Unavailable'"
        );
        // The old bare 'idle' fallback must be gone from the tuner status path
        // (the '||\\'idle\\'' fallback specifically — unrelated 'idle' words in
        // other contexts are not matched by this exact form).
        assert!(
            !flat.contains("at.status||'idle'") && !flat.contains("at.status || 'idle'"),
            "UXFLOW-TUNE-1: the old bare 'idle' tuner-status fallback must be gone"
        );
    }
}

/// S4 `axe-ports` capability-port guards (`axe-ports` lane).
///
/// The S4 second pass ported two DCENT_OS capabilities to axe as ADDITIVE,
/// identity-preserving, handler-free inline surfaces in `dashboard.rs`:
///   • CAP-OS2AXE-2 — a LITE vanilla-SVG fan-curve editor (3-point temp->PWM)
///     over axe's two-scalar fan model, and
///   • CAP-OS2AXE-6 — a LITE 3-4 step first-run wizard overlay.
///
/// Because `dashboard.rs` is espidf-only and CANNOT host-compile, these — exactly
/// like `dcent_flow_vocab_guards` — pin the load-bearing port invariants against
/// the source TEXT via `include_str!`. They are the only CI mechanism that proves:
///   (G-FANCURVE-1)     the fan-curve surface exists, clamps PWM to axe's OWN floor
///                      20..100, and does NOT import OS's 10-30 home clamp (keep-unique);
///   (G-FANCURVE-AUTH)  the fan-curve WRITE is owner-auth-gated — it routes through the
///                      CSRF+Bearer `post('/api/system'…)` helper (which itself routes
///                      `ensureWriteAuth()`+`authHeaders()` -> `authorize_rest_write`),
///                      with NO new bare/unauth mutation route. axe is
///                      FAIL-CLOSED-CONTROL-ON-OPEN on EVERY build, so a CONTROL write
///                      MUST never be an unauthenticated POST;
///   (G-WIZARD-1)       the first-run overlay carries the 4 canonical step labels +
///                      the freedom-first "recommended, not required" / opt-out copy;
///   (G-WIZARD-SAFETY-ACK) the Welcome (step 0) screen carries the explicit
///                      safety-acknowledgement checkbox (id=frSafetyAck) stating the
///                      cut-hash-before-fan-noise home-heater posture, surfaced BEFORE
///                      the pool/mode steps;
///   (G-WIZARD-KEEPUNIQUE) the overlay DROPS the OS-only industrial steps
///                      (Circuit / PSU / Calibration / Power source) — scoped to the
///                      WIZ-FR-START..WIZ-FR-END region so the descriptive comment that
///                      *names* the dropped steps doesn't false-positive;
///   (G-S4-BUDGET)      BOTH ports are handler-FREE: no new register_static route, no
///                      new modular dashboard/*.{js,css} file, and the named handler-
///                      count constants (REGISTERED_HANDLER_ESTIMATE 72 / MAX_URI_
///                      HANDLERS 96) stay untouched — they reuse the existing auth-gated
///                      post('/api/system') write (the embedded-route budget HARD rule).
///
/// Matches are whitespace-robust (substring on a whitespace-collapsed copy), never
/// byte-exact blocks, so a benign HTML/JS reflow cannot flip a guard.
#[cfg(test)]
mod s4_axe_capability_port_guards {
    const DASHBOARD_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard.rs"
    ));
    // main.rs owns the embedded-route budget constants (MAX_URI_HANDLERS /
    // REGISTERED_HANDLER_ESTIMATE). It is espidf-only and cannot host-compile, so
    // the budget guard pins those constants against the source TEXT to prove the
    // S4 ports added ZERO handlers (the embedded-route budget is a HARD rule).
    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Scope helper: the wizard markup between the WIZ-FR-START..WIZ-FR-END markers
    /// (collapse_ws keeps the comment markers intact — no internal whitespace).
    fn wizard_region(flat: &str) -> &str {
        let start = flat
            .find("WIZ-FR-START")
            .expect("CAP-OS2AXE-6: WIZ-FR-START marker must be present");
        let end = flat
            .find("WIZ-FR-END")
            .expect("CAP-OS2AXE-6: WIZ-FR-END marker must be present");
        assert!(
            end > start,
            "CAP-OS2AXE-6: WIZ-FR markers must be ordered (START before END)"
        );
        &flat[start..end]
    }

    // ── (G-FANCURVE-1) the fan-curve surface exists + keeps axe's OWN 20..100 floor ──
    #[test]
    fn fan_curve_surface_clamps_to_axe_floor_not_os_clamp() {
        let flat = collapse_ws(DASHBOARD_RS);
        // The inline vanilla-SVG editor surface (unique marker) is present.
        assert!(
            flat.contains("id=\"fanCurveSvg\""),
            "CAP-OS2AXE-2: the inline fan-curve SVG surface (id=\"fanCurveSvg\") must be present"
        );
        assert!(
            flat.contains("function fanCurveApply(") && flat.contains("function fanCurveRender("),
            "CAP-OS2AXE-2: the fan-curve apply/render helpers must be present"
        );
        // PWM axis clamps to axe's OWN floor (20) and ceiling (100) — the drag/load
        // clamp `Math.max(20,Math.min(100,…))`.
        assert!(
            flat.contains("Math.max(20,Math.min(100,"),
            "CAP-OS2AXE-2: the fan-curve PWM axis must clamp to axe's OWN floor 20..100"
        );
        // KEEP-UNIQUE (component-contract §7 / keep-unique §8): OS's 10-30 home clamp
        // must NOT be pushed onto axe, and the OS multi-point editor's identifiers
        // (PWM_MAX=30) must not appear.
        assert!(
            !flat.contains("Math.min(30,"),
            "CAP-OS2AXE-2: axe must NOT re-clamp the curve to OS's 10-30 home range"
        );
        assert!(
            !flat.contains("PWM_MAX"),
            "CAP-OS2AXE-2: the OS FanCurveEditor's PWM_MAX (0-30 cap) identifier must NOT leak \
             into axe — axe keeps its own floor (20)"
        );
        // Honesty: the lite editor must label itself as NOT a per-point firmware curve
        // (that stays OS-only), so it never implies per-point firmware control.
        assert!(
            flat.contains("per-point firmware curve"),
            "CAP-OS2AXE-2: the lite editor must be labelled honestly (NOT a per-point firmware \
             curve — that stays DCENT_OS-only per component-contract §7)"
        );
    }

    // ── (G-FANCURVE-AUTH) the fan-curve WRITE is owner-auth-gated (structural) ──────
    // axe is FAIL-CLOSED-CONTROL-ON-OPEN on EVERY build. The fan-curve apply is a
    // CONTROL surface, so its WRITE MUST route through the CSRF+Bearer `post()` helper
    // (-> ensureWriteAuth + authHeaders -> authorize_rest_write), never a bare/unauth
    // mutation and never a NEW route.
    #[test]
    fn fan_curve_write_requires_owner_auth() {
        let flat = collapse_ws(DASHBOARD_RS);
        // The apply writes through the auth-gated helper on the EXISTING /api/system
        // route (the same gate setFanAuto uses), carrying the derived knee + floor.
        assert!(
            flat.contains("post('/api/system',{fanMode:'auto',autofanspeed:1,fanTargetTemp:"),
            "CAP-OS2AXE-2: the fan-curve apply must write the derived knee/floor through the \
             auth-gated post('/api/system',…) helper (NOT a bare/unauth mutation)"
        );
        // The post() helper itself ALWAYS routes ensureWriteAuth() + authHeaders()
        // (CSRF X-Requested-With + Bearer) before the POST — this is the structural
        // proof that every post() write (incl. the fan curve) requires owner-auth.
        assert!(
            flat.contains(
                "ensureWriteAuth().then(function(){return fetch(u,{method:'POST',headers:authHeaders("
            ),
            "CAP-OS2AXE-2: post() must gate every write through ensureWriteAuth()+authHeaders() \
             (authorize_rest_write) — the fan-curve CONTROL write inherits this owner-auth gate"
        );
        // No NEW unauthenticated fan/curve mutation route was introduced (+0 handlers).
        assert!(
            !flat.contains("/api/fan-curve") && !flat.contains("/api/fancurve"),
            "CAP-OS2AXE-2: the fan-curve must introduce NO new (unauth) route — it reuses the \
             existing auth-gated /api/system route (+0 URI handlers)"
        );
    }

    // ── (G-WIZARD-1) the first-run overlay carries the canonical steps + opt-out ────
    #[test]
    fn first_run_wizard_steps_and_freedom_copy_present() {
        let flat = collapse_ws(DASHBOARD_RS);
        let region = wizard_region(&flat);
        // The 3-4 step overlay surface + nav wiring.
        assert!(
            flat.contains("id=\"frOverlay\"") && flat.contains("function firstRunApply("),
            "CAP-OS2AXE-6: the first-run overlay (#frOverlay) + apply path must be present"
        );
        // The 4 canonical step labels in OS step ORDERING (Welcome+safety-ack ->
        // Pool/worker -> Mode/heater-target -> Review).
        for label in ["Welcome", "Pool / Worker", "Heater Target", "Review"] {
            assert!(
                region.contains(label),
                "CAP-OS2AXE-6: the canonical first-run step label `{label}` must be present"
            );
        }
        // The freedom-first opt-out copy ("recommended, not required") + a dismiss path.
        assert!(
            region.contains("recommended, not required"),
            "CAP-OS2AXE-6: the freedom-first 'recommended, not required' opt-out copy must survive"
        );
        assert!(
            region.contains("Skip for now") && flat.contains("function firstRunSkip("),
            "CAP-OS2AXE-6: the wizard must stay dismissible ('Skip for now' / firstRunSkip)"
        );
        // Final Apply reuses the EXISTING auth-gated post('/api/system') write (+0 handlers).
        assert!(
            flat.contains("function firstRunApply(") && flat.contains("post('/api/system',b,"),
            "CAP-OS2AXE-6: the wizard Apply must reuse the auth-gated post('/api/system') write"
        );
    }

    // ── (G-WIZARD-KEEPUNIQUE) the overlay DROPS the OS-only industrial steps ────────
    // Scoped to the WIZ-FR-START..WIZ-FR-END region: axe stays a home device — the
    // industrial commissioning steps (Circuit NEC-derate / PSU override / Calibration
    // / Power source) must NOT appear inside the overlay (keep-unique §4.7). The
    // descriptive comment that *names* them lives OUTSIDE the markers on purpose.
    #[test]
    fn first_run_wizard_drops_os_industrial_steps() {
        let flat = collapse_ws(DASHBOARD_RS);
        let region = wizard_region(&flat);
        for banned in ["Circuit", "Calibration", "PSU", "Power source"] {
            assert!(
                !region.contains(banned),
                "CAP-OS2AXE-6 keep-unique §4.7: the OS-only industrial step `{banned}` must NOT \
                 appear in the axe first-run overlay (a 14-step commissioning rail is wrong UX \
                 for a home BitAxe)"
            );
        }
    }

    // ── (G-WIZARD-SAFETY-ACK) the wizard's Welcome step carries the safety-ack ──────
    // The first-run overlay's Welcome screen (step 0) MUST present the explicit
    // safety acknowledgement — the cut-hash-before-fan-noise home-heater posture
    // checkbox (id=frSafetyAck) — surfaced BEFORE the pool/mode steps so the safety
    // contract is shown first. A regression that dropped the ack (or softened its
    // posture phrasing) would not be caught by host compilation (dashboard.rs is
    // espidf-only). Scoped to the WIZ-FR region so it is unambiguously the wizard.
    #[test]
    fn first_run_wizard_has_safety_ack_step() {
        let flat = collapse_ws(DASHBOARD_RS);
        let region = wizard_region(&flat);
        // The safety-acknowledgement checkbox lives inside the wizard region.
        assert!(
            region.contains("id=\"frSafetyAck\""),
            "CAP-OS2AXE-6: the first-run wizard must carry the safety-ack checkbox \
             (id=\"frSafetyAck\")"
        );
        // It states the load-bearing cut-hash-before-fan-noise home-heater posture
        // (LABEL only — the behavior lives in main.rs XPSAFE-5 thermal tiers).
        assert!(
            region.contains("cuts hash before raising fan noise"),
            "CAP-OS2AXE-6: the safety-ack step must state the 'cuts hash before raising fan \
             noise' home-heater posture"
        );
        // The ack sits on the Welcome step (data-fr-step=0), BEFORE the Pool/Worker
        // step (data-fr-step=1) — the safety contract is surfaced first.
        let step0 = region
            .find("data-fr-step=\"0\"")
            .expect("CAP-OS2AXE-6: the wizard must have a Welcome step (data-fr-step=0)");
        let step1 = region
            .find("data-fr-step=\"1\"")
            .expect("CAP-OS2AXE-6: the wizard must have a Pool/Worker step (data-fr-step=1)");
        let ack = region
            .find("id=\"frSafetyAck\"")
            .expect("safety-ack present (checked above)");
        assert!(
            step0 < ack && ack < step1,
            "CAP-OS2AXE-6: the safety-ack must sit on the Welcome step (between step 0 and \
             step 1), surfaced before pool/mode setup"
        );
    }

    // ── (G-S4-BUDGET) the fan-curve editor + wizard stay within the wiring/handler
    //    budget. BOTH are handler-FREE inline surfaces: NO new register_static route,
    //    NO new modular dashboard/*.{js,css} file, and the named handler-count
    //    constants are UNTOUCHED (proof S4 added 0 handlers). The embedded-route
    //    budget is a HARD rule (nav-IA contract); a regression that broke either
    //    surface into a served asset would bump the count toward the 96 cap and is
    //    invisible to host compilation (main.rs/dashboard.rs are espidf-only). ────────
    #[test]
    fn s4_ports_stay_within_wiring_and_handler_budget() {
        let flat = collapse_ws(DASHBOARD_RS);
        // No served route for a fan-curve OR wizard asset (the handler-spending form).
        for route in [
            "/dashboard/fan-curve.js",
            "/dashboard/fancurve.js",
            "/dashboard/fan-curve.css",
            "/dashboard/wizard.js",
            "/dashboard/wizard.css",
            "/dashboard/first-run.js",
        ] {
            assert!(
                !DASHBOARD_RS.contains(route),
                "G-S4-BUDGET: the S4 ports must NOT serve a `{route}` asset (a register_static / \
                 fn_handler cost) — the fan-curve + wizard are handler-free INLINE surfaces"
            );
        }
        // No new include_str! of a fan-curve/wizard module file (baking a served asset).
        for asset in [
            "include_str!(\"dashboard/fan-curve.js\")",
            "include_str!(\"dashboard/fancurve.js\")",
            "include_str!(\"dashboard/wizard.js\")",
            "include_str!(\"dashboard/first-run.js\")",
        ] {
            assert!(
                !flat.contains(asset),
                "G-S4-BUDGET: the S4 ports must NOT include_str! a new dashboard module `{asset}` \
                 (a register_static handler cost)"
            );
        }
        // Both surfaces reuse the EXISTING auth-gated /api/system write (+0 routes):
        // the fan-curve apply and the wizard Apply both post('/api/system', …).
        assert!(
            flat.contains("post('/api/system',{fanMode:'auto',autofanspeed:1,fanTargetTemp:"),
            "G-S4-BUDGET: the fan-curve apply must reuse the existing auth-gated \
             post('/api/system') write (+0 handlers)"
        );
        assert!(
            flat.contains("post('/api/system',b,"),
            "G-S4-BUDGET: the wizard Apply must reuse the existing auth-gated \
             post('/api/system') write (+0 handlers)"
        );
        // The named handler-count constants are UNCHANGED — proof S4 added 0 handlers
        // and the embedded-route budget is intact (REGISTERED_HANDLER_ESTIMATE < cap).
        assert!(
            MAIN_RS.contains("const MAX_URI_HANDLERS: usize = 96;"),
            "G-S4-BUDGET: MAX_URI_HANDLERS must stay 96 (S4 ports add no handler)"
        );
        assert!(
            MAIN_RS.contains("const REGISTERED_HANDLER_ESTIMATE: usize = 73;"),
            "G-S4-BUDGET: REGISTERED_HANDLER_ESTIMATE must stay 73 (S4 fan-curve + wizard are \
             handler-free inline surfaces, +0 routes)"
        );
    }
}

/// S5 `axe-superset` cross-firmware MCP tool-name alignment guards (convergence
/// Phase S5, `axe-superset` lane).
///
/// S5 unifies the MCP control vocabulary across DCENT_axe and DCENT_OS on the
/// `dcent.cross-firmware.tuning.v1` superset (dcent-schema `tuning_profile()`):
/// the 6-tool minimal kernel PLUS 6 richer tuning EXTENSIONS — READS
/// `get_network`/`get_history`; CONTROL `set_frequency`/`set_core_voltage`/
/// `set_fan_speed`/`run_autotune`. axe is the RICH control side and was ALREADY
/// fully superset-conformant before S5: `tools_list_descriptor()` emits all 12
/// canonical names, the dispatch ACCEPTS the 2 kernel legacy aliases inbound-only
/// (`get_asic_info`->`get_device_info`, `get_swarm`->`get_swarm_status`), and
/// `mcp_tool_requires_control` already gates exactly the 7 CONTROL names. So S5
/// applied ZERO renames and ZERO new URI handlers on axe — its deliverable is
/// THIS durability pin.
///
/// `dcentaxe/src/mcp.rs` (and `main.rs`) are the espidf-only binary-crate sources
/// and CANNOT host-compile, so — exactly like `mcp_auth_contract_guards` /
/// `s4_axe_capability_port_guards` — these guards pin the load-bearing facts
/// against the source TEXT via `include_str!` (whitespace-robust substring on a
/// collapsed copy; additive, never byte-exact blocks). They make a future edit
/// that (a) drops a canonical superset name, (b) mis-classes a tuning CONTROL
/// tool OUT of the owner-auth gate (the AOTA-class fail-open regression), (c)
/// wrongly write-gates a READ tool, or (d) spends a URI handler to add a tool,
/// a RED test runnable under `cargo +stable test -p dcentaxe-core --lib`.
///
/// This is a NEW module (S5) added ALONGSIDE `s4_axe_capability_port_guards`; it
/// touches neither S4 nor `mcp_auth_contract_guards`.
#[cfg(test)]
mod s5_axe_mcp_superset_guards {
    const MCP_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/mcp.rs"
    ));
    // main.rs owns the embedded-route budget constants; it is espidf-only and
    // cannot host-compile, so the budget guard pins them against the source TEXT
    // to prove the S5 name alignment added ZERO handlers (HARD route-budget rule).
    const MAIN_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/main.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The 12-row FROZEN `tuning_profile()` superset (kernel 6 first, then the 6
    /// extensions in reads-then-writes tail order) — the EXACT order/contents the
    /// dcent-schema source of truth pins (`tuning_profile_tool_names_are_locked`).
    const SUPERSET_NAMES: [&str; 12] = [
        "get_status",
        "get_device_info",
        "get_swarm_status",
        "identify_device",
        "restart_mining",
        "set_pool",
        "get_network",
        "get_history",
        "set_frequency",
        "set_core_voltage",
        "set_fan_speed",
        "run_autotune",
    ];
    /// The 6 superset EXTENSIONS (rows 7-12) split by auth class — what S5 layers
    /// on top of the minimal kernel.
    const EXTENSION_READS: [&str; 2] = ["get_network", "get_history"];
    const EXTENSION_CONTROLS: [&str; 4] = [
        "set_frequency",
        "set_core_voltage",
        "set_fan_speed",
        "run_autotune",
    ];

    /// Window the `mcp_tool_requires_control` matches!() body (generous 400-char
    /// bound; the gate is a short fn) — the same scoping the sibling
    /// `mcp_auth_contract_guards::control_gate_lists_exactly_the_seven_write_tools`
    /// guard uses. `find("fn mcp_tool_requires_control")` matches the DEFINITION
    /// (the call sites have no `fn ` prefix), so the window is the gate body.
    fn control_gate_region(flat: &str) -> String {
        let gate = flat
            .find("fn mcp_tool_requires_control")
            .expect("mcp.rs must define the read/control split gate mcp_tool_requires_control");
        // Bound at the NEXT sibling `fn ` rather than a fixed +400 slice: the
        // sibling `fn mcp_tool_writes_hardware` lists the SAME control names, so a
        // future edit that shrinks the inter-fn doc comment could let a fixed
        // window spill into it and FALSE-PASS the auth-class assertions even with
        // a tuning control removed from the real gate (completeness-critic L1).
        let end = flat[gate..]
            .match_indices("fn ")
            .nth(1)
            .map(|(i, _)| gate + i)
            .unwrap_or_else(|| (gate + 400).min(flat.len()));
        flat[gate..end].to_string()
    }

    // ── (3a/3b/3c) the 12 canonical superset names are present AND each extension
    //    is in its CORRECT auth class: the 4 tuning CONTROL tools inside the
    //    owner-auth gate, the 2 tuning READ tools outside it. ─────────────────────
    #[test]
    fn tuning_superset_present_and_correctly_classed() {
        let flat = collapse_ws(MCP_RS);
        let region = control_gate_region(&flat);

        // (a) every canonical superset name is referenced by axe's mcp.rs.
        for name in SUPERSET_NAMES {
            assert!(
                flat.contains(&format!("\"{name}\"")),
                "S5: axe mcp.rs must reference canonical superset tool `{name}`"
            );
        }
        // (b) the 4 tuning CONTROL extensions sit INSIDE the owner-auth control
        //     gate — so an unauthorized read session (and an OPEN/passwordless
        //     device) is REFUSED them (the AOTA-class fail-closed posture).
        for name in EXTENSION_CONTROLS {
            assert!(
                region.contains(&format!("\"{name}\"")),
                "S5: tuning CONTROL tool `{name}` MUST be inside mcp_tool_requires_control \
                 (owner-auth gate) — else it is callable on an unauthorized/open session"
            );
        }
        // (c) the 2 tuning READ extensions are NOT control-gated (correct auth
        //     class — write-gating a read tool would force owner-auth on telemetry).
        for name in EXTENSION_READS {
            assert!(
                !region.contains(&format!("\"{name}\"")),
                "S5: READ tool `{name}` must NOT be inside the CONTROL gate (mis-classed \
                 auth — would force owner-auth on a read tool)"
            );
        }
    }

    // ── each of the 12 canonical names is EMITTED exactly once (tools/list) and
    //    DISPATCHED exactly once (inbound) — wiring integrity: no missing tool, no
    //    double-registration, no dangling alias. ──────────────────────────────────
    #[test]
    fn superset_tools_registered_exactly_once_no_dangling_alias() {
        let flat = collapse_ws(MCP_RS);
        for name in SUPERSET_NAMES {
            let emitted = flat.matches(&format!("\"name\": \"{name}\"")).count();
            assert_eq!(
                emitted, 1,
                "S5: superset tool `{name}` must be EMITTED exactly once in tools/list \
                 (found {emitted}) — no missing/duplicate descriptor"
            );
            let dispatched = flat.matches(&format!("\"{name}\" =>")).count();
            assert_eq!(
                dispatched, 1,
                "S5: superset tool `{name}` must be DISPATCHED exactly once inbound \
                 (found {dispatched}) — no dangling/duplicate match arm"
            );
        }
        // The 2 kernel legacy aliases are ACCEPTED inbound (one arm each) but NEVER
        // EMITTED — back-compat without surfacing a non-canonical name (contract §2.1).
        for alias in ["get_asic_info", "get_swarm"] {
            assert_eq!(
                flat.matches(&format!("\"{alias}\" =>")).count(),
                1,
                "S5: legacy alias `{alias}` must keep exactly one inbound dispatch arm (back-compat)"
            );
            assert!(
                !flat.contains(&format!("\"name\": \"{alias}\"")),
                "S5: legacy alias `{alias}` must NOT be emitted as a tools/list entry \
                 (accept inbound only — contract §2.1)"
            );
        }
    }

    // ── the tuning name alignment spends ZERO URI handlers: all 12 tools dispatch
    //    inside the single POST /mcp JSON-RPC handler via the `name` match, and the
    //    named handler-count constants are UNCHANGED (HARD embedded-route budget). ──
    #[test]
    fn superset_alignment_adds_zero_uri_handlers() {
        // The named budget constants are untouched (S5 = name alignment, +0 routes).
        assert!(
            MAIN_RS.contains("const MAX_URI_HANDLERS: usize = 96;"),
            "S5: MAX_URI_HANDLERS must stay 96 (MCP name alignment adds no handler)"
        );
        assert!(
            MAIN_RS.contains("const REGISTERED_HANDLER_ESTIMATE: usize = 73;"),
            "S5: REGISTERED_HANDLER_ESTIMATE must stay 73 (all 12 MCP tools share the single \
             POST /mcp handler — name alignment is handler-free)"
        );
        let flat = collapse_ws(MCP_RS);
        // No per-tool route was introduced for any tuning tool (would be a handler cost).
        for name in SUPERSET_NAMES {
            assert!(
                !flat.contains(&format!("/mcp/{name}")),
                "S5: tuning tool `{name}` must NOT get its own /mcp/{name} route — all tools \
                 dispatch inside the single POST /mcp handler (+0 URI handlers)"
            );
        }
        // The single POST /mcp registration remains the one and only MCP control route.
        assert!(
            flat.contains("\"/mcp\", Method::Post,"),
            "S5: the single POST /mcp JSON-RPC handler must remain the sole MCP control route"
        );
    }

    // ── fail-closed-control-on-OPEN holds for all 4 new tuning controls: the
    //    dispatch refuses a CONTROL tool when !control_authorized regardless of read
    //    access, the 4 tuning writes are real actuators, and freq/voltage keep the
    //    per-board hardware clamp. ─────────────────────────────────────────────────
    #[test]
    fn tuning_controls_inherit_fail_closed_open_posture() {
        let flat = collapse_ws(MCP_RS);
        // The fail-closed control dispatch structure (same pin as the sibling
        // mcp_auth_contract_guards::control_dispatch_is_fail_closed_on_unauthorized).
        assert!(
            flat.contains("if mcp_tool_requires_control(name) { if !auth.control_authorized {"),
            "S5: handle_tool_call must refuse a CONTROL tool when !control_authorized — the 4 \
             tuning writes inherit this fail-closed-on-open posture"
        );
        // Each tuning CONTROL extension actually performs a hardware/config write
        // through its tool fn (so gating it is load-bearing, not cosmetic).
        for (name, tool_fn) in [
            ("set_frequency", "fn tool_set_frequency("),
            ("set_core_voltage", "fn tool_set_core_voltage("),
            ("set_fan_speed", "fn tool_set_fan_speed("),
            ("run_autotune", "fn tool_run_autotune("),
        ] {
            assert!(
                flat.contains(tool_fn),
                "S5: tuning CONTROL tool `{name}` must have its actuating handler `{tool_fn}…`"
            );
        }
        // The two hardware-actuating writes (freq/voltage) route through the
        // per-board hardware clamp (qualify_operating_point) on the Mcp surface —
        // mcp-auth-contract §2 hard rule (do not weaken the clamp).
        assert!(
            flat.contains("config.qualify_operating_point(")
                && flat.contains("crate::config::ControlSurface::Mcp"),
            "S5: set_frequency/set_core_voltage must keep routing through \
             qualify_operating_point(..., ControlSurface::Mcp) (per-board hardware clamp)"
        );
    }
}

/// S2 `axe-conform` MODULAR-DASHBOARD-JS conformance guards (`axe-conform` lane).
///
/// The S2 second pass of the DCENT convergence conformed the two shared
/// components the component-contract named but left unspecced against their axe
/// emissions: `COMP-CHIPSTRIP` (`ChipGrid`, `asic-chips.js`) and
/// `COMP-BLOCKTILE` (`BlockCard`, `block-tile.js`). Both files were already
/// ship-grade — the only genuine divergence was the BlockCard LIVE/STALE pill
/// (axe dimmed opacity but never flipped the pill LABEL the way OS
/// `CurrentBlockCard.liveStatus()` does). These guards pin the now-conformed
/// facts against the JS source TEXT via `include_str!`.
///
/// These two `dashboard/*.js` files are NOT `include_str!`'d by any other guard
/// (the espidf-only `dashboard.rs` guards above can't see them), so without this
/// module a future edit could silently regress the freshness-honesty swap, the
/// closed `ChipState` enum, the honest-null temp path, or trim the
/// `[axe-only-solo]` coinbase user-reward verification (an identity guardrail per
/// terminology-lexicon §3 and keep-unique §4.6). Unlike `dashboard.rs`, these are
/// plain text host-readable, so the asserts run under `cargo test -p
/// dcentaxe-core --lib` on any host.
#[cfg(test)]
mod axe_conform_modular_js_guards {
    const ASIC_CHIPS_JS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/asic-chips.js"
    ));
    const BLOCK_TILE_JS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/dashboard/block-tile.js"
    ));
    const API_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/api.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── COMP-BLOCKTILE: the BlockCard freshness rule (component-contract §5,
    //    "live=true ⇒ LIVE pill; else STALE pill") is byte-identical across both
    //    products — LIVE_THRESHOLD_MS = 120_000. axe pins the literal 120000 in
    //    block-tile.js so the shared 120s gate can't silently drift. ───────────
    #[test]
    fn block_card_freshness_threshold_is_120s_shared_with_os() {
        let flat = collapse_ws(BLOCK_TILE_JS);
        // The 120s stale gate (block-tile.js:360) — the same magnitude OS pins as
        // LIVE_THRESHOLD_MS=120_000 (CurrentBlockCard.tsx:9).
        assert!(
            flat.contains("120000"),
            "COMP-BLOCKTILE: block-tile.js MUST keep the 120000 ms (120 s) \
             staleness threshold — byte-identical to OS LIVE_THRESHOLD_MS=120_000"
        );
        // It is applied as a Date.now()-since-receivedUnixMs comparison, not a
        // fabricated 'always live'.
        assert!(
            flat.contains("Date.now()-d.receivedUnixMs")
                || flat.contains("Date.now() - d.receivedUnixMs"),
            "COMP-BLOCKTILE: the stale gate must be a real Date.now()-receivedUnixMs age check"
        );
    }

    // ── COMP-BLOCKTILE: the S2 genuine divergence closed — the block hero LIVE
    //    pill flips its LABEL to STALE when the block age exceeds the threshold
    //    (matching OS liveStatus()), not just an opacity dim. A regression back
    //    to opacity-only would not be caught by host compilation. ───────────────
    #[test]
    fn block_hero_pill_swaps_live_label_to_stale_when_stale() {
        let flat = collapse_ws(BLOCK_TILE_JS);
        // The pill text node is swapped between the two reused vocabulary strings
        // (no new glossary key invented in S2 — 'STALE'/'LIVE' already exist in
        // the axe surface vocabulary).
        assert!(
            flat.contains("var lpLabel = stale ? 'STALE' : 'LIVE'")
                || flat.contains("var lpLabel=stale?'STALE':'LIVE'"),
            "COMP-BLOCKTILE: the block hero pill MUST flip its LABEL to 'STALE' \
             (else 'LIVE') when stale, matching OS CurrentBlockCard.liveStatus() — \
             opacity-dim alone is the regressed pre-S2 behavior"
        );
        // The text-swap mutates the pill's text node only (preserving the leading
        // <span class="live-dot"> affordance) — NOT a wholesale innerHTML rebuild
        // that would drop the dot.
        assert!(
            flat.contains("lp.lastChild"),
            "COMP-BLOCKTILE: the STALE swap must target the pill text node \
             (lp.lastChild) so the leading <span class=\"live-dot\"> survives"
        );
        // A 'stale' class is toggled and the stale tint references the token ROLE
        // var(--yellow) — never a literal hex (S1 token drift-validators stay
        // green; components reference roles, not values).
        assert!(
            flat.contains("lp.classList.add('stale')"),
            "COMP-BLOCKTILE: the stale pill must toggle a 'stale' class"
        );
        assert!(
            flat.contains("var(--yellow)"),
            "COMP-BLOCKTILE: the stale tint must reference the token ROLE \
             var(--yellow), never a literal hex"
        );
    }

    // ── COMP-CHIPSTRIP: the ChipGrid per-chip state is the CLOSED shared enum
    //    idle | active | warm | hot | error (component-contract §4 Level B,
    //    asic-chips.js chipState()). Pin every member + the absence of any other
    //    state token, so a future edit can't widen/rename the closed vocab. ─────
    #[test]
    fn chip_state_emits_only_the_closed_shared_enum() {
        let flat = collapse_ws(ASIC_CHIPS_JS);
        // chipState() is the closed-enum derivation function.
        assert!(
            flat.contains("function chipState(c)"),
            "COMP-CHIPSTRIP: asic-chips.js must keep chipState(c) as the closed \
             per-chip state derivation"
        );
        // Every member of the closed enum is returned.
        for member in [
            "return 'error'",
            "return 'idle'",
            "return 'hot'",
            "return 'warm'",
            "return 'active'",
        ] {
            assert!(
                flat.contains(member),
                "COMP-CHIPSTRIP: chipState() must emit the closed enum member \
                 `{member}` (idle|active|warm|hot|error)"
            );
        }
        // The temperature ladder thresholds are the contracted ones
        // (t>=70 hot, t>=55 warm, t>=40 active) — not arbitrary values.
        assert!(
            flat.contains("t >= 70") || flat.contains("t>=70"),
            "COMP-CHIPSTRIP: the 'hot' threshold must be t>=70 (contract §4)"
        );
        assert!(
            flat.contains("t >= 55") || flat.contains("t>=55"),
            "COMP-CHIPSTRIP: the 'warm' threshold must be t>=55 (contract §4)"
        );
    }

    // ── COMP-CHIPSTRIP: honest-null temp. An unknown/absent temperature renders
    //    the em-dash '--', never a fabricated 0 — the no-data honesty contract
    //    (terminology TERM-6). The hasTemp guard + chipState's t==null→idle path
    //    enforce it. ────────────────────────────────────────────────────────────
    #[test]
    fn chip_temp_honest_null_renders_em_dash_not_fabricated_zero() {
        let flat = collapse_ws(ASIC_CHIPS_JS);
        // The hasTemp honest-null guard.
        assert!(
            flat.contains("isFinite(c.temp)"),
            "COMP-CHIPSTRIP: the temp render must guard on isFinite(c.temp) \
             (honest-null), not assume a number"
        );
        // The tile renders the em-dash '--' when temp is absent, never a fake 0.
        assert!(
            flat.contains("c.temp.toFixed(0) : '--'") || flat.contains("c.temp.toFixed(0):'--'"),
            "COMP-CHIPSTRIP: an absent temp must render the em-dash '--', \
             never a fabricated 0"
        );
        // In chipState(), a null temp on a non-error chip resolves to 'idle'
        // (not a fabricated active/warm state).
        assert!(
            flat.contains("t == null) return 'idle'") || flat.contains("t==null)return'idle'"),
            "COMP-CHIPSTRIP: a null temp on a live chip must resolve to 'idle', \
             never a fabricated thermal state"
        );
    }

    // ── ANTI-TRIM identity pin: the [axe-only-solo] coinbase user-reward
    //    VERIFICATION (terminology-lexicon §3, keep-unique §4.6) is axe-led and
    //    must NEVER be trimmed toward the OS shell by a 'conformance' edit. Pin
    //    the renderer + the verified/heuristic source tags so a future
    //    simplification can't silently strip the solo-verification block. ───────
    #[test]
    fn coinbase_user_reward_verification_survives_as_axe_only_identity() {
        let flat = collapse_ws(BLOCK_TILE_JS);
        // The coinbase payout renderer is preserved.
        assert!(
            flat.contains("function renderCoinbasePayout(inf)"),
            "ANTI-TRIM (terminology §3 [axe-only-solo]): renderCoinbasePayout(inf) \
             — the coinbase user-reward verification block — must SURVIVE; it is \
             axe-led identity, never trimmed toward the OS shell"
        );
        // The verified-vs-heuristic provenance tags (the ✓ glyph form is matched
        // loosely so the exact glyph byte-form can't false-fail).
        assert!(
            flat.contains("[verified"),
            "ANTI-TRIM: the coinbase block must keep the '[verified …]' \
             source tag (real coinbase-derived reward %)"
        );
        assert!(
            flat.contains("[url heuristic]"),
            "ANTI-TRIM: the coinbase block must keep the '[url heuristic]' \
             source tag (pool-name inference when outputs aren't decoded)"
        );
        // The verifier derives the user's scripthex client-side — the mechanism
        // that makes the verification real, not cosmetic.
        assert!(
            flat.contains("addressToScriptHex"),
            "ANTI-TRIM: the verifier must keep deriving the user scripthex \
             (addressToScriptHex) so the reward % is verified, not asserted"
        );
    }

    #[test]
    fn ota_e2e_factory_flash_uses_raw_write_bin() {
        let script = include_str!("../../scripts/test_ota_e2e.sh");
        assert!(
            script.contains("espflash write-bin --port \"$COM_PORT\" 0x0 \"$FACTORY_PATH\""),
            "merged factory.bin images must be flashed as raw binaries at 0x0"
        );
        assert!(
            !script.contains("espflash flash --port \"$COM_PORT\"")
                || !script.contains("\"$FACTORY_PATH\""),
            "test_ota_e2e.sh must not use espflash flash for merged factory.bin payloads"
        );
    }

    #[test]
    fn package_verifier_and_factory_flash_map_are_wired() {
        let e2e = include_str!("../../scripts/test_ota_e2e.sh");
        let sh_packager = include_str!("../../scripts/package-firmware.sh");
        let ps_packager = include_str!("../../scripts/package-firmware.ps1");
        let verifier = include_str!("../../scripts/verify_ota_package.py");

        assert!(
            e2e.contains("verify_ota_package.py"),
            "the e2e gate must run the offline package verifier before live planning"
        );
        assert!(sh_packager.contains("\"factoryFlashMap\""));
        assert!(ps_packager.contains("factoryFlashMap = @("));
        for token in [
            "bootloader",
            "partition-table",
            "ota-data-initial",
            "update",
            "0x20000",
            "canonical_ota_message",
            "canonical_bundle_message",
            "verify_ed25519",
            "PUBLIC_TARGET_DEVICE_MODELS",
            "allow_internal_targets",
            "otaSignatureAlgorithm",
            "signatureAlgorithm",
        ] {
            assert!(
                verifier.contains(token),
                "verify_ota_package.py must retain flash-map/signature token {token}"
            );
        }
    }

    #[test]
    fn public_release_build_matrix_defaults_to_toolbox_install_targets() {
        let sh_matrix = include_str!("../../scripts/build-matrix.sh");
        let ps_matrix = include_str!("../../scripts/build-matrix.ps1");
        let release_workflow =
            include_str!("../../../../.github/workflows/dcentos-esp-release.yml");
        let ci_matrix_workflow =
            include_str!("../../../../.github/workflows/bitaxe-build-matrix.yml");
        let public_targets = [
            "bitaxe-max",
            "bitaxe-ultra",
            "bitaxe-supra",
            "bitaxe-gamma",
            "bitaxe-hex-ultra",
            "bitaxe-hex-supra",
        ];
        let internal_targets = [
            "bitaxe-gamma-duo",
            "bitaxe-gt",
            "bitaxe-touch",
            "bitaxe-gt-touch",
            "nerdnos",
            "nerdaxe",
            "nerdqaxe-plus",
            "nerdqaxe-pp",
        ];

        for target in &public_targets {
            assert!(
                sh_matrix.contains(&format!("build_one {target} {target}")),
                "POSIX public matrix must build {target}"
            );
            assert!(
                ps_matrix.contains(&format!("BoardTarget = \"{target}\"")),
                "PowerShell public matrix must build {target}"
            );
            assert!(
                release_workflow.contains(&format!("board_target: {target}")),
                "root signed ESP release workflow must publish public target {target}"
            );
            assert!(
                release_workflow.contains(&format!("'{target}':")),
                "root signed ESP release workflow must validate device model for {target}"
            );
            assert!(
                ci_matrix_workflow.contains(&format!("'{target}':")),
                "root build-matrix workflow must validate public target {target}"
            );
        }

        let sh_internal_gate = sh_matrix
            .find("if [ \"$INCLUDE_INTERNAL_TARGETS\" = \"1\" ]")
            .expect("POSIX matrix must keep internal targets opt-in");
        let sh_default_matrix = &sh_matrix[..sh_internal_gate];
        let ps_public_start = ps_matrix
            .find("$publicTargets = @(")
            .expect("PowerShell matrix must define public targets");
        let ps_internal_start = ps_matrix
            .find("$internalTargets = @(")
            .expect("PowerShell matrix must define internal targets separately");
        let ps_default_matrix = &ps_matrix[ps_public_start..ps_internal_start];

        for target in &internal_targets {
            assert!(
                !sh_default_matrix.contains(target),
                "POSIX public matrix must not emit internal target {target} by default"
            );
            assert!(
                !ps_default_matrix.contains(target),
                "PowerShell public matrix must not emit internal target {target} by default"
            );
            assert!(
                !release_workflow.contains(&format!("board_target: {target}")),
                "root signed ESP release workflow must not publish internal target {target}"
            );
            assert!(
                !release_workflow.contains(&format!("'{target}':")),
                "root signed ESP release workflow must not validate internal target {target}"
            );
            assert!(
                !ci_matrix_workflow.contains(&format!("'{target}':")),
                "root build-matrix workflow must not accept internal packaged target {target}"
            );
        }

        assert!(
            sh_matrix.contains("INCLUDE_INTERNAL_TARGETS"),
            "POSIX internal targets must stay opt-in"
        );
        assert!(
            ps_matrix.contains("[switch]$IncludeInternalTargets"),
            "PowerShell internal targets must stay opt-in"
        );
        assert!(
            ci_matrix_workflow.contains("found_boards == set(expected_models)"),
            "root build-matrix workflow must exact-check the public package set"
        );
    }

    #[test]
    fn ota_partition_size_matches_api_upload_limit() {
        let partitions = include_str!("../../partitions.csv");
        let ota0 = partitions
            .lines()
            .find(|line| line.trim_start().starts_with("ota_0,"))
            .expect("partitions.csv must define ota_0");
        let cols: Vec<&str> = ota0.split(',').map(str::trim).collect();
        assert_eq!(
            cols.get(3),
            Some(&"0x20000"),
            "ota_0 offset is the factory update offset"
        );
        assert_eq!(cols.get(4), Some(&"0x300000"), "ota_0 size must stay 3 MB");
        assert!(
            API_RS.contains("const MAX_FIRMWARE_SIZE: usize = 3 * 1024 * 1024"),
            "OTA upload max must match partitions.csv ota_0 size"
        );
    }
}

/// H1 — AxeOS-compat `/api/system/info` WIRE CONTRACT guard.
///
/// `api_system_info.rs` builds the `/api/system/info` response with
/// `#[serde(rename_all = "camelCase")]` plus a set of explicit `#[serde(rename =
/// "...")]` per-field renames that keep the JSON byte-identical to AxeOS so
/// BitAxeHQ / Swarm / pyasic consume it unchanged. That module's doc declares this
/// a CRITICAL contract, but NOTHING enforced it: it is NOT in the `#[path]` set
/// and has no `#[cfg(test)]`, so a dropped rename or a `rename_all` flip would
/// silently break every AxeOS-compat consumer with zero CI signal.
///
/// `api_system_info.rs` is a pure serde-derive module that DOES host-compile
/// (no esp-idf deps), but pinning the contract against the source TEXT via
/// `include_str!` — exactly like `dcent_design_language_guards` /
/// `mcp_auth_contract_guards` — catches a rename drift regardless of whether the
/// struct still compiles (a renamed-but-still-valid field compiles fine yet breaks
/// the wire). This is the host-testable durability mechanism for the wire format.
///
/// Assertions are whitespace-robust (collapse runs of ASCII whitespace) so a
/// rustfmt reflow of an attribute cannot flip a guard.
#[cfg(test)]
mod api_wire_contract_guards {
    const API_SYSTEM_INFO_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/api_system_info.rs"
    ));

    /// Collapse every run of ASCII whitespace to a single space so a reflow of an
    /// attribute / struct line cannot dodge a substring match.
    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── (1) the response struct uses camelCase, and the flag is ON the struct ──
    // A flip of `rename_all` (e.g. to snake_case) — or moving it off the response
    // struct — would silently rename ~140 keys and break every AxeOS consumer.
    #[test]
    fn response_struct_is_camel_case() {
        let flat = collapse_ws(API_SYSTEM_INFO_RS);
        assert!(
            flat.contains("#[serde(rename_all = \"camelCase\")] pub struct SystemInfoResponse"),
            "the SystemInfoResponse struct MUST keep `#[serde(rename_all = \"camelCase\")]` \
             immediately above it — a flip renames every AxeOS-compat key"
        );
    }

    // ── (2) every load-bearing explicit per-field rename literal is present ─────
    // These are the EXACT renames found in api_system_info.rs (the camelCase
    // derive cannot produce `hashRate`/`ASICModel`/`stratumURL`-style mixed-case or
    // the snake_case `overheat_mode` AxeOS keys, so each is pinned explicitly). The
    // closing quote in the match disambiguates `hashRate` from `hashRate_1m`, etc.
    #[test]
    fn axeos_field_renames_are_pinned() {
        let flat = collapse_ws(API_SYSTEM_INFO_RS);
        for key in [
            "vrTemp",
            "hashRate",
            "hashRate_1m",
            "hashRate_5m",
            "hashRate_10m",
            "hashRate_15m",
            "hashRate_1h",
            "isPSRAMAvailable",
            "wifiRSSI",
            "ASICModel",
            "stratumURL",
            "stratumTLS",
            "fallbackStratumURL",
            "fallbackStratumTLS",
            "axeOSVersion",
            // NOTE: snake_case override of the camelCase derive — AxeOS emits
            // `overheat_mode`, NOT `overheatMode`; this rename is load-bearing.
            "overheat_mode",
        ] {
            assert!(
                flat.contains(&format!("#[serde(rename = \"{key}\")]")),
                "AxeOS-compat wire key `{key}` lost its `#[serde(rename = \"{key}\")]` — \
                 this breaks BitAxeHQ/Swarm/pyasic consumption"
            );
        }
    }

    // ── (3) the load-bearing Option / skip_serializing_if fields stay typed ─────
    // `hash_rate_1h` is `Option<f64>` (always None) so it serializes as JSON null
    // to match the legacy `hashRate_1h: null`. `last_panic` / `last_restart_reason`
    // / `temp_source` are `skip_serializing_if = "Option::is_none"` so they are
    // OMITTED when absent — dropping the skip would emit an unexpected key, and
    // dropping the Option would emit a value the legacy wire never had.
    #[test]
    fn optional_and_skipped_fields_stay_honest() {
        let flat = collapse_ws(API_SYSTEM_INFO_RS);
        assert!(
            flat.contains("pub hash_rate_1h: Option"),
            "hashRate_1h must stay `Option<f64>` so it serializes as null (legacy wire)"
        );
        for field in ["last_panic", "last_restart_reason", "temp_source"] {
            assert!(
                flat.contains(&format!(
                    "#[serde(skip_serializing_if = \"Option::is_none\")] pub {field}: Option"
                )),
                "`{field}` must keep `skip_serializing_if = \"Option::is_none\"` over an Option \
                 so it is OMITTED (not emitted) when absent — preserving the legacy wire shape"
            );
        }
    }

    // ── (4) M-dash-1 additive honesty companion stays on the wire ──────────────
    // `acceptanceRate` is a DCENT-original field (NOT AxeOS). The M-dash-1 honesty
    // fix added the additive `acceptanceRateKnown: bool` companion so a freshly-
    // booted miner (zero confirmed shares) is not read as a real 100% accept rate.
    // Pin both fields so the honesty wiring cannot be silently dropped.
    #[test]
    fn acceptance_rate_known_companion_present() {
        let flat = collapse_ws(API_SYSTEM_INFO_RS);
        assert!(
            flat.contains("pub acceptance_rate: f64"),
            "acceptance_rate (DCENT-original) must stay on the wire"
        );
        assert!(
            flat.contains("pub acceptance_rate_known: bool"),
            "M-dash-1: the additive `acceptanceRateKnown` honesty companion must stay on the wire \
             so zero-share `acceptanceRate` cannot read as a real 100% accept rate"
        );
    }

    #[test]
    fn stratum_v2_experimental_companion_present() {
        let flat = collapse_ws(API_SYSTEM_INFO_RS);
        assert!(
            flat.contains("pub stratum_v2_available: bool"),
            "/api/system/info must keep the additive stratumV2Available flag"
        );
        assert!(
            flat.contains("pub stratum_v2_experimental: bool"),
            "ESP-5: /api/system/info must keep stratumV2Experimental so SV2 maturity is honest"
        );
    }
}

/// LoRa `/mcp` registry contract guards (Track C W2.6).
///
/// Two legs, exactly as the brief specifies:
///   * a REAL compile-linked test of `dcentaxe_lora::mcp` proving that with `lora`
///     ON, exactly ONE lora tool is mutating and it is `OwnerControl`; and
///   * a structural guard over the `dcentaxe/src/mcp.rs` source TEXT proving that
///     with `lora` OFF the `/mcp` tools/list + dispatch are BYTE-IDENTICAL — i.e.
///     every lora tool-name string lives ONLY inside the single
///     `#[cfg(feature = "lora")]` `LORA-MCP-REGION`, and both call sites are
///     cfg-gated. `mcp.rs` is the espidf-only binary-crate source (cannot
///     host-compile), so the byte-identical proof is text-based (whitespace-robust,
///     additive) — the same pattern as `mcp_auth_contract_guards`.
#[cfg(test)]
mod mcp_lora_contract_guards {
    use dcentaxe_lora::mcp::{tools, McpAccess};

    const MCP_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/mcp.rs"
    ));
    const LORA_TASK_RS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dcentaxe/src/lora_task.rs"
    ));

    fn collapse_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // ── (1) with lora ON, EXACTLY ONE lora tool is mutating + it is OwnerControl ─
    // Real compile-linked check against the shared crate — not a text pin. A future
    // edit that adds a second mutating tool, or downgrades `lora_send_beacon` to a
    // passwordless read, fails HERE (the BAP-2 / 2026-06-12 passwordless-mutate ban).
    #[test]
    fn lora_mcp_exactly_one_owner_control_tool() {
        let t = tools();
        assert_eq!(t.len(), 3, "the LoRa MCP surface is exactly 3 tools");
        let mutating: Vec<&str> = t
            .iter()
            .filter(|x| x.requires_auth())
            .map(|x| x.name)
            .collect();
        assert_eq!(
            mutating,
            ["lora_send_beacon"],
            "exactly one lora tool is mutating and it MUST be lora_send_beacon"
        );
        for tool in t.iter() {
            match tool.name {
                "lora_send_beacon" => assert_eq!(
                    tool.access,
                    McpAccess::OwnerControl,
                    "lora_send_beacon must be OwnerControl (owner-gated)"
                ),
                "lora_status" | "get_mesh_peers" => assert_eq!(
                    tool.access,
                    McpAccess::Read,
                    "{} must be a read tool",
                    tool.name
                ),
                other => panic!("unexpected lora tool name `{other}`"),
            }
        }
    }

    // ── (2) with lora OFF, the /mcp tools/list + dispatch are BYTE-IDENTICAL ──────
    // All lora tool-NAME strings must live ONLY inside the single cfg-gated
    // LORA-MCP-REGION. The two call sites reference `lora_mcp::*` helpers (never a
    // quoted tool name), so with the feature OFF the region + call sites compile out
    // and nothing lora reaches tools/list or dispatch.
    #[test]
    fn lora_mcp_names_only_in_cfg_region() {
        let begin = MCP_RS
            .find("LORA-MCP-REGION BEGIN")
            .expect("mcp.rs must delimit the lora region (LORA-MCP-REGION BEGIN)");
        let end = MCP_RS
            .find("LORA-MCP-REGION END")
            .expect("mcp.rs must close the lora region (LORA-MCP-REGION END)");
        assert!(begin < end, "region markers out of order");
        let region = &MCP_RS[begin..end];
        // The region's module is cfg(feature="lora")-gated.
        assert!(
            region.contains("#[cfg(feature = \"lora\")]"),
            "the lora_mcp module must be #[cfg(feature = \"lora\")]-gated"
        );
        // Every raw lora tool-name string appears ONLY inside the region.
        let outside = format!("{}{}", &MCP_RS[..begin], &MCP_RS[end..]);
        for quoted in [
            "\"lora_status\"",
            "\"lora_send_beacon\"",
            "\"get_mesh_peers\"",
        ] {
            assert!(
                region.contains(quoted),
                "{quoted} must appear inside the lora region (the dispatch/schema wiring)"
            );
            assert!(
                !outside.contains(quoted),
                "{quoted} must NOT appear outside the cfg-gated lora region — the /mcp tools/list \
                 + dispatch must be byte-identical when `lora` is OFF"
            );
        }
    }

    // ── (3) both call sites are cfg-gated + send_beacon is owner-control fail-closed ─
    #[test]
    fn lora_mcp_call_sites_gated_and_beacon_owner_controlled() {
        let flat = collapse_ws(MCP_RS);
        // tools/list injection is feature-gated.
        assert!(
            flat.contains("#[cfg(feature = \"lora\")] lora_mcp::append_tools(tools);"),
            "the tools/list lora append must be #[cfg(feature = \"lora\")]-gated"
        );
        // dispatch arm is feature-gated + routes to the gated region helper.
        assert!(
            flat.contains(
                "#[cfg(feature = \"lora\")] n if lora_mcp::is_lora_tool(n) => return lora_mcp::dispatch(state, n, &args, auth),"
            ),
            "the tools/call lora dispatch arm must be #[cfg(feature = \"lora\")]-gated"
        );
        // lora_send_beacon is fail-closed on !control_authorized (owner control) and
        // routes through the duty-governed request_beacon (never a raw radio poke).
        let region_flat = {
            let begin = flat.find("LORA-MCP-REGION BEGIN").unwrap();
            let end = flat.find("LORA-MCP-REGION END").unwrap();
            flat[begin..end].to_string()
        };
        assert!(
            region_flat.contains("if !auth.control_authorized"),
            "lora_send_beacon must be refused fail-closed unless authorize_mcp_control() passed"
        );
        assert!(
            region_flat.contains("crate::lora_task::request_beacon(state"),
            "lora_send_beacon must route through the duty-governed lora_task::request_beacon"
        );
    }

    // ── (4) the duty governor + honest refusal + fail-soft init are wired ─────────
    // lora_task.rs is espidf-only (cannot host-compile), so pin its load-bearing
    // honesty invariants against the source TEXT: a duty-clamped beacon returns the
    // honest `"duty_budget"` reason (never a fake "sent"), and a radio-init failure
    // is fail-soft (mining continues).
    #[test]
    fn lora_task_is_duty_governed_honest_and_fail_soft() {
        let flat = collapse_ws(LORA_TASK_RS);
        assert!(
            flat.contains("fn request_beacon"),
            "lora_task must expose request_beacon for the MCP owner-control tool"
        );
        assert!(
            flat.contains("duty.try_acquire"),
            "request_beacon (and the task beacons) must gate transmits on the duty governor"
        );
        assert!(
            flat.contains("\"duty_budget\""),
            "a duty-clamped beacon must return the honest reason \"duty_budget\" (not a fake sent)"
        );
        assert!(
            flat.contains("mining continues"),
            "a radio cold-boot failure must be fail-soft (log once, mining continues)"
        );
    }
}
