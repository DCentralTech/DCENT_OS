// SPDX-License-Identifier: GPL-3.0-or-later
// Register-level SX1262 (Semtech SX126x) driver skeleton.
//
// Clean-room implementation from the Semtech SX1261/2 Data Sheet Rev. 1.2
// (June 2019). Command opcodes, register addresses, IRQ bits, the BUSY-poll
// handshake and the RF-frequency-word maths are all per the datasheet so this
// driver and the sibling DCENT_Raven Meshtastic accessory speak to the SAME
// silicon at the register level.
//
// HARMONIZATION with DCENT_Raven (projects/dcent-raven/, hardware/DESIGN_FREEZE.md):
//   * Same part: Ebyte E22-900M22S class module (SX1262 + on-module 32 MHz TCXO).
//   * Same control set: SPI + BUSY + DIO1(IRQ) + NRESET, TCXO via DIO3, RF
//     switch via discrete host-driven RXEN/TXEN (the E22 exposes both; Raven
//     drives them from its MCU, DCENT_axe from the main-board ESP32-S3 —
//     TXEN=GPIO2 / RXEN=GPIO9 per the dcent-axe-BM1397 schematic, R-24).
//   * Same default region: 915 MHz NA (868 EU build variant).
// The INTEGRATION differs: Raven drives the radio from its OWN MCU as a BAP
// host; DCENT_axe wires the SX1262 directly to the main-board ESP32-S3 on a
// dedicated SPI3/HSPI bus. The register driver is shared in spirit; only the
// pin map + bus owner differ (see GPIO map below).
//
// RF SWITCH (R-24, PREFAB_DESIGN_REVIEW_2026-07-08): the earlier claim that the
// E22's RF switch is "driven internally via DIO2 (no host GPIO)" was STALE and
// wrong for the DCENT_axe BM1397 board — the module's TXEN/RXEN are wired to
// host GPIOs. When the constructor is given the RF-switch pin pair
// (`with_rf_switch`), this driver drives them on every state transition
// (TX: TXEN=1 RXEN=0; RX/CAD: RXEN=1 TXEN=0; standby/sleep: both 0) and does
// NOT enable DIO2-switch mode. DIO2 mode (`SetDIO2AsRfSwitchCtrl`) is kept only
// for pinless maps whose module really is DIO2-switched.
//
// ✅ LOCKED GPIO MAP — dcentaxe-hal::lora_pins (netlist-confirmed BM1397 9/9):
//     LORA_SCLK   = GPIO5    LORA_MOSI  = GPIO6    LORA_MISO = GPIO7
//     LORA_NSS    = GPIO15   LORA_BUSY  = GPIO16   LORA_DIO1 = GPIO21
//     LORA_NRESET = GPIO8
//     LORA_TXEN   = GPIO2    LORA_RXEN  = GPIO9   (E22 RF switch, host-driven)
// (DCENT_Raven freezes a DIFFERENT map on FSPI IOMUX — do not copy it onto
// DCENT_axe; stock Bitaxe pin usage differs. MOSI is NOT GPIO14/fan-tach.)

use crate::{GpioPin, LoraError, SpiBus};

// ---------------------------------------------------------------------------
// Crystal / frequency-word constants
// ---------------------------------------------------------------------------

/// SX1262 reference oscillator (Hz). The E22 module integrates a 32 MHz TCXO;
/// the bare-die XTAL option is also 32 MHz (datasheet Table 3-4).
pub const F_XTAL_HZ: u32 = 32_000_000;

/// The RF-PLL step is `F_XTAL / 2^25`. `SetRfFrequency` takes the 32-bit word
/// `freq_hz * 2^25 / F_XTAL` (datasheet §13.4.1).
const FREQ_DIV_SHIFT: u32 = 25;

// ---------------------------------------------------------------------------
// Command opcodes (datasheet Table 11-1 .. 11-3)
// ---------------------------------------------------------------------------
pub mod opcode {
    // -- Operating-mode commands --
    pub const SET_SLEEP: u8 = 0x84;
    pub const SET_STANDBY: u8 = 0x80;
    pub const SET_FS: u8 = 0xC1;
    pub const SET_TX: u8 = 0x83;
    pub const SET_RX: u8 = 0x82;
    pub const SET_RX_DUTY_CYCLE: u8 = 0x94;
    pub const SET_CAD: u8 = 0xC5;
    pub const SET_TX_CONTINUOUS_WAVE: u8 = 0xD1;
    pub const SET_TX_INFINITE_PREAMBLE: u8 = 0xD2;
    pub const SET_REGULATOR_MODE: u8 = 0x96;
    pub const CALIBRATE: u8 = 0x89;
    pub const CALIBRATE_IMAGE: u8 = 0x98;
    pub const SET_PA_CONFIG: u8 = 0x95;
    pub const SET_RX_TX_FALLBACK_MODE: u8 = 0x93;

    // -- Register / buffer access --
    pub const WRITE_REGISTER: u8 = 0x0D;
    pub const READ_REGISTER: u8 = 0x1D;
    pub const WRITE_BUFFER: u8 = 0x0E;
    pub const READ_BUFFER: u8 = 0x1E;

    // -- DIO / IRQ control --
    pub const SET_DIO_IRQ_PARAMS: u8 = 0x08;
    pub const GET_IRQ_STATUS: u8 = 0x12;
    pub const CLEAR_IRQ_STATUS: u8 = 0x02;
    pub const SET_DIO2_AS_RF_SWITCH_CTRL: u8 = 0x9D;
    pub const SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;

    // -- Modulation / packet config --
    pub const SET_RF_FREQUENCY: u8 = 0x86;
    pub const SET_PACKET_TYPE: u8 = 0x8A;
    pub const GET_PACKET_TYPE: u8 = 0x11;
    pub const SET_TX_PARAMS: u8 = 0x8E;
    pub const SET_MODULATION_PARAMS: u8 = 0x8B;
    pub const SET_PACKET_PARAMS: u8 = 0x8C;
    pub const SET_CAD_PARAMS: u8 = 0x88;
    pub const SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
    pub const SET_LORA_SYMB_NUM_TIMEOUT: u8 = 0xA0;

    // -- Status --
    pub const GET_STATUS: u8 = 0xC0;
    pub const GET_RSSI_INST: u8 = 0x15;
    pub const GET_RX_BUFFER_STATUS: u8 = 0x13;
    pub const GET_PACKET_STATUS: u8 = 0x14;
    pub const GET_DEVICE_ERRORS: u8 = 0x17;
    pub const CLEAR_DEVICE_ERRORS: u8 = 0x07;

    /// SPI NOP byte — clocked out to read a return byte back on MISO.
    pub const NOP: u8 = 0x00;
}

// ---------------------------------------------------------------------------
// Register addresses (datasheet Table 12-1, subset that matters for bring-up)
// ---------------------------------------------------------------------------
pub mod reg {
    /// LoRa sync-word, MSB. Public network = 0x3444, private = 0x1424.
    /// Meshtastic uses 0x2B, which RadioLib expands to the 16-bit 0x24B4
    /// (`crate::meshtastic::phy::SYNC_WORD_MESHTASTIC`) — programmed by
    /// `Sx1262::apply_meshtastic_phy`.
    pub const LORA_SYNC_WORD_MSB: u16 = 0x0740;
    pub const LORA_SYNC_WORD_LSB: u16 = 0x0741;
    /// Over-current protection. SX1262 +22 dBm PA arms OCP to 0x38 (140 mA)
    /// after SetPaConfig (datasheet §5.1 / Table 5-2).
    pub const OCP_CONFIGURATION: u16 = 0x08E7;
    /// RX gain (0x94 = boosted/max-sensitivity, 0x93 = power-saving).
    pub const RX_GAIN: u16 = 0x08AC;
    pub const XTA_TRIM: u16 = 0x0911;
    pub const XTB_TRIM: u16 = 0x0912;
}

/// LoRa public-network sync word.
pub const SYNC_WORD_PUBLIC: u16 = 0x3444;
/// LoRa private-network sync word (RadioLib / single-device default).
pub const SYNC_WORD_PRIVATE: u16 = 0x1424;

/// `WriteRegister` value for [`reg::RX_GAIN`]: boosted / maximum-sensitivity RX.
/// An always-on, mains-powered relay node should run this so it hears the whole
/// mesh (the always-on advantage is wasted at the chip default). Costs ~2 mA.
pub const RX_GAIN_BOOSTED: u8 = 0x94;
/// `WriteRegister` value for [`reg::RX_GAIN`]: power-saving RX (a battery leaf).
pub const RX_GAIN_POWER_SAVING: u8 = 0x93;

/// `SetCadParams` field values (datasheet §13.1.10 / AN1200.48). `cadSymbolNum`
/// is how many preamble symbols the detector integrates over — more symbols =
/// more reliable but slower CAD; `DET_PEAK`/`DET_MIN` are SF-dependent detection
/// thresholds, and the defaults suit mid-range SF7–SF10 (tune per the live SF).
pub mod cad {
    pub const ON_1_SYMB: u8 = 0x00;
    pub const ON_2_SYMB: u8 = 0x01;
    pub const ON_4_SYMB: u8 = 0x02;
    pub const ON_8_SYMB: u8 = 0x03;
    pub const ON_16_SYMB: u8 = 0x04;
    /// Detection-peak threshold (AN1200.48 mid-SF default). Tune per SF.
    pub const DET_PEAK_DEFAULT: u8 = 0x18;
    /// Detection-min threshold (AN1200.48 default).
    pub const DET_MIN_DEFAULT: u8 = 0x0A;
}

/// `SetCadParams` exit mode: what the radio does when CAD completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CadExitMode {
    /// Return to STDBY_RC after CAD — the pure channel-check used for LBT.
    CadOnly = 0x00,
    /// Enter RX if activity was detected, else STDBY (CAD-then-RX).
    CadRx = 0x01,
}

// ---------------------------------------------------------------------------
// Small parameter enums
// ---------------------------------------------------------------------------

/// `SetStandby` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandbyMode {
    /// 13 MHz RC oscillator — lowest current.
    Rc = 0x00,
    /// 32 MHz XOSC/TCXO running — required before some calibrations.
    Xosc = 0x01,
}

/// `SetPacketType` argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    Gfsk = 0x00,
    LoRa = 0x01,
}

/// `SetRegulatorMode` argument. The E22 module is wired for DC-DC; bare-die LDO
/// is the simpler-BOM fallback (datasheet §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegulatorMode {
    LdoOnly = 0x00,
    DcDc = 0x01,
}

/// TCXO control voltage fed from DIO3 (`SetDIO3AsTcxoCtrl`, datasheet Table
/// 13-35). On the E22 the TCXO is on-module but STILL firmware-enabled here, or
/// RF is dead. DCENT_Raven uses 1.8 V (community E22 profile) — match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcxoVoltage {
    V1_6 = 0x00,
    V1_7 = 0x01,
    V1_8 = 0x02,
    V2_2 = 0x03,
    V2_4 = 0x04,
    V2_7 = 0x05,
    V3_0 = 0x06,
    V3_3 = 0x07,
}

/// `SetTxParams` PA ramp-up time (datasheet Table 13-41). 200 µs is the usual
/// LoRa default — long enough to keep spectral splatter inside the mask, short
/// enough not to waste TX-on time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RampTime {
    Ramp10u = 0x00,
    Ramp20u = 0x01,
    Ramp40u = 0x02,
    Ramp80u = 0x03,
    Ramp200u = 0x04,
    Ramp800u = 0x05,
    Ramp1700u = 0x06,
    Ramp3400u = 0x07,
}

/// Region / band select. One populated E22 board covers both; region is a
/// firmware + antenna choice (doc 05 §1.4). The firmware MUST also clamp the
/// legal duty-cycle/dwell envelope per region (not enforced in this skeleton).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// EU 863–870 MHz SRD (ETSI EN 300 220, ~1% duty cycle). Default center.
    Eu868,
    /// NA 902–928 MHz ISM (FCC 15.247 / IC RSS-247, dwell-time limited).
    Na915,
}

impl Region {
    /// Default LoRa center frequency for the region, in Hz.
    ///
    /// These are conservative in-band centers; a hopping/channel plan picks the
    /// actual TX frequency at runtime. EU 868.0 MHz sits in the 1%-duty sub-band;
    /// NA 915.0 MHz is mid-band.
    pub fn center_hz(self) -> u32 {
        match self {
            Region::Eu868 => 868_000_000,
            Region::Na915 => 915_000_000,
        }
    }

    /// Coarse legal-band guard used by [`Sx1262::set_rf_frequency`] to reject an
    /// obviously-illegal request before it reaches the radio. NOT a substitute
    /// for the per-region duty-cycle/dwell clamp (a firmware-policy follow-up).
    pub fn band_hz(self) -> (u32, u32) {
        match self {
            Region::Eu868 => (863_000_000, 870_000_000),
            Region::Na915 => (902_000_000, 928_000_000),
        }
    }
}

// ---------------------------------------------------------------------------
// IRQ status
// ---------------------------------------------------------------------------

/// Decoded `GetIrqStatus` bitfield (datasheet Table 13-29). The DIO1 line
/// (wired to an ESP32 IRQ-capable GPIO) asserts when any unmasked bit here is
/// set; the driver reads + clears the status to learn what happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IrqStatus {
    pub tx_done: bool,
    pub rx_done: bool,
    pub preamble_detected: bool,
    pub sync_word_valid: bool,
    pub header_valid: bool,
    pub header_err: bool,
    pub crc_err: bool,
    pub cad_done: bool,
    pub cad_detected: bool,
    pub timeout: bool,
    /// Raw 16-bit value as read, for logging / forward-compat with RFU bits.
    pub raw: u16,
}

pub mod irq {
    pub const TX_DONE: u16 = 1 << 0;
    pub const RX_DONE: u16 = 1 << 1;
    pub const PREAMBLE_DETECTED: u16 = 1 << 2;
    pub const SYNC_WORD_VALID: u16 = 1 << 3;
    pub const HEADER_VALID: u16 = 1 << 4;
    pub const HEADER_ERR: u16 = 1 << 5;
    pub const CRC_ERR: u16 = 1 << 6;
    pub const CAD_DONE: u16 = 1 << 7;
    pub const CAD_DETECTED: u16 = 1 << 8;
    pub const TIMEOUT: u16 = 1 << 9;
    /// All defined IRQ sources — the usual `SetDioIrqParams` enable mask.
    pub const ALL: u16 = 0x03FF;
}

/// `Calibrate` (0x89) block-select bitmask (datasheet Table 13-3). A full
/// power-on calibration sets [`ALL`](self::ALL) (= 0x7F) while in `STDBY_RC`.
pub mod calib {
    pub const RC64K: u8 = 1 << 0;
    pub const RC13M: u8 = 1 << 1;
    pub const PLL: u8 = 1 << 2;
    pub const ADC_PULSE: u8 = 1 << 3;
    pub const ADC_BULK_N: u8 = 1 << 4;
    pub const ADC_BULK_P: u8 = 1 << 5;
    pub const IMAGE: u8 = 1 << 6;
    /// Calibrate every block — the recommended one-shot startup calibration.
    pub const ALL: u8 = 0x7F;
}

/// Semtech-recommended `SetPaConfig` quartet for the SX1262 at the maximum
/// +22 dBm output (datasheet Table 13-21): `paDutyCycle=0x04`, `hpMax=0x07`,
/// `deviceSel=0x00` (SX1262, NOT SX1261), `paLut=0x01`.
pub const PA_CONFIG_22DBM: [u8; 4] = [0x04, 0x07, 0x00, 0x01];

/// Default TX power for the +22 dBm PA, in dBm. The SX1262 PA accepts
/// [`TX_POWER_MIN_DBM`]..=[`TX_POWER_MAX_DBM`].
pub const DEFAULT_TX_POWER_DBM: i8 = 22;
pub const TX_POWER_MIN_DBM: i8 = -9;
pub const TX_POWER_MAX_DBM: i8 = 22;

/// Over-current-protection value armed after the +22 dBm `SetPaConfig`
/// (datasheet §5.1.2: SX1262 default = 0x38 = 140 mA). Written explicitly so the
/// limit does not depend on silicon power-on defaults.
pub const OCP_22DBM: u8 = 0x38;

/// `CalibrateImage` (0x98) frequency-band bytes (datasheet Table 9-2). The image
/// calibration must be (re)run whenever the operating band changes; the band is
/// selected from the requested center frequency. Anything outside the tabulated
/// sub-GHz bands falls through to the 902–928 MHz pair (the DCENT_axe NA default).
pub fn image_calib_bytes(freq_hz: u32) -> (u8, u8) {
    match freq_hz {
        430_000_000..=440_000_000 => (0x6B, 0x6F),
        470_000_000..=510_000_000 => (0x75, 0x81),
        779_000_000..=787_000_000 => (0xC1, 0xC5),
        863_000_000..=870_000_000 => (0xD7, 0xDB),
        // 902–928 MHz (NA 915) — also the safe default for unmatched inputs.
        _ => (0xE1, 0xE9),
    }
}

/// Stored LoRa packet-parameter configuration. [`Sx1262::transmit`] and
/// [`Sx1262::receive`] re-issue `SetPacketParams` from this so a caller never
/// has to re-specify preamble / header / CRC / IQ to send the next payload —
/// only the per-frame payload length changes. Initialised by
/// [`Sx1262::configure_lora`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoRaPacketConfig {
    pub preamble_len: u16,
    pub implicit_header: bool,
    /// Max payload length used for RX / implicit-header framing.
    pub max_payload_len: u8,
    pub crc_on: bool,
    pub iq_inverted: bool,
}

impl Default for LoRaPacketConfig {
    /// 8-symbol preamble, explicit header, 255-byte cap, CRC on, standard IQ —
    /// the conventional public-LoRa default profile.
    fn default() -> Self {
        Self {
            preamble_len: 8,
            implicit_header: false,
            max_payload_len: 255,
            crc_on: true,
            iq_inverted: false,
        }
    }
}

/// Default LoRa modulation profile applied by [`Sx1262::configure_lora`]:
/// SF7 / BW 125 kHz / coding rate 4/5 / low-data-rate-optimize off. A robust,
/// widely-interoperable starting point; the caller can re-issue
/// `SetModulationParams` for a different air profile.
pub const DEFAULT_SF: u8 = 0x07;
pub const DEFAULT_BW_125K: u8 = 0x04;
pub const DEFAULT_CR_4_5: u8 = 0x01;

impl IrqStatus {
    /// Decode a raw 16-bit `GetIrqStatus` value into named flags.
    pub fn decode(raw: u16) -> Self {
        IrqStatus {
            tx_done: raw & irq::TX_DONE != 0,
            rx_done: raw & irq::RX_DONE != 0,
            preamble_detected: raw & irq::PREAMBLE_DETECTED != 0,
            sync_word_valid: raw & irq::SYNC_WORD_VALID != 0,
            header_valid: raw & irq::HEADER_VALID != 0,
            header_err: raw & irq::HEADER_ERR != 0,
            crc_err: raw & irq::CRC_ERR != 0,
            cad_done: raw & irq::CAD_DONE != 0,
            cad_detected: raw & irq::CAD_DETECTED != 0,
            timeout: raw & irq::TIMEOUT != 0,
            raw,
        }
    }
}

/// Compute the 32-bit `SetRfFrequency` word for `freq_hz`:
/// `word = freq_hz * 2^25 / F_XTAL`  (datasheet §13.4.1).
///
/// Done in `u64` to avoid overflow (`915e6 << 25` exceeds `u32`).
pub fn rf_freq_word(freq_hz: u32) -> u32 {
    (((freq_hz as u64) << FREQ_DIV_SHIFT) / F_XTAL_HZ as u64) as u32
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// Bounded BUSY poll budget. At the host-mock level this is just a loop count;
/// on real HW the caller's poll cadence sets the wall-clock timeout. The SX1262
/// BUSY is asserted for at most a few ms after the heaviest command (calibration).
pub const BUSY_POLL_BUDGET: u32 = 10_000;

/// Register-level SX1262 driver. Generic over the injected [`SpiBus`] and the
/// [`GpioPin`] control lines so it is fully host-testable with mocks.
///
/// `Busy` and `Dio1` are read as inputs; `Nreset` is driven as an output.
/// `RfSw` (defaults to the `Nreset` pin type) is the OPTIONAL host-driven E22
/// RF-switch pin pair (TXEN/RXEN, R-24): populate it via
/// [`Sx1262::with_rf_switch`] on boards that wire the module's switch enables
/// to host GPIOs; leave it `None` for a module whose switch is DIO2-driven.
pub struct Sx1262<SPI, Busy, Dio1, Nreset, RfSw = Nreset> {
    spi: SPI,
    busy: Busy,
    dio1: Dio1,
    nreset: Nreset,
    /// E22 RF-switch TX enable (host-driven, active-high). R-24.
    txen: Option<RfSw>,
    /// E22 RF-switch RX enable (host-driven, active-high). R-24.
    rxen: Option<RfSw>,
    region: Region,
    packet_type: PacketType,
    /// LoRa packet config last programmed by [`Self::configure_lora`]; reused by
    /// [`Self::transmit`]/[`Self::receive`] so only the payload length changes.
    pkt_cfg: LoRaPacketConfig,
}

impl<SPI, Busy, Dio1, Nreset> Sx1262<SPI, Busy, Dio1, Nreset>
where
    SPI: SpiBus,
    Busy: GpioPin,
    Dio1: GpioPin,
    Nreset: GpioPin,
{
    /// Construct a driver over already-configured SPI + control pins. The caller
    /// owns peripheral acquisition (mirrors the dcentaxe-bap transport pattern).
    /// The RF-switch pin pair starts empty (DIO2-switch posture) — a board that
    /// wires TXEN/RXEN to host GPIOs (DCENT_axe BM1397: GPIO2/GPIO9, R-24) MUST
    /// chain [`Self::with_rf_switch`] or the module's RF switch is never driven.
    pub fn new(spi: SPI, busy: Busy, dio1: Dio1, nreset: Nreset, region: Region) -> Self {
        Self {
            spi,
            busy,
            dio1,
            nreset,
            txen: None,
            rxen: None,
            region,
            packet_type: PacketType::LoRa,
            pkt_cfg: LoRaPacketConfig::default(),
        }
    }
}

impl<SPI, Busy, Dio1, Nreset, RfSw> Sx1262<SPI, Busy, Dio1, Nreset, RfSw>
where
    SPI: SpiBus,
    Busy: GpioPin,
    Dio1: GpioPin,
    Nreset: GpioPin,
    RfSw: GpioPin,
{
    /// Attach the host-driven E22 RF-switch enables (R-24). Both are `Option`
    /// so the ONE call site (the esp-idf radio task) stays monomorphic whether
    /// or not the pin map populates them — `(None, None)` keeps the DIO2-switch
    /// posture. When present, the driver drives them on every TX/RX/standby/
    /// sleep/CAD transition and skips `SetDIO2AsRfSwitchCtrl` in
    /// [`Self::configure_lora`].
    pub fn with_rf_switch<NewRfSw: GpioPin>(
        self,
        txen: Option<NewRfSw>,
        rxen: Option<NewRfSw>,
    ) -> Sx1262<SPI, Busy, Dio1, Nreset, NewRfSw> {
        Sx1262 {
            spi: self.spi,
            busy: self.busy,
            dio1: self.dio1,
            nreset: self.nreset,
            txen,
            rxen,
            region: self.region,
            packet_type: self.packet_type,
            pkt_cfg: self.pkt_cfg,
        }
    }

    /// `true` when the host owns the E22 RF switch (TXEN and/or RXEN attached).
    pub fn has_host_rf_switch(&self) -> bool {
        self.txen.is_some() || self.rxen.is_some()
    }

    // -- Host-driven E22 RF switch (R-24) -----------------------------------

    /// Drive the RF-switch enables to `(txen, rxen)`. Break-before-make: the
    /// de-asserted side is dropped FIRST so both enables are never momentarily
    /// high together (the E22 manual forbids TXEN=RXEN=1). No-ops (Ok) when the
    /// pins are not attached (DIO2-switch maps).
    fn drive_rf_switch(&mut self, tx: bool, rx: bool) -> Result<(), LoraError> {
        if !tx {
            if let Some(pin) = self.txen.as_mut() {
                pin.set_low()?;
            }
        }
        if !rx {
            if let Some(pin) = self.rxen.as_mut() {
                pin.set_low()?;
            }
        }
        if tx {
            if let Some(pin) = self.txen.as_mut() {
                pin.set_high()?;
            }
        }
        if rx {
            if let Some(pin) = self.rxen.as_mut() {
                pin.set_high()?;
            }
        }
        Ok(())
    }

    /// The LoRa packet config currently programmed (set by
    /// [`Self::configure_lora`], reused by TX/RX). Exposed for inspection/tests.
    pub fn packet_config(&self) -> LoRaPacketConfig {
        self.pkt_cfg
    }

    pub fn region(&self) -> Region {
        self.region
    }

    // -- BUSY handshake ----------------------------------------------------

    /// Block until BUSY goes low (chip ready), bounded by [`BUSY_POLL_BUDGET`].
    /// MANDATORY on the SX126x before every command — issuing SPI while BUSY is
    /// high is silently dropped. Returns [`LoraError::BusyTimeout`] if the line
    /// never releases (wiring / TCXO / power fault).
    pub fn wait_until_ready(&self) -> Result<(), LoraError> {
        for _ in 0..BUSY_POLL_BUDGET {
            if !self.busy.is_high()? {
                return Ok(());
            }
        }
        Err(LoraError::BusyTimeout)
    }

    // -- Low-level command primitives -------------------------------------

    /// Issue a write-only command: wait BUSY, frame `[opcode, params…]` over a
    /// single NSS transaction. The MISO bytes are discarded.
    pub fn command(&mut self, opcode: u8, params: &[u8]) -> Result<(), LoraError> {
        self.wait_until_ready()?;
        let mut buf = Vec::with_capacity(1 + params.len());
        buf.push(opcode);
        buf.extend_from_slice(params);
        self.spi.transfer(&mut buf)
    }

    /// Issue a read command and return `read_len` response bytes. Wire layout is
    /// `[opcode, NOP, <read_len zero bytes>]`; the SX126x returns a status byte
    /// while the NOP is clocked, then the response bytes — so the response sits
    /// at offset `2` of the MISO transcript (datasheet §11.3 "read commands").
    pub fn read_command(&mut self, opcode: u8, read_len: usize) -> Result<Vec<u8>, LoraError> {
        self.wait_until_ready()?;
        // [opcode, NOP, <read_len zero placeholders>]. Buffer is zero-filled,
        // so the placeholder bytes are already NOP.
        let mut buf = vec![0u8; 2 + read_len];
        buf[0] = opcode;
        buf[1] = opcode::NOP;
        self.spi.transfer(&mut buf)?;
        Ok(buf[2..2 + read_len].to_vec())
    }

    /// `WriteRegister` (0x0D): 16-bit address (big-endian) then data.
    pub fn write_register(&mut self, addr: u16, data: &[u8]) -> Result<(), LoraError> {
        let mut params = Vec::with_capacity(2 + data.len());
        params.push((addr >> 8) as u8);
        params.push(addr as u8);
        params.extend_from_slice(data);
        self.command(opcode::WRITE_REGISTER, &params)
    }

    // -- Configuration commands (skeleton — register-faithful framing) -----

    /// Hardware reset: pull NRESET low ≥100 µs, release, then wait BUSY low.
    /// (The caller is responsible for the actual delay between the two drives;
    /// on real HW insert a short sleep. Kept delay-free here so it is testable.)
    pub fn reset(&mut self) -> Result<(), LoraError> {
        self.nreset.set_low()?;
        self.nreset.set_high()?;
        self.wait_until_ready()
    }

    pub fn set_standby(&mut self, mode: StandbyMode) -> Result<(), LoraError> {
        // R-24: standby idles the host-driven RF switch (TXEN=0, RXEN=0).
        self.drive_rf_switch(false, false)?;
        self.command(opcode::SET_STANDBY, &[mode as u8])
    }

    /// `SetSleep` (0x84): enter sleep with the given config byte (bit 2 = warm
    /// start / retain config, bit 0 = RTC wake-up; datasheet Table 13-2). The
    /// host-driven RF switch is idled FIRST (both enables low) so a sleeping
    /// radio never leaves the E22 switch energized (R-24).
    pub fn set_sleep(&mut self, sleep_config: u8) -> Result<(), LoraError> {
        self.drive_rf_switch(false, false)?;
        self.command(opcode::SET_SLEEP, &[sleep_config])
    }

    pub fn set_regulator_mode(&mut self, mode: RegulatorMode) -> Result<(), LoraError> {
        self.command(opcode::SET_REGULATOR_MODE, &[mode as u8])
    }

    /// `SetDIO3AsTcxoCtrl`: voltage byte + 24-bit startup delay (15.625 µs steps).
    pub fn set_tcxo_mode(&mut self, v: TcxoVoltage, delay_steps: u32) -> Result<(), LoraError> {
        let d = delay_steps & 0x00FF_FFFF;
        self.command(
            opcode::SET_DIO3_AS_TCXO_CTRL,
            &[v as u8, (d >> 16) as u8, (d >> 8) as u8, d as u8],
        )
    }

    /// `SetDIO2AsRfSwitchCtrl`: DIO2 high during TX (drives a TXEN-style switch).
    /// RESOLVED for DCENT_axe (R-24, PREFAB_DESIGN_REVIEW_2026-07-08): the E22
    /// exposes SEPARATE RXEN/TXEN and the DCENT_axe BM1397 board wires them to
    /// host GPIO2/GPIO9 — so on that board this command is NOT used; the driver
    /// drives the discrete pins on every state transition instead (see
    /// [`Self::with_rf_switch`]). [`Self::configure_lora`] enables DIO2 mode
    /// ONLY when no host RF-switch pins are attached (pinless maps).
    pub fn set_dio2_as_rf_switch(&mut self, enable: bool) -> Result<(), LoraError> {
        self.command(opcode::SET_DIO2_AS_RF_SWITCH_CTRL, &[enable as u8])
    }

    pub fn set_packet_type_lora(&mut self) -> Result<(), LoraError> {
        self.packet_type = PacketType::LoRa;
        self.command(opcode::SET_PACKET_TYPE, &[PacketType::LoRa as u8])
    }

    /// `SetRfFrequency` (0x86): 32-bit frequency word, big-endian. Rejects a
    /// frequency outside the region's legal band before touching the radio.
    pub fn set_rf_frequency(&mut self, freq_hz: u32) -> Result<(), LoraError> {
        let (lo, hi) = self.region.band_hz();
        if freq_hz < lo || freq_hz > hi {
            return Err(LoraError::OutOfRange(format!(
                "{} Hz outside {:?} band {}..={}",
                freq_hz, self.region, lo, hi
            )));
        }
        let word = rf_freq_word(freq_hz);
        self.command(opcode::SET_RF_FREQUENCY, &word.to_be_bytes())
    }

    /// `SetModulationParams` for LoRa: SF, BW, CR, low-data-rate-optimize
    /// (datasheet Table 13-50). Raw bytes preserved exactly — do not "simplify".
    pub fn set_modulation_params(
        &mut self,
        sf: u8,
        bw: u8,
        cr: u8,
        low_data_rate_optimize: bool,
    ) -> Result<(), LoraError> {
        self.command(
            opcode::SET_MODULATION_PARAMS,
            &[sf, bw, cr, low_data_rate_optimize as u8],
        )
    }

    /// `SetPacketParams` for LoRa: preamble len (16-bit), header type, payload
    /// len, CRC on/off, IQ standard/inverted (datasheet Table 13-67).
    pub fn set_packet_params(
        &mut self,
        preamble_len: u16,
        implicit_header: bool,
        payload_len: u8,
        crc_on: bool,
        iq_inverted: bool,
    ) -> Result<(), LoraError> {
        self.command(
            opcode::SET_PACKET_PARAMS,
            &[
                (preamble_len >> 8) as u8,
                preamble_len as u8,
                implicit_header as u8,
                payload_len,
                crc_on as u8,
                iq_inverted as u8,
            ],
        )
    }

    /// `SetDioIrqParams`: IRQ mask + per-DIO routing masks (all 16-bit BE).
    /// Routes the chosen IRQ sources to the DIO1 line (datasheet Table 13-21).
    pub fn set_dio_irq_params(
        &mut self,
        irq_mask: u16,
        dio1_mask: u16,
        dio2_mask: u16,
        dio3_mask: u16,
    ) -> Result<(), LoraError> {
        let mut p = [0u8; 8];
        p[0..2].copy_from_slice(&irq_mask.to_be_bytes());
        p[2..4].copy_from_slice(&dio1_mask.to_be_bytes());
        p[4..6].copy_from_slice(&dio2_mask.to_be_bytes());
        p[6..8].copy_from_slice(&dio3_mask.to_be_bytes());
        self.command(opcode::SET_DIO_IRQ_PARAMS, &p)
    }

    /// `SetTx` (0x83): 24-bit timeout in 15.625 µs steps (0 = no timeout).
    /// Routes the host-driven E22 RF switch to TX (TXEN=1, RXEN=0) FIRST when
    /// the pins are attached (R-24) — keying the PA into a de-routed switch
    /// wastes the frame and can stress the PA.
    pub fn set_tx(&mut self, timeout_steps: u32) -> Result<(), LoraError> {
        self.drive_rf_switch(true, false)?;
        let t = timeout_steps & 0x00FF_FFFF;
        self.command(opcode::SET_TX, &[(t >> 16) as u8, (t >> 8) as u8, t as u8])
    }

    /// `SetRx` (0x82): 24-bit timeout in 15.625 µs steps (0xFFFFFF = continuous).
    /// Routes the host-driven E22 RF switch to RX (RXEN=1, TXEN=0) FIRST when
    /// the pins are attached (R-24) — otherwise the antenna is never routed to
    /// the LNA and the module is deaf.
    pub fn set_rx(&mut self, timeout_steps: u32) -> Result<(), LoraError> {
        self.drive_rf_switch(false, true)?;
        let t = timeout_steps & 0x00FF_FFFF;
        self.command(opcode::SET_RX, &[(t >> 16) as u8, (t >> 8) as u8, t as u8])
    }

    // -- Calibration -------------------------------------------------------

    /// `Calibrate` (0x89): run the selected calibration blocks (bitmask from
    /// [`calib`]). MUST be issued in `STDBY_RC`; a full calibration is
    /// [`calib::ALL`] (0x7F). BUSY stays high for the calibration duration —
    /// the standard BUSY handshake before the next command covers it.
    pub fn calibrate(&mut self, calib_param: u8) -> Result<(), LoraError> {
        self.command(opcode::CALIBRATE, &[calib_param])
    }

    /// `CalibrateImage` (0x98): image-reject calibration for the frequency band
    /// containing `freq_hz` (datasheet Table 9-2). Re-run on any band change; for
    /// a DCENT_axe NA build this is the 902–928 MHz pair (`0xE1 0xE9`).
    pub fn calibrate_image(&mut self, freq_hz: u32) -> Result<(), LoraError> {
        let (f1, f2) = image_calib_bytes(freq_hz);
        self.command(opcode::CALIBRATE_IMAGE, &[f1, f2])
    }

    /// `WriteRegister` the over-current-protection limit (register 0x08E7). Call
    /// after [`Self::set_pa_config`] so the OCP matches the armed PA
    /// (datasheet §5.1.2). The +22 dBm value is [`OCP_22DBM`].
    pub fn set_ocp(&mut self, ocp: u8) -> Result<(), LoraError> {
        self.write_register(reg::OCP_CONFIGURATION, &[ocp])
    }

    /// Write the 16-bit LoRa sync word into the two consecutive sync-word
    /// registers (0x0740 MSB / 0x0741 LSB). [`SYNC_WORD_PUBLIC`] (0x3444) for a
    /// public network, [`SYNC_WORD_PRIVATE`] (0x1424) for a private one.
    pub fn set_lora_sync_word(&mut self, sync_word: u16) -> Result<(), LoraError> {
        self.write_register(
            reg::LORA_SYNC_WORD_MSB,
            &[(sync_word >> 8) as u8, sync_word as u8],
        )
    }

    // -- TX path -----------------------------------------------------------

    /// `SetPaConfig` (0x95): the four PA-control bytes (datasheet Table 13-21).
    /// Prefer [`Self::set_pa_config_22dbm`] for the standard SX1262 +22 dBm setup.
    pub fn set_pa_config(
        &mut self,
        pa_duty_cycle: u8,
        hp_max: u8,
        device_sel: u8,
        pa_lut: u8,
    ) -> Result<(), LoraError> {
        self.command(
            opcode::SET_PA_CONFIG,
            &[pa_duty_cycle, hp_max, device_sel, pa_lut],
        )
    }

    /// Arm the Semtech-recommended SX1262 +22 dBm PA config ([`PA_CONFIG_22DBM`]).
    pub fn set_pa_config_22dbm(&mut self) -> Result<(), LoraError> {
        self.command(opcode::SET_PA_CONFIG, &PA_CONFIG_22DBM)
    }

    /// `SetTxParams` (0x8E): output power (dBm, signed) + PA [`RampTime`]. The
    /// SX1262 PA accepts [`TX_POWER_MIN_DBM`]..=[`TX_POWER_MAX_DBM`]; an
    /// out-of-range request is rejected with [`LoraError::OutOfRange`] before it
    /// reaches the radio.
    pub fn set_tx_params(&mut self, power_dbm: i8, ramp_time: RampTime) -> Result<(), LoraError> {
        if !(TX_POWER_MIN_DBM..=TX_POWER_MAX_DBM).contains(&power_dbm) {
            return Err(LoraError::OutOfRange(format!(
                "tx power {power_dbm} dBm outside {TX_POWER_MIN_DBM}..={TX_POWER_MAX_DBM}"
            )));
        }
        self.command(opcode::SET_TX_PARAMS, &[power_dbm as u8, ramp_time as u8])
    }

    /// `SetBufferBaseAddress` (0x8F): TX FIFO base + RX FIFO base (the SX1262 has
    /// one 256-byte buffer shared between TX and RX).
    pub fn set_buffer_base_address(&mut self, tx_base: u8, rx_base: u8) -> Result<(), LoraError> {
        self.command(opcode::SET_BUFFER_BASE_ADDRESS, &[tx_base, rx_base])
    }

    /// `WriteBuffer` (0x0E): write `data` into the radio FIFO starting at
    /// `offset`. Wire layout is `[opcode, offset, data…]`.
    pub fn write_buffer(&mut self, offset: u8, data: &[u8]) -> Result<(), LoraError> {
        let mut params = Vec::with_capacity(1 + data.len());
        params.push(offset);
        params.extend_from_slice(data);
        self.command(opcode::WRITE_BUFFER, &params)
    }

    /// High-level LoRa transmit: stage `payload` into the FIFO and start TX.
    ///
    /// Sequence (datasheet §14.4 "TX"): `SetBufferBaseAddress(0,0)` →
    /// `WriteBuffer` → `SetPacketParams` with the actual payload length (other
    /// packet fields reused from [`Self::configure_lora`]) → `SetTx(0)` (no
    /// timeout). Does NOT block on completion — the caller polls DIO1 /
    /// [`Self::process_irq`] for [`IrqStatus::tx_done`] (LoRa broadcast is
    /// unacknowledged, so TxDone is the only "sent" proof).
    pub fn transmit(&mut self, payload: &[u8]) -> Result<(), LoraError> {
        if payload.is_empty() || payload.len() > 255 {
            return Err(LoraError::OutOfRange(format!(
                "tx payload {} bytes, must be 1..=255",
                payload.len()
            )));
        }
        self.set_buffer_base_address(0x00, 0x00)?;
        self.write_buffer(0x00, payload)?;
        let cfg = self.pkt_cfg;
        self.set_packet_params(
            cfg.preamble_len,
            cfg.implicit_header,
            payload.len() as u8,
            cfg.crc_on,
            cfg.iq_inverted,
        )?;
        self.set_tx(0)
    }

    // -- RX path -----------------------------------------------------------

    /// `GetRxBufferStatus` (0x13) → `(payload_len, rx_start_ptr)`: how many bytes
    /// the last receive landed in the FIFO and where they start (datasheet
    /// §13.5.2). Feed both into [`Self::read_buffer`].
    pub fn get_rx_buffer_status(&mut self) -> Result<(u8, u8), LoraError> {
        let b = self.read_command(opcode::GET_RX_BUFFER_STATUS, 2)?;
        Ok((b[0], b[1]))
    }

    /// `ReadBuffer` (0x1E): read `len` bytes from the FIFO starting at `offset`.
    /// Wire layout is `[opcode, offset, NOP, <len read bytes>]` — the status byte
    /// is returned during the NOP, so the payload sits at transcript offset 3
    /// (datasheet §11.3; note the EXTRA offset byte vs. a plain read command).
    pub fn read_buffer(&mut self, offset: u8, len: usize) -> Result<Vec<u8>, LoraError> {
        self.wait_until_ready()?;
        let mut buf = vec![0u8; 3 + len];
        buf[0] = opcode::READ_BUFFER;
        buf[1] = offset;
        buf[2] = opcode::NOP;
        self.spi.transfer(&mut buf)?;
        Ok(buf[3..3 + len].to_vec())
    }

    /// `GetPacketStatus` (0x14) → `(rssi_dbm, snr_db)` for the last LoRa packet
    /// (datasheet §13.5.3). Decode: `rssi = -RssiPkt/2` dBm;
    /// `snr = (i8)SnrPkt / 4` dB (SnrPkt is two's-complement quarter-dB). The
    /// third returned byte (SignalRssiPkt) is not surfaced here.
    pub fn get_packet_status(&mut self) -> Result<(i16, i8), LoraError> {
        let b = self.read_command(opcode::GET_PACKET_STATUS, 3)?;
        let rssi_dbm = -(b[0] as i16) / 2;
        let snr_db = (b[1] as i8) / 4;
        Ok((rssi_dbm, snr_db))
    }

    /// High-level LoRa receive: pull the decoded payload plus link quality after
    /// an RxDone IRQ. Reads `GetRxBufferStatus` for length+pointer, `ReadBuffer`
    /// for the bytes, and `GetPacketStatus` for RSSI/SNR. Returns
    /// `(payload, rssi_dbm, snr_db)`. The caller is responsible for having seen
    /// [`IrqStatus::rx_done`] (and for checking [`IrqStatus::crc_err`]) first.
    pub fn receive(&mut self) -> Result<(Vec<u8>, i16, i8), LoraError> {
        let (len, start) = self.get_rx_buffer_status()?;
        let payload = self.read_buffer(start, len as usize)?;
        let (rssi_dbm, snr_db) = self.get_packet_status()?;
        Ok((payload, rssi_dbm, snr_db))
    }

    // -- IRQ handling ------------------------------------------------------

    /// `true` while DIO1 is asserted (an unmasked IRQ is pending). Call from the
    /// DIO1 ISR or a poll loop, then [`get_irq_status`](Self::get_irq_status).
    pub fn irq_pending(&self) -> Result<bool, LoraError> {
        self.dio1.is_high()
    }

    /// `GetIrqStatus` (0x12) → decoded [`IrqStatus`].
    pub fn get_irq_status(&mut self) -> Result<IrqStatus, LoraError> {
        let bytes = self.read_command(opcode::GET_IRQ_STATUS, 2)?;
        let raw = ((bytes[0] as u16) << 8) | bytes[1] as u16;
        Ok(IrqStatus::decode(raw))
    }

    /// `ClearIrqStatus` (0x02): clear the given bits (write the same 16-bit mask).
    pub fn clear_irq_status(&mut self, mask: u16) -> Result<(), LoraError> {
        self.command(opcode::CLEAR_IRQ_STATUS, &mask.to_be_bytes())
    }

    /// Read then clear all IRQ sources — the usual DIO1-ISR servicing step.
    pub fn process_irq(&mut self) -> Result<IrqStatus, LoraError> {
        let status = self.get_irq_status()?;
        self.clear_irq_status(irq::ALL)?;
        Ok(status)
    }

    // -- Channel activity detection / listen-before-talk -------------------
    // The building blocks for a managed flood's collision avoidance: a relay
    // should CAD/carrier-sense the channel clear BEFORE it transmits, instead of
    // keying the PA blind (the biggest dense-swarm risk in the mesh maturity
    // audit). The wrappers below emit the datasheet frames; the actual
    // sense→(clear?)→transmit loop lives in the esp-idf task layer.

    /// `SetCadParams` (0x88): configure the channel-activity detector.
    /// `timeout_steps` (24-bit, only used with [`CadExitMode::CadRx`]) is in
    /// 15.625 µs units. Wire layout:
    /// `[symbol_num, det_peak, det_min, exit_mode, T[23:16], T[15:8], T[7:0]]`.
    pub fn set_cad_params(
        &mut self,
        symbol_num: u8,
        det_peak: u8,
        det_min: u8,
        exit_mode: CadExitMode,
        timeout_steps: u32,
    ) -> Result<(), LoraError> {
        let t = timeout_steps.to_be_bytes(); // [_, T23:16, T15:8, T7:0]
        self.command(
            opcode::SET_CAD_PARAMS,
            &[
                symbol_num,
                det_peak,
                det_min,
                exit_mode as u8,
                t[1],
                t[2],
                t[3],
            ],
        )
    }

    /// Configure CAD for a listen-before-talk carrier check with the AN1200.48
    /// mid-SF defaults ([`cad`]): 4-symbol integration, CAD-only exit.
    pub fn configure_cad_lbt(&mut self) -> Result<(), LoraError> {
        self.set_cad_params(
            cad::ON_4_SYMB,
            cad::DET_PEAK_DEFAULT,
            cad::DET_MIN_DEFAULT,
            CadExitMode::CadOnly,
            0,
        )
    }

    /// `SetCad` (0xC5): start one channel-activity detection. Completion raises
    /// the `CadDone` IRQ; [`IrqStatus::cad_detected`] then says whether the
    /// channel is busy. The caller waits on DIO1 / polls [`Self::process_irq`].
    /// CAD runs the RECEIVER, so the host-driven E22 RF switch is routed to RX
    /// (RXEN=1, TXEN=0) first when the pins are attached (R-24) — an un-routed
    /// switch would make every CAD read a falsely-clear channel.
    pub fn start_cad(&mut self) -> Result<(), LoraError> {
        self.drive_rf_switch(false, true)?;
        self.command(opcode::SET_CAD, &[])
    }

    /// `GetRssiInst` (0x15): the instantaneous channel RSSI in dBm (`-RssiInst/2`)
    /// — a fast carrier-sense primitive for an RSSI-threshold LBT when a full CAD
    /// is too slow. Only meaningful while in RX.
    pub fn get_rssi_inst(&mut self) -> Result<i16, LoraError> {
        let b = self.read_command(opcode::GET_RSSI_INST, 1)?;
        Ok(-(b[0] as i16) / 2)
    }

    /// Carrier-sense LBT decision: `true` when the instantaneous RSSI is **below**
    /// `threshold_dbm` (channel clear to transmit). A typical threshold is around
    /// −90 dBm; pick it above the deployment's noise floor.
    pub fn channel_clear_rssi(&mut self, threshold_dbm: i16) -> Result<bool, LoraError> {
        Ok(self.get_rssi_inst()? < threshold_dbm)
    }

    /// Set RX gain to boosted / max-sensitivity ([`RX_GAIN_BOOSTED`]) — the mode
    /// an always-on, mains-powered relay should run so it hears the whole mesh.
    pub fn set_rx_gain_boosted(&mut self) -> Result<(), LoraError> {
        self.write_register(reg::RX_GAIN, &[RX_GAIN_BOOSTED])
    }

    // -- High-level bring-up skeleton --------------------------------------

    /// Full cold bring-up (datasheet §9.2.1 + §14.2 "From a cold boot"):
    /// reset → standby(RC) → DC-DC regulator → enable the DIO3 TCXO → full
    /// `Calibrate` in STDBY_RC → then [`Self::configure_lora`] for the LoRa
    /// packet/PA/image-cal/sync/IRQ config. After this the radio is ready for
    /// [`Self::transmit`] / [`Self::receive`].
    ///
    /// The full calibration is issued AFTER `SetDIO3AsTcxoCtrl` so the image/PLL
    /// blocks recalibrate against the running TCXO clock (datasheet §13.3.6 TCXO
    /// note); it stays in STDBY_RC as the datasheet requires.
    pub fn begin(&mut self) -> Result<(), LoraError> {
        self.reset()?;
        self.set_standby(StandbyMode::Rc)?;
        self.set_regulator_mode(RegulatorMode::DcDc)?;
        // E22 has an on-module TCXO; enabling it via DIO3 is mandatory or RF is
        // dead. 1.8 V matches the DCENT_Raven E22 profile; ~5 ms startup delay.
        self.set_tcxo_mode(TcxoVoltage::V1_8, 320)?;
        // Full power-on calibration, in STDBY_RC, after the TCXO is enabled.
        self.calibrate(calib::ALL)?;
        self.configure_lora()
    }

    /// Program the LoRa air config on a calibrated radio (called from
    /// [`Self::begin`], or directly to re-apply config after a region/band
    /// change without a full reset):
    ///   `SetPacketType(LoRa)` → `SetRfFrequency(center)` →
    ///   `CalibrateImage(band)` → `SetPaConfig(+22 dBm)` → `SetTxParams` →
    ///   OCP → LoRa sync word → `SetBufferBaseAddress` →
    ///   `SetModulationParams(default)` → `SetPacketParams(default)` →
    ///   `SetDioIrqParams(TxDone|RxDone → DIO1)`.
    ///
    /// `SetPacketType` is issued BEFORE modulation/packet params (mandatory:
    /// those parameters are interpreted in the context of the packet type). The
    /// image calibration runs for the region's band (902–928 MHz on an NA build,
    /// 863–870 MHz on EU) right after the frequency is set.
    pub fn configure_lora(&mut self) -> Result<(), LoraError> {
        let center = self.region.center_hz();
        self.set_packet_type_lora()?;
        self.set_rf_frequency(center)?;
        // Image-reject calibration for the operating band (NA 902–928 default).
        self.calibrate_image(center)?;
        // +22 dBm PA + matching TX power, ramp, and OCP limit.
        self.set_pa_config_22dbm()?;
        self.set_tx_params(DEFAULT_TX_POWER_DBM, RampTime::Ramp200u)?;
        self.set_ocp(OCP_22DBM)?;
        // Public-network LoRa sync word (override with set_lora_sync_word()).
        self.set_lora_sync_word(SYNC_WORD_PUBLIC)?;
        self.set_buffer_base_address(0x00, 0x00)?;
        // Default modulation: SF7 / BW125 / CR4/5 / LDRO off.
        self.set_modulation_params(DEFAULT_SF, DEFAULT_BW_125K, DEFAULT_CR_4_5, false)?;
        let cfg = LoRaPacketConfig::default();
        self.pkt_cfg = cfg;
        self.set_packet_params(
            cfg.preamble_len,
            cfg.implicit_header,
            cfg.max_payload_len,
            cfg.crc_on,
            cfg.iq_inverted,
        )?;
        self.set_dio_irq_params(irq::ALL, irq::TX_DONE | irq::RX_DONE, 0, 0)?;
        // RF-switch routing (R-24): with host-driven TXEN/RXEN attached
        // (DCENT_axe BM1397: GPIO2/GPIO9), park the switch idle and do NOT
        // enable DIO2-switch mode — DIO2 is not wired to the switch on that
        // board. A pinless map keeps the DIO2 posture and needs the chip to
        // drive its module's switch, so enable it explicitly here (the chip's
        // reset default is off).
        if self.has_host_rf_switch() {
            self.drive_rf_switch(false, false)?;
        } else {
            self.set_dio2_as_rf_switch(true)?;
        }
        Ok(())
    }
}

/// Phase-2 Meshtastic PHY programming. Feature-gated so the radio driver stays
/// dependency-free for a board that never joins a Meshtastic mesh.
#[cfg(feature = "meshtastic-interop")]
impl<SPI, Busy, Dio1, Nreset, RfSw> Sx1262<SPI, Busy, Dio1, Nreset, RfSw>
where
    SPI: SpiBus,
    Busy: GpioPin,
    Dio1: GpioPin,
    Nreset: GpioPin,
    RfSw: GpioPin,
{
    /// Program the radio for a Meshtastic channel from a resolved
    /// [`MeshtasticPhyConfig`](crate::meshtastic::MeshtasticPhyConfig): LoRa
    /// packet type → channel centre frequency → image calibration → +22 dBm PA +
    /// power/OCP → the **Meshtastic sync word** (0x24B4, NOT the public 0x3444) →
    /// the preset's SF/BW/CR/LDRO modulation → a 16-symbol preamble with CRC-on /
    /// standard IQ → route TxDone|RxDone to DIO1. After this a DCENT_axe is on the
    /// same air as a stock Meshtastic mesh.
    ///
    /// Mirrors [`Self::configure_lora`] but takes the interop config instead of
    /// the crate defaults. Call on a calibrated radio (after [`Self::begin`], or
    /// standalone to re-tune). The frequency is validated against the region band
    /// by [`Self::set_rf_frequency`], so an EU-region radio rejects a US channel.
    pub fn apply_meshtastic_phy(
        &mut self,
        cfg: &crate::meshtastic::MeshtasticPhyConfig,
    ) -> Result<(), LoraError> {
        self.set_packet_type_lora()?;
        self.set_rf_frequency(cfg.freq_hz)?;
        self.calibrate_image(cfg.freq_hz)?;
        self.set_pa_config_22dbm()?;
        self.set_tx_params(DEFAULT_TX_POWER_DBM, RampTime::Ramp200u)?;
        self.set_ocp(OCP_22DBM)?;
        // The load-bearing interop bit: the Meshtastic private sync word.
        self.set_lora_sync_word(cfg.sync_word)?;
        self.set_buffer_base_address(0x00, 0x00)?;
        self.set_modulation_params(
            cfg.sf,
            cfg.bandwidth_byte,
            cfg.coding_rate_byte,
            cfg.low_data_rate_optimize,
        )?;
        let pkt = LoRaPacketConfig {
            preamble_len: cfg.preamble_len,
            implicit_header: false,
            max_payload_len: 255,
            crc_on: cfg.crc_on,
            iq_inverted: cfg.iq_inverted,
        };
        self.pkt_cfg = pkt;
        self.set_packet_params(
            pkt.preamble_len,
            pkt.implicit_header,
            pkt.max_payload_len,
            pkt.crc_on,
            pkt.iq_inverted,
        )?;
        self.set_dio_irq_params(irq::ALL, irq::TX_DONE | irq::RX_DONE, 0, 0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MockPin, MockSpi};

    type TestRadio = Sx1262<MockSpi, MockPin, MockPin, MockPin>;

    fn radio(region: Region, busy: MockPin) -> TestRadio {
        Sx1262::new(
            MockSpi::new(),
            busy,
            MockPin::level(false), // DIO1 idle
            MockPin::level(true),  // NRESET idle-high
            region,
        )
    }

    // ---- BUSY-wait handshake ----

    #[test]
    fn busy_wait_returns_once_low() {
        // BUSY high for 3 polls then low → must succeed.
        let r = radio(Region::Na915, MockPin::sequence(&[true, true, true], false));
        assert!(r.wait_until_ready().is_ok());
    }

    #[test]
    fn busy_wait_times_out_when_stuck_high() {
        let r = radio(Region::Na915, MockPin::level(true));
        assert_eq!(r.wait_until_ready(), Err(LoraError::BusyTimeout));
    }

    #[test]
    fn command_blocks_on_busy_then_sends() {
        // BUSY high once then low; the command must still go out after the wait.
        let mut r = radio(Region::Na915, MockPin::sequence(&[true], false));
        r.command(opcode::SET_STANDBY, &[StandbyMode::Rc as u8])
            .unwrap();
        assert_eq!(&*r.spi.mosi.borrow(), &[opcode::SET_STANDBY, 0x00]);
    }

    // ---- SetRfFrequency frequency-word calc (915 + 868) ----

    #[test]
    fn rf_freq_word_915_and_868() {
        // word = f * 2^25 / 32e6.  915 MHz → 0x39300000, 868 MHz → 0x36400000.
        assert_eq!(rf_freq_word(915_000_000), 0x3930_0000);
        assert_eq!(rf_freq_word(868_000_000), 0x3640_0000);
    }

    #[test]
    fn set_rf_frequency_915_emits_correct_bytes() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_rf_frequency(915_000_000).unwrap();
        // [opcode, BE word]
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::SET_RF_FREQUENCY, 0x39, 0x30, 0x00, 0x00]
        );
    }

    #[test]
    fn set_rf_frequency_868_emits_correct_bytes() {
        let mut r = radio(Region::Eu868, MockPin::level(false));
        r.set_rf_frequency(868_000_000).unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::SET_RF_FREQUENCY, 0x36, 0x40, 0x00, 0x00]
        );
    }

    #[test]
    fn set_rf_frequency_rejects_out_of_band() {
        let mut r = radio(Region::Eu868, MockPin::level(false));
        // 915 MHz is illegal on an EU-region radio.
        assert!(matches!(
            r.set_rf_frequency(915_000_000),
            Err(LoraError::OutOfRange(_))
        ));
        // …and nothing was clocked out.
        assert!(r.spi.mosi.borrow().is_empty());
    }

    // ---- CAD / listen-before-talk ----

    #[test]
    fn set_cad_params_emits_seven_param_bytes() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_cad_params(cad::ON_4_SYMB, 0x18, 0x0A, CadExitMode::CadOnly, 0)
            .unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[
                opcode::SET_CAD_PARAMS,
                0x02,
                0x18,
                0x0A,
                0x00,
                0x00,
                0x00,
                0x00
            ]
        );
    }

    #[test]
    fn configure_cad_lbt_uses_an1200_defaults() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.configure_cad_lbt().unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[
                opcode::SET_CAD_PARAMS,
                cad::ON_4_SYMB,
                cad::DET_PEAK_DEFAULT,
                cad::DET_MIN_DEFAULT,
                CadExitMode::CadOnly as u8,
                0,
                0,
                0
            ]
        );
    }

    #[test]
    fn set_cad_params_packs_24bit_timeout_big_endian() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_cad_params(cad::ON_2_SYMB, 0x18, 0x0A, CadExitMode::CadRx, 0x0012_3456)
            .unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[
                opcode::SET_CAD_PARAMS,
                0x01,
                0x18,
                0x0A,
                0x01,
                0x12,
                0x34,
                0x56
            ]
        );
    }

    #[test]
    fn start_cad_emits_opcode_only() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.start_cad().unwrap();
        assert_eq!(&*r.spi.mosi.borrow(), &[opcode::SET_CAD]);
    }

    #[test]
    fn get_rssi_inst_decodes_negative_half_db() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // read_command frames [0x15, NOP, 0]; RssiInst returns at offset 2.
        // RssiInst=200 → -100 dBm.
        r.spi.queue_miso(&[0x00, 0x00, 200]);
        assert_eq!(r.get_rssi_inst().unwrap(), -100);
    }

    #[test]
    fn channel_clear_rssi_compares_threshold() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.spi.queue_miso(&[0x00, 0x00, 200]); // -100 dBm
        assert!(r.channel_clear_rssi(-90).unwrap(), "-100 < -90 → clear");
        r.spi.queue_miso(&[0x00, 0x00, 80]); // -40 dBm
        assert!(!r.channel_clear_rssi(-90).unwrap(), "-40 > -90 → busy");
    }

    #[test]
    fn set_rx_gain_boosted_writes_register() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_rx_gain_boosted().unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::WRITE_REGISTER, 0x08, 0xAC, 0x94]
        );
    }

    // ---- IRQ-status decode ----

    #[test]
    fn irq_decode_named_bits() {
        let tx = IrqStatus::decode(irq::TX_DONE);
        assert!(tx.tx_done && !tx.rx_done && !tx.timeout);

        let rx_to = IrqStatus::decode(irq::RX_DONE | irq::TIMEOUT);
        assert!(rx_to.rx_done && rx_to.timeout && !rx_to.tx_done);
        assert_eq!(rx_to.raw, 0x0202);

        let crc = IrqStatus::decode(irq::CRC_ERR | irq::HEADER_ERR);
        assert!(crc.crc_err && crc.header_err && !crc.rx_done);
    }

    #[test]
    fn get_irq_status_reads_from_offset_two() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // read_command frames [GET_IRQ_STATUS, NOP, 0, 0]; the chip returns the
        // IRQ bytes at offsets 2..4. Queue status=0x00, irq=0x0001 (TxDone).
        r.spi.queue_miso(&[0x00, 0x00, 0x00, 0x01]);
        let status = r.get_irq_status().unwrap();
        assert!(status.tx_done);
        assert_eq!(status.raw, 0x0001);
    }

    #[test]
    fn process_irq_reads_then_clears() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.spi.queue_miso(&[0x00, 0x00, 0x00, 0x02]); // RxDone for GetIrqStatus
        let status = r.process_irq().unwrap();
        assert!(status.rx_done);
        // Transcript = GetIrqStatus frame (4 bytes) + ClearIrqStatus(ALL) (3 bytes).
        let mosi = r.spi.mosi.borrow();
        assert_eq!(mosi[0], opcode::GET_IRQ_STATUS);
        assert_eq!(mosi[4], opcode::CLEAR_IRQ_STATUS);
        assert_eq!(&mosi[5..7], &irq::ALL.to_be_bytes());
    }

    #[test]
    fn reset_drives_nreset_low_then_high() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.reset().unwrap();
        assert_eq!(&*r.nreset.drives.borrow(), &[false, true]);
    }

    #[test]
    fn begin_runs_cold_boot_without_panicking() {
        // BUSY idle-low throughout; just prove the ordering executes and the
        // first opcode out is the standby after reset.
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.begin().unwrap();
        let mosi = r.spi.mosi.borrow();
        assert_eq!(mosi[0], opcode::SET_STANDBY);
        // The frequency word for the NA center must appear somewhere downstream.
        assert!(mosi
            .windows(5)
            .any(|w| w == [opcode::SET_RF_FREQUENCY, 0x39, 0x30, 0x00, 0x00]));
    }

    // ---- helpers for transcript-window assertions ----

    /// Index of the first occurrence of `pat` in `mosi`, or `None`.
    fn win(mosi: &[u8], pat: &[u8]) -> Option<usize> {
        mosi.windows(pat.len()).position(|w| w == pat)
    }

    // ---- TX path ----

    #[test]
    fn set_tx_params_emits_power_and_ramp() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_tx_params(22, RampTime::Ramp200u).unwrap();
        // +22 dBm = 0x16, ramp 200 µs = 0x04.
        assert_eq!(&*r.spi.mosi.borrow(), &[opcode::SET_TX_PARAMS, 0x16, 0x04]);
    }

    #[test]
    fn set_tx_params_signed_power_is_twos_complement() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_tx_params(-9, RampTime::Ramp10u).unwrap();
        // -9 dBm = 0xF7, ramp 10 µs = 0x00.
        assert_eq!(&*r.spi.mosi.borrow(), &[opcode::SET_TX_PARAMS, 0xF7, 0x00]);
    }

    #[test]
    fn set_tx_params_rejects_out_of_range() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        assert!(matches!(
            r.set_tx_params(23, RampTime::Ramp200u),
            Err(LoraError::OutOfRange(_))
        ));
        assert!(matches!(
            r.set_tx_params(-10, RampTime::Ramp200u),
            Err(LoraError::OutOfRange(_))
        ));
        // Nothing was clocked out for either rejected request.
        assert!(r.spi.mosi.borrow().is_empty());
    }

    #[test]
    fn set_pa_config_22dbm_emits_semtech_bytes() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_pa_config_22dbm().unwrap();
        // paDutyCycle=0x04, hpMax=0x07, deviceSel=0x00 (SX1262), paLut=0x01.
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::SET_PA_CONFIG, 0x04, 0x07, 0x00, 0x01]
        );
    }

    #[test]
    fn set_buffer_base_address_emits_tx_then_rx() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_buffer_base_address(0x80, 0x00).unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::SET_BUFFER_BASE_ADDRESS, 0x80, 0x00]
        );
    }

    #[test]
    fn write_buffer_emits_offset_then_data() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.write_buffer(0x00, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::WRITE_BUFFER, 0x00, 0xDE, 0xAD, 0xBE, 0xEF]
        );
    }

    #[test]
    fn transmit_emits_full_tx_sequence() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.transmit(&[0xAA, 0xBB]).unwrap();
        // base(0,0) → write_buffer(0, payload) → packet_params(len=2, default
        // preamble 8 / explicit / crc on / std IQ) → set_tx(0).
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[
                opcode::SET_BUFFER_BASE_ADDRESS,
                0x00,
                0x00,
                opcode::WRITE_BUFFER,
                0x00,
                0xAA,
                0xBB,
                opcode::SET_PACKET_PARAMS,
                0x00,
                0x08,
                0x00,
                0x02,
                0x01,
                0x00,
                opcode::SET_TX,
                0x00,
                0x00,
                0x00,
            ]
        );
    }

    #[test]
    fn transmit_rejects_empty_and_oversize() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        assert!(matches!(r.transmit(&[]), Err(LoraError::OutOfRange(_))));
        let big = vec![0u8; 256];
        assert!(matches!(r.transmit(&big), Err(LoraError::OutOfRange(_))));
        assert!(r.spi.mosi.borrow().is_empty());
    }

    // ---- RX path ----

    #[test]
    fn get_rx_buffer_status_returns_len_and_ptr() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // Frame [0x13, NOP, 0, 0]; chip returns status at 1, len/ptr at 2..4.
        r.spi.queue_miso(&[0x00, 0x00, 0x05, 0x80]);
        let (len, ptr) = r.get_rx_buffer_status().unwrap();
        assert_eq!((len, ptr), (0x05, 0x80));
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::GET_RX_BUFFER_STATUS, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn read_buffer_reads_payload_from_offset_three() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // Frame [READ_BUFFER, offset, NOP, d0, d1]; data sits at transcript [3..].
        r.spi.queue_miso(&[0x00, 0x00, 0x00, 0x11, 0x22]);
        let data = r.read_buffer(0x80, 2).unwrap();
        assert_eq!(data, vec![0x11, 0x22]);
        // The EXTRA offset byte distinguishes ReadBuffer from a plain read cmd.
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::READ_BUFFER, 0x80, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn get_packet_status_decodes_positive_rssi_snr() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // rssiPkt=0x50(80) → -40 dBm; snrPkt=0x14(20) → +5 dB; signalRssi unused.
        r.spi.queue_miso(&[0x00, 0x00, 0x50, 0x14, 0x60]);
        let (rssi, snr) = r.get_packet_status().unwrap();
        assert_eq!((rssi, snr), (-40, 5));
    }

    #[test]
    fn get_packet_status_decodes_negative_snr() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // snrPkt=0xEC is -20 in two's complement → -20/4 = -5 dB.
        // rssiPkt=0x9C(156) → -78 dBm.
        r.spi.queue_miso(&[0x00, 0x00, 0x9C, 0xEC, 0x00]);
        let (rssi, snr) = r.get_packet_status().unwrap();
        assert_eq!((rssi, snr), (-78, -5));
    }

    #[test]
    fn receive_returns_payload_rssi_snr() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        // 1) GetRxBufferStatus → len=3, ptr=0.
        r.spi.queue_miso(&[0x00, 0x00, 0x03, 0x00]);
        // 2) ReadBuffer(0, 3) → 0x11 0x22 0x33 at transcript [3..6].
        r.spi.queue_miso(&[0x00, 0x00, 0x00, 0x11, 0x22, 0x33]);
        // 3) GetPacketStatus → rssi -40, snr +5.
        r.spi.queue_miso(&[0x00, 0x00, 0x50, 0x14, 0x60]);
        let (payload, rssi, snr) = r.receive().unwrap();
        assert_eq!(payload, vec![0x11, 0x22, 0x33]);
        assert_eq!((rssi, snr), (-40, 5));
    }

    // ---- Calibration / sync / OCP ----

    #[test]
    fn calibrate_emits_block_mask() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.calibrate(calib::ALL).unwrap();
        assert_eq!(&*r.spi.mosi.borrow(), &[opcode::CALIBRATE, 0x7F]);
    }

    #[test]
    fn calibrate_image_na_emits_902_928_band() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.calibrate_image(915_000_000).unwrap();
        // 902–928 MHz band → F1=0xE1, F2=0xE9 (datasheet Table 9-2).
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::CALIBRATE_IMAGE, 0xE1, 0xE9]
        );
    }

    #[test]
    fn image_calib_bytes_cover_all_bands() {
        assert_eq!(image_calib_bytes(434_000_000), (0x6B, 0x6F));
        assert_eq!(image_calib_bytes(490_000_000), (0x75, 0x81));
        assert_eq!(image_calib_bytes(783_000_000), (0xC1, 0xC5));
        assert_eq!(image_calib_bytes(868_000_000), (0xD7, 0xDB)); // EU
        assert_eq!(image_calib_bytes(915_000_000), (0xE1, 0xE9)); // NA + default
        assert_eq!(image_calib_bytes(2_400_000_000), (0xE1, 0xE9)); // fallback
    }

    #[test]
    fn set_ocp_writes_configuration_register() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_ocp(OCP_22DBM).unwrap();
        // WriteRegister(0x08E7) = 0x38 (140 mA).
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::WRITE_REGISTER, 0x08, 0xE7, 0x38]
        );
    }

    #[test]
    fn set_lora_sync_word_writes_two_consecutive_registers() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.set_lora_sync_word(SYNC_WORD_PUBLIC).unwrap();
        // 0x3444 → MSB 0x34 to 0x0740, LSB 0x44 to 0x0741 (one WriteRegister).
        assert_eq!(
            &*r.spi.mosi.borrow(),
            &[opcode::WRITE_REGISTER, 0x07, 0x40, 0x34, 0x44]
        );
    }

    // ---- Full bring-up ordering ----

    #[test]
    fn configure_lora_sets_packet_type_before_modulation_and_calibrates_image() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.configure_lora().unwrap();
        let mosi = r.spi.mosi.borrow();
        // SetPacketType(LoRa) is the first command and precedes modulation/packet.
        assert_eq!(mosi[0], opcode::SET_PACKET_TYPE);
        let pt = win(&mosi, &[opcode::SET_PACKET_TYPE, 0x01]).unwrap();
        let modp = win(
            &mosi,
            &[opcode::SET_MODULATION_PARAMS, 0x07, 0x04, 0x01, 0x00],
        )
        .unwrap();
        let pkp = win(&mosi, &[opcode::SET_PACKET_PARAMS]).unwrap();
        assert!(pt < modp, "SetPacketType must precede SetModulationParams");
        assert!(pt < pkp, "SetPacketType must precede SetPacketParams");
        // Image calibration for the NA band ran during configure.
        assert!(win(&mosi, &[opcode::CALIBRATE_IMAGE, 0xE1, 0xE9]).is_some());
        // PA config + OCP + public sync word were programmed.
        assert!(win(&mosi, &[opcode::SET_PA_CONFIG, 0x04, 0x07, 0x00, 0x01]).is_some());
        assert!(win(&mosi, &[opcode::WRITE_REGISTER, 0x08, 0xE7, 0x38]).is_some());
        assert!(win(&mosi, &[opcode::WRITE_REGISTER, 0x07, 0x40, 0x34, 0x44]).is_some());
        // And the stored packet config matches the default profile.
        assert_eq!(r.packet_config(), LoRaPacketConfig::default());
    }

    #[test]
    fn begin_calibrates_all_after_enabling_tcxo() {
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.begin().unwrap();
        let mosi = r.spi.mosi.borrow();
        let tcxo = win(&mosi, &[opcode::SET_DIO3_AS_TCXO_CTRL]).unwrap();
        let cal = win(&mosi, &[opcode::CALIBRATE, 0x7F]).unwrap();
        assert!(
            tcxo < cal,
            "full Calibrate must follow SetDIO3AsTcxoCtrl so blocks re-cal vs the TCXO clock"
        );
        // Calibrate happens in STDBY_RC, before the LoRa packet-type config.
        let pt = win(&mosi, &[opcode::SET_PACKET_TYPE, 0x01]).unwrap();
        assert!(cal < pt);
    }

    // ---- Host-driven E22 RF switch (R-24, PREFAB_DESIGN_REVIEW_2026-07-08) ----
    // The DCENT_axe BM1397 board wires the E22-900M22S RF-switch enables to host
    // GPIOs (TXEN=GPIO2 / RXEN=GPIO9). These tests pin that, with the pin pair
    // attached, the driver actually routes the switch on every state transition
    // — the exact defect R-24 found was that it never did.

    type RfRadio = Sx1262<MockSpi, MockPin, MockPin, MockPin, MockPin>;

    fn radio_with_rf_switch(region: Region) -> RfRadio {
        radio(region, MockPin::level(false))
            .with_rf_switch(Some(MockPin::level(false)), Some(MockPin::level(false)))
    }

    fn drives(pin: &Option<MockPin>) -> Vec<bool> {
        pin.as_ref().unwrap().drives.borrow().clone()
    }

    #[test]
    fn rf_switch_routes_tx_rx_and_idles_on_standby() {
        let mut r = radio_with_rf_switch(Region::Na915);
        assert!(r.has_host_rf_switch());
        // RX: RXEN=1, TXEN=0 (break-before-make: TXEN dropped first).
        r.set_rx(0x00FF_FFFF).unwrap();
        assert_eq!(drives(&r.txen), vec![false]);
        assert_eq!(drives(&r.rxen), vec![true]);
        // TX: TXEN=1, RXEN=0.
        r.set_tx(0).unwrap();
        assert_eq!(drives(&r.txen), vec![false, true]);
        assert_eq!(drives(&r.rxen), vec![true, false]);
        // Standby: both de-energized.
        r.set_standby(StandbyMode::Rc).unwrap();
        assert_eq!(drives(&r.txen), vec![false, true, false]);
        assert_eq!(drives(&r.rxen), vec![true, false, false]);
    }

    #[test]
    fn rf_switch_never_asserts_both_enables_together() {
        // Replay every transition and check the reconstructed (txen, rxen) state
        // after each individual drive: TXEN=RXEN=1 must never occur (the E22
        // manual forbids it; break-before-make is load-bearing).
        let mut r = radio_with_rf_switch(Region::Na915);
        r.set_rx(0).unwrap();
        r.set_tx(0).unwrap();
        r.set_rx(0).unwrap();
        r.set_standby(StandbyMode::Rc).unwrap();
        // Interleave the two drive logs in call order. drive_rf_switch always
        // performs at most one drive per pin per call, low-side first — so
        // within a call the low drive precedes the high drive. Reconstruct
        // pessimistically: apply each call's drives in recorded order.
        let tx_drives = drives(&r.txen);
        let rx_drives = drives(&r.rxen);
        assert_eq!(tx_drives.len(), rx_drives.len());
        let (mut tx_level, mut rx_level) = (false, false);
        for (tx_d, rx_d) in tx_drives.iter().zip(rx_drives.iter()) {
            // Low-side first within each transition (matches drive_rf_switch).
            if !tx_d {
                tx_level = false;
            }
            if !rx_d {
                rx_level = false;
            }
            assert!(
                !(tx_level && rx_level),
                "TXEN and RXEN must never be high together"
            );
            tx_level = *tx_d;
            rx_level = *rx_d;
            assert!(
                !(tx_level && rx_level),
                "TXEN and RXEN must never be high together"
            );
        }
    }

    #[test]
    fn set_sleep_idles_rf_switch_and_emits_opcode() {
        let mut r = radio_with_rf_switch(Region::Na915);
        r.set_rx(0).unwrap(); // energize RXEN first
        r.set_sleep(0x04).unwrap(); // warm start
        assert_eq!(drives(&r.rxen), vec![true, false], "RXEN idled for sleep");
        assert_eq!(drives(&r.txen), vec![false, false], "TXEN idled for sleep");
        let mosi = r.spi.mosi.borrow();
        assert!(
            win(&mosi, &[opcode::SET_SLEEP, 0x04]).is_some(),
            "SetSleep must be framed after the switch is idled"
        );
    }

    #[test]
    fn start_cad_routes_switch_to_rx() {
        // CAD runs the receiver — an un-routed switch would carrier-sense a
        // falsely-clear channel on every LBT check.
        let mut r = radio_with_rf_switch(Region::Na915);
        r.start_cad().unwrap();
        assert_eq!(drives(&r.rxen), vec![true]);
        assert_eq!(drives(&r.txen), vec![false]);
        assert_eq!(*r.spi.mosi.borrow(), vec![opcode::SET_CAD]);
    }

    #[test]
    fn transmit_drives_txen_before_keying_pa() {
        let mut r = radio_with_rf_switch(Region::Na915);
        r.transmit(&[0xAA, 0xBB]).unwrap();
        // The switch ends routed to TX...
        assert_eq!(drives(&r.txen), vec![true]);
        assert_eq!(drives(&r.rxen), vec![false]);
        // ...and the SPI transcript is byte-identical to the pinless board's
        // (the switch is a GPIO concern, never an SPI concern).
        let mut pinless = radio(Region::Na915, MockPin::level(false));
        pinless.transmit(&[0xAA, 0xBB]).unwrap();
        assert_eq!(*r.spi.mosi.borrow(), *pinless.spi.mosi.borrow());
    }

    #[test]
    fn configure_lora_skips_dio2_mode_with_host_rf_switch() {
        // DCENT_axe BM1397 (R-24): DIO2 is NOT wired to the switch — enabling
        // DIO2-switch mode would be a lie and the discrete pins do the routing.
        let mut r = radio_with_rf_switch(Region::Na915);
        r.configure_lora().unwrap();
        let mosi = r.spi.mosi.borrow();
        assert!(
            win(&mosi, &[opcode::SET_DIO2_AS_RF_SWITCH_CTRL]).is_none(),
            "host-switch maps must NOT enable DIO2-as-RF-switch mode"
        );
        // The switch is parked idle (both enables low) after configuration.
        assert_eq!(drives(&r.txen), vec![false]);
        assert_eq!(drives(&r.rxen), vec![false]);
    }

    #[test]
    fn configure_lora_enables_dio2_mode_for_pinless_maps() {
        // A map without host TXEN/RXEN keeps the DIO2-switch posture — and the
        // chip's reset default is OFF, so it must be enabled explicitly.
        let mut r = radio(Region::Na915, MockPin::level(false));
        assert!(!r.has_host_rf_switch());
        r.configure_lora().unwrap();
        let mosi = r.spi.mosi.borrow();
        assert!(
            win(&mosi, &[opcode::SET_DIO2_AS_RF_SWITCH_CTRL, 0x01]).is_some(),
            "pinless maps must enable DIO2-as-RF-switch mode"
        );
    }

    // ---- Meshtastic PHY (feature-gated) ----

    #[cfg(feature = "meshtastic-interop")]
    #[test]
    fn apply_meshtastic_phy_programs_sync_word_and_longfast_modulation() {
        use crate::meshtastic::MeshtasticPhyConfig;
        let mut r = radio(Region::Na915, MockPin::level(false));
        r.apply_meshtastic_phy(&MeshtasticPhyConfig::us_longfast())
            .unwrap();
        let mosi = r.spi.mosi.borrow();
        // The Meshtastic sync word 0x24B4 is written to the sync-word registers,
        // NOT the public-LoRaWAN 0x3444 — this is what makes us mesh-hearable.
        assert!(
            win(&mosi, &[opcode::WRITE_REGISTER, 0x07, 0x40, 0x24, 0xB4]).is_some(),
            "Meshtastic sync word 0x24B4 must be programmed"
        );
        assert!(
            win(&mosi, &[opcode::WRITE_REGISTER, 0x07, 0x40, 0x34, 0x44]).is_none(),
            "public sync word must NOT be programmed"
        );
        // LongFast modulation: SF11 / BW250 (0x05) / CR4/5 (0x01) / LDRO off.
        assert!(
            win(
                &mosi,
                &[opcode::SET_MODULATION_PARAMS, 0x0B, 0x05, 0x01, 0x00]
            )
            .is_some(),
            "LongFast SF11/BW250/CR45/LDRO-off modulation must be programmed"
        );
        // 16-symbol preamble in the packet params (preamble hi=0x00, lo=0x10).
        assert!(
            win(&mosi, &[opcode::SET_PACKET_PARAMS, 0x00, 0x10]).is_some(),
            "16-symbol Meshtastic preamble must be programmed"
        );
        // The stored packet config reflects the interop preamble.
        assert_eq!(r.packet_config().preamble_len, 16);
    }
}
