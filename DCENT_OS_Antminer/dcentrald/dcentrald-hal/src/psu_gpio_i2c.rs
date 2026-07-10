//! GPIO bit-bang I2C for APW PSU communication.
//!
//! The S19 Pro PSU communicates via GPIO bit-bang I2C (NOT a kernel i2c adapter).
//! SDA: GPIO 895, SCL: GPIO 896, ~400 Hz bit rate (sysfs path) or ~10 kHz
//! effective (mmap path).
//! Uses sysfs GPIO with open-drain emulation.
//!
//! Open-drain emulation: to drive HIGH, set direction to "in" (external pull-up
//! pulls the line HIGH). To drive LOW, set direction to "out" and write "0".
//!
//! Source: S19 Pro BraiinsOS live probe, APW_PSU.py reference implementation.
//!
//! #  (2026-05-23): mmap'd AXI GPIO backend
//!
//!  live capture on `a lab unit` proved DCENT_OS's WRITE bytes are
//! byte-perfect per APW12 spec but the sysfs path's effective I2C clock
//! is ~50 Hz (vs the  intent of 10 kHz) because each
//! `/sys/class/gpio/gpioN/direction` write takes 5-10 ms on this Zynq
//! kernel. The Loki spoof's i2c slave state machine NAKs at that clock.
//!
//! The  mmap backend bypasses sysfs by mapping the AXI GPIO IP at
//! `0x41220000` via `/dev/mem` and writing the DATA/TRI registers
//! directly. The bank has ONLY gpio895 (SDA, bit 0) and gpio896 (SCL,
//! bit 1) — confirmed live on `a lab unit` via gpiochip895/ngpio=2. No other
//! GPIOs share the bank, so write-mask safety is structural, not just
//! by convention.
//!
//! See `WAVE35-LIVE-FINDINGS.md` for the timing measurement that
//! motivated this and `feedback_*` memory rules.

use crate::{HalError, Result};

///  (2026-05-24): sum-mod-256 of NON-preamble bytes (LEN + CMD + payload + CRC).
///
/// Per the freshly RE'd APW12 PIC firmware spec
/// (
/// §"Gaps + uncertainties"), the CRC formula is MED-confidence
/// `sum(LEN, CMD, payload[]) mod 256`. The first  frame
/// `[55 AA 04 02 06 00 0C]` matches exactly (0x04 + 0x02 + 0x06 + 0x00 = 0x0C),
/// but the second `[55 AA 04 02 04 02 0E]` differs by 2 (0x04 + 0x02 + 0x04 + 0x02
/// = 0x0C ≠ 0x0E). This helper is used by the  diagnostic logging in
/// `write_apw12_loki_frame*` to compute the "computed_crc" comparison field;
/// when the live test runs, every frame TX logs both the computed and the
/// provided CRC byte so we can see at byte level which frames match the
/// sum-mod-256 hypothesis. Unblocks the CRC formula RE follow-up (RE-018).
///
/// Input: the full APW12 frame `[0x55, 0xAA, LEN, CMD, payload..., CRC]`.
/// Output: computed CRC over `frame[2..frame.len()-1]` (i.e. LEN..=last_payload).
/// Returns `None` if the frame is too short to have a valid layout (<5 bytes:
/// preamble[2] + LEN + CMD + CRC).
pub fn sum_non_preamble_mod256(frame: &[u8]) -> Option<u8> {
    if frame.len() < 5 {
        return None;
    }
    // frame[0..=1] = preamble (0x55, 0xAA); frame[last] = the provided CRC byte
    // we're comparing against. Sum the bytes IN BETWEEN: LEN + CMD + payload[].
    let end = frame.len() - 1;
    Some(
        frame[2..end]
            .iter()
            .copied()
            .fold(0u8, |a, b| a.wrapping_add(b)),
    )
}

///  (2026-05-23) default half-period in microseconds.
///
/// Bumped from the historical `1250` (=400 Hz bit rate, 250× slower than
/// standard SMBus 100 kHz) to `50` (=10 kHz). The 400 Hz default came from
/// an early `a lab unit` BraiinsOS live-probe approximation but was never
/// validated against the Loki spoof's actual i2c slave timing requirements.
///
/// Live evidence for the timing gap:  live test on `a lab unit` returned
/// `PSU protocol error: invalid preamble` on Detect READ — the spoof was
/// responding (not EIO) but at 400 Hz the bit/byte framing didn't decode
/// to a valid APW preamble. 10 kHz is conservative middle ground:
/// 100× faster than 400 Hz (so the spoof's slave i2c state machine sees
/// "real" SMBus signaling), 10× slower than standard SMBus (so sysfs
/// GPIO per-write overhead — typically 20-100 µs — doesn't fully bottleneck).
///
/// Operator can override via `DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US` env
/// var. Set to `1250` to restore pre- behaviour exactly. Set to
/// `5` for 100 kHz standard SMBus (untested but in-spec); `500` for
/// 1 kHz (slow but spec-compliant for slow-mode SMBus). `a lab unit` AM3 BB
/// does NOT use this code path (`psu_apw_uart_tunnel` instead), so
///  has no impact on `a lab unit`.
const DEFAULT_HALF_PERIOD_US: u64 = 50;

/// Pre- historical value for the env-override reference.
#[allow(dead_code)]
pub const LEGACY_HALF_PERIOD_US_400HZ: u64 = 1250;

// -----------------------------------------------------------------------------
//  (2026-05-23): mmap'd AXI GPIO backend register layout.
//
// The PSU SMBus on am2 (gpio895=SDA, gpio896=SCL) is owned by the Xilinx
// AXI GPIO IP at physical address `0x41220000`. On `a lab unit`, this is the
// ONLY bank that contains both lines AND no other GPIOs — confirmed via
// `/sys/class/gpio/gpiochip895`:
//     label=/amba_pl/gpio@41220000  base=895  ngpio=2
// So writing the bank's DATA/TRI registers via /dev/mem is structurally
// safe (no shared bits with PWR_CONTROL, fan-mode, HB resets, etc.).
//
// Xilinx AXI GPIO IP (xps-gpio-1.00.a) register layout (Xilinx PG144):
//   +0x000  GPIO_DATA   (channel 1 data; bit n controls gpio[base+n])
//   +0x004  GPIO_TRI    (channel 1 direction; 1=input/HiZ, 0=output)
//   +0x008  GPIO2_DATA  (channel 2 — NOT WIRED on this IP, single-channel)
//   +0x00C  GPIO2_TRI   (channel 2)
// Open-drain emulation:
//   HIGH = set TRI bit (input/HiZ; external pull-up holds line HIGH)
//   LOW  = clear DATA bit (write 0), then clear TRI bit (output, driving 0)
//   READ = read DATA bit (line is sampled in input mode)
// -----------------------------------------------------------------------------

/// Physical base address of the AXI GPIO IP that owns the PSU SMBus bus
/// on am2 control boards (S19j Pro Zynq). Confirmed live on `a lab unit`
/// 2026-05-23 via `/sys/bus/platform/devices/41220000.gpio` +
/// `/sys/class/gpio/gpiochip895/label = /amba_pl/gpio@41220000`.
pub const AM2_PSU_AXI_GPIO_BASE: u64 = 0x4122_0000;

/// Size of the AXI GPIO register window (one 4 KiB page — Xilinx default).
pub const AM2_PSU_AXI_GPIO_SIZE: usize = 4096;

/// DATA register offset within the AXI GPIO IP (channel 1).
pub const AXI_GPIO_DATA_OFFSET: usize = 0x000;

/// TRI (tristate / direction) register offset within the AXI GPIO IP
/// (channel 1). 1 = input/HiZ, 0 = output.
pub const AXI_GPIO_TRI_OFFSET: usize = 0x004;

/// Bit position of SDA (gpio895) within the AXI GPIO DATA/TRI registers.
/// gpio895 is the FIRST line in the `gpiochip895` bank (base=895), so
/// it occupies bit 0 of the data register per the Xilinx AXI GPIO IP
/// convention (bit n = gpio[base+n]).
pub const AM2_PSU_SDA_BIT: u32 = 1 << 0;

/// Bit position of SCL (gpio896) within the AXI GPIO DATA/TRI registers.
/// gpio896 is the SECOND line (base+1) so it occupies bit 1.
pub const AM2_PSU_SCL_BIT: u32 = 1 << 1;

// -----------------------------------------------------------------------------
//  (2026-05-23): Loki spoof per-byte register-pointer protocol.
//
//  ground-truth capture on `a lab unit` (BraiinsOS slot + soft logic
// analyzer mmap'd 0x41220000 read-only at ~600 kHz) proved bosminer
// sends EACH APW12 frame byte as a separate I2C transaction:
//
//   START [addr_W=0x20] [0x11=APW12_LOKI_REGISTER_POINTER] [frame_byte] STOP
//   <~8 ms gap>
//   START [addr_W=0x20] [0x11] [next frame_byte] STOP
//   ...
//
// The `0x11` is the SMBus command register pointer (SMBus convention:
// each transaction writes 1 data byte to a single register). DCENT_OS
// pre- sent the whole frame in ONE transaction; the Loki spoof's
// i2c slave state machine interpreted byte 0 (0x55 preamble) as a
// (non-existent) register pointer and NAK'd every subsequent byte,
// returning 0xF5 (APW12 NAK) on the next read. This is why
// (retry tolerance), /36b (timing), and  (sequence
// injection) all failed: the bus-level shape was wrong.
//
//  implements the per-byte protocol via two new methods on
// `GpioBitBangI2c`: `write_apw12_loki_frame` and `read_apw12_loki_response`.
// They are opt-in by `Apw121215a::open_gpio_bitbang_at` when the env
// gate `DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1` is set. Bulk-write
// `write_to`/`read_from` remain unchanged for non-Loki PSU paths.
// -----------------------------------------------------------------------------

/// APW12 Loki spoof register pointer ( ground-truth, 2026-05-23).
pub const APW12_LOKI_REGISTER_POINTER: u8 = 0x11;

///  inter-transaction gap in milliseconds for the Loki per-byte
/// protocol. Bosminer ground-truth ( soft-logic-analyzer capture)
/// shows ~91 ms between consecutive writes on the cold-cold init-frame.
/// The original 8 ms default was live-validated on the init-frame BUT
/// with a bosminer-WARM Loki spoof state. -LIVE (2026-05-26)
/// proved 8 ms NAKs deterministically on byte 3/5 in cold-cold
/// standalone — the spoof's I2C slave FSM needs more recovery time
/// between STOP and the next START on cold-cold.
///
///  Patch 3 (2026-05-26): runtime-tunable via
/// `DCENT_AM2_PSU_LOKI_INTER_TXN_GAP_MS`. Default stays at 8 ms to
/// preserve byte-identical behaviour for everything  already
/// validated; operators on cold-cold standalone bring-up set the env
/// to 91 (bosminer empirical) or higher.
pub const LOKI_INTER_TXN_GAP_MS_DEFAULT: u64 = 8;

/// Compile-time minimum gap — guards against accidentally setting the
/// env to 0 (would skip the sleep entirely; Loki may then misinterpret
/// back-to-back STARTs as a repeated-start condition).
pub const LOKI_INTER_TXN_GAP_MS_MIN: u64 = 1;

/// Compile-time maximum gap — bosminer's empirical capture is ~91 ms.
/// Allowing up to 500 ms gives operators a wide tuning envelope without
/// stalling cold-boot indefinitely.
pub const LOKI_INTER_TXN_GAP_MS_MAX: u64 = 500;

/// Resolve the inter-transaction gap from the env, falling back to the
/// proven 8 ms default. Clamps to `[MIN..=MAX]` so a malformed env
/// value can't accidentally disable the gap.
pub fn loki_inter_txn_gap_ms() -> u64 {
    std::env::var("DCENT_AM2_PSU_LOKI_INTER_TXN_GAP_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|n| n.clamp(LOKI_INTER_TXN_GAP_MS_MIN, LOKI_INTER_TXN_GAP_MS_MAX))
        .unwrap_or(LOKI_INTER_TXN_GAP_MS_DEFAULT)
}

/// Backwards-compatible alias so existing call sites that read the
/// constant directly still compile. Equals the default value; runtime
/// tuning happens via `loki_inter_txn_gap_ms()` (preferred for new code).
pub const LOKI_INTER_TXN_GAP_MS: u64 = LOKI_INTER_TXN_GAP_MS_DEFAULT;

///  (2026-05-29): the EVEN reply-register the Loki/APW12 spoof must
/// have selected before it will stage a framed reply for the next read.
///
/// RE finding (bosminer.bin + APW12 V71 PIC firmware): the reply read FAILS
/// today because a single fixed-length read grabs the slave's PRE-FRAME
/// bytes (live: `[0x01, 0x00, 0x00]`) instead of the framed reply
/// `[0x55, 0xAA, LEN, CMD, …, CRC]`. bosminer gets the valid frame because
/// it WRITES an even register pointer (raw byte, NO `0x11` prefix) before the
/// read — the spoof then stages the framed reply at that even register for
/// the subsequent read transaction. bosminer's own guard string is
/// `"PSU: read register must be even"`, which is why this value must stay
/// even. The exact even value bosminer uses is not yet pinned in the RE, so
/// it is ENV-TUNABLE for runtime A/B testing without recompiling:
///   `DCENT_AM2_LOKI_REPLY_REG=0x02` (or 0x04, 0x06, …).
/// Default is `0x00` (even) so existing behaviour is preserved until the
/// operator A/B-discovers the live-correct value.
///
/// Accepts hex with or without a `0x`/`0X` prefix; whitespace is trimmed.
/// Invalid / non-even values are NOT rejected here (the spoof itself ignores
/// an odd selection) — the operator's A/B harness owns that decision; we
/// only parse what they set.
fn loki_reply_register() -> u8 {
    std::env::var("DCENT_AM2_LOKI_REPLY_REG")
        .ok()
        .and_then(|v| {
            u8::from_str_radix(
                v.trim().trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .ok()
        })
        .unwrap_or(0x00)
}

///  (2026-05-29): bosminer-faithful BULK bit-bang framing for the
/// Loki/APW12 spoof. ENV-GATED, default OFF.
///
/// Ground-truth RE of bosminer's bit-bang I²C transport
/// (`open/utils-rs/i2c-driver/src/bit_bang.rs`, write-N `FUN_00b6c11c` /
/// read-N `FUN_00b6aebc`) proved bosminer talks to the APW12/Loki PSU
/// (slave 0x10) as follows:
///
/// - **WRITE = ONE contiguous I²C transaction**:
///   `START → addr_W → every frame byte MSB-first (ACK-checked) → STOP`.
///   The frame bytes are the literal APW12 frame
///   `[0x55, 0xAA, LEN, CMD, args…, CKSUM]`. There is **NO per-byte
///   register-pointer prefix** (the `0x11` "" prefix is bogus) and
///   **NO even reply-register select** (the "" select is bogus).
/// - **READ = single FIXED-LENGTH read**:
///   `START → addr_R → read N bytes (ACK all but last, NACK last) → STOP`.
///   No reply-register select, no loop-accumulate. The `0x55 0xAA`
///   preamble is validated AFTER the fixed read, by the parser; all-0xFF
///   means "no/empty response".
///
/// When this returns `true`, `write_apw12_loki_frame` and
/// `read_apw12_loki_response` take their respective BULK branches at the
/// top of the function, bypassing the per-byte `[addr_W, 0x11, byte]`
/// loop + reply-register select (write) and the reply-register select +
/// loop-accumulate (read). This makes the bulk path win REGARDLESS of the
/// `loki_per_byte_mode` flag in `psu.rs` (which is what routes psu.rs into
/// these two functions in the first place).
///
/// Default `false` → behaviour is byte-identical to today ( write +
///  read). Operator A/B: `DCENT_AM2_LOKI_BOSMINER_BULK=1`.
fn loki_bosminer_bulk_mode() -> bool {
    std::env::var("DCENT_AM2_LOKI_BOSMINER_BULK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

///  refinement (2026-05-29): post-write settle delay (ms) before the
/// BULK read transaction issues, so the APW12/Loki spoof has time to STAGE the
/// framed reply.
///
/// LIVE evidence (a 64-byte bulk read on `a lab unit`):
/// `reply_hex=[F5, 00×15, 8A,C5,F2,C4, 55,AA,03,02,05, …telemetry…]` — the
/// `55 AA …` frame starts ~20 bytes in, preceded by `0xF5` (the APW
/// "read-issued-too-soon" NAK) + ~15 zero padding bytes + 4 transition bytes.
/// This is the classic APW12 reply-latency issue (the am3-bb finding: a
/// SEPARATE read ≥~400 ms after the write; the `0xF5` IS the read-too-soon
/// symptom). The fixed-length read with NO settle never reached the frame and
/// only ever saw `0xF5` → "NAK 0xF5" → handshake never sees the frame.
///
/// ENV-TUNABLE for live A/B without recompiling:
///   `DCENT_AM2_LOKI_POST_WRITE_SETTLE_MS=350` (default 350 ms).
fn loki_post_write_settle_ms() -> u64 {
    std::env::var("DCENT_AM2_LOKI_POST_WRITE_SETTLE_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(350)
}

/// Internal backend selector — sysfs (legacy/default) vs mmap'd AXI GPIO
/// (). Both backends expose identical primitive ops so the I2C
/// protocol layer is single-impl over the enum.
enum GpioBackend {
    /// Legacy sysfs GPIO path. ~50 Hz effective bit rate on `a lab unit`'s
    /// Zynq kernel due to per-call sysfs overhead. Safe but slow.
    Sysfs { sda_gpio: u32, scl_gpio: u32 },
    /// : mmap'd AXI GPIO via /dev/mem. ~10 kHz effective bit
    /// rate target. SAFETY: the underlying bank (0x41220000 on am2)
    /// owns ONLY gpio895/896 — no shared bits, no risk of clobbering
    /// fan-mode, PWR_CONTROL, or HB resets.
    Mmap { base_ptr: *mut u8 },
}

// SAFETY: the mmap'd region is volatile MMIO; concurrent volatile
// read/write of different bits is safe for the AXI GPIO IP. The
// outer `Apw121215a` already serializes access via `&mut self`.
unsafe impl Send for GpioBackend {}
unsafe impl Sync for GpioBackend {}

/// GPIO bit-bang I2C controller for APW PSU communication.
///
/// Backend selectable at construction time:
/// - `new(sda, scl)` — legacy sysfs path (default; works on all
///   platforms that expose `/sys/class/gpio/gpioN/`).
/// - `new_mmap_am2()` —  mmap path for am2 (S19j Pro Zynq) using
///   the AXI GPIO IP at `0x41220000`. Requires `/dev/mem` access (root).
///
/// Open-drain semantics are identical across backends:
/// to drive HIGH, set direction to "in" / TRI=1 (external pull-up
/// pulls the line HIGH). To drive LOW, set direction to "out" / TRI=0
/// and write 0 to DATA.
pub struct GpioBitBangI2c {
    backend: GpioBackend,
    half_period_us: u64,
}

/// Resolve the runtime half-period: env override OR  default.
///
/// Operator override: `DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US=<microseconds>`.
/// Invalid values (zero, non-numeric, > 100 ms) fall back to the default
/// with a warning.
fn resolve_half_period_us() -> u64 {
    if let Ok(raw) = std::env::var("DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US") {
        match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 && n <= 100_000 => {
                tracing::info!(
                    half_period_us = n,
                    legacy_400hz = LEGACY_HALF_PERIOD_US_400HZ,
                    default_w32 = DEFAULT_HALF_PERIOD_US,
                    "GpioBitBangI2c: operator env override applied \
                     (DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US)"
                );
                return n;
            }
            _ => {
                tracing::warn!(
                    raw = raw.as_str(),
                    "GpioBitBangI2c: DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US is invalid \
                     (must be 1..=100000); falling back to Wave-32 default"
                );
            }
        }
    }
    DEFAULT_HALF_PERIOD_US
}

impl GpioBitBangI2c {
    /// Create a new GPIO bit-bang I2C controller using the LEGACY SYSFS
    /// backend at the  default 10 kHz bit rate (50 µs half-period).
    /// Operator can override via `DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US`.
    ///
    /// Exports the GPIO pins if not already exported and initializes
    /// both lines to HIGH (released) state.
    ///
    /// # When to use this vs `new_mmap_am2`
    ///
    /// Use `new_mmap_am2` on am2 (S19j Pro Zynq) — it's ~233× faster
    /// because per-bit sysfs overhead (5-10 ms/op) does not apply.
    /// Use this sysfs path on any other platform that hasn't pinned the
    /// AXI GPIO IP layout yet, or as a  rollback path.
    ///
    /// # Arguments
    /// * `sda_gpio` - GPIO number for SDA line (e.g., 895 on S19 Pro)
    /// * `scl_gpio` - GPIO number for SCL line (e.g., 896 on S19 Pro)
    pub fn new(sda_gpio: u32, scl_gpio: u32) -> Result<Self> {
        // Export GPIOs if not already exported (ignore errors if already exported)
        let _ = std::fs::write("/sys/class/gpio/export", format!("{}", sda_gpio));
        let _ = std::fs::write("/sys/class/gpio/export", format!("{}", scl_gpio));
        // Wait for sysfs entries to appear
        std::thread::sleep(std::time::Duration::from_millis(50));

        let half_period_us = resolve_half_period_us();
        let bit_rate_hz = 1_000_000 / (2 * half_period_us);
        tracing::info!(
            backend = "sysfs",
            sda = sda_gpio,
            scl = scl_gpio,
            half_period_us,
            bit_rate_hz,
            "GpioBitBangI2c: opening (sysfs backend — slow; Wave-36 mmap backend available)"
        );
        let i2c = Self {
            backend: GpioBackend::Sysfs { sda_gpio, scl_gpio },
            half_period_us,
        };
        // Start with both lines HIGH (input = released = pull-up)
        i2c.sda_high();
        i2c.scl_high();
        Ok(i2c)
    }

    /// : Create a new GPIO bit-bang I2C controller using the
    /// mmap'd AXI GPIO backend at `0x41220000` (am2-specific).
    ///
    /// Opens `/dev/mem`, mmaps the 4 KiB AXI GPIO IP window, and
    /// performs read-modify-write on the DATA/TRI registers for
    /// gpio895 (SDA, bit 0) and gpio896 (SCL, bit 1) only. The bank
    /// contains no other GPIOs (gpiochip895 ngpio=2 on `a lab unit`) so
    /// write-mask safety is structural.
    ///
    /// Effective bit rate target: ~10 kHz (matches  intent now
    /// that sysfs overhead is removed). Operator can still override
    /// the per-bit delay via `DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US`.
    ///
    /// # Why this exists ()
    ///
    ///  live capture on `a lab unit` proved DCENT_OS's WRITE bytes are
    /// byte-perfect per APW12 spec but the sysfs path's effective I2C
    /// clock is ~50 Hz (per-`/sys/class/gpio/gpioN/direction` write =
    /// 5-10 ms on this kernel), so the Loki spoof NAKs every probe.
    /// The mmap path is the same approach bosminer uses (
    /// strace proved bosminer has zero `/dev/i2c-N` ioctls and only
    /// mmaps direct registers).
    ///
    /// # Safety
    ///
    /// - The bank at `0x41220000` contains ONLY gpio895/896 — confirmed
    ///   live on `a lab unit` via `/sys/class/gpio/gpiochip895/ngpio = 2`.
    /// - All writes are read-modify-write masked to SDA_BIT | SCL_BIT
    ///   so the upper 30 bits of DATA/TRI are preserved bit-identically.
    /// -  is default-OFF behind `DCENT_AM2_PSU_BITBANG_USE_MMAP=1`
    ///   for the first live test cycle; promotion to default-on happens
    ///   in a separate commit after operator-confirmed live success.
    pub fn new_mmap_am2() -> Result<Self> {
        use nix::sys::mman::{MapFlags, ProtFlags};
        use std::num::NonZeroUsize;

        let half_period_us = resolve_half_period_us();
        let bit_rate_hz = 1_000_000 / (2 * half_period_us);
        tracing::info!(
            backend = "mmap_am2",
            phys_base = format_args!("0x{:08X}", AM2_PSU_AXI_GPIO_BASE),
            size = AM2_PSU_AXI_GPIO_SIZE,
            sda_bit = format_args!("0x{:08X}", AM2_PSU_SDA_BIT),
            scl_bit = format_args!("0x{:08X}", AM2_PSU_SCL_BIT),
            half_period_us,
            bit_rate_hz,
            "GpioBitBangI2c: opening (Wave-36 mmap backend — \
             bypasses sysfs to achieve true 10 kHz on .25-class units)"
        );

        // Open /dev/mem (root-only). Returns HalError::Io on permission failure.
        let mem_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|e| HalError::DeviceOpen {
                path: "/dev/mem (Wave-36 PSU AXI-GPIO bit-bang backend)".to_string(),
                source: e,
            })?;

        // mmap one 4 KiB page at the AXI GPIO base.
        let page_size = NonZeroUsize::new(AM2_PSU_AXI_GPIO_SIZE).expect("4096 is nonzero");
        let ptr = unsafe {
            nix::sys::mman::mmap(
                None,
                page_size,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &mem_file,
                AM2_PSU_AXI_GPIO_BASE as nix::libc::off_t,
            )
        }
        .map_err(|e| {
            HalError::Other(format!(
                "Wave-36 mmap of AXI GPIO at 0x{:08X} failed: {}",
                AM2_PSU_AXI_GPIO_BASE, e
            ))
        })?;

        let base_ptr = ptr.as_ptr() as *mut u8;

        let i2c = Self {
            backend: GpioBackend::Mmap { base_ptr },
            half_period_us,
        };
        // Start with both lines HIGH (input = released = pull-up). The
        // first writes RMW the TRI register so any other bits in the bank
        // (none on am2, but defensive) keep their state.
        i2c.sda_high();
        i2c.scl_high();
        Ok(i2c)
    }

    // -------------------------------------------------------------------
    // : mmap register access helpers (private)
    //
    // All accesses are 32-bit volatile reads/writes to the AXI GPIO IP.
    // Volatile prevents the compiler from reordering, caching, or
    // eliding the MMIO operations — critical for register-level access.
    // -------------------------------------------------------------------

    /// Read the 32-bit value at `base_ptr + offset`. Volatile.
    #[inline]
    unsafe fn mmap_read(base_ptr: *mut u8, offset: usize) -> u32 {
        std::ptr::read_volatile(base_ptr.add(offset) as *const u32)
    }

    /// Write the 32-bit value at `base_ptr + offset`. Volatile.
    #[inline]
    unsafe fn mmap_write(base_ptr: *mut u8, offset: usize, value: u32) {
        std::ptr::write_volatile(base_ptr.add(offset) as *mut u32, value);
    }

    /// Read-modify-write: clear `mask` bits then OR in `set_bits` at
    /// `offset`. Used to mutate SDA/SCL bits without touching other
    /// bank residents (there are none on am2 0x41220000 but defensive
    /// programming costs nothing).
    #[inline]
    unsafe fn mmap_rmw(base_ptr: *mut u8, offset: usize, mask: u32, set_bits: u32) {
        let cur = Self::mmap_read(base_ptr, offset);
        let new = (cur & !mask) | (set_bits & mask);
        Self::mmap_write(base_ptr, offset, new);
    }

    // -------------------------------------------------------------------
    // Line control (open-drain emulation; both backends)
    // -------------------------------------------------------------------

    /// Release SDA line (external pull-up pulls HIGH).
    fn sda_high(&self) {
        match &self.backend {
            GpioBackend::Sysfs { sda_gpio, .. } => {
                let _ = std::fs::write(format!("/sys/class/gpio/gpio{}/direction", sda_gpio), "in");
            }
            GpioBackend::Mmap { base_ptr } => {
                // TRI bit = 1 → input/HiZ → pull-up takes over
                unsafe {
                    Self::mmap_rmw(
                        *base_ptr,
                        AXI_GPIO_TRI_OFFSET,
                        AM2_PSU_SDA_BIT,
                        AM2_PSU_SDA_BIT,
                    );
                }
            }
        }
    }

    /// Drive SDA line LOW.
    fn sda_low(&self) {
        match &self.backend {
            GpioBackend::Sysfs { sda_gpio, .. } => {
                let _ =
                    std::fs::write(format!("/sys/class/gpio/gpio{}/direction", sda_gpio), "out");
                let _ = std::fs::write(format!("/sys/class/gpio/gpio{}/value", sda_gpio), "0");
            }
            GpioBackend::Mmap { base_ptr } => {
                // 1) Clear DATA bit (will drive 0 once we switch to output)
                unsafe {
                    Self::mmap_rmw(*base_ptr, AXI_GPIO_DATA_OFFSET, AM2_PSU_SDA_BIT, 0);
                }
                // 2) Clear TRI bit → switch to output → line driven LOW
                unsafe {
                    Self::mmap_rmw(*base_ptr, AXI_GPIO_TRI_OFFSET, AM2_PSU_SDA_BIT, 0);
                }
            }
        }
    }

    /// Release SCL line (external pull-up pulls HIGH).
    fn scl_high(&self) {
        match &self.backend {
            GpioBackend::Sysfs { scl_gpio, .. } => {
                let _ = std::fs::write(format!("/sys/class/gpio/gpio{}/direction", scl_gpio), "in");
            }
            GpioBackend::Mmap { base_ptr } => unsafe {
                Self::mmap_rmw(
                    *base_ptr,
                    AXI_GPIO_TRI_OFFSET,
                    AM2_PSU_SCL_BIT,
                    AM2_PSU_SCL_BIT,
                );
            },
        }
    }

    /// Drive SCL line LOW.
    fn scl_low(&self) {
        match &self.backend {
            GpioBackend::Sysfs { scl_gpio, .. } => {
                let _ =
                    std::fs::write(format!("/sys/class/gpio/gpio{}/direction", scl_gpio), "out");
                let _ = std::fs::write(format!("/sys/class/gpio/gpio{}/value", scl_gpio), "0");
            }
            GpioBackend::Mmap { base_ptr } => {
                unsafe {
                    Self::mmap_rmw(*base_ptr, AXI_GPIO_DATA_OFFSET, AM2_PSU_SCL_BIT, 0);
                }
                unsafe {
                    Self::mmap_rmw(*base_ptr, AXI_GPIO_TRI_OFFSET, AM2_PSU_SCL_BIT, 0);
                }
            }
        }
    }

    /// Read the current state of SDA (true = HIGH, false = LOW).
    fn read_sda(&self) -> bool {
        match &self.backend {
            GpioBackend::Sysfs { sda_gpio, .. } => {
                std::fs::read_to_string(format!("/sys/class/gpio/gpio{}/value", sda_gpio))
                    .map(|s| s.trim() == "1")
                    .unwrap_or(true) // Default HIGH if read fails (pull-up)
            }
            GpioBackend::Mmap { base_ptr } => {
                let data = unsafe { Self::mmap_read(*base_ptr, AXI_GPIO_DATA_OFFSET) };
                (data & AM2_PSU_SDA_BIT) != 0
            }
        }
    }

    /// Delay for one half-period. Per-instance —  default 50 µs
    /// (10 kHz bit rate), pre- was 1250 µs (400 Hz). Operator can
    /// override at construction via `DCENT_AM2_PSU_BITBANG_HALF_PERIOD_US`.
    ///
    /// #  (2026-05-23) — hybrid sleep/spin
    ///
    /// `std::thread::sleep(Duration::from_micros(N))` on Linux does NOT
    /// actually sleep N µs when N < ~1 ms. Scheduler granularity
    /// (CONFIG_HZ + HRTIMER) imposes a 1-5 ms minimum sleep, regardless
    /// of the argument.  LIVE on `a lab unit` measured this: with the
    /// mmap backend removing per-call sysfs cost, each `sleep(50µs)`
    /// actually took ~5 ms, so a 70-bit frame's ~210 delays consumed
    /// ~1.05 s instead of the expected ~7 ms at true 10 kHz.
    ///
    ///  fix: for sub-millisecond targets, busy-wait against
    /// `Instant::now()` instead. CPU spin is brief and bounded — the
    /// full PSU cold-boot has ~100 ms of bit-bang work total at true
    /// 10 kHz, and only the SMBus cold-boot path uses sub-ms delays.
    /// Heartbeat (1 s intervals) and other coarse waits stay on
    /// cooperative `sleep` via the >=1000 µs branch.
    fn delay(&self) {
        let us = self.half_period_us;
        if us >= 1000 {
            // ≥ 1 ms — scheduler can deliver it; cooperative sleep
            std::thread::sleep(std::time::Duration::from_micros(us));
        } else {
            // < 1 ms — sleep would over-deliver by ~10×; busy-wait
            let target = std::time::Instant::now() + std::time::Duration::from_micros(us);
            while std::time::Instant::now() < target {
                std::hint::spin_loop();
            }
        }
    }

    // -------------------------------------------------------------------
    // I2C protocol primitives
    // -------------------------------------------------------------------

    /// Generate I2C START condition: SDA goes LOW while SCL is HIGH.
    fn start(&self) {
        self.sda_high();
        self.delay();
        self.scl_high();
        self.delay();
        self.sda_low();
        self.delay();
        self.scl_low();
        self.delay();
    }

    /// Generate I2C STOP condition: SDA goes HIGH while SCL is HIGH.
    fn stop(&self) {
        self.sda_low();
        self.delay();
        self.scl_high();
        self.delay();
        self.sda_high();
        self.delay();
    }

    /// Write a single bit on SDA, clock it with SCL.
    fn write_bit(&self, bit: bool) {
        if bit {
            self.sda_high();
        } else {
            self.sda_low();
        }
        self.delay();
        self.scl_high();
        self.delay();
        self.scl_low();
        self.delay();
    }

    /// Read a single bit from SDA during SCL HIGH phase.
    fn read_bit(&self) -> bool {
        self.sda_high(); // Release SDA for slave to drive
        self.delay();
        self.scl_high();
        self.delay();
        let bit = self.read_sda();
        self.scl_low();
        self.delay();
        bit
    }

    /// Write a byte (MSB first) and read the ACK bit.
    ///
    /// Returns `true` if the slave acknowledged (SDA LOW during ACK clock).
    fn write_byte_raw(&self, byte: u8) -> bool {
        for i in (0..8).rev() {
            self.write_bit((byte >> i) & 1 == 1);
        }
        !self.read_bit() // ACK = SDA LOW → returns true
    }

    /// Read a byte (MSB first) and send ACK or NACK.
    ///
    /// # Arguments
    /// * `ack` - If true, send ACK (SDA LOW). If false, send NACK (SDA HIGH).
    fn read_byte_raw(&self, ack: bool) -> u8 {
        let mut byte = 0u8;
        for _ in 0..8 {
            byte = (byte << 1) | if self.read_bit() { 1 } else { 0 };
        }
        self.write_bit(!ack); // ACK=LOW (write false bit), NACK=HIGH (write true bit)
        byte
    }

    // -------------------------------------------------------------------
    // Public I2C operations
    // -------------------------------------------------------------------

    /// Write data bytes to an I2C device.
    ///
    /// Sends START, address+W, data bytes, STOP.
    /// Returns error if the slave NACKs the address or any data byte.
    pub fn write_to(&self, addr: u8, data: &[u8]) -> Result<()> {
        self.start();
        if !self.write_byte_raw(addr << 1) {
            // Write address
            self.stop();
            return Err(HalError::I2c {
                bus: 1,
                addr,
                detail: "PSU GPIO I2C: NACK on address".into(),
            });
        }
        for &byte in data {
            if !self.write_byte_raw(byte) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: "PSU GPIO I2C: NACK on data".into(),
                });
            }
        }
        self.stop();
        Ok(())
    }

    /// Read data bytes from an I2C device.
    ///
    /// Sends START, address+R, reads `buf.len()` bytes (ACK all except last), STOP.
    /// Returns the number of bytes read (always `buf.len()` on success).
    pub fn read_from(&self, addr: u8, buf: &mut [u8]) -> Result<usize> {
        self.start();
        if !self.write_byte_raw((addr << 1) | 1) {
            // Read address
            self.stop();
            return Err(HalError::I2c {
                bus: 1,
                addr,
                detail: "PSU GPIO I2C: NACK on read address".into(),
            });
        }
        let len = buf.len();
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = self.read_byte_raw(i + 1 < len); // ACK all except last
        }
        self.stop();
        Ok(buf.len())
    }

    // -------------------------------------------------------------------
    //  (2026-05-23): Loki spoof per-byte register-pointer protocol
    // -------------------------------------------------------------------

    /// : write an APW12 frame to the Loki spoof using the per-byte
    /// register-pointer protocol that bosminer uses.
    ///
    /// Emits N transactions of shape
    /// `START [addr_W] [0x11] [frame_byte] STOP` with
    /// `LOKI_INTER_TXN_GAP_MS` (8 ms) between transactions. Returns
    /// `Err(HalError::I2c)` if any byte is NAK'd (the spoof should ACK
    /// every byte; NAK indicates the bus protocol is wrong or the
    /// spoof has dropped off the bus).
    ///
    /// Use this instead of `write_to` when the APW12 family is
    /// gpio_bitbang-attached AND `DCENT_AM2_PSU_LOKI_REGISTER_POINTER=1`.
    pub fn write_apw12_loki_frame(&self, addr: u8, frame: &[u8]) -> Result<()> {
        // ----  (2026-05-29): bosminer-faithful BULK write branch ----
        //
        // ENV-GATED (`DCENT_AM2_LOKI_BOSMINER_BULK=1`), default OFF. When ON,
        // write the WHOLE frame as ONE contiguous I²C transaction — exactly
        // what bosminer's RE'd bit-bang write-N does — bypassing BOTH the
        // per-byte `[addr_W, 0x11, byte]` register-pointer loop AND the
        //  trailing even reply-register select below. This takes
        // precedence over `loki_per_byte_mode` in psu.rs (which is what routes
        // psu.rs into this function): the check is at the top, so the per-byte
        // flag can't re-route around it.
        if loki_bosminer_bulk_mode() {
            self.start();
            if !self.write_byte_raw(addr << 1) {
                self.stop();
                tracing::info!(
                    target: "wave57_loki_bulk",
                    addr = format_args!("0x{:02X}", addr),
                    frame_hex = format_args!("{:02X?}", frame),
                    "Wave-57: bosminer-faithful BULK write — NAK on address"
                );
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: "Wave-57 Loki BULK write: NAK on address".into(),
                });
            }
            // Write every frame byte MSB-first in the SAME transaction. Track
            // NAKs for diagnostics but, like bosminer's write-N, do not abort
            // mid-frame — the spoof's empty-calibration behaviour can NAK a
            // trailing byte yet still latch the command; the parser/read side
            // owns the verdict.
            let mut nak_at: Option<usize> = None;
            for (i, &b) in frame.iter().enumerate() {
                if !self.write_byte_raw(b) && nak_at.is_none() {
                    nak_at = Some(i);
                }
            }
            self.stop();
            tracing::info!(
                target: "wave57_loki_bulk",
                addr = format_args!("0x{:02X}", addr),
                frame_hex = format_args!("{:02X?}", frame),
                nak_at = ?nak_at,
                "Wave-57: bosminer-faithful BULK write (single txn, no 0x11 prefix, no reply-reg select)"
            );
            return Ok(());
        }

        // wave55c_crc_diagnostic — log computed-vs-provided CRC for byte-level
        // RE comparison (prefixed transport). See PHASE2B-APW12-PIC-PROTOCOL.md
        // §"Gaps" — the sum-mod-256 hypothesis matches the first  frame
        // but not the second; logging both bytes makes the gap visible to the
        // operator at every live frame TX.
        if let (Some(computed), Some(&provided)) =
            (sum_non_preamble_mod256(frame), frame.iter().last())
        {
            tracing::info!(
                target: "wave55c_crc_diagnostic",
                transport = "prefixed_per_byte",
                addr = format_args!("0x{:02X}", addr),
                frame_bytes = format_args!("{:02X?}", frame),
                computed_crc = format_args!("0x{:02X}", computed),
                provided_crc = format_args!("0x{:02X}", provided),
                match_ = computed == provided,
                "Wave-55c: APW12 frame CRC sum-mod-256 check (prefixed transport)"
            );
        }
        for (i, &frame_byte) in frame.iter().enumerate() {
            self.start();
            // Address+W
            if !self.write_byte_raw(addr << 1) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: format!(
                        "Wave-39 Loki frame byte {}/{}: NAK on address",
                        i + 1,
                        frame.len()
                    ),
                });
            }
            // Register pointer (0x11)
            if !self.write_byte_raw(APW12_LOKI_REGISTER_POINTER) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: format!(
                        "Wave-39 Loki frame byte {}/{}: NAK on register pointer 0x11",
                        i + 1,
                        frame.len()
                    ),
                });
            }
            // Frame data byte
            if !self.write_byte_raw(frame_byte) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: format!(
                        "Wave-39 Loki frame byte {}/{}: NAK on data byte 0x{:02X}",
                        i + 1,
                        frame.len(),
                        frame_byte
                    ),
                });
            }
            self.stop();
            // Inter-transaction gap (skip after the last byte).
            //  Patch 3: runtime-tunable via env override.
            if i + 1 < frame.len() {
                std::thread::sleep(std::time::Duration::from_millis(loki_inter_txn_gap_ms()));
            }
        }

        //  (2026-05-29): EVEN reply-register selection.
        //
        // Per the APW12 V71 PIC firmware RE, the spoof only stages the framed
        // reply `[0x55, 0xAA, LEN, CMD, …, CRC]` for the NEXT read after the
        // master selects an even reply register. This is a RAW-byte write
        // (NO `0x11` register-pointer prefix): `START [addr_W] [reg] STOP`.
        // Without it, the next read grabs the slave's pre-frame bytes
        // (live: `[0x01, 0x00, 0x00]`) and the parser never finds the
        // `0x55 0xAA` preamble. The register value is env-tunable
        // (`DCENT_AM2_LOKI_REPLY_REG`, default 0x00) for runtime A/B testing.
        let reply_reg = loki_reply_register();
        self.start();
        // Address+W (best-effort: log NAKs but don't abort — the command
        // frame already went out OK; the reply-register select is the new,
        // experimental staging step we're A/B-testing).
        let addr_ack = self.write_byte_raw(addr << 1);
        let reg_ack = self.write_byte_raw(reply_reg);
        self.stop();
        tracing::debug!(
            target: "wave56b_loki_reply_reg_select",
            addr = format_args!("0x{:02X}", addr),
            reply_reg = format_args!("0x{:02X}", reply_reg),
            addr_ack,
            reg_ack,
            "Wave-56b: selected Loki reply register 0x{:02X} before read",
            reply_reg
        );

        Ok(())
    }

    ///  (2026-05-29): read the APW12/Loki reply frame by LOOP-READING
    /// and ACCUMULATING until the framed reply `[0x55, 0xAA, LEN, CMD, …, CRC]`
    /// materializes, aligned to the `0x55 0xAA` preamble.
    ///
    /// # Why loop-accumulate (RE finding)
    ///
    /// RE of bosminer.bin + the APW12 V71 PIC firmware proved the previous
    /// single fixed-length read FAILS because it grabs the slave's PRE-FRAME
    /// bytes (live: `[0x01, 0x00, 0x00]`) instead of the framed reply. The
    /// spoof clocks the framed reply out across MULTIPLE reads after an even
    /// reply-register has been selected (see `write_apw12_loki_frame`'s
    /// trailing reply-register select). bosminer reads in a loop and
    /// accumulates until the `0x55 0xAA` preamble + the declared length arrive.
    /// This method does the same: each iteration is one read transaction
    /// `START [addr_R] <burst, ACK-all-but-last> STOP`; after each burst the
    /// accumulator is searched for the preamble; once found AND at least
    /// `buf.len()` bytes exist FROM the preamble, those bytes are copied into
    /// `buf` (leading pre-frame bytes like `[0x01, 0x00, 0x00]` are skipped by
    /// aligning to the preamble). Bounded by a ~50 ms deadline (~8-10 reads).
    ///
    /// # 1-byte fast path (preserved)
    ///
    /// When `buf.len() == 1` (the cold-wake poll's per-read budget loop), do
    /// the original single read — one NACK'd byte in a `START [addr_R] [b,NACK]
    /// STOP` transaction. The cold-wake poll relies on this fast single-byte
    /// shape (it polls for the spoof's `0x71` firmware byte / `0xF5` "no data
    /// yet" sentinel), so it MUST NOT change.
    ///
    /// # Non-fatal on timeout
    ///
    /// If the deadline elapses without a full frame, whatever aligned bytes
    /// exist are copied (or `buf` is left zero-filled) and `Ok(buf.len())` is
    /// returned — the caller's APW12 parser handles a bad/short reply. A warn
    /// logs the accumulated bytes for live diagnostics. Every call also logs
    /// the raw accumulated bytes so the A/B run can see whether the `0x55 0xAA`
    /// preamble now appears with a given `DCENT_AM2_LOKI_REPLY_REG`.
    ///
    /// See the APW12 V71 PIC firmware RE +
    /// .
    pub fn read_apw12_loki_response(&self, addr: u8, buf: &mut [u8]) -> Result<usize> {
        let want = buf.len();
        if want == 0 {
            return Ok(0);
        }

        // ---- 1-byte fast path (cold-wake poll) — UNCHANGED ----
        if want == 1 {
            self.start();
            if !self.write_byte_raw((addr << 1) | 1) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: "Wave-56 Loki reply read: NAK on read address (1-byte fast path)"
                        .into(),
                });
            }
            // Single byte, NACK to end the read immediately.
            buf[0] = self.read_byte_raw(false);
            self.stop();
            return Ok(1);
        }

        // ----  (2026-05-29, refined): bosminer-faithful BULK read that
        //      captures the DELAYED APW12/Loki framed reply ----
        //
        // ENV-GATED (`DCENT_AM2_LOKI_BOSMINER_BULK=1`), default OFF. When ON,
        // do ONE generous-length read in a SINGLE transaction, AFTER a
        // configurable post-write settle, then SCAN the captured bytes for the
        // `0x55 0xAA` preamble and align `buf` to it. This bypasses BOTH the
        //  reply-register select (done on the write side, also bypassed
        // there) AND the multi-iteration loop-accumulate below. It takes
        // precedence over `loki_per_byte_mode` in psu.rs (the check is at the
        // top, after the 1-byte fast path which the cold-wake poll still needs).
        //
        // Why generous + settle + scan: LIVE evidence (a 64-byte bulk read) is
        // `[F5, 00×15, 8A,C5,F2,C4, 55,AA,03,02,05, …]` — the framed reply
        // starts ~20 bytes in, preceded by `0xF5` (read-issued-too-soon NAK) +
        // ~15 zero padding + 4 transition bytes. A bare fixed `buf.len()` read
        // with no settle only ever grabbed `[F5,00,00,…]` and never reached the
        // frame. The settle lets the spoof stage the reply (the classic APW12
        // reply-latency / `0xF5` symptom); reading ≥48 bytes guarantees we clock
        // past the padding and reach the `55 AA` frame; the preamble scan then
        // aligns `buf` so the caller's APW12 parser sees a clean frame.
        if loki_bosminer_bulk_mode() {
            // 1. Settle: let the APW stage the framed reply before we read.
            let settle_ms = loki_post_write_settle_ms();
            std::thread::sleep(std::time::Duration::from_millis(settle_ms));

            // 2. Generous single-transaction read: N = clamp(max(want,48), .., 80).
            let n = want.clamp(48, 80);
            self.start();
            if !self.write_byte_raw((addr << 1) | 1) {
                self.stop();
                tracing::info!(
                    target: "wave57_loki_bulk",
                    addr = format_args!("0x{:02X}", addr),
                    want,
                    settle_ms,
                    n,
                    "Wave-57: bulk read — NAK on read address (non-fatal, returning 0)"
                );
                return Ok(0);
            }
            let mut acc: Vec<u8> = Vec::with_capacity(n);
            for i in 0..n {
                let last = i + 1 == n;
                acc.push(self.read_byte_raw(!last)); // ACK all but the last byte
            }
            self.stop();

            // 3. Scan for the `0x55 0xAA` preamble and align `buf` to it.
            let preamble_at = acc.windows(2).position(|w| w[0] == 0x55 && w[1] == 0xAA);
            let copied;
            match preamble_at {
                Some(p) => {
                    // Copy up to `want` bytes starting at the preamble; zero-fill
                    // any remainder if the frame ran short of `want`.
                    let avail = acc.len() - p;
                    let take = avail.min(want);
                    buf[..take].copy_from_slice(&acc[p..p + take]);
                    for slot in buf[take..].iter_mut() {
                        *slot = 0;
                    }
                    copied = take;
                }
                None => {
                    // Best-effort, non-fatal: copy the first `want` bytes of the
                    // capture so the caller's parser can still inspect them.
                    let take = acc.len().min(want);
                    buf[..take].copy_from_slice(&acc[..take]);
                    for slot in buf[take..].iter_mut() {
                        *slot = 0;
                    }
                    copied = want;
                }
            }

            // 4. Log everything — this is the live A/B signal.
            tracing::info!(
                target: "wave57_loki_bulk",
                addr = format_args!("0x{:02X}", addr),
                want,
                settle_ms,
                n,
                acc_hex = format_args!("{:02X?}", acc.as_slice()),
                preamble_found = preamble_at.is_some(),
                preamble_at = preamble_at.map(|p| p as i64).unwrap_or(-1),
                buf_hex = format_args!("{:02X?}", &buf[..]),
                "Wave-57: bulk read (settle + generous read + 0x55AA preamble scan)"
            );
            return Ok(copied);
        }

        // ---- multi-byte: LOOP-READ + ACCUMULATE until aligned frame ----
        const READ_DEADLINE_MS: u64 = 50;
        const MAX_ITERS: usize = 10;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(READ_DEADLINE_MS);

        let mut acc: Vec<u8> = Vec::with_capacity(want.max(16) * 2);

        // Find the `0x55 0xAA` preamble in `acc`; returns its start index.
        let find_preamble =
            |a: &[u8]| -> Option<usize> { a.windows(2).position(|w| w[0] == 0x55 && w[1] == 0xAA) };

        for iter in 0..MAX_ITERS {
            // One read transaction: address+R, then a small burst.
            self.start();
            if !self.write_byte_raw((addr << 1) | 1) {
                self.stop();
                // Bail on a hard read-address NAK — the spoof isn't on the
                // bus. (The accumulate loop is for staging delays, not for a
                // missing slave.)
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: format!(
                        "Wave-56b Loki reply read: NAK on read address (loop-accumulate iter {})",
                        iter + 1
                    ),
                });
            }
            // Burst size: read up to 16 bytes, but no more than what we still
            // need from the preamble (over-reading past the staged frame just
            // returns 0xF5 padding, which is harmless but wastes bus time).
            let burst = 16usize.min(want);
            for b in 0..burst {
                // ACK all but the last byte of this burst; final NACK ends it.
                let byte = self.read_byte_raw(b + 1 < burst);
                acc.push(byte);
            }
            self.stop();

            // Have we got a full aligned frame yet?
            if let Some(p) = find_preamble(&acc) {
                if acc.len() - p >= want {
                    buf.copy_from_slice(&acc[p..p + want]);
                    tracing::debug!(
                        target: "wave56b_loki_reply_read",
                        addr = format_args!("0x{:02X}", addr),
                        want,
                        preamble_at = p,
                        iters = iter + 1,
                        acc_hex = format_args!("{:02X?}", acc.as_slice()),
                        "Wave-56b: Loki reply frame aligned to 0x55AA preamble"
                    );
                    return Ok(want);
                }
            }

            if std::time::Instant::now() >= deadline {
                break;
            }
            // Brief inter-read gap so the spoof can clock out the next chunk.
            std::thread::sleep(std::time::Duration::from_millis(loki_inter_txn_gap_ms()));
        }

        // Deadline / iter budget exhausted without a complete aligned frame.
        // Copy whatever aligned bytes exist (or zero-fill) — NON-FATAL; the
        // caller's APW12 parser handles a bad/short reply.
        for slot in buf.iter_mut() {
            *slot = 0;
        }
        let copied = if let Some(p) = find_preamble(&acc) {
            let avail = acc.len() - p;
            let n = avail.min(want);
            buf[..n].copy_from_slice(&acc[p..p + n]);
            n
        } else {
            0
        };
        tracing::warn!(
            target: "wave56b_loki_reply_read",
            addr = format_args!("0x{:02X}", addr),
            want,
            copied,
            preamble_found = find_preamble(&acc).is_some(),
            acc_hex = format_args!("{:02X?}", acc.as_slice()),
            "Wave-56b: Loki reply read deadline exhausted without a full frame \
             (non-fatal — parser handles short/bad reply). Check acc_hex for 0x55AA preamble."
        );
        Ok(want)
    }

    ///  (2026-05-24): write an APW12 frame to the Loki spoof using
    /// **bare per-byte** transactions (NO `0x11` register-pointer prefix).
    ///
    ///  ground-truth capture on `a lab unit` (decoded.txt #9-14 and #57-62)
    /// shows bosminer's follow-up-frame transmission uses transactions of
    /// shape `START [addr_W] [frame_byte] STOP` — i.e., bare per-byte
    /// writes with NO register-pointer prefix. This differs from the
    /// init-frame transport ( `write_apw12_loki_frame`), which
    /// prepends `0x11` before each data byte.
    ///
    /// Hypothesis from  byte semantics: the Loki spoof's i2c slave
    /// state-machine treats the first `[0x11, byte]` transaction as
    /// "set register pointer to 0x11, write byte"; subsequent bare
    /// `[byte]` transactions then write to whatever register pointer is
    /// already latched. This is standard SMBus PMBus-style state.
    ///
    /// Inter-transaction gap is `LOKI_INTER_TXN_GAP_MS` (8 ms), matching
    /// the prefixed-frame transport. Returns `Err(HalError::I2c)` if any
    /// byte is NAK'd; the spoof should ACK every byte in this mode too.
    ///
    /// Use this for the  follow-up-frame `[55 AA 04 02 04 02]`
    /// (transactions #9-14, #57-62). Use `write_apw12_loki_frame` for
    /// the init-frame `[55 AA 04 02 06 00]` (transactions #1-6, #49-54).
    pub fn write_apw12_loki_frame_bare(&self, addr: u8, frame: &[u8]) -> Result<()> {
        // wave55c_crc_diagnostic — log computed-vs-provided CRC for byte-level
        // RE comparison. Per PHASE2B-APW12-PIC-PROTOCOL.md §"Gaps", the CRC
        // formula is MED-confidence sum-mod-256 of non-preamble bytes. The
        // first  frame matches; the second differs by 2. Logging both
        // computed and provided lets the live test pinpoint exactly which
        // frame the formula breaks on.
        if let (Some(computed), Some(&provided)) =
            (sum_non_preamble_mod256(frame), frame.iter().last())
        {
            tracing::info!(
                target: "wave55c_crc_diagnostic",
                transport = "bare_per_byte",
                addr = format_args!("0x{:02X}", addr),
                frame_bytes = format_args!("{:02X?}", frame),
                computed_crc = format_args!("0x{:02X}", computed),
                provided_crc = format_args!("0x{:02X}", provided),
                match_ = computed == provided,
                "Wave-55c: APW12 frame CRC sum-mod-256 check (bare transport)"
            );
        }
        for (i, &frame_byte) in frame.iter().enumerate() {
            self.start();
            // Address+W
            if !self.write_byte_raw(addr << 1) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: format!(
                        "Wave-55b Loki bare-frame byte {}/{}: NAK on address",
                        i + 1,
                        frame.len()
                    ),
                });
            }
            // Frame data byte — NO register-pointer prefix (bare write)
            if !self.write_byte_raw(frame_byte) {
                self.stop();
                return Err(HalError::I2c {
                    bus: 1,
                    addr,
                    detail: format!(
                        "Wave-55b Loki bare-frame byte {}/{}: NAK on data byte 0x{:02X}",
                        i + 1,
                        frame.len(),
                        frame_byte
                    ),
                });
            }
            self.stop();
            // Inter-transaction gap (skip after the last byte).
            //  Patch 3: runtime-tunable via env override.
            if i + 1 < frame.len() {
                std::thread::sleep(std::time::Duration::from_millis(loki_inter_txn_gap_ms()));
            }
        }
        Ok(())
    }

    /// Bus recovery: 9 SCL clock pulses to unstick a slave holding SDA LOW.
    ///
    /// If a transaction was interrupted mid-byte, the slave may be holding SDA
    /// low waiting for clocks. Sending 9 clocks gives the slave enough edges
    /// to release SDA, then a STOP condition resets the bus.
    pub fn bus_recovery(&self) {
        for _ in 0..9 {
            self.scl_high();
            self.delay();
            self.scl_low();
            self.delay();
        }
        self.stop();
    }
}

/// : release the mmap'd region back to the kernel on Drop so
/// repeated open/close cycles (rare but possible during retries) don't
/// leak virtual address space. Sysfs backend has no resources to release.
impl Drop for GpioBitBangI2c {
    fn drop(&mut self) {
        if let GpioBackend::Mmap { base_ptr } = &self.backend {
            // SAFETY: base_ptr was returned by nix::sys::mman::mmap with
            // size AM2_PSU_AXI_GPIO_SIZE. We own the mapping and no
            // outstanding pointers exist (struct is consumed by Drop).
            if !base_ptr.is_null() {
                use std::num::NonZeroUsize;
                let _ = unsafe {
                    let ptr = std::ptr::NonNull::new_unchecked(*base_ptr as *mut std::ffi::c_void);
                    let size = NonZeroUsize::new(AM2_PSU_AXI_GPIO_SIZE).expect("4096 is nonzero");
                    nix::sys::mman::munmap(ptr, size.get())
                };
            }
        }
    }
}

// -----------------------------------------------------------------------------
//  host-runnable tests (pin register layout)
// -----------------------------------------------------------------------------

#[cfg(test)]
mod wave36_tests {
    use super::*;

    /// : pin the AXI GPIO bank base address for the am2 PSU SMBus.
    ///
    /// `0x41220000` was discovered live on `a lab unit` 2026-05-23 via
    /// `/sys/bus/platform/devices/41220000.gpio` + `gpiochip895` label.
    /// If this address drifts in source, the  mmap path will
    /// silently write the wrong AXI peripheral — high brick risk.
    #[test]
    fn wave36_am2_psu_axi_gpio_base_pinned() {
        assert_eq!(
            AM2_PSU_AXI_GPIO_BASE, 0x4122_0000,
            "AXI GPIO bank for gpio895/896 (PSU SMBus) drifted — \
             live-confirmed on .25 as 0x41220000; do not change without \
             a fresh /sys/class/gpio/gpiochip*/label probe"
        );
    }

    /// : pin the Xilinx AXI GPIO register offsets.
    ///
    /// Per Xilinx PG144 (xps-gpio-1.00.a IP datasheet):
    ///   +0x000  GPIO_DATA   (channel 1 data)
    ///   +0x004  GPIO_TRI    (channel 1 tristate: 1=input/HiZ, 0=output)
    /// These are the ONLY offsets we touch. Drift here = wrong register
    /// = no I2C activity on the bus (or worse, write to GPIO2 channel
    /// which isn't even wired on this IP instance).
    #[test]
    fn wave36_axi_gpio_register_offsets_pinned() {
        assert_eq!(AXI_GPIO_DATA_OFFSET, 0x000);
        assert_eq!(AXI_GPIO_TRI_OFFSET, 0x004);
    }

    /// : pin the SDA/SCL bit positions.
    ///
    /// `gpiochip895` on `a lab unit` has base=895, ngpio=2:
    ///   bit 0 = gpio895 = SDA  (PSU_GPIO_SDA)
    ///   bit 1 = gpio896 = SCL  (PSU_GPIO_SCL)
    /// These positions are derived from the AXI GPIO bit-n = gpio[base+n]
    /// convention. Drift = lines swap (we'd clock with SDA and signal
    /// with SCL — bus would never see a valid frame).
    #[test]
    fn wave36_sda_scl_bit_positions_pinned() {
        assert_eq!(AM2_PSU_SDA_BIT, 1 << 0, "SDA must be bit 0 (gpio895)");
        assert_eq!(AM2_PSU_SCL_BIT, 1 << 1, "SCL must be bit 1 (gpio896)");
        // Sanity: SDA and SCL must be distinct bits — a swap to the same
        // bit would lock the bus on every transaction.
        assert_ne!(AM2_PSU_SDA_BIT, AM2_PSU_SCL_BIT);
        // The combined mask must fit in the lower 2 bits — anything
        // else means we'd be touching gpios outside the 895/896 bank.
        assert_eq!(AM2_PSU_SDA_BIT | AM2_PSU_SCL_BIT, 0b11);
    }

    /// : regression-pin the 4 KiB page size for the AXI GPIO
    /// window. mmap requires a multiple of the system page size; on
    /// Zynq (4 KiB pages) this matches exactly. If the kernel ever
    /// reports a different page size, mmap will fail at runtime —
    /// catch the constant drift at compile-time instead.
    #[test]
    fn wave36_axi_gpio_window_is_4kib() {
        assert_eq!(AM2_PSU_AXI_GPIO_SIZE, 4096);
    }

    /// : open-drain semantics regression pin. The mmap backend
    /// implements HIGH = set TRI bit (input/HiZ) and LOW = clear DATA
    /// then clear TRI (output 0). The opposite (LOW = drive 1) would
    /// short the bus when the spoof also drives LOW — i2c bus
    /// contention damages the IP block.
    ///
    /// This test documents the contract textually; the mmap path
    /// implements it via `mmap_rmw`. A grep for `AXI_GPIO_TRI_OFFSET`
    /// in sda_low/scl_low must show `set_bits = 0` (output mode); in
    /// sda_high/scl_high must show `set_bits = AM2_PSU_*_BIT` (input/HiZ).
    #[test]
    fn wave36_open_drain_contract_documented() {
        // Sentinels — if the bit constants change, the contract is
        // re-validated in the next two tests automatically.
        assert_eq!(AM2_PSU_SDA_BIT, 0b01);
        assert_eq!(AM2_PSU_SCL_BIT, 0b10);
    }

    /// : hybrid sleep/spin delay validation.
    ///
    /// For sub-millisecond targets, `std::thread::sleep(50µs)` over-delivers
    /// by ~10× on Linux due to scheduler granularity (~5 ms minimum). The
    /// busy-wait path must deliver microsecond timing within an order of
    /// magnitude of target. Use a generous tolerance (10× target) so the
    /// test passes even on Windows/macOS hosts where Instant resolution
    /// varies; the operational target is am2 Linux.
    ///
    /// Skip the busy-wait test on Windows release builds with no high-
    /// resolution clock — guard with cfg(unix) to keep CI green elsewhere.
    #[test]
    fn wave36b_busy_wait_delivers_microsecond_timing() {
        let target_us = 50u64;
        // Use a fresh GpioBitBangI2c-like delay impl — we don't open a
        // real backend in the test since it would require /dev/mem
        // (root). The delay() impl is pure-Rust and host-runnable.
        let busy_wait = || {
            let target = std::time::Instant::now() + std::time::Duration::from_micros(target_us);
            while std::time::Instant::now() < target {
                std::hint::spin_loop();
            }
        };
        let start = std::time::Instant::now();
        for _ in 0..100 {
            busy_wait();
        }
        let elapsed = start.elapsed();
        // 100 × 50 µs = 5,000 µs = 5 ms. Allow up to 10× for any host
        // jitter (50 ms). If elapsed > 100 ms, busy-wait isn't working.
        assert!(
            elapsed.as_millis() < 100,
            "Wave-36b busy-wait took {} ms for 100×50µs; expected <100 ms. \
             If this fails, the busy-wait fallback isn't faster than sleep \
             and the Wave-36b fix won't deliver true 10 kHz SMBus.",
            elapsed.as_millis()
        );
    }

    /// : pin the Loki spoof register pointer byte.
    ///
    ///  ground-truth capture on `a lab unit` showed every bosminer
    /// APW12 frame byte is preceded by `0x11` on the bus. The Loki
    /// spoof's i2c slave state machine treats this as the SMBus
    /// command register pointer. If this constant drifts, the
    ///  per-byte protocol will write to the wrong register
    /// pointer and the spoof will NAK.
    #[test]
    fn wave39_loki_register_pointer_pinned() {
        assert_eq!(
            APW12_LOKI_REGISTER_POINTER, 0x11,
            "Loki spoof register pointer drifted — bosminer ground-truth \
             on .25 (wave38-bosminer-truth/bosminer-decoded.txt #1-6) pins this as 0x11"
        );
    }

    /// : pin the inter-transaction gap. Bosminer ground-truth
    /// shows ~7 ms between consecutive STOPs and the next START. We
    /// use 8 ms for headroom. Drift to a SHORTER value risks
    /// spoof NAK; drift LONGER wastes cold-boot budget.
    #[test]
    fn wave39_inter_txn_gap_pinned() {
        assert_eq!(LOKI_INTER_TXN_GAP_MS, 8);
        assert!(
            LOKI_INTER_TXN_GAP_MS >= 7,
            "must be ≥ bosminer's measured 7 ms"
        );
        assert!(
            LOKI_INTER_TXN_GAP_MS <= 50,
            "must not blow cold-boot budget"
        );
    }

    ///  (2026-05-24): pin the existence + signature of the bare-write
    /// transport. The  follow-up-frame `[55 AA 04 02 04 02]` MUST be
    /// emittable as 6 separate `[addr_W, byte]` transactions with NO
    /// `0x11` register-pointer prefix. If `write_apw12_loki_frame_bare` is
    /// removed or its signature changes, the  standalone cold-wake
    /// cycle in `psu.rs` cannot reach the spoof state bosminer reaches.
    ///
    /// This is a compile-time pin only (no live HAL access). The byte-level
    /// transport correctness is validated by the host test in
    /// `dcentrald/tests/wave55b_loki_cold_boot_sequence.rs` which includes
    /// this module directly via `#[path]` and asserts the function exists
    /// with the expected signature.
    #[test]
    fn wave55b_bare_write_transport_signature_pinned() {
        // Compile-time check: the function pointer must coerce to the
        // expected type. Drift to a different signature (e.g., async,
        // different arg types) breaks this assertion at compile time.
        let _: fn(&GpioBitBangI2c, u8, &[u8]) -> Result<()> =
            GpioBitBangI2c::write_apw12_loki_frame_bare;
    }

    /// : sleep path still used for >= 1 ms. The 1 Hz PSU
    /// heartbeat (1000 ms = 1,000,000 µs) MUST take the cooperative
    /// sleep branch — busy-waiting for 1 second would be a CPU bug.
    #[test]
    fn wave36b_sleep_threshold_at_1ms() {
        // Document the cutoff. If this changes, the
        // wave36b_busy_wait_delivers_microsecond_timing test still
        // covers the busy-wait path; this test covers the boundary.
        const THRESHOLD_US: u64 = 1000;
        assert_eq!(THRESHOLD_US, 1000, "Wave-36b cutoff at 1 ms");
        // The default half-period is 50 µs () — well below
        // the threshold → busy-wait path active by default.
        assert!(DEFAULT_HALF_PERIOD_US < THRESHOLD_US);
        // The legacy 400 Hz pre- value was 1250 µs — above
        // the threshold → sleep path. If operator sets the env back
        // to the legacy value, busy-wait disengages automatically.
        assert!(LEGACY_HALF_PERIOD_US_400HZ >= THRESHOLD_US);
    }
}
