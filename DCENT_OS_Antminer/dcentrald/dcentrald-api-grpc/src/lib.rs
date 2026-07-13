//! `dcentrald-api-grpc` — Supremacy S5.1 gRPC + protobuf + reflection
//! scaffold for dcentrald.
//!
//! ## Status (Wave I, 2026-05-19 — read RPCs wired)
//!
//! - All 5 services (Miner / Tuner / Pool / Fan / Locate) are wired with the
//!   correct proto schema + tonic plumbing.
//! - **READ RPCs are LIVE** (Wave I): `MinerService.GetStatus`,
//!   `PoolService.GetPools`, `FanService.GetFanState`, `TunerService.GetTunerMode`
//!   serve real state via the OnceLock `RUNTIME_SNAPSHOT_RX` snapshot the daemon
//!   installs (see `install_runtime_snapshot_rx`). Pre-install reads return
//!   `Status::unavailable`; pool passwords are never surfaced.
//! - `TunerService.GetConstraints` returns silicon-profile envelopes with the
//!   load-bearing SUPREMACY clamps (voltage <= 14500 mV am2, fan <= 30 PWM home).
//!   See `constraints.rs`.
//! - **WRITE RPCs are delegate-backed** (SW-02): `Reboot`, `SetPools`,
//!   `SetFanMode`, `SetTunerMode`, `LocateDevice` call a daemon-installed
//!   [`GrpcWriteDelegate`] (see `install_write_delegate`) that bridges each
//!   one to the SAME gated REST handlers the dashboard + cgminer-LuxOS surface
//!   use — so every safety/validation cap (fan <= 30 PWM home cap, voltage
//!   <= 14500 mV, pool URL validation, mode gate) is enforced in one place and
//!   the gRPC control plane cannot bypass it. **Until the daemon installs a
//!   delegate, every write RPC returns `Status::unimplemented`** — byte-
//!   identical to the prior read-only contract, so this is strictly additive.
//! - `tonic-reflection` is enabled so `grpcurl -plaintext <host>:50051 list`
//!   discovers every service.
//!
//! ## Wiring (dcentrald)
//!
//! `[api.grpc] enabled = false` by default. When enabled, `dcentrald/src/main.rs`
//! calls `serve(addr, home_mode)` alongside the existing REST/CGMiner API, and
//! `Daemon::run` installs the runtime snapshot that backs the read RPCs. Calling
//! a WRITE RPC returns a clean `UNIMPLEMENTED` with no side-effects.
//!
//! ## Why a scaffold first
//!
//! Per Supremacy S5 sequencing: shipping the proto contract + transport now
//! lets every downstream (Python tooling, Go fleet manager, third-party
//! pyasic-style libs) pin to v1 message shapes immediately. Handler bodies
//! land as the dcentrald runtime grows the AppState surface needed to back
//! each RPC. The `unimplemented` stubs are an explicit contract: "this RPC
//! exists, the wire format is stable, but the runtime isn't backing it yet".

#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::OnceLock;

use tokio::sync::watch;
use tonic::{service::Interceptor, transport::Server, Request, Response, Status};
use tracing::info;

pub mod constraints;

// ---------------------------------------------------------------------------
// SEC-W24-4 — Bearer auth interceptor (release-image gated).
// ---------------------------------------------------------------------------

/// Release-image marker (mirrors `dcentrald-api::auth::is_release_image()` and
/// the MCP server). On a PRODUCTION/release build this file is stamped into the
/// rootfs; on a DEV/LAB image it is absent. The gRPC server is open (no auth)
/// on a dev image (byte-identical to today) and Bearer-gated on a release
/// image.
const RELEASE_IMAGE_MARKER: &str = "/etc/dcentos/release-image";

/// Token files the interceptor accepts on a release image, in order. The
/// daemon's REST auth surface (`/data/dcent/auth.json`) is not directly
/// linkable from this scaffold crate without a heavy dependency, so the
/// interceptor reads a dedicated root-only gRPC token; the daemon provisions
/// it from the same secret as the REST session model. If no token is
/// provisioned on a release image, the interceptor fails CLOSED (rejects all
/// RPCs) rather than fail-open.
const GRPC_TOKEN_FILES: &[&str] = &["/run/dcentos/grpc_token", "/data/dcent/grpc_token"];

fn release_image() -> bool {
    std::path::Path::new(RELEASE_IMAGE_MARKER).exists()
}

fn read_grpc_token() -> Option<String> {
    for path in GRPC_TOKEN_FILES {
        if let Ok(raw) = std::fs::read_to_string(path) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Constant-time string comparison (length difference mixed into the
/// accumulator) so a release-image attacker cannot time-oracle the token.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let ba = a.get(i).copied().unwrap_or(0);
        let bb = b.get(i).copied().unwrap_or(0);
        diff |= ba ^ bb;
    }
    diff == 0
}

/// tonic interceptor enforcing Bearer auth. Behaviour is decided per-request by
/// `evaluate_grpc_auth` so the policy is host-testable without a live image.
#[derive(Clone, Copy, Default)]
pub struct BearerAuthInterceptor;

impl Interceptor for BearerAuthInterceptor {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> {
        let presented = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        match evaluate_grpc_auth(
            release_image(),
            read_grpc_token().as_deref(),
            presented.as_deref(),
        ) {
            Ok(()) => Ok(request),
            Err(status) => Err(status),
        }
    }
}

/// Pure auth decision (SEC-W24-4), parameterized for host tests.
///
/// - **DEV/LAB image** (`release_image == false`): always `Ok` — open, exactly
///   as today.
/// - **Release image, no token provisioned**: fail CLOSED (`unauthenticated`).
/// - **Release image, token provisioned**: require `authorization: Bearer
///   <token>` with a constant-time match.
#[allow(clippy::result_large_err)]
fn evaluate_grpc_auth(
    release_image: bool,
    required_token: Option<&str>,
    presented_authorization: Option<&str>,
) -> Result<(), Status> {
    if !release_image {
        return Ok(());
    }
    let required = match required_token {
        Some(t) => t,
        None => {
            return Err(Status::unauthenticated(
                "release image: gRPC auth token not provisioned — refusing all RPCs",
            ));
        }
    };
    let presented = presented_authorization
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.trim());
    match presented {
        Some(tok) if constant_time_eq(tok, required) => Ok(()),
        _ => Err(Status::unauthenticated(
            "release image: valid Bearer token required",
        )),
    }
}

// tonic-generated prost types + service traits land in OUT_DIR/dcent.v1.rs
// via build.rs. The `dcent::v1` module path matches `package dcent.v1`.
pub mod dcent {
    pub mod v1 {
        tonic::include_proto!("dcent.v1");

        /// File-descriptor set produced by `prost-build` for tonic-reflection.
        /// The byte slice ships in the compiled binary; reflection clients
        /// read it without us re-parsing the .proto at runtime.
        pub const FILE_DESCRIPTOR_SET: &[u8] =
            tonic::include_file_descriptor_set!("dcent_v1_descriptor");
    }
}

use crate::dcent::v1::{
    fan_service_server::{FanService, FanServiceServer},
    locate_service_server::{LocateService, LocateServiceServer},
    miner_service_server::{MinerService, MinerServiceServer},
    pool_service_server::{PoolService, PoolServiceServer},
    tuner_service_server::{TunerService, TunerServiceServer},
    Empty, Fan, FanState, LocateRequest, LocateResult, MinerStatus, Pool, PoolList, RebootResult,
    SetFanModeRequest, SetFanModeResult, SetPoolsRequest, SetPoolsResult, SetTunerModeRequest,
    SetTunerModeResult, TunerConstraints, TunerMode,
};

const UNIMPLEMENTED_MSG: &str = "grpc handler not yet wired to dcentrald state";

/// Returned by the read RPCs when the daemon hasn't installed a runtime
/// snapshot yet — i.e. `[api.grpc].enabled` but the daemon is in a mode that
/// doesn't publish live `MinerState` (proxy/hybrid), or it's still booting.
/// Honest: `UNAVAILABLE` (retryable) not `unimplemented`.
const SNAPSHOT_UNAVAILABLE_MSG: &str =
    "runtime snapshot not yet installed (daemon booting or running a mode without live state)";

// ---------------------------------------------------------------------------
// Wave-I Lane A: live read-RPC backing via the proven OnceLock+watch snapshot
// pattern (mirrors `dcentrald-api::THERMAL_SUPERVISOR_RX`, Wave-G G1).
//
// The daemon owns the `MinerState` watch channel inside `Daemon::run()`, which
// is unreachable from the `serve()` spawn site. Instead of threading state
// into `serve()`, the daemon converts `MinerState` (+ autotune config) into the
// plain snapshot structs below and `install_runtime_snapshot_rx`-es a watch
// receiver into this static. The read RPCs `borrow()` it — zero-copy, no
// `dcentrald-api` dependency (keeps this crate lean), no write path.
// ---------------------------------------------------------------------------

/// Plain mirror of the proto `MinerStatus` (no prost/tonic in the snapshot so
/// the daemon doesn't need to depend on the generated types to build it).
#[derive(Clone, Debug, Default)]
pub struct GrpcMinerStatus {
    pub firmware_version: String,
    pub platform_marker: String,
    pub chip_family: String,
    pub hashrate_ths: f64,
    pub chain_count: u32,
    pub chain_alive_count: u32,
    pub uptime_seconds: u64,
    pub mining_state: String,
}

/// Plain mirror of one proto `Pool` (read side).
#[derive(Clone, Debug, Default)]
pub struct GrpcPoolEntry {
    pub url: String,
    pub worker: String,
    pub priority: u32,
}

/// Plain mirror of one proto `Fan`.
#[derive(Clone, Debug, Default)]
pub struct GrpcFanReading {
    pub index: u32,
    pub rpm: u32,
    pub pwm: u32,
    pub failed: bool,
}

/// Plain mirror of the proto `FanState`.
#[derive(Clone, Debug, Default)]
pub struct GrpcFanSnapshot {
    pub fans: Vec<GrpcFanReading>,
    pub control_mode: String,
    pub home_cap_pwm: u32,
}

/// Plain mirror of the proto `TunerMode` (read side). Built from the autotune
/// config discriminant at install time (config is restart-static).
#[derive(Clone, Debug, Default)]
pub struct GrpcTunerSnapshot {
    pub mode: String,
    pub power_target_watts: u32,
    pub hashrate_target_ths: f64,
    pub manual_freq_mhz: u32,
    pub manual_voltage_mv: u32,
}

/// Bundled runtime snapshot the daemon publishes on each `MinerState` update.
#[derive(Clone, Debug, Default)]
pub struct GrpcRuntimeSnapshot {
    pub status: GrpcMinerStatus,
    pub pools: Vec<GrpcPoolEntry>,
    pub fan: GrpcFanSnapshot,
    pub tuner: GrpcTunerSnapshot,
}

static RUNTIME_SNAPSHOT_RX: OnceLock<watch::Receiver<Option<GrpcRuntimeSnapshot>>> =
    OnceLock::new();

// ---------------------------------------------------------------------------
// SW-02 Lane B: write-RPC delegation.
//
// This crate is intentionally lean — it does NOT depend on `dcentrald-api`
// (which is HAL-bound and Linux-only), so it cannot call `rest::post_pools` /
// `rest::post_fan` / `rest::post_led_locate` directly. Mirroring the proven
// `install_runtime_snapshot_rx` read-path pattern, the daemon installs a
// boxed `GrpcWriteDelegate` whose methods bridge each write RPC to the SAME
// gated REST handlers the dashboard + cgminer-LuxOS surface use — so every
// safety/validation cap (fan <= 30 PWM home cap, voltage <= 14500 mV, pool URL
// validation, mode gate) is enforced in ONE place and the gRPC control plane
// can never bypass it.
//
// When NO delegate is installed (the daemon hasn't wired it, gRPC disabled, or
// a host test), the write RPCs return `Status::unimplemented` — byte-identical
// to the previous read-only contract. So shipping this is strictly additive:
// it only does something once the daemon opts in by installing a delegate.
// ---------------------------------------------------------------------------

/// Plain write request for `SetPools` (no prost types in the delegate boundary
/// so the daemon doesn't need the generated types to implement it).
#[derive(Clone, Debug, Default)]
pub struct GrpcSetPools {
    /// `(url, worker, password, priority)` per pool, priority 0 = primary.
    pub pools: Vec<(String, String, String, u32)>,
}

/// Plain write request for `SetFanMode`. `manual_pwm` is the operator-requested
/// duty; the REST handler the delegate calls clamps it to the active mode's cap
/// (home mode = 30 PWM HARD) — the delegate MUST report the actually-applied
/// value back in [`GrpcWriteOutcome::applied_value`].
#[derive(Clone, Debug, Default)]
pub struct GrpcSetFanMode {
    pub mode: String,
    pub manual_pwm: u32,
}

/// Plain write request for `SetTunerMode`. Mirrors the read-side
/// [`GrpcTunerSnapshot`] discriminant fields.
#[derive(Clone, Debug, Default)]
pub struct GrpcSetTunerMode {
    pub mode: String,
    pub power_target_watts: u32,
    pub hashrate_target_ths: f64,
    pub manual_freq_mhz: u32,
    pub manual_voltage_mv: u32,
}

/// Plain write request for `LocateDevice`.
#[derive(Clone, Debug, Default)]
pub struct GrpcLocate {
    pub duration_seconds: u32,
    pub off: bool,
}

/// Outcome returned by every delegate write. `detail` is a human-readable
/// status; `applied_value` carries the post-clamp result for the RPCs that
/// have one (fan PWM applied, locate LED state).
#[derive(Clone, Debug)]
pub struct GrpcWriteOutcome {
    pub acknowledged: bool,
    pub detail: String,
    /// Post-clamp applied numeric value (e.g. fan PWM). `None` when N/A.
    pub applied_value: Option<u32>,
    /// Free-form applied string (e.g. locate LED state "on"/"off"/"blinking").
    pub applied_text: Option<String>,
}

impl GrpcWriteOutcome {
    pub fn ack(detail: impl Into<String>) -> Self {
        Self {
            acknowledged: true,
            detail: detail.into(),
            applied_value: None,
            applied_text: None,
        }
    }

    /// Map a delegate error string into a gRPC `Status`. We use
    /// `failed_precondition` (not `internal`) because delegate failures are
    /// almost always rejected-input / unsupported-on-this-platform, mirroring
    /// the REST handlers' 4xx semantics.
    pub fn reject(detail: impl Into<String>) -> Self {
        Self {
            acknowledged: false,
            detail: detail.into(),
            applied_value: None,
            applied_text: None,
        }
    }
}

/// The write surface the daemon installs. Each method bridges to the
/// corresponding gated REST handler. The daemon's implementation lives in
/// `dcentrald` (which depends on both this crate and `dcentrald-api`); this
/// crate only defines the contract.
#[tonic::async_trait]
pub trait GrpcWriteDelegate: Send + Sync + 'static {
    async fn set_pools(&self, req: GrpcSetPools) -> Result<GrpcWriteOutcome, Status>;
    async fn set_fan_mode(&self, req: GrpcSetFanMode) -> Result<GrpcWriteOutcome, Status>;
    async fn set_tuner_mode(&self, req: GrpcSetTunerMode) -> Result<GrpcWriteOutcome, Status>;
    async fn reboot(&self) -> Result<GrpcWriteOutcome, Status>;
    async fn locate_device(&self, req: GrpcLocate) -> Result<GrpcWriteOutcome, Status>;
}

static WRITE_DELEGATE: OnceLock<Box<dyn GrpcWriteDelegate>> = OnceLock::new();

/// Install the write delegate. Called once by the daemon (`Daemon::run`) when
/// `[api.grpc].enabled`. Returns `false` if a delegate was already installed
/// (the existing one is kept — same once-only contract as
/// `install_runtime_snapshot_rx`). Until this is called, every write RPC
/// returns `UNIMPLEMENTED` exactly as before.
pub fn install_write_delegate(delegate: Box<dyn GrpcWriteDelegate>) -> bool {
    WRITE_DELEGATE.set(delegate).is_ok()
}

/// Borrow the installed write delegate, if any.
fn write_delegate() -> Option<&'static dyn GrpcWriteDelegate> {
    WRITE_DELEGATE.get().map(|b| b.as_ref())
}

/// Install the live runtime-snapshot channel. Called once by the daemon
/// (`Daemon::run`) when `[api.grpc].enabled`. Returns `false` if a channel was
/// already installed (the existing publisher is kept — same contract as
/// `dcentrald_api::install_runtime_health_rx`).
pub fn install_runtime_snapshot_rx(rx: watch::Receiver<Option<GrpcRuntimeSnapshot>>) -> bool {
    RUNTIME_SNAPSHOT_RX.set(rx).is_ok()
}

/// Read the latest runtime snapshot. `None` when the daemon never installed
/// the channel (gRPC disabled, or a mode without live `MinerState`) OR
/// installed-but-no-tick-yet — the read RPCs map that to `UNAVAILABLE`.
pub fn runtime_snapshot() -> Option<GrpcRuntimeSnapshot> {
    RUNTIME_SNAPSHOT_RX.get().and_then(|rx| rx.borrow().clone())
}

// ---------------------------------------------------------------------------
// MinerService — GetStatus is LIVE (read); Reboot stays unimplemented (write).
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct MinerSvc;

#[tonic::async_trait]
impl MinerService for MinerSvc {
    async fn get_status(&self, _req: Request<Empty>) -> Result<Response<MinerStatus>, Status> {
        let s = runtime_snapshot()
            .ok_or_else(|| Status::unavailable(SNAPSHOT_UNAVAILABLE_MSG))?
            .status;
        Ok(Response::new(MinerStatus {
            firmware_version: s.firmware_version,
            platform_marker: s.platform_marker,
            chip_family: s.chip_family,
            hashrate_ths: s.hashrate_ths,
            chain_count: s.chain_count,
            chain_alive_count: s.chain_alive_count,
            uptime_seconds: s.uptime_seconds,
            mining_state: s.mining_state,
        }))
    }

    // SW-02: Reboot delegates to the daemon-installed write delegate (which
    // bridges to the same gated REST/action handler the dashboard uses). When
    // no delegate is installed, returns UNIMPLEMENTED — byte-identical to the
    // prior read-only contract.
    async fn reboot(&self, _req: Request<Empty>) -> Result<Response<RebootResult>, Status> {
        let delegate = write_delegate().ok_or_else(|| Status::unimplemented(UNIMPLEMENTED_MSG))?;
        let outcome = delegate.reboot().await?;
        Ok(Response::new(RebootResult {
            acknowledged: outcome.acknowledged,
            detail: outcome.detail,
        }))
    }
}

// ---------------------------------------------------------------------------
// TunerService — GetConstraints is the ONE real handler in the S5.1 scaffold.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct TunerSvc {
    /// Mirrors `OperatingMode::from_config_str(config.mode.active).is_home()`
    /// from `dcentrald.toml`. Captured at server-construction time so the
    /// constraint reply is deterministic per-process; flipping mode requires a
    /// daemon reload.
    pub home_mode: bool,
    /// CE-122: chip family (e.g. "bm1362", "bm1387") resolved by the daemon at
    /// serve() time so `GetConstraints` returns a family-honest envelope instead
    /// of always advertising the BM1362 band + 14500 mV am2 cap. Empty → the
    /// pinned BM1362 default (backward compatible).
    pub chip_family: String,
}

#[tonic::async_trait]
impl TunerService for TunerSvc {
    async fn get_tuner_mode(&self, _req: Request<Empty>) -> Result<Response<TunerMode>, Status> {
        let t = runtime_snapshot()
            .ok_or_else(|| Status::unavailable(SNAPSHOT_UNAVAILABLE_MSG))?
            .tuner;
        Ok(Response::new(TunerMode {
            mode: t.mode,
            power_target_watts: t.power_target_watts,
            hashrate_target_ths: t.hashrate_target_ths,
            manual_freq_mhz: t.manual_freq_mhz,
            manual_voltage_mv: t.manual_voltage_mv,
        }))
    }

    // SW-02: SetTunerMode delegates to the daemon (which bridges to the gated
    // autotuner-mode REST path — `rest::persist_autotuner_mode` +
    // `rest::dispatch_autotuner_mode_command`). The REST path enforces the
    // voltage/freq clamps; this RPC adds no new bypass. UNIMPLEMENTED until the
    // delegate is installed.
    async fn set_tuner_mode(
        &self,
        req: Request<SetTunerModeRequest>,
    ) -> Result<Response<SetTunerModeResult>, Status> {
        let delegate = write_delegate().ok_or_else(|| Status::unimplemented(UNIMPLEMENTED_MSG))?;
        let mode = req.into_inner().mode.unwrap_or_default();
        let outcome = delegate
            .set_tuner_mode(GrpcSetTunerMode {
                mode: mode.mode.clone(),
                power_target_watts: mode.power_target_watts,
                hashrate_target_ths: mode.hashrate_target_ths,
                manual_freq_mhz: mode.manual_freq_mhz,
                manual_voltage_mv: mode.manual_voltage_mv,
            })
            .await?;
        // Reflect the post-apply active mode from the live snapshot when
        // available (best-effort; falls back to the requested mode).
        let active_mode = runtime_snapshot().map(|s| s.tuner).map(|t| TunerMode {
            mode: t.mode,
            power_target_watts: t.power_target_watts,
            hashrate_target_ths: t.hashrate_target_ths,
            manual_freq_mhz: t.manual_freq_mhz,
            manual_voltage_mv: t.manual_voltage_mv,
        });
        Ok(Response::new(SetTunerModeResult {
            acknowledged: outcome.acknowledged,
            detail: outcome.detail,
            active_mode: Some(active_mode.unwrap_or(mode)),
        }))
    }

    async fn get_constraints(
        &self,
        _req: Request<Empty>,
    ) -> Result<Response<TunerConstraints>, Status> {
        let constraints =
            constraints::build_constraints_for_chip(&self.chip_family, self.home_mode);
        Ok(Response::new(constraints))
    }
}

// ---------------------------------------------------------------------------
// PoolService scaffold.
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct PoolSvc;

#[tonic::async_trait]
impl PoolService for PoolSvc {
    async fn get_pools(&self, _req: Request<Empty>) -> Result<Response<PoolList>, Status> {
        let pools = runtime_snapshot()
            .ok_or_else(|| Status::unavailable(SNAPSHOT_UNAVAILABLE_MSG))?
            .pools
            .into_iter()
            .map(|p| Pool {
                url: p.url,
                worker: p.worker,
                // Passwords are never surfaced over the read RPC.
                password: String::new(),
                priority: p.priority,
            })
            .collect();
        Ok(Response::new(PoolList { pools }))
    }

    // SW-02: SetPools delegates to the daemon (which bridges to
    // `rest::post_pools` — pool URL validation, TOML write + reload). The
    // delegate carries passwords through to the config write (the read RPC
    // still never surfaces them). UNIMPLEMENTED until the delegate is
    // installed.
    async fn set_pools(
        &self,
        req: Request<SetPoolsRequest>,
    ) -> Result<Response<SetPoolsResult>, Status> {
        let delegate = write_delegate().ok_or_else(|| Status::unimplemented(UNIMPLEMENTED_MSG))?;
        let pools = req
            .into_inner()
            .pools
            .into_iter()
            .map(|p| (p.url, p.worker, p.password, p.priority))
            .collect();
        let outcome = delegate.set_pools(GrpcSetPools { pools }).await?;
        Ok(Response::new(SetPoolsResult {
            acknowledged: outcome.acknowledged,
            detail: outcome.detail,
        }))
    }
}

// ---------------------------------------------------------------------------
// FanService scaffold.
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct FanSvc {
    /// W3 E-05: propagated from `serve(home_mode)` so the gRPC fan write surface
    /// can enforce the PWM-30 home cap itself (defense-in-depth), independent of
    /// the delegate/REST clamp. `false` (industrial) forwards the requested PWM.
    pub home_mode: bool,
}

#[tonic::async_trait]
impl FanService for FanSvc {
    async fn get_fan_state(&self, _req: Request<Empty>) -> Result<Response<FanState>, Status> {
        let f = runtime_snapshot()
            .ok_or_else(|| Status::unavailable(SNAPSHOT_UNAVAILABLE_MSG))?
            .fan;
        let fans = f
            .fans
            .into_iter()
            .map(|fan| Fan {
                index: fan.index,
                rpm: fan.rpm,
                pwm: fan.pwm,
                failed: fan.failed,
            })
            .collect();
        Ok(Response::new(FanState {
            fans,
            control_mode: f.control_mode,
            home_cap_pwm: f.home_cap_pwm,
        }))
    }

    // SW-02: SetFanMode delegates to the daemon (which bridges to
    // `rest::post_fan`). The REST handler enforces the per-mode PWM clamp —
    // HOME mode caps at 30 PWM HARD (load-bearing safety contract). This RPC
    // passes the operator-requested PWM through; the *applied* (post-clamp)
    // value comes back in `applied_pwm`, so a client asking for 100 on a home
    // unit sees `applied_pwm: 30`. UNIMPLEMENTED until the delegate is
    // installed.
    async fn set_fan_mode(
        &self,
        req: Request<SetFanModeRequest>,
    ) -> Result<Response<SetFanModeResult>, Status> {
        let delegate = write_delegate().ok_or_else(|| Status::unimplemented(UNIMPLEMENTED_MSG))?;
        let r = req.into_inner();
        // W3 E-05 / load-bearing PWM-30 contract: clamp at the gRPC surface too.
        // The delegate + REST path also clamp, but a home unit must NEVER forward
        // a fan command above the home cap, independent of the delegate impl.
        const HOME_FAN_PWM_CAP: u32 = 30;
        let manual_pwm = if self.home_mode {
            r.manual_pwm.min(HOME_FAN_PWM_CAP)
        } else {
            r.manual_pwm
        };
        let outcome = delegate
            .set_fan_mode(GrpcSetFanMode {
                mode: r.mode,
                manual_pwm,
            })
            .await?;
        Ok(Response::new(SetFanModeResult {
            acknowledged: outcome.acknowledged,
            detail: outcome.detail,
            // The applied PWM is the post-clamp value the daemon actually set
            // (home cap honoured). Falls back to 0 if the delegate didn't
            // report one (e.g. a rejected request).
            applied_pwm: outcome.applied_value.unwrap_or(0),
        }))
    }
}

// ---------------------------------------------------------------------------
// LocateService scaffold.
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone)]
pub struct LocateSvc;

#[tonic::async_trait]
impl LocateService for LocateSvc {
    // SW-02: LocateDevice delegates to the daemon (which bridges to
    // `rest::post_led_locate`). LED-only, no hash/power/thermal effect.
    // UNIMPLEMENTED until the delegate is installed.
    async fn locate_device(
        &self,
        req: Request<LocateRequest>,
    ) -> Result<Response<LocateResult>, Status> {
        let delegate = write_delegate().ok_or_else(|| Status::unimplemented(UNIMPLEMENTED_MSG))?;
        let r = req.into_inner();
        let outcome = delegate
            .locate_device(GrpcLocate {
                duration_seconds: r.duration_seconds,
                off: r.off,
            })
            .await?;
        Ok(Response::new(LocateResult {
            acknowledged: outcome.acknowledged,
            detail: outcome.detail,
            led_state: outcome.applied_text.unwrap_or_default(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Server entry point.
// ---------------------------------------------------------------------------

/// Start the gRPC server on `addr`. Returns once the server exits (typically
/// on shutdown signal forwarded by tonic). `home_mode` propagates the
/// home-mining fan cap into `GetConstraints` replies.
///
/// Reflection is always enabled — there's no security cost (reflection
/// only enumerates services already exposed) and it dramatically improves
/// CLI debuggability with `grpcurl` / `bloomrpc` / Postman.
pub async fn serve(
    addr: SocketAddr,
    home_mode: bool,
    chip_family: String,
) -> Result<(), tonic::transport::Error> {
    info!(
        %addr,
        home_mode,
        chip_family = %chip_family,
        "starting dcentrald-api-grpc server (S5.1 scaffold)"
    );

    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(dcent::v1::FILE_DESCRIPTOR_SET)
        .build_v1()
        .expect("reflection service builder cannot fail with a valid descriptor set");

    // SEC-W24-4: wrap each RPC service with the Bearer interceptor. On a DEV
    // image the interceptor is a pass-through (open, as today); on a release
    // image it enforces a constant-time Bearer token. Reflection is left
    // un-intercepted — it only enumerates service names already public, and
    // CLI debuggability (`grpcurl list`) must keep working.
    info!(
        release_image = release_image(),
        "gRPC Bearer auth interceptor installed (enforced on release images only)"
    );
    let auth = BearerAuthInterceptor;

    Server::builder()
        .add_service(MinerServiceServer::with_interceptor(MinerSvc, auth))
        .add_service(TunerServiceServer::with_interceptor(
            TunerSvc {
                home_mode,
                chip_family: chip_family.clone(),
            },
            auth,
        ))
        .add_service(PoolServiceServer::with_interceptor(PoolSvc, auth))
        .add_service(FanServiceServer::with_interceptor(
            FanSvc { home_mode },
            auth,
        ))
        .add_service(LocateServiceServer::with_interceptor(LocateSvc, auth))
        .add_service(reflection)
        .serve(addr)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dcent::v1::{Pool, SetPoolsRequest};
    use proptest::prelude::*;
    use prost::Message;
    use std::sync::{Arc, Mutex};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn grpc_protobuf_decoders_never_panic_on_arbitrary_bytes(
            data in proptest::collection::vec(any::<u8>(), 0..4096)
        ) {
            let _ = PoolList::decode(data.as_slice());
            let _ = SetPoolsRequest::decode(data.as_slice());
            let _ = TunerConstraints::decode(data.as_slice());
        }

        #[test]
        fn grpc_auth_evaluator_never_panics_on_arbitrary_headers(
            release_image in any::<bool>(),
            provisioned in proptest::option::of(".{0,128}"),
            header in proptest::option::of(".{0,256}")
        ) {
            let _ = evaluate_grpc_auth(release_image, provisioned.as_deref(), header.as_deref());
        }
    }

    // SEC-W24-4: gRPC Bearer auth gate (release-image only).

    #[test]
    fn grpc_auth_dev_image_is_open() {
        // DEV image: open regardless of token/header (byte-identical to today).
        assert!(evaluate_grpc_auth(false, None, None).is_ok());
        assert!(evaluate_grpc_auth(false, Some("tok"), None).is_ok());
        assert!(evaluate_grpc_auth(false, Some("tok"), Some("Bearer wrong")).is_ok());
    }

    #[test]
    fn grpc_auth_release_image_requires_token() {
        // Release image, correct Bearer token → Ok.
        assert!(evaluate_grpc_auth(true, Some("s3cr3t"), Some("Bearer s3cr3t")).is_ok());
        // Wrong token → unauthenticated.
        let err = evaluate_grpc_auth(true, Some("s3cr3t"), Some("Bearer nope"))
            .expect_err("wrong token rejected");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
        // Missing header → unauthenticated.
        let err = evaluate_grpc_auth(true, Some("s3cr3t"), None).expect_err("no header rejected");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
        // Non-Bearer scheme → unauthenticated.
        let err = evaluate_grpc_auth(true, Some("s3cr3t"), Some("Basic s3cr3t"))
            .expect_err("basic scheme rejected");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn grpc_auth_release_image_no_token_fails_closed() {
        // Release image with no provisioned token → fail CLOSED (never open).
        let err = evaluate_grpc_auth(true, None, Some("Bearer anything"))
            .expect_err("missing token fails closed");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
        let err = evaluate_grpc_auth(true, None, None)
            .expect_err("missing token + no header fails closed");
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn grpc_constant_time_eq_matches_and_rejects() {
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("abc", "abc123"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn proto_roundtrip_pool_list() {
        let original = PoolList {
            pools: vec![Pool {
                url: "stratum+tcp://public-pool.io:21496".to_string(),
                worker: "bc1q-test".to_string(),
                password: "x".to_string(),
                priority: 0,
            }],
        };
        let bytes = original.encode_to_vec();
        let decoded = PoolList::decode(bytes.as_slice()).expect("decode round-trips");
        assert_eq!(decoded, original);
    }

    #[test]
    fn proto_roundtrip_set_pools_request() {
        let req = SetPoolsRequest {
            pools: vec![
                Pool {
                    url: "stratum+tcp://primary:3333".into(),
                    worker: "w1".into(),
                    password: "x".into(),
                    priority: 0,
                },
                Pool {
                    url: "stratum+tcp://backup:3333".into(),
                    worker: "w2".into(),
                    password: "x".into(),
                    priority: 1,
                },
            ],
        };
        let bytes = req.encode_to_vec();
        let decoded = SetPoolsRequest::decode(bytes.as_slice()).expect("decode round-trips");
        assert_eq!(decoded.pools.len(), 2);
        assert_eq!(decoded.pools[0].priority, 0);
        assert_eq!(decoded.pools[1].priority, 1);
    }

    #[test]
    fn proto_roundtrip_constraints_preserves_clamps() {
        let c = constraints::build_bm1362_constraints(true);
        let bytes = c.encode_to_vec();
        let decoded = TunerConstraints::decode(bytes.as_slice()).expect("decode round-trips");
        let fan = decoded.fan_envelope.expect("fan envelope present");
        assert_eq!(fan.max_pwm, constraints::HOME_FAN_PWM_MAX);
        let v = decoded.voltage_envelope.expect("voltage envelope present");
        assert_eq!(v.max_mv, constraints::VOLTAGE_MAX_MV_AM2);
    }

    #[tokio::test]
    async fn get_constraints_returns_real_envelope_with_clamps() {
        let svc = TunerSvc {
            home_mode: true,
            ..Default::default()
        };
        let resp = svc
            .get_constraints(Request::new(Empty {}))
            .await
            .expect("real handler does not error");
        let c = resp.into_inner();
        let fan = c.fan_envelope.expect("fan envelope present");
        assert_eq!(fan.max_pwm, constraints::HOME_FAN_PWM_MAX);
        let v = c.voltage_envelope.expect("voltage envelope present");
        assert!(v.max_mv <= constraints::VOLTAGE_MAX_MV_AM2);
        assert_eq!(c.chip_family, "bm1362");
        assert_eq!(c.source, "silicon-profiles");
    }

    // SW-02: mock delegate that echoes a post-clamp fan PWM (mimics the
    // daemon's home-cap behaviour) so the test can prove the gRPC layer
    // surfaces the *applied* value, not the requested one.
    struct MockDelegate {
        home_fan_cap: u32,
        seen_fan_pwms: Arc<Mutex<Vec<u32>>>,
    }

    impl Default for MockDelegate {
        fn default() -> Self {
            Self {
                home_fan_cap: 0,
                seen_fan_pwms: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[tonic::async_trait]
    impl GrpcWriteDelegate for MockDelegate {
        async fn set_pools(&self, req: GrpcSetPools) -> Result<GrpcWriteOutcome, Status> {
            let n = req.pools.len();
            Ok(GrpcWriteOutcome::ack(format!("{n} pools written")))
        }
        async fn set_fan_mode(&self, req: GrpcSetFanMode) -> Result<GrpcWriteOutcome, Status> {
            // Mimic the REST home-cap clamp the real delegate inherits.
            self.seen_fan_pwms
                .lock()
                .expect("recording fan PWM should not be poisoned")
                .push(req.manual_pwm);
            let applied = req.manual_pwm.min(self.home_fan_cap);
            Ok(GrpcWriteOutcome {
                acknowledged: true,
                detail: format!("fan set to {applied} (requested {})", req.manual_pwm),
                applied_value: Some(applied),
                applied_text: None,
            })
        }
        async fn set_tuner_mode(&self, _req: GrpcSetTunerMode) -> Result<GrpcWriteOutcome, Status> {
            Ok(GrpcWriteOutcome::ack("tuner mode applied"))
        }
        async fn reboot(&self) -> Result<GrpcWriteOutcome, Status> {
            Ok(GrpcWriteOutcome::ack("reboot scheduled"))
        }
        async fn locate_device(&self, req: GrpcLocate) -> Result<GrpcWriteOutcome, Status> {
            let mut out = GrpcWriteOutcome::ack("locate toggled");
            out.applied_text = Some(if req.off { "off" } else { "blinking" }.to_string());
            Ok(out)
        }
    }

    // Sole toucher of the `WRITE_DELEGATE` OnceLock in this test binary — keep
    // ALL write-delegation assertions here (before-install UNIMPLEMENTED + the
    // post-install delegation) so parallel tests can't race the once-only
    // install.
    #[tokio::test]
    async fn write_rpcs_unimplemented_before_delegate_then_delegate_after() {
        // Before install: every WRITE RPC returns a clean UNIMPLEMENTED, never
        // a mutation — byte-identical to the prior read-only contract.
        let err = MinerSvc
            .reboot(Request::new(Empty {}))
            .await
            .expect_err("write RPC stays unimplemented");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        let err = PoolSvc
            .set_pools(Request::new(SetPoolsRequest { pools: vec![] }))
            .await
            .expect_err("write RPC stays unimplemented");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        let err = FanSvc::default()
            .set_fan_mode(Request::new(SetFanModeRequest {
                mode: "auto".into(),
                manual_pwm: 0,
            }))
            .await
            .expect_err("write RPC stays unimplemented");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        let err = (TunerSvc {
            home_mode: false,
            ..Default::default()
        })
        .set_tuner_mode(Request::new(SetTunerModeRequest { mode: None }))
        .await
        .expect_err("write RPC stays unimplemented");
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        let err = LocateSvc
            .locate_device(Request::new(LocateRequest {
                duration_seconds: 30,
                off: false,
            }))
            .await
            .expect_err("write RPC stays unimplemented");
        assert_eq!(err.code(), tonic::Code::Unimplemented);

        // Install the mock delegate (the real daemon installs one that bridges
        // to the gated REST handlers). HOME fan cap = 30 PWM. The delegate's
        // outcome detail / applied value carries the proof we assert on below.
        let seen_fan_pwms = Arc::new(Mutex::new(Vec::new()));
        let delegate = Box::new(MockDelegate {
            home_fan_cap: 30,
            seen_fan_pwms: Arc::clone(&seen_fan_pwms),
        });
        assert!(install_write_delegate(delegate));
        // OnceLock rejects a second install.
        assert!(!install_write_delegate(Box::new(MockDelegate::default())));

        // SetPools delegates → ack.
        let resp = PoolSvc
            .set_pools(Request::new(SetPoolsRequest {
                pools: vec![Pool {
                    url: "stratum+tcp://public-pool.io:21496".into(),
                    worker: "bc1qexample".into(),
                    password: "x".into(),
                    priority: 0,
                }],
            }))
            .await
            .expect("set_pools delegates")
            .into_inner();
        assert!(resp.acknowledged);
        assert_eq!(resp.detail, "1 pools written");

        // SetFanMode: a request for 100 PWM on a home unit must come back with
        // applied_pwm = 30. With home_mode the gRPC handler pre-clamps to 30
        // (W3 E-05 defense-in-depth) AND the delegate clamps — both must hold.
        let resp = FanSvc { home_mode: true }
            .set_fan_mode(Request::new(SetFanModeRequest {
                mode: "manual".into(),
                manual_pwm: 100,
            }))
            .await
            .expect("set_fan_mode delegates")
            .into_inner();
        assert!(resp.acknowledged);
        assert_eq!(
            resp.applied_pwm, 30,
            "home fan cap must clamp the applied PWM to 30"
        );
        assert_eq!(
            seen_fan_pwms
                .lock()
                .expect("recorded fan PWM should be readable")
                .last()
                .copied(),
            Some(30),
            "home gRPC handler must pre-clamp before delegating"
        );

        let heater_home_mode =
            dcentrald_api_types::OperatingMode::from_config_str("heater").is_home();
        let resp = FanSvc {
            home_mode: heater_home_mode,
        }
        .set_fan_mode(Request::new(SetFanModeRequest {
            mode: "manual".into(),
            manual_pwm: 100,
        }))
        .await
        .expect("heater alias set_fan_mode delegates")
        .into_inner();
        assert_eq!(
            resp.applied_pwm, 30,
            "heater alias must select the same home fan pre-clamp"
        );
        assert_eq!(
            seen_fan_pwms
                .lock()
                .expect("recorded fan PWM should be readable")
                .as_slice(),
            &[30, 30],
            "heater alias must pre-clamp before the delegate sees the request"
        );

        // Reboot delegates → ack.
        let resp = MinerSvc
            .reboot(Request::new(Empty {}))
            .await
            .expect("reboot delegates")
            .into_inner();
        assert!(resp.acknowledged);

        // LocateDevice delegates → blinking when not explicitly off.
        let resp = LocateSvc
            .locate_device(Request::new(LocateRequest {
                duration_seconds: 30,
                off: false,
            }))
            .await
            .expect("locate delegates")
            .into_inner();
        assert!(resp.acknowledged);
        assert_eq!(resp.led_state, "blinking");

        // SetTunerMode delegates → ack + reflects active mode.
        let resp = (TunerSvc {
            home_mode: true,
            ..Default::default()
        })
        .set_tuner_mode(Request::new(SetTunerModeRequest {
            mode: Some(TunerMode {
                mode: "efficiency".into(),
                ..Default::default()
            }),
        }))
        .await
        .expect("set_tuner_mode delegates")
        .into_inner();
        assert!(resp.acknowledged);
        assert!(resp.active_mode.is_some());
    }

    // Sole toucher of the `RUNTIME_SNAPSHOT_RX` OnceLock in this test binary —
    // keep all snapshot lifecycle assertions here so parallel tests can't race
    // the once-only install.
    #[tokio::test]
    async fn read_rpcs_unavailable_before_install_then_map_after() {
        // Before install: read RPCs report UNAVAILABLE (retryable), NOT
        // unimplemented — the contract is "wired but no live state yet".
        assert!(runtime_snapshot().is_none());
        let err = MinerSvc
            .get_status(Request::new(Empty {}))
            .await
            .expect_err("no snapshot installed yet");
        assert_eq!(err.code(), tonic::Code::Unavailable);

        // Install a snapshot exactly as `Daemon::run` does.
        let snap = GrpcRuntimeSnapshot {
            status: GrpcMinerStatus {
                firmware_version: "1.2.3".into(),
                platform_marker: "am2-s19jpro".into(),
                chip_family: "bm1362".into(),
                hashrate_ths: 13.5,
                chain_count: 3,
                chain_alive_count: 3,
                uptime_seconds: 42,
                mining_state: "mining".into(),
            },
            pools: vec![GrpcPoolEntry {
                url: "stratum+tcp://public-pool.io:21496".into(),
                worker: "bc1qexample".into(),
                priority: 0,
            }],
            fan: GrpcFanSnapshot {
                fans: vec![GrpcFanReading {
                    index: 0,
                    rpm: 1260,
                    pwm: 30,
                    failed: false,
                }],
                control_mode: "auto".into(),
                home_cap_pwm: 30,
            },
            tuner: GrpcTunerSnapshot {
                mode: "efficiency".into(),
                ..Default::default()
            },
        };
        let (tx, rx) = watch::channel(Some(snap));
        assert!(install_runtime_snapshot_rx(rx));
        // OnceLock rejects a second install (existing publisher kept).
        let (_tx2, rx2) = watch::channel(None);
        assert!(!install_runtime_snapshot_rx(rx2));

        // After install: each read RPC maps its snapshot sub-struct.
        let status = MinerSvc
            .get_status(Request::new(Empty {}))
            .await
            .expect("status maps after install")
            .into_inner();
        assert_eq!(status.firmware_version, "1.2.3");
        assert_eq!(status.hashrate_ths, 13.5);
        assert_eq!(status.chain_alive_count, 3);
        assert_eq!(status.mining_state, "mining");

        let pools = PoolSvc
            .get_pools(Request::new(Empty {}))
            .await
            .expect("pools map after install")
            .into_inner();
        assert_eq!(pools.pools.len(), 1);
        assert_eq!(pools.pools[0].priority, 0);
        assert!(
            pools.pools[0].password.is_empty(),
            "password must never be surfaced over the read RPC"
        );

        let fan = FanSvc::default()
            .get_fan_state(Request::new(Empty {}))
            .await
            .expect("fan maps after install")
            .into_inner();
        assert_eq!(fan.home_cap_pwm, 30);
        assert_eq!(fan.fans.len(), 1);
        assert_eq!(fan.fans[0].pwm, 30);

        let tuner = (TunerSvc {
            home_mode: true,
            ..Default::default()
        })
        .get_tuner_mode(Request::new(Empty {}))
        .await
        .expect("tuner maps after install")
        .into_inner();
        assert_eq!(tuner.mode, "efficiency");

        // Keep the sender alive until all reads are done.
        drop(tx);
    }

    #[test]
    fn file_descriptor_set_is_non_empty() {
        // tonic-reflection refuses to start with an empty FDS — pin the build
        // output so a botched build.rs regression fails the test rather than
        // silently shipping a no-reflection server.
        assert!(!dcent::v1::FILE_DESCRIPTOR_SET.is_empty());
        assert!(dcent::v1::FILE_DESCRIPTOR_SET.len() > 32);
    }
}
