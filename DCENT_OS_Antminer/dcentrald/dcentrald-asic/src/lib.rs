//! ASIC chip driver subsystem for dcentrald.
//!
//! Provides the ChipDriver trait for universal hash board compatibility,
//! the BM13xx wire protocol implementation, PIC microcontroller control,
//! and per-chip-family driver implementations.
//!
//! The ChipDriver trait is the central abstraction that makes Universal Hash
//! Board Compatibility possible. Each ASIC chip family implements this trait
//! with its specific initialization sequence, register values, job format,
//! and nonce decoding.
//!
//! Modules:
//! - `protocol` - BM13xx wire protocol (CRC5, CRC16, framing)
//! - `chain`    - Chain struct for FIFO access and command encoding
//! - `pic`      - PIC16F1704 microcontroller protocol (S9 I2C voltage control)
//! - `dspic`    - dsPIC33EP16GS202 voltage controller (S17/S19 framed I2C protocol)
//! - `pic1704`  - PIC1704 short-form-register voltage controller (CV1835 / AM335x BB / Amlogic S19j Pro)
//! - `drivers`  - Per-chip driver implementations (BM1387, BM1397, etc.)
//! - `bm1362`   - BM1362 cold-boot orchestration (W2.5, byte-sequence-tested)
//! - `bm1387`   - BM1387 protocol reference catalog (W11.10, RE2 §8.1 / §4.1) — reference only
//! - `bm1393`   - BM1393 protocol reference catalog (W11.10, RE2 §8.3 / §8.5) — reference only

// ASIC drivers intentionally retain reverse-engineered register constants,
// alternate firmware frame builders, and gated scaffold paths ahead of live
// platform promotion. Removing them to satisfy dead-code/doc-format lints
// would lose evidence; runtime safety gates still decide whether they can run.
#![allow(
    dead_code,
    clippy::doc_lazy_continuation,
    clippy::empty_line_after_doc_comments
)]

pub mod bm1362;
pub mod bm1387;
pub mod bm1393;
pub mod chain;
pub mod drivers;
pub mod dspic;
pub mod hw_err_tracker;
pub mod pic;
pub mod pic1704;
pub mod protocol;
pub mod uart_trans;

use thiserror::Error;

/// ASIC subsystem error type.
#[derive(Debug, Error)]
pub enum AsicError {
    /// HAL-level error (UIO, I2C, GPIO).
    #[error("HAL error: {0}")]
    Hal(#[from] dcentrald_hal::HalError),

    /// Chip not found in driver registry.
    #[error("unknown chip ID: 0x{chip_id:04X}")]
    UnknownChip { chip_id: u16 },

    /// No chips detected on chain.
    #[error("no chips detected on chain {chain_id}")]
    NoChipsDetected { chain_id: u8 },

    /// PIC communication failure.
    #[error("PIC error on addr 0x{addr:02X}: {detail}")]
    Pic { addr: u8, detail: String },

    /// CRC mismatch in chip response.
    #[error("CRC error on chain {chain_id}: expected 0x{expected:02X}, got 0x{actual:02X}")]
    CrcMismatch {
        chain_id: u8,
        expected: u8,
        actual: u8,
    },

    /// FIFO timeout (no response within expected time).
    #[error("FIFO timeout on chain {chain_id}: {detail}")]
    FifoTimeout { chain_id: u8, detail: String },

    /// GetAddress returned bytes, but the complete FPGA enumeration window did
    /// not satisfy the integrity contract required to select one ASIC driver.
    #[error("enumeration integrity failure on chain {chain_id}: {reason:?}")]
    EnumerationIntegrity {
        chain_id: u8,
        reason: crate::chain::EnumerationIdentityIneligibility,
    },

    /// Chip initialization failed.
    #[error("chip init failed on chain {chain_id}: {detail}")]
    InitFailed { chain_id: u8, detail: String },

    /// Invalid frequency or voltage parameter.
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
}

pub type Result<T> = std::result::Result<T, AsicError>;

pub use hw_err_tracker::{
    EwmaState, HwErrTracker, DEFAULT_ALPHA as HW_ERR_DEFAULT_ALPHA,
    DEFAULT_THRESHOLD as HW_ERR_DEFAULT_THRESHOLD,
};

#[cfg(test)]
mod mock_chain_mini_soak {
    #[derive(Debug, Clone, Copy)]
    struct MockNonce {
        value: u32,
        hw_error: bool,
        temp_c: f64,
    }

    #[derive(Debug)]
    struct MockChain {
        seed: u32,
        tick: u32,
        temp_c: f64,
        hw_error_every: u32,
    }

    impl MockChain {
        fn new(seed: u32, hw_error_every: u32) -> Self {
            Self {
                seed,
                tick: 0,
                temp_c: 58.0,
                hw_error_every,
            }
        }

        fn next_nonce(&mut self) -> MockNonce {
            self.tick = self.tick.saturating_add(1);
            self.seed = self
                .seed
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            self.temp_c += 0.05;
            let hw_error = self.hw_error_every > 0 && self.tick % self.hw_error_every == 0;
            MockNonce {
                value: self.seed,
                hw_error,
                temp_c: self.temp_c,
            }
        }
    }

    #[derive(Debug, Default)]
    struct MiniSoakResult {
        accepted_shares: u32,
        hw_errors: u32,
        max_temp_c: f64,
        failover_triggered: bool,
        ota_preflight_ok: bool,
    }

    fn mock_pool_accepts(nonce: MockNonce) -> bool {
        !nonce.hw_error && nonce.value != 0
    }

    fn run_mini_soak(
        chain: &mut MockChain,
        ticks: u32,
        failover_error_limit: u32,
    ) -> MiniSoakResult {
        let mut result = MiniSoakResult::default();
        let mut consecutive_errors = 0u32;

        for _ in 0..ticks {
            let nonce = chain.next_nonce();
            result.max_temp_c = result.max_temp_c.max(nonce.temp_c);

            if nonce.hw_error {
                result.hw_errors = result.hw_errors.saturating_add(1);
                consecutive_errors = consecutive_errors.saturating_add(1);
            } else {
                consecutive_errors = 0;
            }

            if consecutive_errors >= failover_error_limit {
                result.failover_triggered = true;
                break;
            }

            if mock_pool_accepts(nonce) {
                result.accepted_shares = result.accepted_shares.saturating_add(1);
            }
        }

        result.ota_preflight_ok =
            result.accepted_shares > 0 && result.max_temp_c < 95.0 && !result.failover_triggered;
        result
    }

    #[test]
    fn deterministic_mock_chain_mini_soak_covers_share_failover_and_ota_preflight() {
        let mut healthy_chain = MockChain::new(0xDCE0_0001, 29);
        let healthy = run_mini_soak(&mut healthy_chain, 240, 3);
        assert!(healthy.accepted_shares > 200);
        assert!(healthy.hw_errors > 0);
        assert!(!healthy.failover_triggered);
        assert!(healthy.ota_preflight_ok);
        assert!(healthy.max_temp_c < 95.0);

        let mut failing_chain = MockChain::new(0xDCE0_0001, 1);
        let failing = run_mini_soak(&mut failing_chain, 240, 3);
        assert!(failing.failover_triggered);
        assert!(!failing.ota_preflight_ok);
        assert_eq!(failing.hw_errors, 3);
    }
}

#[cfg(test)]
mod process_environment_source_contract {
    use std::path::{Path, PathBuf};

    const ENV_CONTRACT_CHILD: &str = "DCENT_ASIC_ENV_CONTRACT_CHILD";
    const POLICY_ENV_NAMES: &[&str] = &[
        "DCENT_AM2_DSPIC_READ_CONFIG_LATCH",
        "DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH",
        "DCENT_AM2_DSPIC_SENSOR_ONLY",
        "DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE",
        "DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS",
        "DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE",
        "DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE",
        "DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE",
        "DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX",
        "DCENT_AM2_DSPIC_RESET_DWELL_MS",
        "DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX",
        "DCENT_AM2_DSPIC_BOSMINER_FAITHFUL",
        "DCENT_CV1835_ACCEPT_INFERRED_SOC_REGS",
        "DCENT_CV1835_ACCEPT_INFERRED_FPGA",
        "DCENT_PIC_RECOVERY_LOG_DIR",
    ];

    fn collect_rust_sources(dir: &Path, sources: &mut Vec<PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()))
            .map(|entry| entry.expect("source directory entry"))
            .collect();
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_sources(&path, sources);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                sources.push(path);
            }
        }
    }

    #[test]
    fn driver_tests_do_not_mutate_process_global_environment() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = Vec::new();
        collect_rust_sources(&source_root, &mut sources);

        // Build the needles at runtime so this source-contract test does not
        // match itself. Environment integration belongs in child processes;
        // in-process tests must inject parsed policy values explicitly.
        let forbidden = [["set", "_var("].concat(), ["remove", "_var("].concat()];
        let mut violations = Vec::new();
        for path in sources {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for (line_index, line) in source.lines().enumerate() {
                if forbidden.iter().any(|needle| line.contains(needle)) {
                    let relative = path
                        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(&path);
                    violations.push(format!("{}:{}", relative.display(), line_index + 1));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "dcentrald-asic source must not mutate process-global environment; \
             inject explicit policy values or use a child process for environment \
             integration tests. Violations: {}",
            violations.join(", ")
        );
    }

    fn assert_environment_adapters(active_env: Option<&str>) {
        use crate::bm1362::uart_relay::{
            cold_boot_enable_nonce_path, NoncePathOutcome, UartRelayReg,
        };
        use crate::dspic::bosminer_warmup;

        assert_eq!(
            bosminer_warmup::am2_dspic_read_config_latch_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_READ_CONFIG_LATCH")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_lm75_passthrough_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_BOSMINER_LM75_PASSTHROUGH")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_sensor_only_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_SENSOR_ONLY")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_postjump_heartbeat_keepalive_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_POSTJUMP_HEARTBEAT_KEEPALIVE")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_rejump_before_enable_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_REJUMP_BEFORE_ENABLE")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_skip_setvoltage_keep_enable_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_SKIP_SETVOLTAGE_KEEP_ENABLE")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_bosminer_minimal_enable_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_BOSMINER_MINIMAL_ENABLE")
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_keepalive_interval_ms(),
            if active_env == Some("DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS") {
                150
            } else {
                300
            }
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_jump_reverify_max(),
            if active_env == Some("DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX") {
                6
            } else {
                0
            }
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_reset_dwell_ms(),
            if active_env == Some("DCENT_AM2_DSPIC_RESET_DWELL_MS") {
                1_500
            } else {
                1_000
            }
        );
        assert_eq!(
            bosminer_warmup::am2_dspic_reset_jump_reverify_max(),
            if active_env == Some("DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX") {
                4
            } else {
                0
            }
        );
        assert_eq!(
            crate::dspic::dspic_bosminer_faithful_enabled(),
            active_env == Some("DCENT_AM2_DSPIC_BOSMINER_FAITHFUL")
        );

        let relay = cold_boot_enable_nonce_path(UartRelayReg::zero());
        let relay_expected = matches!(
            active_env,
            Some("DCENT_CV1835_ACCEPT_INFERRED_SOC_REGS" | "DCENT_CV1835_ACCEPT_INFERRED_FPGA")
        );
        assert_eq!(
            matches!(relay, NoncePathOutcome::Frame { .. }),
            relay_expected
        );

        #[cfg(feature = "recovery-tool")]
        {
            let path = crate::dspic::recovery_fw86::path_c_log_path();
            if active_env == Some("DCENT_PIC_RECOVERY_LOG_DIR") {
                let dir = std::env::var("DCENT_PIC_RECOVERY_LOG_DIR")
                    .expect("explicit recovery log directory");
                assert_eq!(
                    path,
                    PathBuf::from(dir).join(crate::dspic::recovery_fw86::PATH_C_LOG_FILENAME)
                );
            } else {
                assert_eq!(
                    path,
                    PathBuf::from(crate::dspic::recovery_fw86::DEFAULT_PATH_C_LOG_DIR)
                        .join(crate::dspic::recovery_fw86::PATH_C_LOG_FILENAME)
                );
            }
        }
    }

    fn run_environment_contract_child(mode: &str) {
        let current_exe = std::env::current_exe().expect("current ASIC test executable");
        let mut child = std::process::Command::new(current_exe);
        child
            .arg("--exact")
            .arg(
                "process_environment_source_contract::environment_adapters_are_exercised_in_child_process",
            )
            .arg("--nocapture")
            .env(ENV_CONTRACT_CHILD, mode);
        for name in POLICY_ENV_NAMES {
            child.env_remove(name);
        }

        match mode {
            "defaults" => {}
            "DCENT_AM2_DSPIC_KEEPALIVE_INTERVAL_MS" => {
                child.env(mode, "150");
            }
            "DCENT_AM2_DSPIC_JUMP_REVERIFY_MAX" => {
                child.env(mode, "6");
            }
            "DCENT_AM2_DSPIC_RESET_DWELL_MS" => {
                child.env(mode, "1500");
            }
            "DCENT_AM2_DSPIC_RESET_JUMP_REVERIFY_MAX" => {
                child.env(mode, "4");
            }
            "DCENT_PIC_RECOVERY_LOG_DIR" => {
                child.env(
                    mode,
                    std::env::temp_dir().join("dcent-recovery-env-contract"),
                );
            }
            name if POLICY_ENV_NAMES.contains(&name) => {
                child.env(name, "1");
            }
            other => panic!("unknown environment contract mode: {other}"),
        }

        let status = child.status().expect("run isolated environment contract");
        assert!(
            status.success(),
            "{mode} environment child failed: {status}"
        );
    }

    #[test]
    fn environment_adapters_are_exercised_in_child_process() {
        match std::env::var(ENV_CONTRACT_CHILD) {
            Ok(mode) if mode == "defaults" => assert_environment_adapters(None),
            Ok(mode) if POLICY_ENV_NAMES.contains(&mode.as_str()) => {
                assert_environment_adapters(Some(&mode));
            }
            Ok(other) => panic!("unknown environment contract mode: {other}"),
            Err(_) => {
                run_environment_contract_child("defaults");
                for name in POLICY_ENV_NAMES {
                    run_environment_contract_child(name);
                }
            }
        }
    }
}
