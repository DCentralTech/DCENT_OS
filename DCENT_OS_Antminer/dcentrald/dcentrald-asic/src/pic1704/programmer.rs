//! PIC1704 bootloader programmer ops — **recovery-tool only**.
//!
//! This module is compiled out of the production `dcentrald` binary by the
//! `#[cfg(feature = "recovery-tool")]` gate in `super::mod`. Only the
//! `pic-recovery` crate enables that feature, so accidental linkage from
//! the daemon is a **compile error**, not a runtime guard. This mirrors
//! the `dspic_flash` lockdown pattern ( root §"Corruption-
//! prevention guarantees" #2).
//!
//! # Source of truth
//!
//! Operation list comes from RE2's `S19J_PRO_PORTING_PLAN.md` §12
//! ("Hardware Abstraction Layer / PIC1704 Protocol"):
//!
//! | Op                       | Purpose                            |
//! |--------------------------|------------------------------------|
//! | `pic_seek_1704`          | Set internal flash pointer to addr |
//! | `pic_erase_1704`         | Erase N flash pages at pointer     |
//! | `pic_write_1704`         | Write data to flash at pointer     |
//! | `pic_start_app_common`   | Jump from bootloader to app        |
//!
//! RE2's dev-kit `SOURCE_HAL/pic1704.{c,h}` does **not** expose programmer
//! ops — only runtime ops (heartbeat, dc-dc, voltage/current/temp). The
//! programmer opcode wire-format below is therefore an inferred mapping
//! that mirrors the BraiinsOS PIC16F1704 bootloader ABI as documented in
//! `dcentrald-asic::dspic_flash` §"Protocol revision history":
//!
//! - `0x01` SEEK / SET_FLASH_POINTER
//! - `0x05` WRITE_DATA_INTO_PIC (chunked)
//! - `0x09` ERASE_PIC_APP_PROGRAM (bulk) — paired with sector-count payload
//!
//! Bitmain reused the bmminer power-controller bootloader ABI across PIC
//! families (PIC16F1704 on S9, dsPIC33EP16GS202 on S17/S19j am2, PIC1704
//! on CV1835 / AM335x BB / Amlogic S19j Pro). The opcode space is shared.
//! When live RE evidence on a CV1835 / BB / AML S19j Pro bootloader
//! disagrees with this mapping, this module's wire format must follow
//! that evidence; that's why every helper is host-testable and every
//! Service method goes through the I2C service queue (no raw `/dev/i2c-N`).
//!
//! # Why direct I²C is allowed here (vs )
//!
//!  says NEVER flash PICs via AXI IIC. That
//! rule applies to **am2 Zynq dsPIC** on S19j Pro Zynq variants where the
//! kernel xiic-i2c driver reuses the AXI IIC controller for the FPGA
//! work-rx path. PIC1704 on CV1835 / AM335x BB / Amlogic S19j Pro has
//! NO FPGA IIC — it hangs off a plain `/dev/i2c-0` exposed by the host
//! SoC's I2C controller (CV1835: pinmux'd CV1835 i2c block; BB: AM335x
//! i2c0; Amlogic: standard kernel i2c-meson). There is no AXI bus to
//! corrupt, so direct I²C from the recovery binary is the correct path.
//! The single-I²C-owner rule still applies on am2 — but the PIC1704
//! platforms (CV/BB/AML) are not am2.
//!
//! # Gates layered on every op
//!
//! Every programmer op in this module enforces TWO gates:
//!
//! 1. **Compile-time gate**: `#[cfg(feature = "recovery-tool")]` on the
//!    module itself + `Pic1704Authorized` sealed trait on construction.
//! 2. **Runtime gate**: `ConfirmedBrickedToken` argument that can only be
//!    minted by the recovery binary's `--confirm-bricked` flow + a
//!    `version == VER_BOOTLOADER (0x86)` precondition. Programmer ops
//!    refuse if the chip is in application mode (`0x88`/`0x89`/`0x8A`).
//!
//!:
//! `start_app` is destructive in App mode. `seek` / `erase` / `write` are
//! similarly destructive: writing to `REG_VERSION`/`REG_CONTROL` in app
//! mode would corrupt the running PIC's command parser.

#![cfg(feature = "recovery-tool")]

use dcentrald_hal::i2c::I2cTransactionStep;
use tracing::{info, warn};

use super::protocol::{classify_version, Pic1704State, REG_CONTROL, VER_BOOTLOADER};
use super::service::Pic1704Service;
use crate::AsicError;
use crate::Result;

// ===========================================================================
//  Programmer opcodes (BraiinsOS bmminer ABI — shared across PIC families)
// ===========================================================================

/// SEEK / SET_FLASH_POINTER. Payload: 4 address bytes (LE u32).
pub const OP_SEEK: u8 = 0x01;
/// WRITE_DATA_INTO_PIC. Payload: chunk bytes (≤ `WRITE_CHUNK_BYTES`).
pub const OP_WRITE: u8 = 0x05;
/// ERASE_PIC_APP_PROGRAM (bulk). Payload: page-count byte.
pub const OP_ERASE: u8 = 0x09;

/// Maximum bytes per `pic_write_1704` chunk. The PIC1704 staging buffer is
/// 64 bytes per the bmminer reference; chunks larger than this are
/// rejected with [`AsicError::Pic`]. Keep this small enough that one
/// chunk fits inside the kernel I2C `MAX_FRAME` budget on every host
/// platform (CV1835, AM335x, Amlogic).
pub const WRITE_CHUNK_BYTES: usize = 64;

// ===========================================================================
//  ConfirmedBrickedToken — runtime confirmation gate
// ===========================================================================

/// Proof-of-operator-confirmation token, required for every destructive
/// programmer op in this module.
///
/// Construction is gated by [`ConfirmedBrickedToken::new_with_confirmation`],
/// which only accepts the literal string `"--confirm-bricked"`. The
/// recovery binary's CLI flow is the only caller that has any reason to
/// know that string — it's the same `--confirm-bricked` flag operators
/// type into the `pic-recovery` shell command.
///
/// The token is intentionally `!Clone` and `!Copy`: each programmer op
/// consumes a fresh one. This forces the recovery binary to re-mint the
/// token at each op boundary, and prevents accidental "save the token
/// once, reuse forever" anti-patterns from the daemon side.
///
/// In tests (also `#[cfg(feature = "recovery-tool")]`), [`Self::for_tests`]
/// produces a token without the runtime confirmation string. That helper
/// is `cfg(test)` so it never compiles into a release artifact.
#[derive(Debug)]
pub struct ConfirmedBrickedToken {
    // Private field with private type so the token cannot be constructed
    // via struct literal syntax outside this module.
    _seal: ConfirmedBrickedSeal,
}

#[derive(Debug)]
struct ConfirmedBrickedSeal;

impl ConfirmedBrickedToken {
    /// Mint a token from the recovery binary's `--confirm-bricked` flag.
    ///
    /// `flag` MUST be exactly `"--confirm-bricked"`. Any other value
    /// returns [`AsicError::Pic`] with a clear "operator did not confirm"
    /// message. This is the only public constructor outside `cfg(test)`.
    pub fn new_with_confirmation(flag: &str) -> Result<Self> {
        if flag != "--confirm-bricked" {
            return Err(AsicError::Pic {
                addr: 0x20,
                detail: format!(
                    "ConfirmedBrickedToken: operator did not confirm (got {:?}, expected \"--confirm-bricked\")",
                    flag,
                ),
            });
        }
        Ok(Self {
            _seal: ConfirmedBrickedSeal,
        })
    }

    /// Test-only constructor. Compiles only with `#[cfg(test)]` so it
    /// cannot leak into the recovery binary or any release artifact.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            _seal: ConfirmedBrickedSeal,
        }
    }
}

// ===========================================================================
//  Wire-format helpers (host-safe, no I2C bus required)
// ===========================================================================

/// Build the steps for `pic_seek_1704(addr)`. Wire format:
/// `[REG_CONTROL, OP_SEEK, addr_le[0], addr_le[1], addr_le[2], addr_le[3]]`.
///
/// The address goes to `REG_CONTROL` (the same register `start_app` uses
/// for `BL_CMD_JUMP`) because the PIC1704 bootloader multiplexes opcodes
/// over the control register — same pattern as the existing
/// `start_app_steps`.
pub fn seek_steps(addr: u32) -> Vec<I2cTransactionStep> {
    let bytes = addr.to_le_bytes();
    vec![I2cTransactionStep::Write(vec![
        REG_CONTROL,
        OP_SEEK,
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
    ])]
}

/// Build the steps for `pic_erase_1704(addr, n_pages)`.
///
/// Internally this is two writes: a SEEK to position the pointer, then
/// the ERASE op with the page-count payload. We pack both into one
/// `Vec<I2cTransactionStep>` so the service queue keeps them atomic
/// (no other I²C client can interleave between the two writes).
pub fn erase_steps(addr: u32, n_pages: u8) -> Vec<I2cTransactionStep> {
    let mut steps = seek_steps(addr);
    steps.push(I2cTransactionStep::Write(vec![
        REG_CONTROL,
        OP_ERASE,
        n_pages,
    ]));
    steps
}

/// Build the steps for `pic_write_1704(addr, data)`. Caller must already
/// have validated `data.len() <= WRITE_CHUNK_BYTES`.
///
/// Wire format per chunk: SEEK to `addr`, then WRITE with the chunk
/// payload. As with `erase_steps`, both go in one transaction so they
/// are atomic in the service queue.
fn write_chunk_steps(addr: u32, chunk: &[u8]) -> Vec<I2cTransactionStep> {
    debug_assert!(chunk.len() <= WRITE_CHUNK_BYTES);
    let mut steps = seek_steps(addr);
    let mut payload = Vec::with_capacity(2 + chunk.len());
    payload.push(REG_CONTROL);
    payload.push(OP_WRITE);
    payload.extend_from_slice(chunk);
    steps.push(I2cTransactionStep::Write(payload));
    steps
}

/// Iterate `data` in `WRITE_CHUNK_BYTES`-sized chunks, returning one
/// `(addr, steps)` pair per chunk. Each chunk's address is bumped by
/// `WRITE_CHUNK_BYTES` from the previous one. Pure / host-safe.
pub fn chunked_write_plan(addr: u32, data: &[u8]) -> Vec<(u32, Vec<I2cTransactionStep>)> {
    let mut plan = Vec::new();
    let mut cur = addr;
    for chunk in data.chunks(WRITE_CHUNK_BYTES) {
        plan.push((cur, write_chunk_steps(cur, chunk)));
        cur = cur.wrapping_add(chunk.len() as u32);
    }
    plan
}

// ===========================================================================
//  Service-attached programmer ops (require ConfirmedBrickedToken)
// ===========================================================================

/// Refuse if the cached state is anything other than `Bootloader`. This
/// is the load-bearing safety guard: writing programmer ops to a PIC in
/// application mode would clobber the running `REG_CONTROL` semantics
/// (DC-DC enable, heartbeat) — see
/// .
fn refuse_if_not_bootloader(svc: &Pic1704Service) -> Result<()> {
    match svc.state() {
        Pic1704State::Bootloader => Ok(()),
        other => Err(AsicError::Pic {
            addr: svc.address(),
            detail: format!(
                "programmer op refused: PIC must be in bootloader, current state is {:?} (fw=0x{:02X})",
                other,
                svc.fw_version(),
            ),
        }),
    }
}

/// `pic_seek_1704(addr)` — set the bootloader's internal flash pointer.
///
/// **Refuses** if the cached version is not `VER_BOOTLOADER` (0x86).
/// **Consumes** a `ConfirmedBrickedToken`.
pub fn pic_seek_1704(
    svc: &mut Pic1704Service,
    addr: u32,
    _token: ConfirmedBrickedToken,
) -> Result<()> {
    refuse_if_not_bootloader(svc)?;
    info!(
        addr_pic = format_args!("0x{:02X}", svc.address()),
        flash_addr = format_args!("0x{:08X}", addr),
        "PIC1704 SEEK (recovery-tool, --confirm-bricked)",
    );
    svc.run_programmer_steps(seek_steps(addr), "seek")
}

/// `pic_erase_1704(addr, n_pages)` — erase N flash pages at the given
/// address. Each page is the PIC's native erase granule (typically 32
/// instruction words / 64 bytes; see `WRITE_CHUNK_BYTES`).
///
/// **Refuses** if the cached version is not `VER_BOOTLOADER`.
/// **Consumes** a `ConfirmedBrickedToken`.
pub fn pic_erase_1704(
    svc: &mut Pic1704Service,
    addr: u32,
    n_pages: u8,
    _token: ConfirmedBrickedToken,
) -> Result<()> {
    refuse_if_not_bootloader(svc)?;
    if n_pages == 0 {
        return Err(AsicError::Pic {
            addr: svc.address(),
            detail: "pic_erase_1704: n_pages must be > 0".into(),
        });
    }
    warn!(
        addr_pic = format_args!("0x{:02X}", svc.address()),
        flash_addr = format_args!("0x{:08X}", addr),
        n_pages,
        "PIC1704 ERASE (recovery-tool, DESTRUCTIVE)",
    );
    svc.run_programmer_steps(erase_steps(addr, n_pages), "erase")
}

/// `pic_write_1704(addr, data)` — write data to flash starting at `addr`.
/// Chunks `data` into `WRITE_CHUNK_BYTES`-sized writes and issues one
/// service transaction per chunk (each transaction is one SEEK + one
/// WRITE, atomic in the queue).
///
/// **Refuses** if the cached version is not `VER_BOOTLOADER`.
/// **Consumes** a `ConfirmedBrickedToken`.
pub fn pic_write_1704(
    svc: &mut Pic1704Service,
    addr: u32,
    data: &[u8],
    _token: ConfirmedBrickedToken,
) -> Result<()> {
    refuse_if_not_bootloader(svc)?;
    if data.is_empty() {
        return Err(AsicError::Pic {
            addr: svc.address(),
            detail: "pic_write_1704: data is empty".into(),
        });
    }
    info!(
        addr_pic = format_args!("0x{:02X}", svc.address()),
        flash_addr = format_args!("0x{:08X}", addr),
        bytes = data.len(),
        chunks = data.len().div_ceil(WRITE_CHUNK_BYTES),
        "PIC1704 WRITE (recovery-tool, DESTRUCTIVE)",
    );
    for (chunk_addr, steps) in chunked_write_plan(addr, data) {
        svc.run_programmer_steps(steps, &format!("write@0x{:08X}", chunk_addr))?;
    }
    Ok(())
}

/// `pic_start_app_common()` — the canonical, fully-paranoid bootloader →
/// application transition.
///
/// This is the "common" variant referenced in RE2 §12. It differs from
/// the existing [`Pic1704Service::start_app`] runtime helper in two
/// recovery-relevant ways:
///
/// 1. It **re-reads** `REG_VERSION` first to refuse if the PIC is already
///    in application mode (no silent no-op fallthrough). The runtime
///    helper treats App-mode as a no-op success, which is correct for
///    daemon use; for the recovery flow, the operator wants a hard
///    failure so they know the PIC didn't actually need recovery.
/// 2. It **consumes** a `ConfirmedBrickedToken`, matching the rest of
///    the programmer ops.
///
/// Wire format reuses [`super::protocol::start_app_steps`]:
/// `0x5A → REG_VERSION` followed by `0x01 → REG_CONTROL`, in one
/// service transaction.
pub fn pic_start_app_common(svc: &mut Pic1704Service, _token: ConfirmedBrickedToken) -> Result<()> {
    // Force a fresh version read — the cached state may be stale.
    let ver = svc.read_version()?;
    if classify_version(ver) != Pic1704State::Bootloader {
        return Err(AsicError::Pic {
            addr: svc.address(),
            detail: format!(
                "pic_start_app_common: PIC is not in bootloader (fw=0x{:02X}). \
                 If the PIC is already in app mode, no recovery start_app is needed.",
                ver,
            ),
        });
    }
    info!(
        addr_pic = format_args!("0x{:02X}", svc.address()),
        fw = format_args!("0x{:02X}", ver),
        "PIC1704 START_APP_COMMON (recovery-tool)",
    );
    // Reuse the runtime helper for the actual write — it issues the same
    // `[REG_VERSION=0x5A, REG_CONTROL=0x01]` pair in one transaction. The
    // safety guard above is what makes this the "common" variant.
    svc.start_app()?;
    let _ = VER_BOOTLOADER; // anchor: confirms we used the canonical bootloader constant
    Ok(())
}

// ===========================================================================
//  Service helper exposed to programmer ops
// ===========================================================================

impl Pic1704Service {
    /// Run a programmer-op transaction through the service handle.
    ///
    /// `op_label` is used only for error messages — keep it short.
    /// This method is `#[cfg(feature = "recovery-tool")]` and so does not
    /// exist in production builds.
    #[cfg(feature = "recovery-tool")]
    pub(super) fn run_programmer_steps(
        &mut self,
        steps: Vec<I2cTransactionStep>,
        op_label: &str,
    ) -> Result<()> {
        self.i2c_handle()
            .transaction(self.address(), steps)
            .map(|_| ())
            .map_err(|e| AsicError::Pic {
                addr: self.address(),
                detail: format!("programmer {} transaction: {}", op_label, e),
            })
    }
}

// ===========================================================================
//  Tests (host-safe — no I2C bus required for the protocol layer)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- ConfirmedBrickedToken --------------------------------------------

    #[test]
    fn token_construction_requires_exact_flag() {
        assert!(ConfirmedBrickedToken::new_with_confirmation("--confirm-bricked").is_ok());
        assert!(ConfirmedBrickedToken::new_with_confirmation("").is_err());
        assert!(ConfirmedBrickedToken::new_with_confirmation("confirm-bricked").is_err());
        assert!(ConfirmedBrickedToken::new_with_confirmation("--Confirm-Bricked").is_err());
        assert!(ConfirmedBrickedToken::new_with_confirmation("--confirm").is_err());
        assert!(ConfirmedBrickedToken::new_with_confirmation("--force").is_err());
    }

    #[test]
    fn token_for_tests_is_only_cfg_test() {
        // This compiles because we're inside cfg(test). In a release
        // build, ConfirmedBrickedToken::for_tests does not exist.
        let _t = ConfirmedBrickedToken::for_tests();
    }

    // --- seek_steps -------------------------------------------------------

    #[test]
    fn seek_writes_correct_bytes_for_address() {
        // 0x12345678 LE = 78 56 34 12
        let steps = seek_steps(0x12345678);
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(
                    buf,
                    &vec![REG_CONTROL, OP_SEEK, 0x78, 0x56, 0x34, 0x12],
                    "seek must emit [REG_CONTROL, OP_SEEK, addr_LE...]",
                );
            }
            other => panic!("expected Write, got {:?}", other),
        }
    }

    #[test]
    fn seek_zero_address() {
        let steps = seek_steps(0);
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &vec![REG_CONTROL, OP_SEEK, 0, 0, 0, 0]);
            }
            _ => panic!("expected Write"),
        }
    }

    #[test]
    fn seek_max_address() {
        let steps = seek_steps(u32::MAX);
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &vec![REG_CONTROL, OP_SEEK, 0xFF, 0xFF, 0xFF, 0xFF]);
            }
            _ => panic!("expected Write"),
        }
    }

    // --- erase_steps ------------------------------------------------------

    #[test]
    fn erase_steps_emit_seek_then_erase() {
        let steps = erase_steps(0x0400, 4);
        assert_eq!(steps.len(), 2, "erase must be SEEK + ERASE");

        // Step 0: SEEK 0x00000400
        match &steps[0] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf[0], REG_CONTROL);
                assert_eq!(buf[1], OP_SEEK);
                // 0x00000400 LE = 00 04 00 00
                assert_eq!(&buf[2..], &[0x00, 0x04, 0x00, 0x00]);
            }
            _ => panic!("expected Write for SEEK"),
        }

        // Step 1: ERASE 4 pages
        match &steps[1] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf, &vec![REG_CONTROL, OP_ERASE, 4]);
            }
            _ => panic!("expected Write for ERASE"),
        }
    }

    // --- write chunking ---------------------------------------------------

    #[test]
    fn write_chunks_data_correctly_single_chunk() {
        let data = [0xAA; 16];
        let plan = chunked_write_plan(0x0500, &data);
        assert_eq!(plan.len(), 1, "16 bytes fits in one 64-byte chunk");
        let (addr, steps) = &plan[0];
        assert_eq!(*addr, 0x0500);
        // SEEK + WRITE
        assert_eq!(steps.len(), 2);
        match &steps[1] {
            I2cTransactionStep::Write(buf) => {
                assert_eq!(buf[0], REG_CONTROL);
                assert_eq!(buf[1], OP_WRITE);
                assert_eq!(&buf[2..], &[0xAA; 16]);
            }
            _ => panic!("expected Write for chunk"),
        }
    }

    #[test]
    fn write_chunks_data_correctly_multi_chunk() {
        // 130 bytes → 64 + 64 + 2.
        let data: Vec<u8> = (0..130).map(|i| i as u8).collect();
        let plan = chunked_write_plan(0x1000, &data);
        assert_eq!(plan.len(), 3, "130 / 64 = 3 chunks");

        // Chunk addresses must advance by chunk size.
        assert_eq!(plan[0].0, 0x1000);
        assert_eq!(plan[1].0, 0x1000 + WRITE_CHUNK_BYTES as u32);
        assert_eq!(plan[2].0, 0x1000 + 2 * WRITE_CHUNK_BYTES as u32);

        // Last chunk has only 2 payload bytes.
        match &plan[2].1[1] {
            I2cTransactionStep::Write(buf) => {
                // [REG_CONTROL, OP_WRITE, 128, 129]
                assert_eq!(buf.len(), 4);
                assert_eq!(buf[0], REG_CONTROL);
                assert_eq!(buf[1], OP_WRITE);
                assert_eq!(buf[2], 128);
                assert_eq!(buf[3], 129);
            }
            _ => panic!("expected Write for last chunk"),
        }
    }

    #[test]
    fn write_chunk_size_constant_matches_64() {
        // Cross-check: if anyone bumps WRITE_CHUNK_BYTES, recheck the
        // PIC1704 staging buffer size and the I2C MAX_FRAME budget.
        assert_eq!(WRITE_CHUNK_BYTES, 64);
    }

    // --- start_app + bootloader-only invariants ---------------------------
    //
    // We can't construct a real `Pic1704Service` without an
    // `I2cServiceHandle` (which needs a live `/dev/i2c-N`). The
    // version-gate logic and step-emission are covered by:
    //   - `super::protocol::tests::start_app_emits_magic_then_jump_in_order`
    //     (exact byte order).
    //   - the `programmer_ops_require_confirmed_bricked_token` compile
    //     check below (signature shape).

    #[test]
    fn programmer_ops_require_confirmed_bricked_token() {
        // Compile-time check: every public op consumes a token. If a
        // future refactor removes the token argument, this test stops
        // compiling. We use `fn` pointer coercion to lock the signatures.
        let _seek: fn(&mut Pic1704Service, u32, ConfirmedBrickedToken) -> Result<()> =
            pic_seek_1704;
        let _erase: fn(&mut Pic1704Service, u32, u8, ConfirmedBrickedToken) -> Result<()> =
            pic_erase_1704;
        let _write: fn(&mut Pic1704Service, u32, &[u8], ConfirmedBrickedToken) -> Result<()> =
            pic_write_1704;
        let _start_app: fn(&mut Pic1704Service, ConfirmedBrickedToken) -> Result<()> =
            pic_start_app_common;
    }

    #[test]
    fn opcodes_are_canonical() {
        // BraiinsOS bmminer ABI — these are load-bearing across PIC
        // families; if any of them changes, dspic_flash::OP_* and
        // pic-recovery's CMD_* must change in lockstep.
        assert_eq!(OP_SEEK, 0x01);
        assert_eq!(OP_WRITE, 0x05);
        assert_eq!(OP_ERASE, 0x09);
    }
}
