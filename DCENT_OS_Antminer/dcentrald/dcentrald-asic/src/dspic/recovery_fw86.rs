//! dsPIC fw=0x86 recovery protocol research — **not shipped**.
//!
//! Per RE3 R3-6 (`DCENT_OS_DEVELOPMENT_KITRE3/.../RE_DELIVERABLES/dspic_fw86_recovery.md`):
//! fw=0x86 on the dsPIC33EP16GS202 voltage controller is the
//! **bootloader version byte**, NOT a permanent silicon corruption
//! state. The bootloader is alive, ACKs on I²C, and accepts a
//! register-style unlock-and-jump sequence to start the application
//! firmware (which then identifies as fw=0x88/0x89/0x8A).
//!
//! RE3 confidence: 100% on jump procedure (cross-verified against
//! `pic1704.h:31` `PIC1704_VER_BOOTLOADER 0x86`, the bmminer init
//! trace, the hardware catalog, and the master handoff). RE3
//! confidence on framed-reflash packet format is only 60% — the
//! protocol *flow* is known (seek → erase → write → verify →
//! start_app) but the exact wire bytes need a logic-analyzer trace
//! of bmminer's `_update_pic_app_program_1704`. The
//! [`reflash_app_via_framed_protocol`] entrypoint is therefore a
//! **partial implementation**: it validates preconditions, then bails before
//! any seek, erase, or write step lands on the wire. The historical
//! `crate::dspic_flash` mutating API was removed after its inferred opcode
//! model conflicted with canonical evidence. See the `// XXX:` notes in
//! [`reflash_app_via_framed_protocol`].
//!
//! # This is NOT a silent override of the production refusal rule
//!
//! ,
//! , and
//!  all stay in force. The
//! production daemon (no `recovery-tool` feature) STILL refuses
//! voltage commands on fw=0x86 by default. That refusal is correct
//! for the daemon — fw=0x86 in app context means the application
//! never started AND the bootloader has no rail telemetry. The
//! correct response in production is "stop, do not mine, surface
//! the fault." This module is retained as compile-gated protocol research;
//! no shipped binary calls it. Operator recovery remains ICSP-only until a
//! typed executor satisfies the controller-recovery authority contract.
//!
//! # Preserved research invariants (not deployment authority)
//!
//! 1. **Compile-time gate**: this entire module is
//!    `#[cfg(feature = "recovery-tool")]`. No shipped package enables the
//!    feature; `dcentrald` and diagnostic-only controller tools cannot link
//!    any symbol below.
//! 2. **Platform gate (am2 dsPIC ONLY)**: every public op runs the
//!    [`refuse_if_not_am2_dspic`] guard against a [`RecoveryPlatform`]
//!    enum. Calling `jump_to_app` or `reflash_app_via_framed_protocol`
//!    on a `Pic1704` platform marker (CV1835 / AM335x BB / Amlogic
//!    S19j Pro) returns [`AsicError::Pic`] with an explicit "wrong
//!    family" error — for why
//!    cross-family destructive ops MUST fail closed.
//! 3. **Legacy acknowledgement token (Path B — jump-only)**:
//!    [`jump_to_app`] consumes a [`ConfirmedBrickedToken`]. This pins the
//!    historical call boundary for tests; a string token is not sufficient
//!    authority for a future executor.
//! 4. **Legacy double acknowledgement (Path C — framed reflash)**:
//!    [`reflash_app_via_framed_protocol`] consumes an
//!    [`AcknowledgeSixtyPercentConfidence`] token and verifies a typed serial.
//!    These checks preserve uncertainty and target-binding requirements for
//!    research, but no shipped CLI mints the token. A future executor must put
//!    them behind the separate controller-recovery authority architecture.
//! (W13.A3, 2026-05-10),
//!    RE3 §6 confidence on the framed-reflash byte format is only 60%;
//!    sacrificial-dsPIC and logic-analyzer validation remain prerequisites.
//!
//! Tokens are `!Clone` / `!Copy` so a fresh token is required at each
//! op boundary.
//!
//! # Wire format (jump-to-app — the well-specified path)
//!
//! Per RE3 §5.2 + the bmminer init trace cross-reference: the
//! dsPIC bootloader exposes the SAME register-write interface as
//! PIC1704 in this exact byte sequence:
//!
//! ```text
//!   I²C: write [0x00, 0x5A]   ; REG_VERSION = BL_MAGIC (unlock)
//!   I²C: write [0x09, 0x01]   ; REG_CONTROL = BL_CMD_JUMP
//!   wait ~100 ms
//!   I²C: read  [0x00] -> 1 byte = 0x88 / 0x89 / 0x8A on success,
//!                                  0x86 if app partition is corrupt.
//! ```
//!
//! Both writes go through the I²C service queue as one atomic
//! transaction so no other client can interleave between them.
//! Order is load-bearing: BL_MAGIC must precede BL_CMD_JUMP.

#![cfg(feature = "recovery-tool")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dcentrald_hal::i2c::{I2cMutationLabel, I2cServiceHandle, I2cTransactionStep};
use tracing::{info, warn};

use crate::pic1704::programmer::ConfirmedBrickedToken;
use crate::AsicError;
use crate::Result;

// ===========================================================================
//  Recovery platform marker (am2 dsPIC ONLY)
// ===========================================================================

/// Platform identity passed to every recovery op.
///
/// The retained protocol model applies only to **am2 dsPIC** voltage
/// controllers (S17 / S19 Pro / S19j Pro Zynq am2 / S19jpro). It must never be
/// mixed with PIC1704, S9 PIC16F1704, or S21 NoPic families. Shipped software
/// provides no mutation surface for any of them; physical ICSP is the current
/// recovery path. Mixing families risks destroying the wrong chip.
///
/// This enum exists only inside explicit `recovery-tool` research/test builds;
/// no shipped package enables the feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPlatform {
    /// Antminer S17 (am1-s17 / am2-s17). dsPIC33EP16GS202 at I²C
    /// 0x20 / 0x21 / 0x22 per chain.
    Am2S17,
    /// Antminer S19 Pro (am2-s19pro). dsPIC33EP16GS202.
    Am2S19Pro,
    /// Antminer S19j Pro **Zynq am2 variant** (am2-s19jpro-zynq).
    /// NOT the Amlogic / CV1835 / AM335x BB variants — those use
    /// PIC1704 and have their own recovery path.
    Am2S19jProZynq,
}

impl RecoveryPlatform {
    /// Human-readable label used in log lines and error messages.
    pub fn label(self) -> &'static str {
        match self {
            RecoveryPlatform::Am2S17 => "am2-s17",
            RecoveryPlatform::Am2S19Pro => "am2-s19pro",
            RecoveryPlatform::Am2S19jProZynq => "am2-s19jpro-zynq",
        }
    }
}

// ===========================================================================
//  dsPIC bootloader register / opcode constants (RE3 §5 + pic1704 mirror)
// ===========================================================================

/// Bootloader REG_VERSION (R) — read returns the firmware byte (0x86 in
/// bootloader, 0x88/0x89/0x8A once application starts). Write of
/// [`BL_MAGIC`] is the first half of the unlock-and-jump sequence.
///
/// Mirrors `pic1704::protocol::REG_VERSION` exactly. RE3 §5.2 confirms
/// the dsPIC bootloader reuses the same register layout when in
/// fw=0x86 mode.
pub const REG_VERSION: u8 = 0x00;

/// Bootloader REG_CONTROL (W) — second half of the unlock-and-jump.
/// Write [`BL_CMD_JUMP`] after [`BL_MAGIC`] to trigger the jump.
pub const REG_CONTROL: u8 = 0x09;

/// Bootloader unlock magic written to [`REG_VERSION`].
pub const BL_MAGIC: u8 = 0x5A;

/// Bootloader jump-to-application command written to [`REG_CONTROL`].
pub const BL_CMD_JUMP: u8 = 0x01;

/// Application firmware versions (revisions A / canonical / B). After a
/// successful jump, [`REG_VERSION`] reads one of these. 0x86 still after
/// jump means the application partition is missing or corrupted →
/// caller should fall back to [`reflash_app_via_framed_protocol`] or
/// physical ICSP.
pub const VER_BOOTLOADER: u8 = 0x86;
pub const VER_APP_REV_A: u8 = 0x88;
pub const VER_APP_CANONICAL: u8 = 0x89;
pub const VER_APP_REV_B: u8 = 0x8A;

/// Time the bootloader takes to reset and re-enumerate after a successful
/// jump. RE3 §5 procedure uses `sleep 2`; we poll at 100 ms intervals
/// against this 5 s ceiling, mirroring `pic1704::protocol::POLL_INTERVAL_MS`
/// and `WAIT_APP_TIMEOUT_MS`.
pub const POLL_INTERVAL_MS: u64 = 100;
pub const WAIT_APP_TIMEOUT_MS: u64 = 5_000;

// ===========================================================================
//  Wire-format helpers (host-safe — no I²C bus required)
// ===========================================================================

/// Build the atomic two-write transaction for the bootloader→application
/// jump. Order is load-bearing — see module docs.
///
/// Wire bytes (one transaction):
///
/// 1. `Write [REG_VERSION, BL_MAGIC]`  → unlock
/// 2. `Write [REG_CONTROL, BL_CMD_JUMP]` → jump
///
/// The two writes go in one [`I2cServiceHandle::transaction`] call so no
/// other I²C client can interleave between them.
pub fn jump_steps() -> Vec<I2cTransactionStep> {
    vec![
        I2cTransactionStep::Write(vec![REG_VERSION, BL_MAGIC]),
        I2cTransactionStep::Write(vec![REG_CONTROL, BL_CMD_JUMP]),
    ]
}

/// Build a one-byte read of `REG_VERSION` (write-then-read).
pub fn read_version_steps() -> Vec<I2cTransactionStep> {
    vec![I2cTransactionStep::WriteRead {
        write_data: vec![REG_VERSION],
        read_len: 1,
    }]
}

// ===========================================================================
//  Internal guards
// ===========================================================================

/// Refuse if a research caller supplies a non-am2-dsPIC platform marker.
///
/// This is the load-bearing safety guard that prevents accidental
/// destructive ops on PIC1704 platforms — those have their own
/// programmer surface (`pic1704::programmer`) with different opcodes
/// (SEEK 0x01 / WRITE 0x05 / ERASE 0x09 layered over REG_CONTROL),
/// and writing the dsPIC jump sequence to a PIC1704 would not
/// produce the expected effect (PIC1704 also uses 0x5A→VERSION +
/// 0x01→CONTROL — they share the wire pattern — but this guard
/// stays in place to enforce the API contract: this module is for
/// am2 dsPIC ONLY).
fn refuse_if_not_am2_dspic(platform: RecoveryPlatform) -> Result<()> {
    // All variants of `RecoveryPlatform` are am2 dsPIC by construction —
    // the enum cannot encode non-am2 platforms. The check is here as a
    // *future* tripwire: if a future refactor expands the enum, this
    // match will need to gain explicit refuse arms. For now it's a no-op
    // that documents the intent and gives a hook for adding RE-found
    // sub-variants (e.g. a possible future S17 native variant) without
    // breaking the safety contract.
    match platform {
        RecoveryPlatform::Am2S17
        | RecoveryPlatform::Am2S19Pro
        | RecoveryPlatform::Am2S19jProZynq => Ok(()),
    }
}

// ===========================================================================
//  AcknowledgeSixtyPercentConfidence — Path C double-gate token
// ===========================================================================

/// Legacy double-acknowledgement token for the **Path C framed-reflash**
/// research model.
///
/// (W13.A3, 2026-05-10):
/// the dsPIC fw=0x86 framed-reflash protocol is only ~60% byte-exact per
/// RE3 §3.4 + §6 confidence table. Sacrificial-dsPIC + logic-analyzer
/// validation is the R4-8 hardware-acquisition carry-forward blocker.
/// The retained research API therefore pins all of these invariants:
///
/// 1. The canonical [`ConfirmedBrickedToken`] acknowledgement literal.
/// 2. A second acknowledgement of the 60% byte-exact confidence.
/// 3. A typed serial matching the connected dsPIC's hashboard EEPROM serial.
///
/// These are testable protocol defenses, not a complete authority model, and
/// no shipped CLI exposes either path. Path B (jump-only, [`jump_to_app`]) has
/// stronger byte evidence but still requires the future controller-recovery
/// authority architecture before runtime use.
///
/// `!Clone` / `!Copy` so a fresh token is required at each call site.
#[derive(Debug)]
pub struct AcknowledgeSixtyPercentConfidence {
    // Private field with private type so the token cannot be constructed
    // via struct literal syntax outside this module; only
    // `mint_with_double_confirmation` can produce an instance.
    _seal: AcknowledgeSeal,
    // Carry the confirmed serial through to the op site so research audit
    // records can bind results without re-reading it.
    confirmed_serial: String,
}

#[derive(Debug)]
struct AcknowledgeSeal;

impl AcknowledgeSixtyPercentConfidence {
    /// Mint the legacy Path C research token from two acknowledgement literals
    /// plus a typed serial matching the connected dsPIC's hashboard EEPROM.
    ///
    /// `confirm_bricked_flag` MUST be exactly `"--confirm-bricked"`.
    /// `acknowledge_flag` MUST be exactly
    /// `"--i-acknowledge-60-percent-byte-exact-confidence"`.
    /// `typed_serial` MUST match `connected_serial` byte-for-byte
    /// (case-sensitive — hashboard serials are alphanumeric).
    ///
    /// Any mismatch returns [`AsicError::Pic`] with a clear refusal
    /// message. This is the only public constructor; struct-literal
    /// construction is impossible due to the private `_seal` field.
    pub fn mint_with_double_confirmation(
        confirm_bricked_flag: &str,
        acknowledge_flag: &str,
        typed_serial: &str,
        connected_serial: &str,
    ) -> Result<Self> {
        if confirm_bricked_flag != "--confirm-bricked" {
            return Err(AsicError::Pic {
                addr: 0x21,
                detail: format!(
                    "AcknowledgeSixtyPercentConfidence: --confirm-bricked not provided \
                     (got {:?}, expected \"--confirm-bricked\")",
                    confirm_bricked_flag,
                ),
            });
        }
        if acknowledge_flag != "--i-acknowledge-60-percent-byte-exact-confidence" {
            return Err(AsicError::Pic {
                addr: 0x21,
                detail: format!(
                    "AcknowledgeSixtyPercentConfidence: \
                     --i-acknowledge-60-percent-byte-exact-confidence not provided \
                     (got {:?}). Path C requires explicit acknowledgment that the \
                     framed-reflash byte format is only 60% confident per RE3 \
                     §3.4 + §6 (dspic_fw86_recovery.md). See \
                     .",
                    acknowledge_flag,
                ),
            });
        }
        if typed_serial.is_empty() {
            return Err(AsicError::Pic {
                addr: 0x21,
                detail: "AcknowledgeSixtyPercentConfidence: typed serial is empty — \
                         operator must type the connected dsPIC's hashboard EEPROM \
                         serial to confirm the target unit"
                    .into(),
            });
        }
        if typed_serial != connected_serial {
            return Err(AsicError::Pic {
                addr: 0x21,
                detail: format!(
                    "AcknowledgeSixtyPercentConfidence: connected dsPIC serial {:?} does \
                     not match --serial / typed input {:?}; refusing to flash to wrong unit",
                    connected_serial, typed_serial,
                ),
            });
        }
        Ok(Self {
            _seal: AcknowledgeSeal,
            confirmed_serial: typed_serial.to_string(),
        })
    }

    /// Confirmed serial carried through from minting. Used by
    /// [`reflash_app_via_framed_protocol`] for the persistent audit log.
    pub fn confirmed_serial(&self) -> &str {
        &self.confirmed_serial
    }

    /// Test-only constructor bypassing the acknowledgement literals.
    #[cfg(test)]
    pub fn for_tests(serial: &str) -> Self {
        Self {
            _seal: AcknowledgeSeal,
            confirmed_serial: serial.to_string(),
        }
    }
}

// ===========================================================================
//  dsPIC serial proxy — connected-unit identity for Path C confirmation
// ===========================================================================

/// AT24C02 EEPROM I²C address on am2 hashboards. The dsPIC and its
/// paired EEPROM are physically 1:1 per board; the EEPROM serial is
/// therefore the canonical "this connected dsPIC" identity for the
/// Path C typed-serial confirmation.
///
/// The HAL write-denylist on am2 already protects 0x50-0x57 against
/// accidental writes ( +
/// `corruption-prevention guarantee #1` in the root ). Reads
/// always work.
pub const HASHBOARD_EEPROM_I2C_ADDR: u8 = 0x51;

/// Hashboard EEPROM serial-byte length (16 ASCII bytes per Bitmain
/// AT24C02 layout — see `dcentrald::runtime::hardware_info::read_miner_serial`).
pub const HASHBOARD_SERIAL_LEN: usize = 16;

/// Read the connected dsPIC's hashboard-EEPROM serial via the HAL's
/// sanctioned read-only one-shot helper.
///
/// Returns the serial as a UTF-8 string (with trailing zero/whitespace
/// trimmed). On read failure, returns [`AsicError::Pic`] so a future authority
/// layer can fail closed rather than target an unidentified board.
///
/// Used by the Path C research entrypoint to compare against its typed-serial
/// confirmation. Reads are denylist-safe; writes are blocked at the HAL layer
/// regardless of caller. No shipped package invokes this entrypoint.
pub fn read_dspic_serial_proxy(bus: u8) -> Result<String> {
    let bytes = dcentrald_hal::i2c::read_eeprom_bytes(
        bus,
        HASHBOARD_EEPROM_I2C_ADDR,
        0x00,
        HASHBOARD_SERIAL_LEN,
    )
    .map_err(|e| AsicError::Pic {
        addr: HASHBOARD_EEPROM_I2C_ADDR,
        detail: format!(
            "dspic recovery: cannot read hashboard EEPROM serial @ bus {} addr 0x{:02X}: {}. \
             Refusing to mint Path C confirmation token without verified target identity.",
            bus, HASHBOARD_EEPROM_I2C_ADDR, e,
        ),
    })?;
    Ok(parse_serial_bytes(&bytes))
}

/// Trim trailing NUL / whitespace from raw EEPROM serial bytes. Pure
/// host-safe helper so unit tests can validate the parse logic without
/// touching I²C.
pub fn parse_serial_bytes(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes).into_owned();
    s.trim_end_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

// ===========================================================================
//  Path C persistent invocation log — forensic audit trail
// ===========================================================================

/// Historical default directory retained for Path C audit-log tests.
/// `DCENT_PIC_RECOVERY_LOG_DIR` redirects host tests to a temporary directory;
/// no shipped package invokes Path C.
pub const DEFAULT_PATH_C_LOG_DIR: &str = "/var/log/dcent";

/// Filename within the log directory.
pub const PATH_C_LOG_FILENAME: &str = "pic_recovery_path_c.log";

/// Resolve the on-disk path for the Path C invocation log. Honors
/// `$DCENT_PIC_RECOVERY_LOG_DIR` for test redirection.
pub fn path_c_log_path() -> PathBuf {
    let dir = std::env::var("DCENT_PIC_RECOVERY_LOG_DIR").ok();
    path_c_log_path_from_dir(dir.as_deref().map(Path::new))
}

fn path_c_log_path_from_dir(dir: Option<&Path>) -> PathBuf {
    dir.unwrap_or_else(|| Path::new(DEFAULT_PATH_C_LOG_DIR))
        .join(PATH_C_LOG_FILENAME)
}

/// Append a single Path C invocation record to the persistent log.
///
/// Format (one line, tab-separated, append-only):
///
/// ```text
/// <unix_secs>\tpath_c\taddr=0x<NN>\tplatform=<label>\tserial=<S>\toutcome=<O>
/// ```
///
/// `outcome` is a short tag like `pre_flight_refused`, `partial_bail_60pct`,
/// `success_unimplemented` (the partial implementation cannot reach
/// success today — this is reserved for the post-R4-8 wire-up).
///
/// This retained research helper warns on log-write failure without changing
/// the protocol-validation result. That historical behavior is not authority
/// guidance: a future executor must define fail-closed, durable audit semantics
/// in the controller-recovery authority contract.
pub fn append_path_c_invocation_log(
    addr: u8,
    platform: RecoveryPlatform,
    serial: &str,
    outcome: &str,
) {
    let path = path_c_log_path();
    append_path_c_invocation_log_to(&path, addr, platform, serial, outcome);
}

fn append_path_c_invocation_log_to(
    path: &Path,
    addr: u8,
    platform: RecoveryPlatform,
    serial: &str,
    outcome: &str,
) {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!(
        "{}\tpath_c\taddr=0x{:02X}\tplatform={}\tserial={}\toutcome={}\n",
        unix_secs,
        addr,
        platform.label(),
        serial,
        outcome,
    );
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                warn!(
                    target: "dspic_recovery",
                    log_path = %path.display(),
                    error = %e,
                    "Path C invocation log write failed (non-fatal)",
                );
            }
        }
        Err(e) => {
            warn!(
                target: "dspic_recovery",
                log_path = %path.display(),
                error = %e,
                "Path C invocation log open failed (non-fatal — recovery continues)",
            );
        }
    }
}

// ===========================================================================
//  Public ops
// ===========================================================================

/// Perform the dsPIC bootloader→application jump per RE3 §5.2.
///
/// **Wire sequence** (one atomic service transaction):
///
/// 1. `Write [REG_VERSION=0x00, BL_MAGIC=0x5A]` — unlock
/// 2. `Write [REG_CONTROL=0x09, BL_CMD_JUMP=0x01]` — jump
///
/// Then polls `REG_VERSION` every 100 ms (up to 5 s) until it observes
/// 0x88 / 0x89 / 0x8A (application started successfully) or times out.
/// A timeout returning fw=0x86 means the application partition may be corrupt.
/// Shipped recovery remains ICSP-only; the framed-reflash function below is
/// retained protocol research, not an executable follow-up.
///
/// On error path, the bootloader is left in a known state: the unlock
/// + jump bytes are atomic, so the dsPIC either jumped (and we observe
/// success) or it didn't (and it stays at fw=0x86 in bootloader). There
/// is no half-jumped state.
///
/// # Arguments
///
/// * `i2c` — the process-wide I²C service handle (am2 single-owner rule).
/// * `addr` — 7-bit dsPIC address (typically 0x20, 0x21, or 0x22).
/// * `platform` — must be an am2 dsPIC family marker.
/// * `_token` — legacy research acknowledgement token. Consumed.
///
/// # Errors
///
/// * `AsicError::Pic` if the platform marker is wrong, the I²C
///   transaction fails, or the post-jump version poll times out.
pub fn jump_to_app(
    i2c: &I2cServiceHandle,
    addr: u8,
    platform: RecoveryPlatform,
    _token: ConfirmedBrickedToken,
) -> Result<()> {
    refuse_if_not_am2_dspic(platform)?;

    info!(
        target: "dspic_recovery",
        addr = format_args!("0x{:02X}", addr),
        platform = platform.label(),
        "dsPIC fw=0x86 JUMP_TO_APP (recovery-tool, --confirm-bricked) — \
         step 1: write [0x00, 0x5A] (BL_MAGIC), step 2: write [0x09, 0x01] (BL_CMD_JUMP)",
    );

    // Issue the two-step unlock+jump as one atomic service transaction.
    i2c.transaction_mutating(I2cMutationLabel::Recovery, addr, jump_steps())
        .map_err(|e| AsicError::Pic {
            addr,
            detail: format!("dspic fw=0x86 jump_to_app transaction: {}", e),
        })?;

    info!(
        target: "dspic_recovery",
        addr = format_args!("0x{:02X}", addr),
        "dsPIC jump command issued — polling REG_VERSION for app transition",
    );

    // Poll REG_VERSION for up to 5 s waiting for the app to come up.
    let deadline = std::time::Instant::now() + Duration::from_millis(WAIT_APP_TIMEOUT_MS);
    let poll = Duration::from_millis(POLL_INTERVAL_MS);
    loop {
        let bytes = i2c
            .transaction_mutating(I2cMutationLabel::QueryPrelude, addr, read_version_steps())
            .map_err(|e| AsicError::Pic {
                addr,
                detail: format!("dspic post-jump REG_VERSION read: {}", e),
            })?
            .into_iter()
            .next()
            .unwrap_or_default();
        let ver = bytes.first().copied().unwrap_or(0xFF);

        match ver {
            VER_APP_REV_A | VER_APP_CANONICAL | VER_APP_REV_B => {
                info!(
                    target: "dspic_recovery",
                    addr = format_args!("0x{:02X}", addr),
                    fw = format_args!("0x{:02X}", ver),
                    "dsPIC application running — recovery jump succeeded",
                );
                return Ok(());
            }
            VER_BOOTLOADER => {
                // Stuck — keep polling until deadline.
            }
            other => {
                // Bus noise / unexpected — keep polling.
                warn!(
                    target: "dspic_recovery",
                    addr = format_args!("0x{:02X}", addr),
                    fw = format_args!("0x{:02X}", other),
                    "dsPIC post-jump version unrecognised — continuing to poll",
                );
            }
        }

        if std::time::Instant::now() >= deadline {
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "dspic fw=0x86 jump_to_app: REG_VERSION still 0x{:02X} after {} ms — \
                     application partition likely missing/corrupt; try \
                     reflash_app_via_framed_protocol or ICSP",
                    ver, WAIT_APP_TIMEOUT_MS,
                ),
            });
        }
        std::thread::sleep(poll);
    }
}

/// Reflash the dsPIC application partition via the bootloader's framed
/// protocol — **PARTIAL implementation, returns `PartialNotImplemented`**.
///
/// # Status: PARTIAL (60% per RE3 §6 confidence table)
///
/// RE3's `dspic_fw86_recovery.md` §3 / §4 / §5 documents the recovery
/// flow at the *step level* but explicitly notes:
///
/// > "Implementation status: The framed protocol packet format is
/// > defined in bmminer's `_update_pic_app_program_1704` function.
/// > Exact framing bytes need I2C logic analyzer trace or RE of the
/// > bootloader. The protocol flow is well-understood."
/// >
/// > "Note: The exact packet framing bytes are only ~60% decoded. Full
/// > implementation requires either: I2C logic analyzer trace of
/// > bmminer performing a PIC firmware update, Ghidra disassembly of
/// > the framed protocol handler in the bootloader portion of the
/// > hex, or Reimplementation from the HAL source (`pic1704.c/h` —
/// > currently only covers short-form protocol)."
///
/// We therefore stage the well-specified portion of the flow (state
/// classification + jump bail-out) and return an explicit error if the
/// caller asks for the partial-spec write phase. The framed-write path
/// is intentionally NOT implemented; rolling it out blind would risk
/// bricking the dsPIC beyond ICSP-only recovery.
///
/// # What this function DOES do (safe portion)
///
/// 1. Refuses if `platform` is not an am2 dsPIC family marker.
/// 2. Reads `REG_VERSION` to confirm the chip is in fw=0x86 bootloader
///    state. If it's already in app mode, returns `Ok(())` (no reflash
///    needed — caller probably wanted [`jump_to_app`] instead).
/// 3. Validates `hex` is non-empty and fits in the controller-specific
///    writable application-region bounds.
/// 4. Returns a `PartialNotImplemented` error citing the RE3 reference so a
///    research caller cannot mistake validation for a completed mutation.
///
/// # What this function INTENTIONALLY does NOT do
///
/// * Issue any `WRITE_DATA_INTO_PIC` / `ERASE_PIC_APP_PROGRAM` /
///   `SEND_DATA_TO_PIC` opcodes on the wire. Without confirmed
///   wire-byte traces, those would risk a permanent brick.
/// * Use the removed historical `crate::dspic_flash` mutation helpers — their
///   inferred opcode model conflicted with the canonical protocol catalog.
///
/// // XXX: framed-reflash protocol partial — see RE3
/// // dspic_fw86_recovery.md §3.4 ("Path C: Framed Protocol Full
/// // Reflash") + §6 confidence table (60%). Closing this requires
/// // either a logic-analyzer trace of bmminer's
/// // `_update_pic_app_program_1704` performing a live update, or
/// // Ghidra disassembly of the bootloader portion of the dsPIC
/// // application hex. Track in RE Round 4 backlog.
///
/// # Historical W13.D4 double-gate design (2026-05-10)
///
///, this research entrypoint
/// consumes [`AcknowledgeSixtyPercentConfidence`] and binds a typed serial
/// (read via [`read_dspic_serial_proxy`]). Those invariants remain useful test
/// fixtures, but the former CLI UX no longer exists and no shipped package
/// enables this module. A future executor requires the separate typed authority
/// architecture for both Path B and Path C.
pub fn reflash_app_via_framed_protocol(
    i2c: &I2cServiceHandle,
    addr: u8,
    hex: &[u8],
    platform: RecoveryPlatform,
    token: AcknowledgeSixtyPercentConfidence,
) -> Result<()> {
    refuse_if_not_am2_dspic(platform)?;

    if hex.is_empty() {
        append_path_c_invocation_log(
            addr,
            platform,
            token.confirmed_serial(),
            "pre_flight_refused_empty_hex",
        );
        return Err(AsicError::Pic {
            addr,
            detail: "dspic fw=0x86 reflash: hex payload is empty".into(),
        });
    }

    info!(
        target: "dspic_recovery",
        addr = format_args!("0x{:02X}", addr),
        platform = platform.label(),
        serial = %token.confirmed_serial(),
        hex_len = hex.len(),
        "dsPIC fw=0x86 REFLASH Path C (recovery-tool, double-gate confirmed) — \
         pre-flight: reading REG_VERSION to classify chip state",
    );

    // Phase 1 (specified, safe): probe REG_VERSION to confirm chip is in
    // fw=0x86 bootloader. Refuse to proceed if it's anything else.
    let bytes = i2c
        .transaction_mutating(I2cMutationLabel::QueryPrelude, addr, read_version_steps())
        .map_err(|e| {
            append_path_c_invocation_log(
                addr,
                platform,
                token.confirmed_serial(),
                "pre_flight_refused_i2c_error",
            );
            AsicError::Pic {
                addr,
                detail: format!("dspic reflash REG_VERSION pre-flight read: {}", e),
            }
        })?
        .into_iter()
        .next()
        .unwrap_or_default();
    let ver = bytes.first().copied().unwrap_or(0xFF);
    match ver {
        VER_BOOTLOADER => {
            // Expected — chip is in bootloader, OK to proceed (partially).
        }
        VER_APP_REV_A | VER_APP_CANONICAL | VER_APP_REV_B => {
            append_path_c_invocation_log(
                addr,
                platform,
                token.confirmed_serial(),
                "pre_flight_refused_app_running",
            );
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "dspic reflash refused: REG_VERSION = 0x{:02X} (application running). \
                     If you intended to recover a stuck-in-bootloader chip, no reflash is \
                     needed. If the application is misbehaving, use ICSP rework — DO NOT \
                     run reflash on a healthy app-mode chip.",
                    ver,
                ),
            });
        }
        other => {
            append_path_c_invocation_log(
                addr,
                platform,
                token.confirmed_serial(),
                "pre_flight_refused_bus_noise",
            );
            return Err(AsicError::Pic {
                addr,
                detail: format!(
                    "dspic reflash refused: REG_VERSION = 0x{:02X} (unknown / bus noise). \
                     Verify I²C bus is alive and chip ACKs at 0x{:02X} before retrying.",
                    other, addr,
                ),
            });
        }
    }

    // Phase 2 (PARTIAL — RE3 60% confidence): the actual seek/erase/write
    // wire format is not byte-confirmed. Bail with an explicit
    // partial-not-implemented error so a research caller knows this is by
    // design, not a completed mutation path, and so a future RE round can
    // close it without having to reverse a blind implementation.
    //
    // XXX: framed-reflash protocol partial — see RE3
    // dspic_fw86_recovery.md §3.4 ("Path C: Framed Protocol Full
    // Reflash") + §6 confidence table (60%). Track in RE Round 4
    // backlog.
    warn!(
        target: "dspic_recovery",
        addr = format_args!("0x{:02X}", addr),
        serial = %token.confirmed_serial(),
        "dsPIC framed-protocol reflash is PARTIAL — refusing destructive \
         seek/erase/write phase until RE Round 4 closes the wire-byte trace. \
         See RE3 dspic_fw86_recovery.md §3.4 + §6.",
    );

    append_path_c_invocation_log(
        addr,
        platform,
        token.confirmed_serial(),
        "partial_bail_60pct",
    );

    Err(AsicError::Pic {
        addr,
        detail: format!(
            "dspic fw=0x86 reflash: framed-protocol seek/erase/write phase NOT \
             IMPLEMENTED (partial — RE3 §3.4 / §6 60% confidence). hex_len={} \
             bytes were validated but not sent. Recovery options: (a) try \
             jump_to_app first — bootloader may still be able to launch the \
             existing app partition; (b) ICSP rework via PICkit3/4 on the \
             dsPIC test pads. Track wire-byte closure in RE Round 4 backlog.",
            hex.len(),
        ),
    })
}

// ===========================================================================
//  Tests (host-safe — no I²C bus required for the protocol layer)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_steps_emit_5a_then_01_in_order() {
        let steps = jump_steps();
        assert_eq!(steps.len(), 2, "jump must emit exactly two Write steps");

        // Step 0: REG_VERSION = BL_MAGIC (0x00, 0x5A) — unlock.
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(
                    buf,
                    &vec![REG_VERSION, BL_MAGIC],
                    "first write must be 0x5A → REG_VERSION (bootloader unlock)",
                );
                assert_eq!(buf[0], 0x00, "REG_VERSION must be 0x00");
                assert_eq!(buf[1], 0x5A, "BL_MAGIC must be 0x5A");
            }
            other => panic!("step 0 must be Write, got {:?}", other),
        }

        // Step 1: REG_CONTROL = BL_CMD_JUMP (0x09, 0x01) — jump.
        match &steps[1] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(
                    buf,
                    &vec![REG_CONTROL, BL_CMD_JUMP],
                    "second write must be 0x01 → REG_CONTROL (jump command)",
                );
                assert_eq!(buf[0], 0x09, "REG_CONTROL must be 0x09");
                assert_eq!(buf[1], 0x01, "BL_CMD_JUMP must be 0x01");
            }
            other => panic!("step 1 must be Write, got {:?}", other),
        }
    }

    #[test]
    fn read_version_steps_is_one_write_read_byte() {
        let steps = read_version_steps();
        assert_eq!(steps.len(), 1, "read_version is one WriteRead step");
        match &steps[0] {
            I2cTransactionStep::WriteRead {
                write_data,
                read_len,
            } => {
                assert_eq!(write_data, &vec![REG_VERSION]);
                assert_eq!(*read_len, 1);
            }
            other => panic!("expected WriteRead, got {:?}", other),
        }
    }

    #[test]
    fn constants_match_pic1704_register_layout() {
        // The dsPIC bootloader at fw=0x86 reuses the PIC1704 register
        // layout per RE3 §5.2. If `pic1704::protocol::REG_VERSION` /
        // `REG_CONTROL` / `BL_MAGIC` / `BL_CMD_JUMP` / `VER_BOOTLOADER`
        // ever drift, this assertion stops compiling and the divergence
        // must be addressed in lockstep.
        assert_eq!(REG_VERSION, crate::pic1704::REG_VERSION);
        assert_eq!(REG_CONTROL, crate::pic1704::REG_CONTROL);
        assert_eq!(BL_MAGIC, crate::pic1704::BL_MAGIC);
        assert_eq!(BL_CMD_JUMP, crate::pic1704::BL_CMD_JUMP);
        assert_eq!(VER_BOOTLOADER, crate::pic1704::VER_BOOTLOADER);
        assert_eq!(VER_APP_REV_A, crate::pic1704::VER_REV_A);
        assert_eq!(VER_APP_CANONICAL, crate::pic1704::VER_APPLICATION);
        assert_eq!(VER_APP_REV_B, crate::pic1704::VER_REV_B);
    }

    #[test]
    fn refuse_if_not_am2_dspic_accepts_all_am2_variants() {
        // All RecoveryPlatform variants are am2 dsPIC by construction.
        // If a future variant is added that is NOT am2 dsPIC, the match
        // arm in `refuse_if_not_am2_dspic` will fail to compile (forcing
        // the safety guard to be updated in lockstep with the enum).
        assert!(refuse_if_not_am2_dspic(RecoveryPlatform::Am2S17).is_ok());
        assert!(refuse_if_not_am2_dspic(RecoveryPlatform::Am2S19Pro).is_ok());
        assert!(refuse_if_not_am2_dspic(RecoveryPlatform::Am2S19jProZynq).is_ok());
    }

    #[test]
    fn recovery_platform_label_is_stable() {
        // Labels are user-visible (log lines, error messages). Pin them
        // so a refactor doesn't silently change wire-format-adjacent
        // error strings.
        assert_eq!(RecoveryPlatform::Am2S17.label(), "am2-s17");
        assert_eq!(RecoveryPlatform::Am2S19Pro.label(), "am2-s19pro");
        assert_eq!(RecoveryPlatform::Am2S19jProZynq.label(), "am2-s19jpro-zynq");
    }

    #[test]
    fn poll_constants_match_pic1704_runtime_timing() {
        // Mirror the PIC1704 runtime timing — see module docs.
        assert_eq!(POLL_INTERVAL_MS, crate::pic1704::POLL_INTERVAL_MS);
        assert_eq!(WAIT_APP_TIMEOUT_MS, crate::pic1704::WAIT_APP_TIMEOUT_MS);
    }

    // -- Path C double-gate token (W13.D4) ---------------------------------

    #[test]
    fn jump_to_app_accepts_single_confirm_bricked() {
        // Path B keeps single-flag UX. Mintability of the canonical
        // ConfirmedBrickedToken with just `--confirm-bricked` is the
        // load-bearing assertion that jump_to_app's UX has not regressed.
        // Wire-byte order is covered by jump_steps_emit_5a_then_01_in_order.
        let tok = ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked");
        assert!(
            tok.is_ok(),
            "Path B (jump-only) must accept single --confirm-bricked flag",
        );
        // Compile-time signature pin: jump_to_app takes ConfirmedBrickedToken,
        // NOT AcknowledgeSixtyPercentConfidence. If a future refactor ever
        // promotes Path B to the double-gate, this stops compiling.
        let _f: fn(&I2cServiceHandle, u8, RecoveryPlatform, ConfirmedBrickedToken) -> Result<()> =
            jump_to_app;
    }

    #[test]
    fn reflash_fw86_requires_double_flag() {
        // Single --confirm-bricked is NOT enough for Path C.
        let r = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "--confirm-bricked",
            "",
            "ABC123",
            "ABC123",
        );
        assert!(
            r.is_err(),
            "Path C must reject missing --i-acknowledge flag"
        );
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("60-percent")
                || err.contains("60%")
                || err.contains("acknowledge")
                || err.contains("confidence"),
            "error must explain the 60% confidence requirement, got {:?}",
            err,
        );

        // Wrong second flag is also rejected.
        let r2 = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "--confirm-bricked",
            "--force",
            "ABC123",
            "ABC123",
        );
        assert!(r2.is_err(), "Path C must reject wrong second flag");

        // Missing first flag is also rejected.
        let r3 = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "",
            "--i-acknowledge-60-percent-byte-exact-confidence",
            "ABC123",
            "ABC123",
        );
        assert!(r3.is_err(), "Path C must reject missing --confirm-bricked");
    }

    #[test]
    fn reflash_fw86_serial_mismatch_aborts() {
        // Both flags present, but typed serial does not match the connected
        // dsPIC's hashboard EEPROM serial → must refuse.
        let r = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "--confirm-bricked",
            "--i-acknowledge-60-percent-byte-exact-confidence",
            "WRONG_BOARD",
            "ABC123",
        );
        assert!(r.is_err(), "serial mismatch must abort token mint");
        let err = r.unwrap_err().to_string();
        assert!(
            err.contains("does not match")
                || err.contains("wrong unit")
                || err.contains("WRONG_BOARD"),
            "error must explain the serial-mismatch refusal, got {:?}",
            err,
        );

        // Empty typed serial is also refused; an empty string cannot bind the
        // research token to a target fixture.
        let r2 = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "--confirm-bricked",
            "--i-acknowledge-60-percent-byte-exact-confidence",
            "",
            "ABC123",
        );
        assert!(r2.is_err(), "empty typed serial must be refused");
    }

    #[test]
    fn acknowledge_token_mintable_only_with_both_flags() {
        // Happy path — both flags + matching serial → token mints.
        let tok = AcknowledgeSixtyPercentConfidence::mint_with_double_confirmation(
            "--confirm-bricked",
            "--i-acknowledge-60-percent-byte-exact-confidence",
            "ABC123",
            "ABC123",
        );
        assert!(
            tok.is_ok(),
            "happy path with both flags + matching serial must mint",
        );
        let tok = tok.unwrap();
        assert_eq!(
            tok.confirmed_serial(),
            "ABC123",
            "minted token must carry the operator-confirmed serial through",
        );

        // Compile-time signature pin: reflash_app_via_framed_protocol takes
        // AcknowledgeSixtyPercentConfidence, NOT ConfirmedBrickedToken. If a
        // future refactor weakens Path C back to single-gate, this stops
        // compiling.
        let _f: fn(
            &I2cServiceHandle,
            u8,
            &[u8],
            RecoveryPlatform,
            AcknowledgeSixtyPercentConfidence,
        ) -> Result<()> = reflash_app_via_framed_protocol;

        // Test-only constructor exists for token-consuming integration tests.
        let _t = AcknowledgeSixtyPercentConfidence::for_tests("ABC123");
    }

    #[test]
    fn parse_serial_bytes_trims_trailing_padding() {
        assert_eq!(parse_serial_bytes(b"ABC123\0\0\0\0\0\0\0\0\0\0"), "ABC123");
        assert_eq!(parse_serial_bytes(b"ABC123          "), "ABC123");
        assert_eq!(parse_serial_bytes(b"ABC123\0  \0  "), "ABC123");
        assert_eq!(parse_serial_bytes(b"ABC123"), "ABC123");
        assert_eq!(parse_serial_bytes(b""), "");
    }

    #[test]
    fn path_c_log_path_honors_env_override() {
        let dir = std::env::temp_dir().join("dcent_recovery_path_c_test_pin");
        let p = path_c_log_path_from_dir(Some(&dir));
        assert_eq!(p, dir.join(PATH_C_LOG_FILENAME));

        // Without an override, the retained historical default path applies.
        let p = path_c_log_path_from_dir(None);
        assert_eq!(
            p,
            std::path::PathBuf::from(DEFAULT_PATH_C_LOG_DIR).join(PATH_C_LOG_FILENAME)
        );
    }

    #[test]
    fn append_path_c_invocation_log_writes_record() {
        let dir = std::env::temp_dir()
            .join(format!("dcent_recovery_path_c_test_{}", std::process::id(),));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(PATH_C_LOG_FILENAME);

        append_path_c_invocation_log_to(
            &path,
            0x21,
            RecoveryPlatform::Am2S19jProZynq,
            "ABC123",
            "partial_bail_60pct",
        );

        let log = std::fs::read_to_string(path).expect("log file must exist after append");
        assert!(
            log.contains("path_c"),
            "log line missing path_c tag: {:?}",
            log
        );
        assert!(
            log.contains("addr=0x21"),
            "log line missing addr: {:?}",
            log
        );
        assert!(
            log.contains("platform=am2-s19jpro-zynq"),
            "log line missing platform: {:?}",
            log,
        );
        assert!(
            log.contains("serial=ABC123"),
            "log line missing serial: {:?}",
            log
        );
        assert!(
            log.contains("outcome=partial_bail_60pct"),
            "log line missing outcome: {:?}",
            log,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
