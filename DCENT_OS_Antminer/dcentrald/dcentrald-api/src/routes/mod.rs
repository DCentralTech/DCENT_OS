//! Route modules for `dcentrald-api`.
//!
//! Each submodule produces an `axum::Router<Arc<AppState>>` that can be
//! merged into the top-level router built in `rest::build_router()`.
//!
//!  W8-D: split the silicon-profile import endpoints into their
//! own module so the existing autotuner-mode `GET/POST /api/profiles`
//! handlers in `rest.rs` are not disturbed. The new endpoints live
//! under `/api/profiles/silicon/*` to avoid colliding with the
//! pre-existing `/api/profiles` route — see `routes/profiles.rs` for
//! the full path inventory.
//!
//!  W8-F: Restore-to-Stock backend ships at
//! `/api/system/restore-to-stock/*`. Multi-step destructive flow with
//! mandatory NAND backup, safety preflight (SECURE_BOOT_SET / Hashcore
//! root hash / atlas / hotelfee / daemons / dtu / factory-reset
//! detectors), and operator-typed serial confirmation. Default is
//! dry-run; the operator must pass an explicit `confirm` flag to
//! actually flash. See `routes/restore_to_stock.rs`.
//!
//! W9.4: J/TH calibration loop ships at `/api/perf/{calibrate,efficiency}`.
//! Operator supplies an external wattmeter reading; the daemon bakes it
//! into the persisted `PowerCalibration` as `operator_confirmed=true`,
//! and the autotuner's `TuneTarget::EfficiencyJTH` mode treats it as the
//! source of truth. See `routes/perf.rs`.

/// W13.D1 — `GET /api/boot/phase` + `GET /api/boot/timeline` (dev-mode
/// gated). Surfaces the live cold-boot substate read from
/// `AppState::boot_phase_tracker`. The taxonomy follows the R4
/// `bmminer_init_trace_cv1835.md` 6-substate breakdown for CV1835
/// (boot_psu_init through boot_awaiting_first_nonce) and a generic
/// 3-substate fallback for non-CV1835 platforms. See `routes/boot_phase.rs`.
/// 2026-05-17 — `POST /api/autotuner/quota`. Hashrate split-quota
/// planner (ePIC UMC OS V1.18.2 analog). Resolves a `fraction` /
/// `absolute_ths` quota to the equivalent wattage target + the
/// canonical `[autotune] mode = "hashrate-quota"` TOML block. Read-only
/// planner — the daemon's `TunerMode::HashrateQuota` delegates to the
/// gated `PowerTargetController` for the actual clamped tick path. See
/// `routes/autotuner_quota.rs`.
/// GROUP C (W8 parity) — `GET /api/audit-log`. Paginated, redacted read-back
/// of the PERSISTENT, reboot-surviving NDJSON audit log at
/// `crate::audit_log_path()` (default `/data/audit.log`). The persistent write
/// path + size-capped rotation already live in `crate::lib`
/// (`append_audit_record_to_path` / `trim_audit_log_to_max_bytes` /
/// `push_audit_event`); the W8 gap was that the reboot-surviving file was
/// written but never readable through the API (`/api/history/audit` reads only
/// the volatile in-memory ring). This closes that gap so operators + fleet
/// tools can see what happened before the last reboot. See `routes/audit_log.rs`.
pub mod audit_log;
pub mod autotuner_quota;
pub mod boot_phase;
/// A06 (2026-06-10 knowledge-goldmine, finding s5-luxminer CAND-04) —
/// `GET /api/chips/health`. Flat, read-only per-chip hashrate-ratio /
/// error-rate array from the live autotuner chip-health snapshot. Distinct
/// from the mode-gated, envelope-shaped `/api/autotuner/chip-health`. See
/// `routes/chip_health.rs`.
pub mod chip_health;
pub mod donation;
pub mod jd;
pub mod perf;
pub mod profiles;
/// W13.D1 — `GET /api/miner/pvt-table`. Returns the full per-SKU PVT
/// freq/voltage table for the detected hashboard, sourced from
/// `dcentrald-silicon-profiles::bm1362::Bm1362HashboardSku`. See
/// `routes/pvt_table.rs`.
pub mod pvt_table;
pub mod re_catalog;
pub mod restore_to_stock;
/// A03 (2026-06-10 knowledge-goldmine, finding s5-luxminer CAND-01) —
/// `GET /api/metrics/rolling` + `/api/metrics/rolling.csv`. Read-only 3-tier
/// (5s/1m/5m) rolling-average ring (LuxOS `/metrics` parity). See
/// `routes/rolling_metrics.rs`.
pub mod rolling_metrics;
pub mod stock_parity;
pub mod sv2;
/// A04 (2026-06-10 knowledge-goldmine, finding s5-luxminer API-07/CAND-02) —
/// `GET /api/profile/download` + `POST /api/profile/upload`. V/F profile
/// save/restore with a byte-exact round trip (LuxOS `:8080` parity). See
/// `routes/vf_profile.rs`.
pub mod vf_profile;
