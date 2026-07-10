//! Core types shared across the stratum subsystem.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default Stratum V1 extranonce2 byte width used when a pool omits it.
pub const DEFAULT_V1_EXTRANONCE2_SIZE: usize = 4;

/// Maximum Stratum V1 extranonce2 byte width accepted from pool input.
///
/// V1 work generation stores the extranonce2 counter in a u64 and formats it
/// into heap buffers. Wider pool-provided values cannot be represented
/// honestly and must not become allocation sizes.
pub const MAX_V1_EXTRANONCE2_SIZE: usize = 8;

/// True when a Stratum V1 pool-provided extranonce2 width is safe to use.
pub fn is_valid_v1_extranonce2_size(size: usize) -> bool {
    (1..=MAX_V1_EXTRANONCE2_SIZE).contains(&size)
}

/// Stratum subsystem error type.
#[derive(Debug, Error)]
pub enum StratumError {
    /// Pool connection failure.
    #[error("connection error: {0}")]
    Connection(String),

    /// Authentication failure (subscribe or authorize rejected).
    #[error("auth error: {0}")]
    Auth(String),

    /// Protocol parse error (malformed JSON-RPC).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// I/O error on the TCP socket.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// All configured pools exhausted.
    #[error("all pools failed")]
    AllPoolsFailed,
}

/// A job template received from a pool via mining.notify.
///
/// Contains all the data needed to construct block headers and dispatch
/// work to ASIC chips. The work builder uses this to generate midstates
/// and package ASIC-ready work units.
#[derive(Debug, Clone)]
pub struct JobTemplate {
    /// Pool-assigned unique job identifier (e.g., "bf", "1a3f")
    pub job_id: String,

    /// Previous block hash in pool byte order (32 bytes).
    /// Must be word-reversed before placing in block header.
    pub prev_block_hash: [u8; 32],

    /// First part of coinbase transaction (hex bytes).
    /// Everything before the extranonce insertion point.
    pub coinbase1: Vec<u8>,

    /// Second part of coinbase transaction (hex bytes).
    /// Everything after the extranonce insertion point.
    pub coinbase2: Vec<u8>,

    /// Merkle branch hashes for computing the merkle root.
    /// Coinbase is always leftmost leaf — branches are concatenated on the right.
    pub merkle_branches: Vec<[u8; 32]>,

    /// Block version (little-endian as received from pool).
    pub version: u32,

    /// Compact difficulty target (nbits).
    pub nbits: u32,

    /// Block timestamp (Unix epoch seconds).
    pub ntime: u32,

    /// If true, discard all pending work and switch to this job immediately.
    /// The extranonce2 counter should be reset.
    pub clean_jobs: bool,

    /// Pool-assigned share target derived from current difficulty.
    /// A share is valid if double_sha256(header) <= share_target.
    pub share_target: [u8; 32],

    /// Pool-assigned extranonce1 (unique per session).
    pub extranonce1: Vec<u8>,

    /// Size in bytes of the miner-controlled extranonce2 field.
    pub extranonce2_size: usize,

    /// Negotiated version rolling mask from mining.configure (BIP 310).
    /// 0 = no version rolling. Non-zero = ASICBoost enabled.
    /// Propagated from the Stratum client so the work dispatcher can generate
    /// distinct midstates per FPGA slot and correct version_bits for submissions.
    pub version_mask: u32,

    /// Pre-computed merkle root (32 bytes, raw SHA-256d byte order).
    /// Used by SV2 Standard Channels where the pool provides the merkle root
    /// directly (no coinbase parts or merkle branches). For V1 jobs this is
    /// [0; 32] and the WorkBuilder computes it from coinbase + branches.
    pub merkle_root: [u8; 32],

    /// Current pool-assigned difficulty for share validation.
    ///
    /// The autotuner needs the raw difficulty number (not just share_target)
    /// to compute expected nonce rates per chip:
    ///   expected_nps = freq_mhz * 1e6 * 114 / (difficulty * 2^32)
    ///
    /// Without this, the autotuner hardcodes 256 and makes wrong backoff
    /// decisions when the pool changes difficulty (e.g., 10000 → 50000).
    pub pool_difficulty: f64,
}

/// SW-11: default forward ntime-roll window, in seconds.
///
/// Host-driven ntime rolling lets a miner advance the block-header timestamp to
/// extend the per-job search space without waiting for a new `mining.notify`.
/// The roll MUST stay inside the pool's accepted window (Bitcoin consensus is
/// ±~2 h of network-adjusted time; the SW-03 pre-submit guard
/// `NTIME_VALIDITY_WINDOW_SECS = 7200` in `v1::client` is the hard reject edge).
///
/// We default the *roll* budget to a conservative 60 s forward. Stratum V1 does
/// not surface an explicit per-job ntime-roll allowance the way SV2's
/// `min_ntime`/extended-job fields do, so this is the safe house value: a few
/// nonce-ranges of headroom, far inside the ±7200 s consensus window, and small
/// enough that a clock-skewed pool will not reject a rolled share. A driver/
/// dispatcher that knows a wider pool-specific allowance can pass a larger
/// `window_secs` to [`JobTemplate::roll_ntime_within_window`] — but the helper
/// always clamps to the requested window so a rolled ntime can never leave it.
pub const DEFAULT_NTIME_ROLL_WINDOW_SECS: u32 = 60;

impl JobTemplate {
    /// True for control-only templates used to flush stale ASIC work without
    /// dispatching a new header.
    pub fn is_flush_only(&self) -> bool {
        self.clean_jobs
            && self.job_id.is_empty()
            && self.coinbase1.is_empty()
            && self.coinbase2.is_empty()
            && self.merkle_branches.is_empty()
            && self.prev_block_hash == [0u8; 32]
            && self.merkle_root == [0u8; 32]
    }

    /// SW-11: roll this job's ntime forward by `roll_secs`, **hard-bounded** to
    /// `window_secs` of the job's base ntime (pure → host-tested).
    ///
    /// Returns the ntime a work unit should be both *hashed with* and *submitted
    /// with*. The whole point of a single canonical helper is parity: the value
    /// returned here is the value the ASIC hashes AND the value that goes into
    /// `mining.submit`, so the submitted ntime can never diverge from the hashed
    /// ntime (the SW-11 parity invariant).
    ///
    /// Bounding rules (so a host roll can never produce an out-of-window, and
    /// therefore guaranteed-reject, share — see the SW-03 pre-submit guard):
    /// - `roll_secs` is clamped to `window_secs` (a request to roll further than
    ///   the window lands exactly on the window edge, never past it).
    /// - The forward roll is applied with saturating arithmetic so it can never
    ///   wrap the `u32` timestamp.
    /// - `roll_secs == 0` returns the base `ntime` unchanged — so the default
    ///   (no-roll) path is byte-identical to today's behavior.
    ///
    /// This is intentionally a *forward-only* roll: the SW-03 window is
    /// symmetric (it also catches stale backward drift), but a miner has no
    /// reason to roll a timestamp *backward*, and forward-only keeps the share
    /// monotonic with wall-clock.
    pub fn roll_ntime_within_window(&self, roll_secs: u32, window_secs: u32) -> u32 {
        let bounded_roll = roll_secs.min(window_secs);
        self.ntime.saturating_add(bounded_roll)
    }
}

/// A valid share to submit to the pool via mining.submit.
#[derive(Debug, Clone)]
pub struct ValidShare {
    /// Worker name used for authorization.
    pub worker_name: String,

    /// Job ID from the original mining.notify.
    pub job_id: String,

    /// Miner-generated extranonce2 (hex string, extranonce2_size * 2 chars).
    pub extranonce2: String,

    /// Block timestamp that produced the valid hash (hex, 8 chars).
    /// May differ from the job's ntime if ntime rolling was used.
    pub ntime: String,

    /// The 32-bit nonce that produced a valid hash (hex, 8 chars).
    pub nonce: String,

    /// Version bits delta set by the miner (hex, 8 chars).
    /// Only present when version rolling (BIP 310) is active.
    /// This is the XOR delta from the base version — used by V1 mining.submit.
    pub version_bits: Option<String>,

    /// Full block header version (base version with rolled bits applied).
    /// Used by SV2 SubmitSharesStandard which needs the actual version, not the delta.
    pub version: u32,

    /// Locally computed achieved pool difficulty for this share hash, when the
    /// dispatcher had enough header metadata to validate the exact hash.
    ///
    /// This is distinct from the pool target difficulty. A share accepted at
    /// target 8192 may have achieved difficulty much higher than 8192, but the
    /// pool target alone is only minimum/credit evidence.
    pub achieved_difficulty: Option<f64>,
}

/// Optional correlated metadata for accepted/rejected share events.
#[derive(Debug, Clone)]
pub struct ShareEventMeta {
    /// The exact share payload that was submitted to the pool.
    pub share: ValidShare,
}

/// Pool configuration for a single pool endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Pool URL in format "stratum+tcp://host:port"
    pub url: String,

    /// Worker name (format: "username.worker_name")
    pub worker: String,

    /// Password (often "x" or empty)
    pub password: String,

    /// SV2 endpoint URL (e.g., "stratum2+tcp://v2.braiins.com:3336").
    /// When set with protocol="auto", the router will prefer SV2.
    /// When set with protocol="sv2", this URL is used instead of `url`.
    #[serde(default)]
    pub sv2_url: Option<String>,

    /// Optional per-pool protocol hint: "sv1"/"v1", "sv2"/"v2", or "auto".
    /// Runtime currently uses this in Auto mode when deciding how to connect to
    /// the active pool endpoint.
    #[serde(default)]
    pub protocol: Option<String>,

    /// User-hashrate split weight in basis points when weighted split routing is
    /// enabled. 8000 means 80.00% of user-pool mining time.
    #[serde(default)]
    pub split_bps: Option<u16>,
}

fn default_pool_routing_mode() -> String {
    "failover".to_string()
}

fn default_split_cycle_duration_s() -> u64 {
    1800
}

/// Stable-primary-return anti-flap cool-down (seconds). Conservative
/// default (15 min) — far longer than any reconnect backoff cycle, so
/// the primary can never oscillate. `0` disables the feature (legacy
/// pure round-robin failover). Pool-failover robustness increment 1.
fn default_primary_return_stability_secs() -> u64 {
    900
}

/// No-`mining.notify` failover timeout (seconds). After the handshake,
/// if the pool sends no new job for this long the session is failed
/// (feeding the existing consecutive-failure/failover machinery — not a
/// parallel path). Conservative default (300s) — well beyond the 120s
/// canonical stall value and normal notify/vardiff cadence, so a
/// legitimately-quiet pool never triggers a false failover. `0`
/// disables. Pool-failover robustness increment 2.
fn default_no_notify_failover_secs() -> u64 {
    300
}

/// Reject-rate failover threshold (percent, 0-100). HIGHEST flap risk
/// (vardiff transitions / transient pool blips spike rejects), so this
/// is OPT-IN: default `0` = DISABLED. When >0, a session whose
/// reject-rate ≥ this — measured over ≥ `reject_rate_failover_min_samples`
/// shares since the handshake — fails into the existing failover path.
/// No universally-safe threshold exists; operators set it per
/// pool/hardware. Pool-failover robustness increment 3.
fn default_reject_rate_failover_pct() -> u8 {
    0
}

/// Minimum post-handshake shares before reject-rate failover may act —
/// prevents acting on tiny/transient samples (vardiff step, warm-up).
fn default_reject_rate_failover_min_samples() -> u64 {
    100
}

/// Operational cap (bytes) on an inbound SV2 mining-client frame
/// payload. 1 MiB — ≈8× the largest realistic pool→miner mining message
/// yet far below the memory-amplification danger zone on a 228 MB-class
/// miner. `0` = disabled (16 MiB wire-format protocol max). SV2
/// inbound-frame cap (strat-09 hardening).
fn default_sv2_max_inbound_frame_bytes() -> u32 {
    1_048_576
}

/// Operational cap (bytes) on an inbound **Stratum V1** line. 64 KiB —
/// ≈16× the largest realistic pool→miner V1 line (`mining.notify` with
/// merkle branches < 4 KB) yet far below the OOM zone on a 228 MB-class
/// miner. `0` = disabled (→ a finite 16 MiB sane backstop, never
/// literally unbounded). V1 inbound-line cap (strat-09 hardening; the
/// V1 analog of `sv2_max_inbound_frame_bytes`, on the primary live
/// mining path).
fn default_v1_max_inbound_line_bytes() -> u32 {
    65_536
}

/// Read-only pool failover observability state.
///
/// This intentionally carries no pool password or mutable command surface. It is
/// safe to expose through REST/WebSocket dashboards and support bundles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PoolFailoverStatus {
    /// Whether more than one user pool is configured.
    pub enabled: bool,
    /// Whether the operator opted into the SmartSwitch FSM
    /// (`[stratum].smart_failover_enabled`). **Telemetry only** — reflects
    /// the config toggle, NOT that the FSM is driving pool selection (the
    /// FSM-drives-selection promotion is Wave-H gated; see
    /// `StratumConfig::smart_failover_enabled`). Truthful by construction:
    /// it never claims SmartSwitch is active, only that it is enabled in
    /// config.
    #[serde(default)]
    pub smart_failover_enabled: bool,
    /// Number of configured user pools, excluding donation pool.
    pub configured_pool_count: usize,
    /// Zero-based active pool index.
    pub active_pool_index: usize,
    /// One-based active pool priority for display.
    pub active_pool_priority: usize,
    /// Active pool URL. Passwords are never included in pool URLs.
    pub active_pool_url: String,
    /// Backoff failure count used by the V1 reconnect loop.
    pub consecutive_failures: u32,
    /// Cumulative user-pool failover switches during this client lifetime.
    pub switch_count: u64,
    /// Last pool-switch reason, if a switch has occurred.
    pub last_switch_reason: Option<String>,
    /// Last failure reason observed on a user pool.
    pub last_failure_reason: Option<String>,
    /// Pool index that produced the last failure.
    pub last_failure_pool_index: Option<usize>,
    /// Pool priority that produced the last failure.
    pub last_failure_pool_priority: Option<usize>,
    /// Whether the failover switch itself flushed stale dispatcher work.
    pub stale_jobs_flushed_on_switch: bool,
    /// Number of pending submit correlation records cleared at the last session end.
    pub pending_submit_correlations_cleared: u64,
    /// Current unresolved mining.submit correlation records awaiting pool replies.
    pub shares_unresolved: u64,
    /// Cumulative pending submit correlation records dropped by the cap.
    pub pending_submit_dropped: u64,
    /// Whether a not-yet-written share is being preserved for the next connection.
    pub pending_share_preserved: bool,
    /// Current reconnect backoff delay, in milliseconds.
    pub backoff_ms: u64,
    /// Event that produced this snapshot.
    pub event: String,
    /// Provenance label for dashboards/support tools.
    pub telemetry_source: String,
}

/// Read-only weighted user-hashrate split state.
///
/// This is intentionally separate from donation state. User split controls how
/// the operator's remaining hashrate is routed between user pools; donation, if
/// enabled, is composed on top as its own transparent time slice.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HashrateSplitStatus {
    /// Whether weighted split routing is configured and active for V1.
    pub enabled: bool,
    /// Whether the current Stratum session is on a split route.
    pub active: bool,
    /// Active user route label: primary, secondary, disabled, or donation_override.
    pub active_route: String,
    /// Zero-based active user pool index for the current split route.
    pub active_pool_index: usize,
    /// One-based active user pool priority for display.
    pub active_pool_priority: usize,
    /// Primary route weight in basis points.
    pub primary_bps: u16,
    /// Secondary route weight in basis points.
    pub secondary_bps: u16,
    /// Total split cycle duration.
    pub cycle_duration_s: u64,
    /// Seconds remaining before the next split route transition.
    pub cycle_remaining_s: u64,
    /// Cumulative split route switches during this client lifetime.
    pub switch_count: u64,
    /// Shares submitted while the secondary user route was active.
    pub secondary_shares: u64,
    /// Provenance label for dashboards/support tools.
    pub telemetry_source: String,
}

/// Donation configuration — voluntary, transparent, disableable.
///
/// DCENT_OS replaces the industry-standard 2.8% mandatory fee with a
/// transparent 2% donation. Our autotuner saves more than 2% in energy —
/// the donation can pay for itself. Fully configurable (0-5%), fully disableable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DonationConfig {
    /// Whether donation is enabled. Default: true.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Donation percentage (0.0 to 5.0). Default: 2.0.
    #[serde(default = "default_donation_percent")]
    pub percent: f32,

    /// Donation pool URL.
    #[serde(default = "default_donation_pool_url")]
    pub pool_url: String,

    /// Donation worker name.
    #[serde(default = "default_donation_worker")]
    pub worker: String,

    /// Donation pool password.
    #[serde(default = "default_donation_password")]
    pub password: String,

    /// Whether to use a visible backup donation route if the primary donation
    /// pool is unreachable during the donation window.
    #[serde(default = "default_true")]
    pub fallback_enabled: bool,

    /// Backup donation pool URL. This is only used for donation windows and
    /// never participates in user-pool failover.
    #[serde(default = "default_donation_fallback_pool_url")]
    pub fallback_pool_url: String,

    /// Backup donation worker name.
    #[serde(default = "default_donation_fallback_worker")]
    pub fallback_worker: String,

    /// Backup donation pool password.
    #[serde(default = "default_donation_password")]
    pub fallback_password: String,

    /// Total cycle duration in seconds. Default: 3600 (1 hour).
    /// At 2% donation with 3600s cycle: mine user pool for 3528s,
    /// then donation pool for 72s.
    #[serde(default = "default_donation_cycle")]
    pub cycle_duration_s: u64,
}

fn default_donation_percent() -> f32 {
    2.0
}
fn default_donation_pool_url() -> String {
    "stratum+tcp://pool.d-central.tech:3333".into()
}
fn default_donation_worker() -> String {
    "DungeonMaster".into()
}
fn default_donation_fallback_pool_url() -> String {
    "stratum+tcp://stratum.braiins.com:3333".into()
}
fn default_donation_fallback_worker() -> String {
    "DungeonMaster".into()
}
fn default_donation_password() -> String {
    "x".into()
}
fn default_donation_cycle() -> u64 {
    3600
}

impl Default for DonationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            percent: 2.0,
            pool_url: default_donation_pool_url(),
            worker: default_donation_worker(),
            password: default_donation_password(),
            fallback_enabled: true,
            fallback_pool_url: default_donation_fallback_pool_url(),
            fallback_worker: default_donation_fallback_worker(),
            fallback_password: default_donation_password(),
            cycle_duration_s: 3600,
        }
    }
}

/// Full stratum client configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StratumConfig {
    /// Primary pool (required).
    pub pool1: PoolConfig,

    /// Secondary pool (optional failover).
    pub pool2: Option<PoolConfig>,

    /// Tertiary pool (optional failover).
    pub pool3: Option<PoolConfig>,

    /// User-pool routing mode. "failover" preserves legacy behavior.
    /// "weighted_split" time-slices V1 mining between pool1 and pool2.
    #[serde(default = "default_pool_routing_mode")]
    pub routing_mode: String,

    /// Weighted split cycle duration in seconds.
    #[serde(default = "default_split_cycle_duration_s")]
    pub split_cycle_duration_s: u64,

    /// Stable-primary-return anti-flap cool-down (seconds). After
    /// failover off the primary, the primary is only re-preferred once
    /// this cool-down has fully elapsed AND the active backup itself
    /// faults — never disturbing a healthy backup, never oscillating
    /// (window >> backoff). `0` = disabled (legacy round-robin).
    #[serde(default = "default_primary_return_stability_secs")]
    pub primary_return_stability_secs: u64,

    /// No-`mining.notify` failover timeout (seconds). Post-handshake, no
    /// new job for this long → fail the session into the existing
    /// failover machinery. Conservative default (300s) ≫ 120s stall ≫
    /// normal cadence (no false failover on a quiet-but-healthy pool).
    /// `0` = disabled.
    #[serde(default = "default_no_notify_failover_secs")]
    pub no_notify_failover_secs: u64,

    /// Reject-rate failover threshold (percent). `0` = DISABLED (default
    /// — highest flap risk, opt-in). Acts only over ≥
    /// `reject_rate_failover_min_samples` post-handshake shares.
    #[serde(default = "default_reject_rate_failover_pct")]
    pub reject_rate_failover_pct: u8,

    /// Minimum post-handshake shares before reject-rate failover may act.
    #[serde(default = "default_reject_rate_failover_min_samples")]
    pub reject_rate_failover_min_samples: u64,

    /// **Default false.** Opt-in toggle for the LuxOS-shape SmartSwitch
    /// pool-failover FSM (`pool_failover::PoolFailoverFsm`, RE-006). With
    /// this flag false (the shipped default) the existing user-pool
    /// failover machinery in `v1/client.rs` is the sole driver of pool
    /// selection and behavior is byte-identical to the pre-toggle daemon.
    ///
    /// Maps to `[stratum].smart_failover_enabled` in `dcentrald.toml`.
    ///
    /// Wiring status (matrix §7 #2 / §6 SmartSwitch row): this config knob
    /// is plumbed end-to-end (TOML → daemon `Config` → `StratumConfig` →
    /// the V1 client constructor, readable via
    /// `StratumV1Client::smart_failover_enabled()`) and surfaced truthfully
    /// in `PoolFailoverStatus.smart_failover_enabled`. With this flag ON the
    /// FSM runs in shadow (LOGs what it would decide); it only *drives* live
    /// pool selection when this flag AND a drive arm — the
    /// `DCENT_POOL_FAILOVER_FSM_DRIVE` env gate or [`StratumConfig::smart_failover_drive`]
    /// — are BOTH set (SW-01, see the `pool_failover` module docs). All gates
    /// default OFF, so the shipped daemon's active failover behavior is
    /// byte-identical whether this is ON or OFF; promoting drive to a fleet
    /// default is gated on an operator soak, not host tests alone.
    #[serde(default)]
    pub smart_failover_enabled: bool,

    /// SW-01 drive arm (config form). When this is `true` AND
    /// `smart_failover_enabled` is `true`, the `PoolFailoverFsm` DRIVES live
    /// pool selection (its recommended active pool index is applied to the V1
    /// client's `current_pool_index`) instead of only logging in shadow. This
    /// is the config-file equivalent of the `DCENT_POOL_FAILOVER_FSM_DRIVE`
    /// env gate; either arm enables drive. Default OFF. There is NO
    /// hardware/voltage/frequency/fan path here — this only changes which
    /// already-configured pool the client connects to.
    #[serde(default)]
    pub smart_failover_drive: bool,

    /// Operational cap (bytes) on an inbound **SV2 mining-client** frame
    /// payload, enforced at header-parse time before any large
    /// allocation. Conservative default (1 MiB) — ≈8× the largest
    /// realistic pool→miner mining message (a full-coinbase
    /// `NewExtendedMiningJob` ≈136 KB) yet far below the
    /// memory-amplification danger zone on a 228 MB-class miner. An
    /// over-cap announced frame is a protocol violation on this
    /// connection → the SV2 session fails into the **existing**
    /// reconnect/backoff (no parallel path). `0` = disabled (fall back
    /// to the 16 MiB wire-format protocol max — keeps the door open for
    /// a future Template-Distribution / Job-Declaration use that
    /// legitimately needs large frames; this is an operational
    /// mining-client policy, never a wire/protocol change).
    #[serde(default = "default_sv2_max_inbound_frame_bytes")]
    pub sv2_max_inbound_frame_bytes: u32,

    /// Operational cap (bytes) on an inbound **Stratum V1** line,
    /// enforced as bytes are read (bounded read) before the line buffer
    /// can grow. Conservative default (64 KiB) — ≈16× the largest
    /// realistic pool→miner V1 line yet far below the OOM zone on a
    /// 228 MB-class miner; protects the **primary live mining path**
    /// (all 5 proven platforms mine via V1). An over-cap line is a
    /// protocol violation on this connection → `ConnectionError` into
    /// the **existing** V1 reconnect/backoff (no parallel path). `0` =
    /// disabled (→ a finite 16 MiB sane backstop, never literally
    /// unbounded — true-unbounded inbound is always a bug).
    #[serde(default = "default_v1_max_inbound_line_bytes")]
    pub v1_max_inbound_line_bytes: u32,

    /// Donation configuration.
    #[serde(default)]
    pub donation: DonationConfig,

    /// Whether to enable version rolling (BIP 310 / ASICBoost).
    #[serde(default = "default_true")]
    pub version_rolling: bool,

    /// Requested version-rolling mask for BIP 310 / SV2 setup.
    ///
    /// The pool may return a narrower mask. Keep this configurable so lab runs
    /// can match pool-specific evidence without source edits.
    #[serde(default = "default_version_rolling_mask")]
    pub version_rolling_mask: u32,

    /// Static startup difficulty hint to the pool.
    ///
    /// This value is sent only during the V1 handshake: as the optional
    /// `minimum-difficulty.value` in `mining.configure` and as a legacy
    /// `mining.suggest_difficulty` request. It is advisory; pools may ignore
    /// it or later override it with `mining.set_difficulty`. Do not use this
    /// configured value as live difficulty telemetry, share-target truth, a
    /// ticket-mask substitute, or a runtime floor after the handshake.
    pub suggest_difficulty: Option<u64>,

    /// Informational-only flag (does NOT control whether the ASICs keep
    /// hashing). When `true`, the V1 client emits a log note that the chips are
    /// continuing to hash the last job while the pool reconnects.
    ///
    /// The name is historical; the field is deliberately NOT renamed to avoid
    /// config/schema churn. It does not act as a safety stop: on a same-pool
    /// reconnect the work dispatcher is never flushed (only a pool *switch*
    /// flushes it), so the ASICs keep hashing the last job across a disconnect
    /// REGARDLESS of this flag. Setting it `false` does not stop hashing on
    /// pool loss — it only suppresses the `info!` note in `v1/client.rs`.
    ///
    /// The real "don't spin hot forever" backstop is thermal supervision (the
    /// PID/threshold loop), NOT pool connection state — chips hashing stale
    /// work are bounded by measured temperature, not by pool reachability. Do
    /// not treat this flag as a thermal or safety control.
    #[serde(default = "default_true")]
    pub hash_on_disconnect: bool,

    /// Nominal device hashrate in GH/s for SV2 OpenStandardMiningChannel.
    /// Helps pool set appropriate initial difficulty. Default: 13500 (S9 ~13.5 TH/s).
    /// Set by daemon from mining config (frequency × chips × chains × cores / 1000).
    #[serde(default = "default_nominal_hashrate")]
    pub nominal_hashrate_ghs: f32,

    /// Open an SV2 Extended Mining Channel instead of a Standard channel.
    ///
    /// Standard channels are only safe for low-hashrate devices because the
    /// pool owns the coinbase/extranonce space. Multi-TH/s miners must use an
    /// Extended/JD path or fall back to V1.
    #[serde(default)]
    pub sv2_extended_channel: bool,

    /// Protocol selection: "sv1"/"v1" (default), "sv2"/"v2", or "auto".
    /// When absent or unrecognized, defaults to V1 for backward compatibility.
    #[serde(default)]
    pub protocol: Option<String>,
}

fn default_true() -> bool {
    true
}

pub fn default_version_rolling_mask() -> u32 {
    0x1fff_e000
}

fn default_nominal_hashrate() -> f32 {
    13500.0 // S9 default: ~13.5 TH/s = 13500 GH/s
}

/// Current state of the stratum connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StratumState {
    /// Not connected to any pool.
    Disconnected,

    /// TCP connection established, performing handshake.
    Connecting,

    /// Handshake complete (subscribed + authorized). Waiting for first job.
    Authorized,

    /// Actively mining — receiving jobs and submitting shares.
    Mining,

    /// Connected to donation pool (transparent time-based switching).
    Donating,

    /// Pool rejected our worker credentials (`mining.authorize` = false, banned
    /// wallet, or invalid worker name). Distinct from [`Disconnected`] so the
    /// operator gets an actionable "check your worker/wallet" signal instead of
    /// indefinite connecting/disconnected churn — one of the most common real
    /// "why isn't my miner working" causes (FWT-3).
    ///
    /// [`Disconnected`]: StratumState::Disconnected
    AuthFailed,
}

/// Status update sent from stratum client to the main daemon.
#[derive(Debug, Clone)]
pub enum StratumStatus {
    /// Connection state changed.
    StateChanged(StratumState),

    /// New difficulty received from pool.
    DifficultyChanged(f64),

    /// Share accepted by pool.
    ShareAccepted {
        job_id: String,
        /// Pool-assigned target difficulty active when the share was submitted.
        ///
        /// This is the pool credit/minimum difficulty, not lucky-share proof.
        pool_target_difficulty: f64,
        /// Locally computed achieved difficulty of the accepted share hash.
        ///
        /// `None` means the Stratum layer could not prove the exact achieved
        /// difficulty and consumers must not infer it from the pool target.
        achieved_difficulty: Option<f64>,
        /// Exact correlated share metadata when the protocol preserves it.
        meta: Option<ShareEventMeta>,
    },

    /// Share rejected by pool.
    ShareRejected {
        job_id: String,
        error_code: i64,
        error_msg: String,
        /// Exact correlated share metadata when the protocol preserves it.
        meta: Option<ShareEventMeta>,
    },

    /// Pool sent an informational message.
    PoolMessage(String),

    /// Pool requested reconnection to a different endpoint.
    ReconnectRequested {
        host: String,
        port: u16,
        wait_seconds: u32,
    },

    /// Read-only pool failover state update.
    PoolFailoverUpdated(PoolFailoverStatus),

    /// Read-only weighted user-hashrate split state update.
    HashrateSplitUpdated(HashrateSplitStatus),

    /// Latency measurement for the current pool.
    Latency(u64),

    /// Donation state changed (transparent voluntary pool switching).
    DonationStateChanged {
        /// Whether donation mining is currently active.
        active: bool,
        /// Configured donation percentage.
        percent: f32,
        /// Seconds remaining in current phase before next switch.
        cycle_remaining_s: u64,
        /// W5.5: URL of the active donation pool when `active == true`.
        /// Empty when `active == false`. Lets the dashboard render the route
        /// the donation slice is currently flowing through (primary D-Central
        /// vs visible Braiins fallback) without needing to read shared stats.
        active_url: String,
        /// W5.5: Worker name authenticated with the active donation pool.
        /// Empty when `active == false`.
        active_worker: String,
        /// W5.5: Zero-based donation route index. 0 = primary D-Central
        /// donation pool, 1 = visible Braiins Pool fallback
        /// (`DungeonMaster`).
        pool_index: usize,
    },

    /// Auto-mode temporary fallback state changed.
    AutoFallbackStateChanged {
        /// Whether Auto mode is currently running on temporary V1 fallback.
        active: bool,
        /// Seconds before Auto mode will retry the preferred SV2 endpoint.
        retry_after_s: u64,
        /// Human-readable fallback reason.
        reason: String,
    },

    /// Rolling pool acceptance changed from real accepted/rejected share
    /// evidence. `accepted` and `total` cover the same rolling window as `pct`.
    RollingAcceptanceUpdated { pct: f64, accepted: u32, total: u32 },

    /// SV2 session metadata update (sent after handshake + periodically during mining).
    Sv2SessionUpdated {
        cipher_suite: String,
        handshake_latency_ms: u64,
        pool_pubkey_fingerprint: String,
        certificate_valid_from: u64,
        certificate_not_after: u64,
        channel_id: Option<u32>,
        noise_nonce_tx: u64,
        noise_nonce_rx: u64,
        bytes_encrypted: u64,
        bytes_decrypted: u64,
        messages_sent: u64,
        messages_received: u64,
    },

    /// SV2 custom job was sent to the upstream pool and is awaiting commitment.
    Sv2CustomJobDeclared {
        channel_id: u32,
        request_id: u32,
        template_id: u64,
    },

    /// SV2 custom job was accepted by the upstream pool and dispatched locally.
    Sv2CustomJobAccepted {
        channel_id: u32,
        request_id: u32,
        template_id: u64,
        job_id: u32,
    },

    /// SV2 custom job was rejected by the upstream pool.
    Sv2CustomJobRejected {
        channel_id: u32,
        request_id: u32,
        template_id: Option<u64>,
        reason: String,
    },
}

/// Statistics tracked by the stratum client.
///
/// `Default` is hand-written below so `rolling_acceptance_pct` defaults
/// to 100.0 (the "no rolling evidence of rejection" baseline) instead
/// of the derive-default 0.0 — see W6.3 wiring in `v1::client`.
#[derive(Debug, Clone, Serialize)]
pub struct StratumStats {
    /// Total shares submitted.
    pub shares_submitted: u64,

    /// Submitted shares still awaiting pool accept/reject responses.
    pub shares_unresolved: u64,

    /// Submit correlation records dropped because the pending queue hit its cap.
    pub pending_submit_dropped: u64,

    /// Shares accepted by pool.
    pub shares_accepted: u64,

    /// Shares rejected by pool.
    pub shares_rejected: u64,

    /// Total jobs received from pool.
    pub jobs_received: u64,

    /// Current pool difficulty.
    pub current_difficulty: f64,

    /// Index of the currently active pool (0, 1, or 2).
    pub active_pool_index: usize,

    /// Whether currently in donation window.
    pub donating: bool,

    /// W5.5: URL of the active donation pool when `donating == true`. Empty
    /// otherwise. This lets the dashboard render "Donating to <pool>" instead
    /// of a bare "DONATING" chip — operators want to see whether the primary
    /// donation endpoint is up or whether the visible Braiins Pool fallback
    /// (DungeonMaster) is currently carrying the donation slice.
    /// Passwords are never included in pool URLs; this is safe to expose.
    #[serde(default)]
    pub donation_active_url: String,

    /// W5.5: Worker name authenticated with the active donation pool. Empty
    /// when not in a donation window. Reveals whether donation is currently
    /// going through the primary D-Central route or the visible Braiins Pool
    /// fallback (DungeonMaster).
    #[serde(default)]
    pub donation_active_worker: String,

    /// W5.5: Zero-based index of the active donation route. 0 = primary
    /// donation pool (`pool.d-central.tech`), 1 = visible Braiins Pool
    /// fallback (`DungeonMaster`). Stays at 0 outside the donation
    /// window, so consumers should pair it with the `donating` flag.
    #[serde(default)]
    pub donation_pool_index: usize,

    /// Cumulative seconds spent mining on donation pool.
    pub donation_time_s: u64,

    /// Shares submitted to donation pool.
    pub donation_shares: u64,

    /// Last measured pool latency in milliseconds.
    ///
    /// Legacy scalar kept for back-compat. `0` is ambiguous — it can mean
    /// either "a real sub-ms RTT" or "never measured". Prefer
    /// [`last_latency_ms`](Self::last_latency_ms) /
    /// [`per_pool_latency_ms`](Self::per_pool_latency_ms) for the honest
    /// None-before-sample contract.
    pub latency_ms: u64,

    /// LANE S — last measured round-trip latency for the **active** pool, in
    /// milliseconds. `None` until the first `mining.submit` response has been
    /// correlated (so consumers never mistake "never measured" for a 0 ms RTT).
    /// Populated from the same submit→response sample as `latency_ms`; no new
    /// measurement is taken.
    #[serde(default)]
    pub last_latency_ms: Option<u32>,

    /// LANE S — per-pool last measured round-trip latency, indexed by pool
    /// index (0 = primary `pool1`, 1 = `pool2`, 2 = `pool3`). Each entry is
    /// `None` until that specific pool has produced a correlated submit
    /// response, so each pool's latency is independent. The vector is grown to
    /// the configured pool count the first time any sample lands; consumers
    /// must treat a missing index as `None`. Pure numeric telemetry — carries
    /// no pool URL, so nothing to mask here.
    #[serde(default)]
    pub per_pool_latency_ms: Vec<Option<u32>>,

    /// Whether the stratum connection is alive.
    pub connected: bool,

    /// Read-only failover observability snapshot.
    pub failover: PoolFailoverStatus,

    /// Read-only weighted user-hashrate split snapshot.
    pub hashrate_split: HashrateSplitStatus,

    /// W6.3: rolling 30-minute share acceptance percentage (0..=100).
    ///
    /// Computed by `AcceptanceTracker` from every `mining.submit` response
    /// over the last 30 minutes. The autotuner step-up gate uses this
    /// (alongside per-chip HW error EWMA) to refuse frequency increases
    /// while a rejection storm is in flight. Empty rolling window
    /// reports 100.0% — see `AcceptanceTracker` docs for the gate
    /// composition.
    #[serde(default = "default_rolling_acceptance_pct")]
    pub rolling_acceptance_pct: f64,

    /// W6.3 / W6.4 dashboard surface — accepted vs total share counts
    /// inside the rolling window. Lets the dashboard render
    /// "X / Y accepted (last 30 min)" without re-deriving it from the
    /// percentage. Defaults to (0, 0) on a fresh boot.
    #[serde(default)]
    pub rolling_acceptance_count: (u32, u32),

    /// W11.13 (Bitmain `client.show_message` extension). Bounded ring buffer
    /// of the most recent operator-facing pool messages. Pools push these
    /// via the JSON-RPC `client.show_message` notification — typical content
    /// is maintenance windows, per-account fee changes, or pool-side worker
    /// notices. The dashboard renders these as a per-pool message log so the
    /// operator doesn't have to scrape the daemon log.
    ///
    /// Capped at 16 entries (FIFO eviction). Each entry carries a UNIX-ms
    /// timestamp captured by the V1 client when the notification arrived,
    /// the active pool URL, and the raw message string (no parsing — pools
    /// occasionally embed HTML/ANSI). Truncated to 1024 chars per entry to
    /// bound memory under a hostile/buggy pool that streams long messages.
    #[serde(default)]
    pub pool_message_log: Vec<PoolMessageEntry>,
}

/// One entry in the bounded `pool_message_log` ring buffer (W11.13).
#[derive(Debug, Clone, Serialize)]
pub struct PoolMessageEntry {
    /// UNIX timestamp in milliseconds when the V1 client received the
    /// notification. Wall-clock; uses `SystemTime::now()` so the dashboard
    /// can render "5 min ago" without a separate uptime offset.
    pub timestamp_ms: u64,

    /// URL of the pool that pushed the message, in the same shape that
    /// `PoolConfig::url` carries (`stratum+tcp://host:port`). Empty string
    /// if the message arrived before a pool URL was known (should not happen
    /// in practice since `client.show_message` is mining-loop-only, but
    /// defensive).
    pub pool_url: String,

    /// The raw message string from the pool, truncated to 1024 chars on
    /// ingest. Not parsed — pools occasionally embed HTML/ANSI that the
    /// dashboard renders as plain text.
    pub message: String,
}

#[allow(dead_code)]
fn default_rolling_acceptance_pct() -> f64 {
    100.0
}

impl Default for StratumStats {
    fn default() -> Self {
        Self {
            shares_submitted: 0,
            shares_unresolved: 0,
            pending_submit_dropped: 0,
            shares_accepted: 0,
            shares_rejected: 0,
            jobs_received: 0,
            current_difficulty: 0.0,
            active_pool_index: 0,
            donating: false,
            donation_active_url: String::new(),
            donation_active_worker: String::new(),
            donation_pool_index: 0,
            donation_time_s: 0,
            donation_shares: 0,
            latency_ms: 0,
            last_latency_ms: None,
            per_pool_latency_ms: Vec::new(),
            connected: false,
            failover: PoolFailoverStatus::default(),
            hashrate_split: HashrateSplitStatus::default(),
            // W6.3: empty rolling window reports 100.0% — the "no
            // rolling evidence of rejection" baseline. The gate
            // composes this with chip HW err + clean-window count, so
            // a fresh boot can never spuriously authorize step-up.
            rolling_acceptance_pct: 100.0,
            rolling_acceptance_count: (0, 0),
            pool_message_log: Vec::new(),
        }
    }
}

/// W11.13 — bounded per-pool message log capacity. Pools occasionally
/// stream chatty MOTD-style notices; cap at 16 to bound memory while still
/// preserving enough recent history for the dashboard to render a useful
/// timeline.
pub const POOL_MESSAGE_LOG_CAPACITY: usize = 16;

/// W11.13 — per-message length cap. Pools that embed long banners (HTML or
/// ANSI escape sequences) can otherwise force unbounded `String` growth on
/// every notification.
pub const POOL_MESSAGE_MAX_LEN: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Constant + range-helper pins.
    // -----------------------------------------------------------------------

    #[test]
    fn extranonce2_size_constants_are_pinned() {
        // Default 4 bytes (typical V1 pool default), max 8 bytes (the
        // upper bound that fits in a u64 counter without lossy formatting).
        // A bump in either silently changes the wire shape for every
        // downstream V1 path.
        assert_eq!(DEFAULT_V1_EXTRANONCE2_SIZE, 4);
        assert_eq!(MAX_V1_EXTRANONCE2_SIZE, 8);
    }

    #[test]
    fn is_valid_v1_extranonce2_size_accepts_inclusive_range_1_to_8() {
        // Pin every accepted value across the 1..=MAX boundary. Off-by-one
        // here would silently disable mid-session extranonce rotation
        // for a pool using the boundary size.
        for size in 1..=MAX_V1_EXTRANONCE2_SIZE {
            assert!(
                is_valid_v1_extranonce2_size(size),
                "size={size} must be accepted (within 1..={MAX_V1_EXTRANONCE2_SIZE})"
            );
        }
    }

    #[test]
    fn is_valid_v1_extranonce2_size_rejects_out_of_range() {
        // Zero and oversize values must be rejected. The pool-message
        // parser uses this gate to reject malformed `mining.set_extranonce`
        // sizes; a refactor that flipped to inclusive zero would silently
        // accept a degenerate "no extranonce" rotation.
        assert!(!is_valid_v1_extranonce2_size(0));
        assert!(!is_valid_v1_extranonce2_size(MAX_V1_EXTRANONCE2_SIZE + 1));
        assert!(!is_valid_v1_extranonce2_size(100));
        assert!(!is_valid_v1_extranonce2_size(usize::MAX));
    }

    #[test]
    fn default_version_rolling_mask_is_bip310_canonical() {
        // 0x1fff_e000 is the BIP310 canonical version-rolling mask used
        // by every standard ASICBoost implementation. A refactor that
        // narrowed it would silently lose ASICBoost efficiency; widening
        // would risk pool rejection of shares.
        assert_eq!(default_version_rolling_mask(), 0x1fff_e000);
    }

    #[test]
    fn default_sv2_max_inbound_frame_bytes_is_secure_by_default_1mib() {
        // SV2 inbound-frame cap (strat-09 hardening): a TOML config
        // without `sv2_max_inbound_frame_bytes` MUST get the 1 MiB
        // operational cap, NOT 0 (disabled). Pinning the literal here
        // catches a refactor that flips the secure-by-default value or
        // silently turns the protection off. Plan/review:
        //
        assert_eq!(default_sv2_max_inbound_frame_bytes(), 1_048_576);
        // Far below the 16 MiB wire max (so it actually constrains the
        // amplification vector) and far above the largest realistic
        // pool→miner mining message (≈139 KB) so it never false-rejects.
        assert!(default_sv2_max_inbound_frame_bytes() < (1 << 24) - 1);
        assert!(default_sv2_max_inbound_frame_bytes() > 65_535 + 65_535 + 255 * 32);
    }

    #[test]
    fn default_v1_max_inbound_line_bytes_is_secure_by_default_64kib() {
        // V1 inbound-line cap (strat-09): a TOML config without
        // `v1_max_inbound_line_bytes` MUST get the 64 KiB operational
        // cap, NOT 0 (disabled). Pinning the literal catches a refactor
        // that flips the secure-by-default value off. Plan/review:
        //
        assert_eq!(default_v1_max_inbound_line_bytes(), 65_536);
        // Comfortably above the largest realistic pool→miner V1 line
        // (a full `mining.notify` < ~4 KB) so it never false-rejects,
        // and far below the OOM zone on a 228 MB-class miner.
        assert!(default_v1_max_inbound_line_bytes() >= 16 * 1024);
        assert!(default_v1_max_inbound_line_bytes() <= 256 * 1024);
    }

    // -----------------------------------------------------------------------
    // JobTemplate::is_flush_only contract.
    //
    // The flush-only sentinel differentiates "stale-work clear" templates
    // from real mining work.  wired the work dispatcher to flush
    // ASIC work without dispatching when this returns true. A refactor
    // that broke any of the seven conditions would silently dispatch
    // empty work to the ASIC OR fail to flush stale work on pool switch.
    // -----------------------------------------------------------------------

    fn flush_only_template() -> JobTemplate {
        JobTemplate {
            job_id: String::new(),
            prev_block_hash: [0u8; 32],
            coinbase1: Vec::new(),
            coinbase2: Vec::new(),
            merkle_branches: Vec::new(),
            version: 0,
            nbits: 0,
            ntime: 0,
            clean_jobs: true,
            share_target: [0xFFu8; 32],
            extranonce1: Vec::new(),
            extranonce2_size: 0,
            version_mask: 0,
            merkle_root: [0u8; 32],
            pool_difficulty: 0.0,
        }
    }

    #[test]
    fn is_flush_only_true_for_clean_jobs_with_empty_payload() {
        let template = flush_only_template();
        assert!(template.is_flush_only());
    }

    #[test]
    fn is_flush_only_false_when_clean_jobs_is_false() {
        let mut template = flush_only_template();
        template.clean_jobs = false;
        assert!(
            !template.is_flush_only(),
            "non-clean_jobs template is NOT a flush sentinel"
        );
    }

    #[test]
    fn is_flush_only_false_when_job_id_present() {
        // A real notify always carries a job_id. Pin that any non-empty
        // job_id disqualifies the flush sentinel.
        let mut template = flush_only_template();
        template.job_id = "real-job".to_string();
        assert!(!template.is_flush_only());
    }

    #[test]
    fn is_flush_only_false_when_coinbase_present() {
        let mut template = flush_only_template();
        template.coinbase1 = vec![0x01];
        assert!(!template.is_flush_only());

        let mut template = flush_only_template();
        template.coinbase2 = vec![0x02];
        assert!(!template.is_flush_only());
    }

    #[test]
    fn is_flush_only_false_when_merkle_branches_present() {
        let mut template = flush_only_template();
        template.merkle_branches = vec![[0x33; 32]];
        assert!(!template.is_flush_only());
    }

    #[test]
    fn is_flush_only_false_when_prev_block_hash_present() {
        let mut template = flush_only_template();
        template.prev_block_hash = [0x44; 32];
        assert!(!template.is_flush_only());
    }

    #[test]
    fn is_flush_only_false_when_merkle_root_present() {
        // SV2 standard channel templates carry merkle_root directly. A
        // template with merkle_root set is real work, not a flush.
        let mut template = flush_only_template();
        template.merkle_root = [0x55; 32];
        assert!(!template.is_flush_only());
    }

    // -----------------------------------------------------------------------
    // SW-11: bounded host-driven ntime roll.
    //
    // The parity invariant is that the ntime a work unit is HASHED with is the
    // exact ntime it is SUBMITTED with — and that a host-driven roll can never
    // step a share's ntime out of the pool's accepted window (which would make
    // it a guaranteed reject, caught by the SW-03 pre-submit guard). These
    // tests pin: (a) no-roll is byte-identical to the job ntime, (b) a normal
    // roll advances by exactly the requested amount, (c) an over-window roll
    // clamps to the window edge — never past it, and (d) the roll is
    // wrap-safe at the u32 ceiling.
    // -----------------------------------------------------------------------

    fn ntime_roll_template(base_ntime: u32) -> JobTemplate {
        JobTemplate {
            job_id: "roll".to_string(),
            prev_block_hash: [0x11; 32],
            coinbase1: vec![0x01],
            coinbase2: vec![0x02],
            merkle_branches: Vec::new(),
            version: 0x2000_0000,
            nbits: 0x1703_4219,
            ntime: base_ntime,
            clean_jobs: false,
            share_target: [0xFF; 32],
            extranonce1: vec![0xAA, 0xBB, 0xCC, 0xDD],
            extranonce2_size: 4,
            version_mask: 0x1fff_e000,
            merkle_root: [0u8; 32],
            pool_difficulty: 1.0,
        }
    }

    #[test]
    fn roll_ntime_zero_is_identity_to_job_ntime() {
        // The default (no-roll) path MUST be byte-identical to the job ntime —
        // this is what keeps the shipped behavior unchanged until a driver opts
        // into rolling.
        let job = ntime_roll_template(1_700_000_000);
        assert_eq!(
            job.roll_ntime_within_window(0, DEFAULT_NTIME_ROLL_WINDOW_SECS),
            1_700_000_000
        );
    }

    #[test]
    fn roll_ntime_advances_by_requested_amount_within_window() {
        // A 30 s roll inside the 60 s default window advances by exactly 30 s.
        let job = ntime_roll_template(1_700_000_000);
        assert_eq!(
            job.roll_ntime_within_window(30, DEFAULT_NTIME_ROLL_WINDOW_SECS),
            1_700_000_030
        );
    }

    #[test]
    fn roll_ntime_clamps_to_window_edge_never_past_it() {
        // Requesting a roll FAR beyond the window lands exactly on the window
        // edge — never past it. This is the load-bearing safety property: a
        // host roll can never produce an out-of-window (guaranteed-reject)
        // ntime, no matter what roll value a buggy caller passes.
        let job = ntime_roll_template(1_700_000_000);
        let window = DEFAULT_NTIME_ROLL_WINDOW_SECS;
        assert_eq!(
            job.roll_ntime_within_window(u32::MAX, window),
            1_700_000_000u32.saturating_add(window)
        );
        // Exactly at the window is allowed (inclusive edge).
        assert_eq!(
            job.roll_ntime_within_window(window, window),
            1_700_000_000 + window
        );
    }

    #[test]
    fn roll_ntime_is_wrap_safe_at_u32_ceiling() {
        // A job whose ntime is near the u32 ceiling must saturate, not wrap to a
        // tiny timestamp (which would be a 1970-era ntime → guaranteed reject).
        let job = ntime_roll_template(u32::MAX - 5);
        assert_eq!(
            job.roll_ntime_within_window(60, DEFAULT_NTIME_ROLL_WINDOW_SECS),
            u32::MAX
        );
    }

    #[test]
    fn default_ntime_roll_window_is_conservative_and_inside_consensus() {
        // Pin the default roll budget: a small forward headroom (60 s), far
        // inside the ±7200 s SW-03 consensus reject window. A refactor that
        // widened this toward (or past) the consensus edge would risk
        // pool-side rejects on clock-skewed pools and must update this test.
        assert_eq!(DEFAULT_NTIME_ROLL_WINDOW_SECS, 60);
        assert!(DEFAULT_NTIME_ROLL_WINDOW_SECS < 7200);
    }

    // -----------------------------------------------------------------------
    // DonationConfig defaults — drift here silently redirects miner donations.
    //
    // CRITICAL: the donation config defaults specify which pool, worker,
    // and percentage donations go to. A refactor that changed any of
    // these without explicit operator coordination would silently
    // redirect donations to a different recipient. Pin every field.
    // -----------------------------------------------------------------------

    #[test]
    fn donation_default_donates_two_percent_to_d_central_pool() {
        let donation = DonationConfig::default();

        // Enabled by default — donations are opt-out, not opt-in.
        assert!(donation.enabled);

        // 2.0% donation.
        assert!(
            (donation.percent - 2.0).abs() < f32::EPSILON,
            "donation percent must default to 2.0%, got {}",
            donation.percent
        );

        // Primary recipient is D-Central's pool with the "DungeonMaster"
        // worker name. A refactor that flipped either of these would
        // silently redirect donations.
        assert_eq!(donation.pool_url, "stratum+tcp://pool.d-central.tech:3333");
        assert_eq!(donation.worker, "DungeonMaster");
        assert_eq!(donation.password, "x");
    }

    #[test]
    fn donation_default_fallback_is_braiins_pool_with_dungeonmaster_worker() {
        let donation = DonationConfig::default();

        assert!(donation.fallback_enabled);
        assert_eq!(
            donation.fallback_pool_url,
            "stratum+tcp://stratum.braiins.com:3333"
        );
        // The fallback authorizes as D-Central's "DungeonMaster" worker — the
        // same handle as the primary DCENT_Pool route — so donations are
        // credited to D-Central even while shares route through Braiins Pool
        // when the primary endpoint is unreachable.
        assert_eq!(donation.fallback_worker, "DungeonMaster");
        assert_eq!(donation.fallback_password, "x");
    }

    #[test]
    fn donation_default_cycle_is_one_hour() {
        // 3600s cycle at 2% donation = 72s donating per hour. Pin so a
        // refactor doesn't silently shorten or lengthen the cycle.
        assert_eq!(DonationConfig::default().cycle_duration_s, 3600);
    }

    #[test]
    fn donation_config_round_trips_through_json_with_all_defaults_present() {
        // Serializing the default and parsing back must produce an
        // identical struct — pins that no field gets accidentally
        // dropped or renamed.
        let original = DonationConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let recovered: DonationConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(original.enabled, recovered.enabled);
        assert_eq!(original.percent, recovered.percent);
        assert_eq!(original.pool_url, recovered.pool_url);
        assert_eq!(original.worker, recovered.worker);
        assert_eq!(original.password, recovered.password);
        assert_eq!(original.fallback_enabled, recovered.fallback_enabled);
        assert_eq!(original.fallback_pool_url, recovered.fallback_pool_url);
        assert_eq!(original.fallback_worker, recovered.fallback_worker);
        assert_eq!(original.fallback_password, recovered.fallback_password);
        assert_eq!(original.cycle_duration_s, recovered.cycle_duration_s);
    }

    #[test]
    fn donation_config_serde_default_fills_missing_fields() {
        // Old config files predate some donation fields. Pin that the
        // `#[serde(default = "...")]` attributes correctly fill the
        // missing fields with the intended defaults.
        let bare = "{}";
        let donation: DonationConfig = serde_json::from_str(bare).unwrap();

        assert!(donation.enabled);
        assert!((donation.percent - 2.0).abs() < f32::EPSILON);
        assert_eq!(donation.pool_url, "stratum+tcp://pool.d-central.tech:3333");
        assert_eq!(donation.worker, "DungeonMaster");
        assert!(donation.fallback_enabled);
        assert_eq!(donation.cycle_duration_s, 3600);
    }

    #[test]
    fn donation_config_partial_override_keeps_remaining_defaults() {
        // An operator override of just `percent` should NOT reset the
        // pool URL or worker to anything other than the pinned defaults.
        let overridden = r#"{"percent":1.5}"#;
        let donation: DonationConfig = serde_json::from_str(overridden).unwrap();

        assert!((donation.percent - 1.5).abs() < f32::EPSILON);
        // Other fields stay at default.
        assert_eq!(donation.pool_url, "stratum+tcp://pool.d-central.tech:3333");
        assert_eq!(donation.worker, "DungeonMaster");
        assert_eq!(donation.cycle_duration_s, 3600);
    }
}
