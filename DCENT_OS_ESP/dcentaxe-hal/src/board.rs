//! Board configuration and model detection for DCENT_axe hardware variants.
//!
//! Supports both BitAxe and Nerd-family boards. Each board has different ASIC
//! chips, pin mappings, voltage ranges, power IC configurations, and peripherals.
//! This module centralizes all board-specific parameters so the rest of the HAL
//! can be board-agnostic.
//!
//! Pin families are selected at compile time via Cargo features:
//! - `pins-bitaxe`: I2C SDA=47, SCL=48; UART TX=17, RX=18 (all BitAxe boards)
//! - `pins-nerd`:   I2C SDA=18, SCL=17; UART TX=43, RX=44 (Nerd-family / TTGO T-Display S3)

// Compile-time safety: exactly one pin family must be selected
#[cfg(all(feature = "pins-bitaxe", feature = "pins-nerd"))]
compile_error!("Cannot enable both pins-bitaxe and pins-nerd — pick one board feature");

#[cfg(not(any(feature = "pins-bitaxe", feature = "pins-nerd")))]
compile_error!("No board selected — use --features bitaxe-gamma (or nerdnos, nerdaxe, etc)");

use log::{info, warn};
use serde::{Deserialize, Serialize};

/// Supported board models across BitAxe and Nerd families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitAxeModel {
    // ── BitAxe family ──
    /// BitAxe Max — BM1397 (S17-era chip), single ASIC
    Max,
    /// BitAxe Ultra — BM1366 (S19XP-era chip), single ASIC
    Ultra,
    /// BitAxe Hex Ultra — 6x BM1366
    HexUltra,
    /// BitAxe Supra — BM1368 (S21-era chip), single ASIC
    Supra,
    /// BitAxe Hex Supra — 6x BM1368
    HexSupra,
    /// BitAxe Gamma — BM1370 (S21 Pro chip), single ASIC
    Gamma,
    /// BitAxe Gamma Duo — 2x BM1370XP, 5V input
    GammaDuo,
    /// BitAxe Gamma Turbo — 2x BM1370, 12V input
    GammaTurbo,
    /// BitAxe Touch — single BM1370 + separate ESP32-S3 LVGL accessory
    /// over BAP (UART_NUM_2). Same mining board as Gamma; differs only in
    /// the populated BAP header + preinstalled Touch-aware self-test path.
    Touch,
    /// BitAxe Turbo Touch — GT-801 mining board (2× BM1370) + the same
    /// LVGL accessory over BAP. Electrically identical to GammaTurbo; the
    /// variant exists so the dashboard, `board_target`, and self-test flow
    /// know to advertise Touch-aware behaviour.
    GtTouch,

    // ── Nerd family ──
    /// NerdNOS — BM1397, headless, USB-C 5V ~8W, fixed voltage, no fan
    NerdNOS,
    /// NerdAxe — BM1370, TTGO T-Display S3, TPS546D, EMC2101
    NerdAxe,
    /// NerdQaxe+ — 4x BM1368, TTGO T-Display S3
    NerdQaxePlus,
    /// NerdQaxe++ — 4x BM1370, TTGO T-Display S3
    NerdQaxePP,

    // ── DCENT_axe family (D-Central BM1397 SKUs) ──
    /// DCENT_axe BM1397 — single BM1397 (S17-era chip), EMC2101 fan, TPS546D24A
    /// PMBus VRM (EN wired to GPIO10, ACTIVE-HIGH). There is NO DS4432U and NO
    /// INA260 on this board — verified from the dcent-axe-BM1397 schematic
    /// netlist (PREFAB_DESIGN_REVIEW_2026-07-08 R-10).
    /// D-Central's own BitAxe-Max-class single-chip board.
    DcentAxeBm1397,
    /// DCENT_axe Quad BM1397 — 4x BM1397 single UART daisy chain, EMC2302
    /// dual fan, TPS546. Same driver as the single, scaled to 4 chips.
    DcentAxeQuadBm1397,
    /// DCENT_axe Hex BM1397 — 6x BM1397 single UART daisy chain, EMC2302
    /// dual fan, TPS546, 3 series voltage domains (Hex-class topology).
    DcentAxeHexBm1397,
}

impl BitAxeModel {
    pub fn canonical_key(&self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Ultra => "ultra",
            Self::HexUltra => "hexultra",
            Self::Supra => "supra",
            Self::HexSupra => "suprahex",
            Self::Gamma => "gamma",
            Self::GammaDuo => "gammaduo",
            Self::GammaTurbo => "gammaturbo",
            Self::Touch => "touch",
            Self::GtTouch => "gt_touch",
            Self::NerdNOS => "nerdnos",
            Self::NerdAxe => "nerdaxe",
            Self::NerdQaxePlus => "nerdqaxeplus",
            Self::NerdQaxePP => "nerdqaxepp",
            Self::DcentAxeBm1397 => "dcentaxe_bm1397",
            Self::DcentAxeQuadBm1397 => "dcentaxe_quad_bm1397",
            Self::DcentAxeHexBm1397 => "dcentaxe_hex_bm1397",
        }
    }

    pub fn from_device_model(model: &str) -> Option<Self> {
        match model.trim().to_ascii_lowercase().as_str() {
            "max" => Some(Self::Max),
            "ultra" => Some(Self::Ultra),
            "hex" | "hexultra" | "hex_ultra" | "ultrahex" => Some(Self::HexUltra),
            "supra" => Some(Self::Supra),
            "hexsupra" | "hex_supra" | "suprahex" | "supra_hex" => Some(Self::HexSupra),
            "gamma" => Some(Self::Gamma),
            "gammaduo" => Some(Self::GammaDuo),
            "gammaturbo" | "gt" => Some(Self::GammaTurbo),
            "touch" | "bitaxe_touch" | "bitaxetouch" => Some(Self::Touch),
            "gt_touch" | "gttouch" | "turbotouch" | "turbo_touch" => Some(Self::GtTouch),
            "nerdnos" => Some(Self::NerdNOS),
            "nerdaxe" => Some(Self::NerdAxe),
            "nerdqaxe+" | "nerdqaxeplus" | "nerdqaxe_plus" => Some(Self::NerdQaxePlus),
            "nerdqaxe++" | "nerdqaxepp" | "nerdqaxe_pp" => Some(Self::NerdQaxePP),
            // Canonical key, compact aliases, and the lowercased marketing name
            // ("DCENT_axe BM1397" -> "dcent_axe bm1397").
            "dcentaxe_bm1397" | "dcentaxebm1397" | "dcent_axe_bm1397" | "dcentaxe bm1397"
            | "dcent_axe bm1397" => Some(Self::DcentAxeBm1397),
            "dcentaxe_quad_bm1397"
            | "dcentaxequadbm1397"
            | "dcent_axe_quad_bm1397"
            | "quad_bm1397"
            | "dcentaxe quad bm1397"
            | "dcent_axe quad bm1397" => Some(Self::DcentAxeQuadBm1397),
            "dcentaxe_hex_bm1397"
            | "dcentaxehexbm1397"
            | "dcent_axe_hex_bm1397"
            | "hex_bm1397"
            | "dcentaxe hex bm1397"
            | "dcent_axe hex bm1397" => Some(Self::DcentAxeHexBm1397),
            _ => None,
        }
    }

    pub fn board_target(&self) -> &'static str {
        match self {
            Self::Max => "bitaxe-max",
            Self::Ultra => "bitaxe-ultra",
            Self::HexUltra => "bitaxe-hex-ultra",
            Self::Supra => "bitaxe-supra",
            Self::HexSupra => "bitaxe-hex-supra",
            Self::Gamma => "bitaxe-gamma",
            Self::GammaDuo => "bitaxe-gamma-duo",
            Self::GammaTurbo => "bitaxe-gt",
            // Touch variants reuse the underlying mining board targets but
            // flip on the BAP accessory driver via the `bap` Cargo feature.
            Self::Touch => "bitaxe-touch",
            Self::GtTouch => "bitaxe-gt-touch",
            Self::NerdNOS => "nerdnos",
            Self::NerdAxe => "nerdaxe",
            Self::NerdQaxePlus => "nerdqaxe-plus",
            Self::NerdQaxePP => "nerdqaxe-pp",
            Self::DcentAxeBm1397 => "dcent-axe-bm1397",
            Self::DcentAxeQuadBm1397 => "dcent-axe-quad-bm1397",
            Self::DcentAxeHexBm1397 => "dcent-axe-hex-bm1397",
        }
    }

    /// Number of ASIC chips on this board variant.
    pub fn asic_count(&self) -> u8 {
        match self {
            Self::HexUltra | Self::HexSupra | Self::DcentAxeHexBm1397 => 6,
            Self::GammaDuo | Self::GammaTurbo | Self::GtTouch => 2,
            Self::NerdQaxePlus | Self::NerdQaxePP | Self::DcentAxeQuadBm1397 => 4,
            _ => 1,
        }
    }

    /// Human-readable name for logging and UI.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Max => "BitAxe Max",
            Self::Ultra => "BitAxe Ultra",
            Self::HexUltra => "BitAxe Hex Ultra",
            Self::Supra => "BitAxe Supra",
            Self::HexSupra => "BitAxe Hex Supra",
            Self::Gamma => "BitAxe Gamma",
            Self::GammaDuo => "BitAxe Gamma Duo",
            Self::GammaTurbo => "BitAxe Gamma Turbo",
            Self::Touch => "BitAxe Touch",
            Self::GtTouch => "BitAxe Turbo Touch",
            Self::NerdNOS => "NerdNOS",
            Self::NerdAxe => "NerdAxe",
            Self::NerdQaxePlus => "NerdQaxe+",
            Self::NerdQaxePP => "NerdQaxe++",
            Self::DcentAxeBm1397 => "DCENT_axe BM1397",
            Self::DcentAxeQuadBm1397 => "DCENT_axe Quad BM1397",
            Self::DcentAxeHexBm1397 => "DCENT_axe Hex BM1397",
        }
    }

    /// Honest release-support status for operator-facing surfaces.
    ///
    /// This is deliberately coarse and conservative. Unknown board versions are
    /// handled by config-level recognition helpers and surface as `unknown`.
    pub fn support_status(&self) -> &'static str {
        match self {
            Self::Gamma | Self::Max | Self::Ultra | Self::Supra => "supported",
            Self::HexUltra
            | Self::HexSupra
            | Self::GammaDuo
            | Self::GammaTurbo
            | Self::Touch
            | Self::GtTouch
            | Self::NerdNOS
            | Self::NerdAxe
            | Self::NerdQaxePlus
            | Self::NerdQaxePP
            | Self::DcentAxeBm1397
            | Self::DcentAxeQuadBm1397
            | Self::DcentAxeHexBm1397 => "experimental",
        }
    }

    /// Returns true if this is a Hex (6-ASIC series-chain) variant.
    pub fn is_hex(&self) -> bool {
        matches!(
            self,
            Self::HexUltra | Self::HexSupra | Self::DcentAxeHexBm1397
        )
    }

    /// Returns true if this is a Nerd-family board.
    pub fn is_nerd(&self) -> bool {
        matches!(
            self,
            Self::NerdNOS | Self::NerdAxe | Self::NerdQaxePlus | Self::NerdQaxePP
        )
    }

    /// Returns true if this is a D-Central DCENT_axe family board.
    ///
    /// Used by `normalize_power_pins` to give the family its own deterministic
    /// power path: the TPS546D24A EN pin is wired to **GPIO10, ACTIVE-HIGH** on
    /// these boards (verified from the dcent-axe-BM1397 schematic netlist,
    /// PREFAB_DESIGN_REVIEW_2026-07-08 R-10) — NOT the stock-BitAxe GPIO46 the
    /// generic Tps546 arm picks (unconnected here), and NOT active-low.
    pub fn is_dcent_axe(&self) -> bool {
        matches!(
            self,
            Self::DcentAxeBm1397 | Self::DcentAxeQuadBm1397 | Self::DcentAxeHexBm1397
        )
    }

    /// Returns true if this board has a multi-ASIC UART daisy chain.
    pub fn is_multi_asic(&self) -> bool {
        self.asic_count() > 1
    }

    /// Whether this board has I2C-programmable voltage control.
    pub fn has_voltage_control(&self) -> bool {
        !matches!(self, Self::NerdNOS)
    }

    /// Whether this board has a fan controller.
    pub fn has_fan(&self) -> bool {
        !matches!(self, Self::NerdNOS)
    }

    /// Whether this board has a hardware display (OLED or LCD).
    pub fn has_display(&self) -> bool {
        self.display_kind().has_hardware()
    }

    pub fn display_kind(&self) -> DisplayKind {
        match self {
            Self::NerdNOS => DisplayKind::None,
            Self::NerdAxe | Self::NerdQaxePlus | Self::NerdQaxePP => DisplayKind::TDisplayS3,
            Self::Touch | Self::GtTouch => DisplayKind::BapTouch,
            Self::Max
            | Self::Ultra
            | Self::HexUltra
            | Self::Supra
            | Self::HexSupra
            | Self::Gamma
            | Self::GammaDuo
            | Self::GammaTurbo
            | Self::DcentAxeBm1397
            | Self::DcentAxeQuadBm1397
            | Self::DcentAxeHexBm1397 => DisplayKind::Ssd1306,
        }
    }

    /// ASIC chip ID expected during detection (read from register 0x00).
    pub fn expected_chip_id(&self) -> u16 {
        match self {
            Self::Max
            | Self::NerdNOS
            | Self::DcentAxeBm1397
            | Self::DcentAxeQuadBm1397
            | Self::DcentAxeHexBm1397 => 0x1397,
            Self::Ultra | Self::HexUltra => 0x1366,
            Self::Supra | Self::HexSupra | Self::NerdQaxePlus => 0x1368,
            Self::Gamma
            | Self::GammaDuo
            | Self::GammaTurbo
            | Self::NerdAxe
            | Self::NerdQaxePP
            | Self::Touch
            | Self::GtTouch => 0x1370,
        }
    }

    /// Whether this variant ships the BAP accessory header populated + the
    /// stock Touch / Turbo Touch LVGL board attached. Firmware uses this to
    /// decide whether to start the BAP UART server and to adjust self-test
    /// flow (auto-reboot after pass because no reset button is reachable).
    pub fn has_bap(&self) -> bool {
        matches!(
            self,
            Self::Touch
                | Self::GtTouch
                // Every DCENT_axe board ships the BAP accessory header populated.
                | Self::DcentAxeBm1397
                | Self::DcentAxeQuadBm1397
                | Self::DcentAxeHexBm1397
        )
    }

    /// Status-LED hardware kind (M-7, FULL_PREFAB_REVIEW_2026-07-11).
    ///
    /// `Sk6812` ONLY for the DCENT_axe BM1397 single board, whose 2026-07-11
    /// netlist confirms D1 = SK6812MINI-E on GPIO4. Quad/Hex stay `PlainGpio`
    /// until their own netlists exist (no speculative claims). NOTE: this is
    /// honest metadata, not a shipped driver — see [`StatusLedKind`] and
    /// `docs/STATUS_LED_SK6812_GAP.md`.
    pub fn status_led_kind(&self) -> StatusLedKind {
        match self {
            Self::DcentAxeBm1397 => StatusLedKind::Sk6812,
            _ => StatusLedKind::PlainGpio,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerControllerKind {
    None,
    Tps546,
    Ds4432u,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FanControllerKind {
    None,
    Emc2101,
    Emc2103,
    Emc2302,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TempSensorKind {
    None,
    Emc2101,
    Tmp1075,
    Emc2103,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayKind {
    None,
    Ssd1306,
    TDisplayS3,
    BapTouch,
}

impl DisplayKind {
    pub fn has_hardware(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessoryMode {
    None,
    BapTouch,
    W5500Lan,
}

/// Status-LED hardware kind (M-7, FULL_PREFAB_REVIEW_2026-07-11).
///
/// Most boards wire a plain push-pull LED to the status-LED GPIO. The
/// DCENT_axe BM1397 single board instead places an SK6812MINI-E addressable
/// one-wire LED (D1) on GPIO4 — driving it push-pull (today's `GpioController`
/// path) never lights it, because SK6812 needs the RMT-timed one-wire
/// protocol. This enum is HONEST METADATA ONLY: the RMT driver is NOT shipped
/// (it is esp-idf/xtensa-only and cannot be host-verified — see
/// `docs/STATUS_LED_SK6812_GAP.md`), and `main.rs` still drives GPIO4
/// push-pull on every board (electrically harmless on an SK6812 DIN pin; the
/// LED simply stays dark).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusLedKind {
    /// Plain push-pull GPIO LED (every stock BitAxe / Nerd board).
    PlainGpio,
    /// SK6812MINI-E addressable one-wire LED — needs an RMT driver the
    /// firmware does not ship yet (documented gap, LED stays dark).
    Sk6812,
}

/// Retained proof level for a board-version row.
///
/// This is intentionally separate from [`BitAxeModel::support_status`]:
/// `supported` means the firmware supports the board class, while this field
/// records the strongest retained evidence artifact. Do not infer soak proof
/// from a support label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveProof {
    None,
    Host,
    FocusedRun,
    SustainedSoak,
}

impl LiveProof {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Host => "host",
            Self::FocusedRun => "focused-run",
            Self::SustainedSoak => "sustained-soak",
        }
    }
}

/// Explicit hardware overrides migrated from ESP-Miner custom-board NVS keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardHardwareConfig {
    pub plug_sense: bool,
    pub asic_enable: bool,
    pub fan_controller: FanControllerKind,
    pub temp_sensor: TempSensorKind,
    pub power_controller: PowerControllerKind,
    pub has_ina260: bool,
    pub emc_internal_temp: bool,
    pub emc_ideality_factor: u8,
    pub emc_beta_compensation: u8,
    pub temp_offset_c: i8,
    pub power_consumption_target_w: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardVersionProfile {
    pub board_version: &'static str,
    pub device_model: &'static str,
    pub asic_model: &'static str,
    pub model: BitAxeModel,
    pub live_proof: LiveProof,
    pub fan_controller: FanControllerKind,
    pub temp_sensor: TempSensorKind,
    pub power_controller: PowerControllerKind,
    pub has_ina260: bool,
    pub emc_internal_temp: bool,
    pub emc_ideality_factor: u8,
    pub emc_beta_compensation: u8,
    pub temp_offset_c: i8,
    pub power_consumption_target_w: u16,
    /// Pass-5 audit: per ESP-Miner PR #1616 (`33d7210`), some boards have
    /// the EMC2103 internal/external temp readings physically swapped on
    /// the silicon. Currently true only for v801 GT. Consumers should swap
    /// `chip_temp` and `board_temp` when this is set, regardless of which
    /// fan/temp controller they're using.
    pub temp_flip: bool,
}

impl BoardVersionProfile {
    pub const ALL: [BoardVersionProfile; 29] = [
        Self {
            board_version: "2.2",
            device_model: "max",
            asic_model: "BM1397",
            model: BitAxeModel::Max,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "102",
            device_model: "max",
            asic_model: "BM1397",
            model: BitAxeModel::Max,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "0.11",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "201",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "202",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "203",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "204",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "205",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "207",
            device_model: "ultra",
            asic_model: "BM1366",
            model: BitAxeModel::Ultra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "302",
            device_model: "hex",
            asic_model: "BM1366",
            model: BitAxeModel::HexUltra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 40,
            temp_flip: false,
        },
        Self {
            board_version: "303",
            device_model: "hex",
            asic_model: "BM1366",
            model: BitAxeModel::HexUltra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 40,
            temp_flip: false,
        },
        Self {
            board_version: "400",
            device_model: "supra",
            asic_model: "BM1368",
            model: BitAxeModel::Supra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "401",
            device_model: "supra",
            asic_model: "BM1368",
            model: BitAxeModel::Supra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Ds4432u,
            has_ina260: true,
            emc_internal_temp: true,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 5,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        Self {
            board_version: "402",
            device_model: "supra",
            asic_model: "BM1368",
            model: BitAxeModel::Supra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 8,
            temp_flip: false,
        },
        Self {
            board_version: "403",
            device_model: "supra",
            asic_model: "BM1368",
            model: BitAxeModel::Supra,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 8,
            temp_flip: false,
        },
        Self {
            board_version: "600",
            device_model: "gamma",
            asic_model: "BM1370",
            model: BitAxeModel::Gamma,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 19,
            temp_flip: false,
        },
        Self {
            board_version: "601",
            device_model: "gamma",
            asic_model: "BM1370",
            model: BitAxeModel::Gamma,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 19,
            temp_flip: false,
        },
        Self {
            board_version: "602",
            device_model: "gamma",
            asic_model: "BM1370",
            model: BitAxeModel::Gamma,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 22,
            temp_flip: false,
        },
        Self {
            // Gamma 603 — BM1370, hardware config identical to 602 (EMC2101 +
            // TPS546, ideality 0x24, 22 W target). Added for board-version parity
            // with ESP-Miner master `device_config.h`, which added 603 after our
            // vendored clone snapshot. Lets DCENT_OS auto-identify a 603 Gamma
            // flashed from stock AxeOS instead of falling back to the default.
            board_version: "603",
            device_model: "gamma",
            asic_model: "BM1370",
            model: BitAxeModel::Gamma,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 22,
            temp_flip: false,
        },
        Self {
            board_version: "650",
            device_model: "gammaduo",
            asic_model: "BM1370",
            model: BitAxeModel::GammaDuo,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 35,
            temp_flip: false,
        },
        Self {
            board_version: "701",
            device_model: "suprahex",
            asic_model: "BM1368",
            model: BitAxeModel::HexSupra,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 90,
            temp_flip: false,
        },
        Self {
            board_version: "702",
            device_model: "suprahex",
            asic_model: "BM1368",
            model: BitAxeModel::HexSupra,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 90,
            temp_flip: false,
        },
        Self {
            board_version: "801",
            device_model: "gammaturbo",
            asic_model: "BM1370",
            model: BitAxeModel::GammaTurbo,
            live_proof: LiveProof::FocusedRun,
            fan_controller: FanControllerKind::Emc2103,
            temp_sensor: TempSensorKind::Emc2103,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x24,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 36,
            temp_flip: true,
        },
        // ── DCENT_axe BM1397 family (D-Central) ──
        // Single 1x — EMC2101 fan/temp like the BitAxe Max, but the power path
        // is a TPS546D24A PMBus VRM at 0x24 (EN on GPIO10, ACTIVE-HIGH) with NO
        // DS4432U and NO INA260 — verified from the dcent-axe-BM1397 schematic
        // netlist (PREFAB_DESIGN_REVIEW_2026-07-08 R-10). The old Ds4432u +
        // has_ina260 row made `normalize_power_pins` treat GPIO10 as active-LOW,
        // which inverted fail-closed "power OFF" into driving the VRM rail ON.
        //
        // LEGACY ALIAS — the canonical board_version is `9010` (see the 9###
        // registry rows below; BOARD_VERSION_REGISTRY.md §5 migration
        // 900→9010 / 910→9040 / 920→9060). These 3-digit rows are kept so any
        // NVS blob written before the migration still resolves; no fabricated
        // board reports them (Phase 0), and `default_for_model` now points at
        // the canonical 9### rows.
        Self {
            board_version: "900",
            device_model: "dcentaxe_bm1397",
            asic_model: "BM1397",
            model: BitAxeModel::DcentAxeBm1397,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        // Quad 4x — single UART daisy chain, EMC2302 dual fan + TPS546 (Hex-class
        // power/fan/temp pairing) with a single parallel voltage domain.
        Self {
            board_version: "910",
            device_model: "dcentaxe_quad_bm1397",
            asic_model: "BM1397",
            model: BitAxeModel::DcentAxeQuadBm1397,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 48,
            temp_flip: false,
        },
        // Hex 6x — single UART daisy chain, EMC2302 dual fan + TPS546, 3 series
        // voltage domains (mirrors the Hex Ultra / Hex Supra topology).
        Self {
            board_version: "920",
            device_model: "dcentaxe_hex_bm1397",
            asic_model: "BM1397",
            model: BitAxeModel::DcentAxeHexBm1397,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 72,
            temp_flip: false,
        },
        // ── DCENT_axe `9###` registry rows (CANONICAL — BOARD_VERSION_REGISTRY.md §5) ──
        // Wired live 2026-07-11 (operator-authorized, FULL_PREFAB_REVIEW_2026-07-11
        // H-3): the dcent-axe-BM1397 hardware self-describes `board_version=9010`
        // in its board_config.json, so provisioning must resolve it here instead
        // of falling back to the model default / "custom board" lab bypass.
        // Encoding is `9 C F R` (9=DCENT_axe namespace, C=0 BM1397, F=ASIC count,
        // R=rev 0). Electrical profiles are byte-identical to the legacy
        // 900/910/920 rows above (same boards, renumbered — clean Phase-0 rename).
        // Keep these rows byte-parallel with the toolbox mirror
        // `dcent-toolbox/.../core/board_catalog.py` `ESP_BOARD_VERSION_PROFILES`.
        //
        // 9010 — DCENT_axe BM1397 Single (canonical for legacy 900).
        Self {
            board_version: "9010",
            device_model: "dcentaxe_bm1397",
            asic_model: "BM1397",
            model: BitAxeModel::DcentAxeBm1397,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2101,
            temp_sensor: TempSensorKind::Emc2101,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 0,
            power_consumption_target_w: 12,
            temp_flip: false,
        },
        // 9040 — DCENT_axe Quad BM1397 (canonical for legacy 910).
        Self {
            board_version: "9040",
            device_model: "dcentaxe_quad_bm1397",
            asic_model: "BM1397",
            model: BitAxeModel::DcentAxeQuadBm1397,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 48,
            temp_flip: false,
        },
        // 9060 — DCENT_axe Hex BM1397 (canonical for legacy 920).
        Self {
            board_version: "9060",
            device_model: "dcentaxe_hex_bm1397",
            asic_model: "BM1397",
            model: BitAxeModel::DcentAxeHexBm1397,
            live_proof: LiveProof::Host,
            fan_controller: FanControllerKind::Emc2302,
            temp_sensor: TempSensorKind::Tmp1075,
            power_controller: PowerControllerKind::Tps546,
            has_ina260: false,
            emc_internal_temp: false,
            emc_ideality_factor: 0x12,
            emc_beta_compensation: 0x00,
            temp_offset_c: 10,
            power_consumption_target_w: 72,
            temp_flip: false,
        },
    ];

    pub fn all() -> &'static [BoardVersionProfile] {
        &Self::ALL
    }

    pub fn plug_sense(&self) -> bool {
        matches!(
            self.board_version,
            "2.2" | "102" | "0.11" | "201" | "202" | "203" | "204" | "205" | "400" | "401"
        )
    }

    pub fn asic_enable(&self) -> bool {
        matches!(
            self.board_version,
            "2.2" | "102" | "0.11" | "201" | "202" | "203" | "205" | "400" | "401"
        )
    }

    pub fn display_kind(&self) -> DisplayKind {
        self.model.display_kind()
    }

    pub fn find(board_version: &str) -> Option<&'static Self> {
        let normalized = board_version.trim();
        Self::ALL
            .iter()
            .find(|profile| profile.board_version == normalized)
    }

    pub fn default_for_model(model: BitAxeModel) -> &'static Self {
        match model {
            BitAxeModel::Max => Self::find("102").unwrap(),
            BitAxeModel::Ultra => Self::find("207").unwrap(),
            BitAxeModel::HexUltra => Self::find("302").unwrap(),
            BitAxeModel::Supra => Self::find("402").unwrap(),
            BitAxeModel::HexSupra => Self::find("701").unwrap(),
            BitAxeModel::Gamma => Self::find("601").unwrap(),
            BitAxeModel::GammaDuo => Self::find("650").unwrap(),
            BitAxeModel::GammaTurbo => Self::find("801").unwrap(),
            // Touch variants reuse the underlying mining board profile;
            // BAP is a purely orthogonal accessory.
            BitAxeModel::Touch => Self::find("601").unwrap(),
            BitAxeModel::GtTouch => Self::find("801").unwrap(),
            BitAxeModel::NerdNOS => Self::find("102").unwrap(),
            BitAxeModel::NerdAxe => Self::find("601").unwrap(),
            BitAxeModel::NerdQaxePlus => Self::find("402").unwrap(),
            BitAxeModel::NerdQaxePP => Self::find("601").unwrap(),
            // Canonical `9###` registry rows (BOARD_VERSION_REGISTRY.md §5
            // migration 900→9010 / 910→9040 / 920→9060). The legacy 3-digit
            // rows stay resolvable via `find` for pre-migration NVS blobs.
            BitAxeModel::DcentAxeBm1397 => Self::find("9010").unwrap(),
            BitAxeModel::DcentAxeQuadBm1397 => Self::find("9040").unwrap(),
            BitAxeModel::DcentAxeHexBm1397 => Self::find("9060").unwrap(),
        }
    }

    pub fn infer(board_version: &str, device_model: &str, asic_model: &str) -> &'static Self {
        if let Some(profile) = Self::find(board_version) {
            return profile;
        }

        let model_hint = device_model.trim().to_ascii_lowercase();
        let asic_hint = asic_model.trim().to_ascii_uppercase();

        match model_hint.as_str() {
            "2.2" | "max" => Self::find("102").unwrap(),
            "0.11" | "ultra" => {
                if asic_hint == "BM1366" {
                    Self::find("201").unwrap()
                } else {
                    Self::find("207").unwrap()
                }
            }
            "hex" | "hexultra" | "ultrahex" => Self::find("302").unwrap(),
            "supra" => {
                if asic_hint == "BM1368" {
                    Self::find("402").unwrap()
                } else {
                    Self::find("400").unwrap()
                }
            }
            "suprahex" => Self::find("701").unwrap(),
            "gamma" => Self::find("601").unwrap(),
            "gammaduo" => Self::find("650").unwrap(),
            "gammaturbo" | "gt" => Self::find("801").unwrap(),
            _ => {
                warn!(
                    "Unknown board version '{}' (device_model='{}', asic_model='{}'), using model default",
                    board_version, device_model, asic_model
                );
                if let Some(model) = BitAxeModel::from_device_model(device_model) {
                    return Self::default_for_model(model);
                }
                match asic_hint.as_str() {
                    "BM1397" => Self::find("102").unwrap(),
                    "BM1368" => Self::find("402").unwrap(),
                    "BM1370" => Self::find("601").unwrap(),
                    _ => Self::find("201").unwrap(),
                }
            }
        }
    }
}

/// Complete board configuration — pins, voltages, and power parameters.
///
/// All GPIO pin numbers are configurable to support different board revisions.
/// Voltage limits are enforced by the power management layer as a safety measure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    /// Board model identifier
    pub model: BitAxeModel,
    /// Runtime board version from NVS / AxeOS config.
    pub board_version: String,
    /// Runtime device model from NVS / AxeOS config.
    pub device_model: String,
    /// Runtime ASIC model from NVS / AxeOS config.
    pub asic_model: String,
    /// Number of ASICs on the board
    pub asic_count: u8,
    /// Runtime fan controller selection.
    pub fan_controller: FanControllerKind,
    /// Runtime temperature sensor selection.
    pub temp_sensor: TempSensorKind,
    /// Runtime power controller selection.
    pub power_controller: PowerControllerKind,
    /// True when the board exposes an INA260 power monitor.
    pub has_ina260: bool,
    /// Board display hardware kind. This is separate from the compiled display driver.
    pub display_kind: DisplayKind,
    /// True when EMC internal temperature should be trusted.
    pub emc_internal_temp: bool,
    /// EMC diode ideality factor to apply when supported.
    pub emc_ideality_factor: u8,
    /// EMC beta compensation to apply when supported.
    pub emc_beta_compensation: u8,
    /// Temperature offset from ESP-Miner board tables.
    pub temp_offset_c: i8,
    /// Power target from ESP-Miner board tables.
    pub power_consumption_target_w: u16,
    /// Whether barrel-jack plug sensing should gate board power-up.
    pub plug_sense: bool,
    /// Stock ESP-Miner ASIC-enable flag for this board.
    pub asic_enable: bool,

    // --- GPIO pin assignments ---
    /// UART TX pin (ESP32 -> ASIC RX)
    pub uart_tx_pin: i32,
    /// UART RX pin (ASIC TX -> ESP32)
    pub uart_rx_pin: i32,
    /// I2C SDA pin (shared bus for power ICs, temp sensors)
    pub i2c_sda_pin: i32,
    /// I2C SCL pin
    pub i2c_scl_pin: i32,
    /// Barrel-jack plug-sense input pin, -1 if unused.
    pub plug_sense_pin: i32,
    /// Fan PWM output pin (LEDC channel), -1 if no fan
    pub fan_pwm_pin: i32,
    /// Fan tachometer input pin (pulse counting), -1 if no fan
    pub fan_tach_pin: i32,
    /// Status LED pin
    pub led_pin: i32,
    /// ASIC chain reset pin (active low — pull low to reset, high for normal operation)
    pub asic_reset_pin: i32,
    /// Buck converter enable pin (controls TPS546 power stage), -1 if not applicable
    pub buck_enable_pin: i32,
    /// Buck enable is active-low (true for Max/Ultra with DS4432U, false for Gamma with TPS546)
    pub buck_enable_active_low: bool,

    // --- Frequency defaults ---
    /// Default ASIC hash frequency in MHz
    pub default_frequency: f32,

    // --- Voltage safety limits ---
    /// Default core voltage in millivolts
    pub default_voltage_mv: u16,
    /// Maximum safe core voltage in millivolts — NEVER exceed this
    pub max_voltage_mv: u16,
    /// Minimum operating core voltage in millivolts
    pub min_voltage_mv: u16,

    // --- Power IC configuration ---
    /// Number of voltage domains (1 for single ASIC, 3 for Hex with series chain)
    pub voltage_domains: u16,
    /// Power offset in watts for board-level power not measured by regulator
    pub power_offset_w: f32,
    /// Pass-5 audit: per ESP-Miner PR #1616, some EMC2103 boards (currently
    /// only v801 GT) have the internal/external sensor mapping physically
    /// swapped. Consumers should swap chip_temp / board_temp reads when set.
    pub temp_flip: bool,
    /// XPSAFE-7: operator-asserted "this single-fan board HAS a tachometer wire
    /// connected." Default `false` (matches every shipping board and the prior
    /// behavior). When an operator who knows their fan is wired sets this, the
    /// boot tach proof and the runtime "tach must be >0 while fan is driven"
    /// rule that Hex/GT boards already enforce become fail-closed for this
    /// board too (consumed by the `main.rs` boot/runtime gates — see
    /// [`tach_proof_required`](Self::tach_proof_required)). Genuinely tachless
    /// boards leave it `false` and keep the existing `fan1_ever_seen` heuristic
    /// + the 90/95/105 C thermal ladder as the backstop.
    pub fan_tach_present: bool,
}

/// I2C SDA pin — compile-time selected by pin family feature.
#[inline]
pub const fn i2c_sda_gpio() -> i32 {
    #[cfg(feature = "pins-bitaxe")]
    {
        47
    }
    #[cfg(feature = "pins-nerd")]
    {
        18
    } // TTGO T-Display S3: GPIO18 = I2C SDA
}

/// I2C SCL pin — compile-time selected by pin family feature.
#[inline]
pub const fn i2c_scl_gpio() -> i32 {
    #[cfg(feature = "pins-bitaxe")]
    {
        48
    }
    #[cfg(feature = "pins-nerd")]
    {
        17
    } // TTGO T-Display S3: GPIO17 = I2C SCL
}

/// UART TX pin — compile-time selected by pin family feature.
#[inline]
pub const fn uart_tx_gpio() -> i32 {
    #[cfg(feature = "pins-bitaxe")]
    {
        17
    }
    #[cfg(feature = "pins-nerd")]
    {
        43
    } // TTGO T-Display S3: GPIO43 = UART TX
}

/// UART RX pin — compile-time selected by pin family feature.
#[inline]
pub const fn uart_rx_gpio() -> i32 {
    #[cfg(feature = "pins-bitaxe")]
    {
        18
    }
    #[cfg(feature = "pins-nerd")]
    {
        44
    } // TTGO T-Display S3: GPIO44 = UART RX
}

impl BoardConfig {
    /// Create the default configuration for a given board model.
    ///
    /// Pin assignments are based on the open-source schematics for each board.
    /// These can be overridden after construction for custom boards.
    pub fn for_model(model: BitAxeModel) -> Self {
        Self::for_profile_with_model(BoardVersionProfile::default_for_model(model), model)
    }

    pub fn for_profile(profile: &BoardVersionProfile) -> Self {
        Self::for_profile_with_model(profile, profile.model)
    }

    pub fn for_profile_with_model(profile: &BoardVersionProfile, model: BitAxeModel) -> Self {
        let sda = i2c_sda_gpio();
        let scl = i2c_scl_gpio();

        // Common pin assignments (UART TX/RX are the same across all boards)
        let common = BoardConfig {
            model,
            board_version: profile.board_version.to_string(),
            device_model: model.canonical_key().to_string(),
            asic_model: profile.asic_model.to_string(),
            asic_count: model.asic_count(),
            fan_controller: profile.fan_controller,
            temp_sensor: profile.temp_sensor,
            power_controller: profile.power_controller,
            has_ina260: profile.has_ina260,
            display_kind: if profile.model == model {
                profile.display_kind()
            } else {
                model.display_kind()
            },
            emc_internal_temp: profile.emc_internal_temp,
            emc_ideality_factor: profile.emc_ideality_factor,
            emc_beta_compensation: profile.emc_beta_compensation,
            temp_offset_c: profile.temp_offset_c,
            power_consumption_target_w: profile.power_consumption_target_w,
            temp_flip: profile.temp_flip,
            // XPSAFE-7: default-OFF — no shipping board asserts a wired tach, so
            // every board keeps its prior boot/runtime fan-proof behavior until
            // an operator opts in via config.
            fan_tach_present: false,
            plug_sense: profile.plug_sense(),
            asic_enable: profile.asic_enable(),
            uart_tx_pin: uart_tx_gpio(),
            uart_rx_pin: uart_rx_gpio(),
            i2c_sda_pin: sda,
            i2c_scl_pin: scl,
            plug_sense_pin: -1,
            fan_pwm_pin: 11,
            fan_tach_pin: 14,
            led_pin: 4,
            asic_reset_pin: 1,
            buck_enable_pin: 46,
            buck_enable_active_low: false,
            default_frequency: 0.0,
            default_voltage_mv: 0,
            max_voltage_mv: 0,
            min_voltage_mv: 0,
            voltage_domains: 1,
            power_offset_w: 2.0,
        };

        if model.is_hex() {
            info!("Hex board: 6-ASIC single UART daisy chain, 3 voltage domains, 12V input");
        }

        let mut board = match model {
            // ── BitAxe family ──
            BitAxeModel::Max => BoardConfig {
                default_frequency: 425.0,
                default_voltage_mv: 1400,
                max_voltage_mv: 1550,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0, // ESP-Miner: FAMILY_MAX.power_offset = 5
                ..common
            },
            BitAxeModel::Ultra => BoardConfig {
                default_frequency: 485.0,
                default_voltage_mv: 1200,
                max_voltage_mv: 1400,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0, // ESP-Miner: FAMILY_ULTRA.power_offset = 5
                ..common
            },
            BitAxeModel::HexUltra => BoardConfig {
                default_frequency: 485.0,
                default_voltage_mv: 1200,
                max_voltage_mv: 1350,
                min_voltage_mv: 850,
                voltage_domains: 3,
                power_offset_w: 12.0, // ESP-Miner: FAMILY_HEX.power_offset = 12
                ..common
            },
            BitAxeModel::Supra => BoardConfig {
                default_frequency: 490.0,
                default_voltage_mv: 1166,
                max_voltage_mv: 1400,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0, // ESP-Miner: FAMILY_SUPRA.power_offset = 5
                ..common
            },
            BitAxeModel::HexSupra => BoardConfig {
                default_frequency: 490.0,
                default_voltage_mv: 1166,
                max_voltage_mv: 1350,
                min_voltage_mv: 850,
                voltage_domains: 3,
                power_offset_w: 25.0, // ESP-Miner: FAMILY_SUPRA_HEX.power_offset = 25
                ..common
            },
            BitAxeModel::Gamma => BoardConfig {
                default_frequency: 525.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0, // ESP-Miner: FAMILY_GAMMA.power_offset = 5
                ..common
            },
            BitAxeModel::GammaDuo => BoardConfig {
                asic_count: 2,
                default_frequency: 400.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0,
                ..common
            },
            BitAxeModel::GammaTurbo => BoardConfig {
                asic_count: 2,
                default_frequency: 525.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 10.0,
                ..common
            },
            // Touch variants are electrically identical to their mining-board
            // base — the only difference is the LVGL accessory hanging off BAP.
            BitAxeModel::Touch => BoardConfig {
                asic_count: 1,
                default_frequency: 525.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0, // Same as Gamma (Touch = Gamma + BAP)
                ..common
            },
            BitAxeModel::GtTouch => BoardConfig {
                asic_count: 2,
                default_frequency: 525.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 10.0,
                ..common
            },

            // ── Nerd family ──
            BitAxeModel::NerdNOS => BoardConfig {
                // NerdNOS: BM1397, USB-C ~8W, fixed TPSM863257RDX, no fan, headless
                // GPIO10 = regulator EN pin — active HIGH to enable power
                fan_pwm_pin: -1, // No fan
                fan_tach_pin: -1,
                default_frequency: 400.0,
                default_voltage_mv: 1200, // Fixed — not adjustable
                max_voltage_mv: 1200,
                min_voltage_mv: 1200,
                voltage_domains: 1,
                power_offset_w: 1.0,
                ..common
            },
            BitAxeModel::NerdAxe => BoardConfig {
                // NerdAxe: BM1370, TTGO T-Display S3, TPS546D, EMC2101
                default_frequency: 525.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 2.0,
                ..common
            },
            BitAxeModel::NerdQaxePlus => BoardConfig {
                // NerdQaxe+: 4x BM1368, UART daisy chain
                asic_count: 4,
                default_frequency: 490.0,
                default_voltage_mv: 1166,
                max_voltage_mv: 1400,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0,
                ..common
            },
            BitAxeModel::NerdQaxePP => BoardConfig {
                // NerdQaxe++: 4x BM1370, UART daisy chain
                asic_count: 4,
                default_frequency: 525.0,
                default_voltage_mv: 1150,
                max_voltage_mv: 1350,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 8.0,
                ..common
            },

            // ── DCENT_axe family (BM1397) ──
            // fan_pwm_pin honesty (FULL_PREFAB_REVIEW_2026-07-11 LOW): the
            // DCENT_axe boards have NO ESP-driven fan-PWM line — the I2C fan
            // controller (EMC2101 single / EMC2302 Quad+Hex) generates PWM
            // itself, and GPIO11 is unconnected on the BM1397 netlist. The
            // inherited `fan_pwm_pin: 11` was fictional; -1 = "no ESP PWM pin".
            // The tach wire IS routed (GPIO14 / FAN_TACH net), so fan_tach_pin
            // stays; `fan_tach_present` remains default-false opt-in (XPSAFE-7).
            // Single 1x: same chip envelope as the BitAxe Max BM1397.
            BitAxeModel::DcentAxeBm1397 => BoardConfig {
                fan_pwm_pin: -1, // EMC2101 generates PWM; GPIO11 unconnected
                default_frequency: 425.0,
                default_voltage_mv: 1400,
                max_voltage_mv: 1550,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 5.0,
                ..common
            },
            // Quad 4x: single UART daisy chain, one parallel voltage domain.
            BitAxeModel::DcentAxeQuadBm1397 => BoardConfig {
                fan_pwm_pin: -1, // EMC2302 generates PWM; no ESP PWM line
                asic_count: 4,
                default_frequency: 425.0,
                default_voltage_mv: 1400,
                max_voltage_mv: 1550,
                min_voltage_mv: 1000,
                voltage_domains: 1,
                power_offset_w: 10.0,
                ..common
            },
            // Hex 6x: single UART daisy chain, 3 series voltage domains (Hex-class).
            BitAxeModel::DcentAxeHexBm1397 => BoardConfig {
                fan_pwm_pin: -1, // EMC2302 generates PWM; no ESP PWM line
                asic_count: 6,
                default_frequency: 425.0,
                default_voltage_mv: 1400,
                max_voltage_mv: 1550,
                min_voltage_mv: 1000,
                voltage_domains: 3,
                power_offset_w: 12.0,
                ..common
            },
        };

        board.normalize_power_pins();
        board
    }

    fn normalize_power_pins(&mut self) {
        if self.model.is_dcent_axe() {
            // DCENT_axe family (PREFAB_DESIGN_REVIEW_2026-07-08 R-10): the
            // TPS546D24A EN pin is wired to GPIO10, ACTIVE-HIGH, verified from
            // the dcent-axe-BM1397 schematic netlist. This arm is keyed on the
            // MODEL, not the power-controller kind, so the pin path stays
            // deterministic even if an NVS hardware override rewrites
            // `power_controller`: the generic Tps546 arm below would pick
            // GPIO46 (UNCONNECTED on these boards) and the Ds4432u arm would
            // make GPIO10 active-LOW — inverting fail-closed "power OFF" into
            // driving the VRM rail ON.
            self.buck_enable_pin = 10;
            self.buck_enable_active_low = false;
            self.plug_sense_pin = -1;
        } else if self.model.is_nerd() {
            self.buck_enable_pin = 10;
            self.buck_enable_active_low = false;
            self.plug_sense_pin = -1;
        } else if self.power_controller == PowerControllerKind::Ds4432u {
            self.buck_enable_pin = 10;
            self.buck_enable_active_low = true;
            self.plug_sense_pin = if self.plug_sense { 12 } else { -1 };
        } else {
            self.buck_enable_pin = 46;
            self.buck_enable_active_low = false;
            self.plug_sense_pin = if self.plug_sense { 12 } else { -1 };
        }
    }

    pub fn apply_hardware_config(&mut self, hw: &BoardHardwareConfig) {
        self.plug_sense = hw.plug_sense;
        self.asic_enable = hw.asic_enable;
        self.fan_controller = hw.fan_controller;
        self.temp_sensor = hw.temp_sensor;
        self.power_controller = hw.power_controller;
        self.has_ina260 = hw.has_ina260;
        self.emc_internal_temp = hw.emc_internal_temp;
        self.emc_ideality_factor = hw.emc_ideality_factor;
        self.emc_beta_compensation = hw.emc_beta_compensation;
        self.temp_offset_c = hw.temp_offset_c;
        self.power_consumption_target_w = hw.power_consumption_target_w;
        self.normalize_power_pins();
    }

    pub fn mining_capable(&self) -> bool {
        self.asic_count > 0 && self.default_frequency > 0.0
    }

    pub fn has_trusted_thermal_source_configured(&self) -> bool {
        self.temp_sensor != TempSensorKind::None
            || self.power_controller == PowerControllerKind::Tps546
    }

    pub fn has_display(&self) -> bool {
        self.display_kind.has_hardware()
    }

    pub fn requires_fan_tach(&self) -> bool {
        self.model.is_hex() || self.fan_controller == FanControllerKind::Emc2103
    }

    /// XPSAFE-7: opt a single-fan EMC2101 board into fail-closed tach proof.
    ///
    /// For an operator who knows their fan's tach wire is connected. Setting
    /// this makes [`tach_proof_required`](Self::tach_proof_required) true so the
    /// `main.rs` boot gate proves RPM>0 at startup and the runtime loop treats a
    /// stalled fan as a fault — the same fail-closed posture Hex/GT boards
    /// already get — instead of the lenient `fan1_ever_seen` heuristic that lets
    /// a fan which NEVER spins masquerade as a tachless board.
    pub fn set_fan_tach_present(&mut self, present: bool) {
        self.fan_tach_present = present;
    }

    /// XPSAFE-7: the capability the `main.rs` fan-proof gates should consume.
    ///
    /// True when this board must prove a working tachometer — either because the
    /// hardware inherently requires it ([`requires_fan_tach`](Self::requires_fan_tach):
    /// Hex / EMC2103-GT), OR because the operator asserted a wired tach via
    /// [`set_fan_tach_present`](Self::set_fan_tach_present) (`fan_tach_present`).
    ///
    /// WF-F note: `main.rs` should gate the EMC2101 single-fan boot RPM assert
    /// and the runtime "tach must be >0 while fan>0" rule on THIS method, not on
    /// `requires_fan_tach()` alone, so an opted-in single-fan board fails closed.
    /// When this is false the board legitimately has no tach proof — surface
    /// "no fan proof (heuristic only); thermal ladder is the backstop" in
    /// telemetry / self-test so the operator knows.
    pub fn tach_proof_required(&self) -> bool {
        self.requires_fan_tach() || self.fan_tach_present
    }

    pub fn accessory_mode(&self) -> AccessoryMode {
        if self.model.has_bap() {
            AccessoryMode::BapTouch
        } else {
            AccessoryMode::None
        }
    }

    pub fn validate_accessory_mode(&self, requested: AccessoryMode) -> Result<(), &'static str> {
        match (self.accessory_mode(), requested) {
            (AccessoryMode::BapTouch, AccessoryMode::W5500Lan)
            | (AccessoryMode::W5500Lan, AccessoryMode::BapTouch) => Err(
                "BAP Touch and W5500 LAN accessory modes share pins and cannot be enabled together",
            ),
            _ => Ok(()),
        }
    }

    /// Validate that the configuration is internally consistent.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.min_voltage_mv > self.max_voltage_mv {
            return Err("min_voltage_mv must be <= max_voltage_mv");
        }
        if self.default_voltage_mv < self.min_voltage_mv
            || self.default_voltage_mv > self.max_voltage_mv
        {
            return Err("default_voltage_mv must be within [min, max] range");
        }
        if self.asic_count == 0 {
            return Err("asic_count must be at least 1");
        }
        if self.voltage_domains == 0 {
            return Err("voltage_domains must be at least 1");
        }
        if self.mining_capable() && !self.has_trusted_thermal_source_configured() {
            return Err("mining-capable board requires a trusted temperature source");
        }
        Ok(())
    }
}

#[cfg(test)]
mod xpsafe7_fan_tach_capability {
    use super::*;

    // ── XPSAFE-7: default-OFF — no shipping board profile flips the new flag ──
    #[test]
    fn fan_tach_present_defaults_false_for_all_profiles() {
        for profile in BoardVersionProfile::ALL {
            let cfg = BoardConfig::for_profile(&profile);
            assert!(
                !cfg.fan_tach_present,
                "{} ({:?}) must default fan_tach_present=false (opt-in only)",
                profile.board_version, profile.model
            );
        }
    }

    // ── XPSAFE-7: tach_proof_required matches requires_fan_tach when not opted-in ─
    // Default-preserving: with the flag off, the new capability is exactly the
    // old hardware-required predicate, so existing Hex/GT behavior is unchanged
    // and single-fan EMC2101 boards still get the heuristic (no proof required).
    #[test]
    fn tach_proof_required_equals_requires_fan_tach_by_default() {
        for profile in BoardVersionProfile::ALL {
            let cfg = BoardConfig::for_profile(&profile);
            assert_eq!(
                cfg.tach_proof_required(),
                cfg.requires_fan_tach(),
                "{} ({:?}): with fan_tach_present=false the capability must equal \
                 the hardware-required predicate",
                profile.board_version,
                profile.model
            );
        }
    }

    // ── XPSAFE-7: Hex / EMC2103 boards already require proof regardless of flag ──
    #[test]
    fn hex_and_emc2103_always_require_proof() {
        // A Hex board (inherently requires tach) keeps proof required even with
        // the opt-in flag left off.
        let hex = BoardConfig::for_model(BitAxeModel::HexUltra);
        assert!(hex.requires_fan_tach());
        assert!(hex.tach_proof_required());

        // An EMC2103 (GT-class) board requires proof via fan_controller too.
        let gt = BoardConfig::for_model(BitAxeModel::GammaTurbo);
        if gt.fan_controller == FanControllerKind::Emc2103 {
            assert!(gt.requires_fan_tach());
            assert!(gt.tach_proof_required());
        }
    }

    // ── XPSAFE-7: opting a single-fan EMC2101 board in makes proof required ──
    #[test]
    fn opt_in_single_fan_board_becomes_fail_closed() {
        // A standard single-ASIC EMC2101 board: no inherent tach requirement.
        let mut cfg = BoardConfig::for_model(BitAxeModel::Ultra);
        assert_eq!(cfg.fan_controller, FanControllerKind::Emc2101);
        assert!(
            !cfg.requires_fan_tach(),
            "single-fan EMC2101 board must not inherently require tach"
        );
        assert!(
            !cfg.tach_proof_required(),
            "before opt-in, an EMC2101 single-fan board relies on the heuristic"
        );

        // Operator asserts the tach wire is connected.
        cfg.set_fan_tach_present(true);
        assert!(cfg.fan_tach_present);
        assert!(
            cfg.tach_proof_required(),
            "after opt-in, the board must require fail-closed tach proof"
        );
        // requires_fan_tach() (pure hardware predicate) is unchanged by the flag.
        assert!(
            !cfg.requires_fan_tach(),
            "opt-in must NOT rewrite the hardware-required predicate"
        );

        // Toggling back off restores the heuristic posture.
        cfg.set_fan_tach_present(false);
        assert!(!cfg.tach_proof_required());
    }
}

#[cfg(test)]
mod public_bitaxe_install_targets {
    use super::*;

    const PUBLIC_TARGETS: [(BitAxeModel, &str, &str, u8, u16); 6] = [
        (BitAxeModel::Max, "bitaxe-max", "max", 1, 0x1397),
        (BitAxeModel::Ultra, "bitaxe-ultra", "ultra", 1, 0x1366),
        (BitAxeModel::Supra, "bitaxe-supra", "supra", 1, 0x1368),
        (BitAxeModel::Gamma, "bitaxe-gamma", "gamma", 1, 0x1370),
        (
            BitAxeModel::HexUltra,
            "bitaxe-hex-ultra",
            "hexultra",
            6,
            0x1366,
        ),
        (
            BitAxeModel::HexSupra,
            "bitaxe-hex-supra",
            "suprahex",
            6,
            0x1368,
        ),
    ];

    #[test]
    fn requested_public_install_targets_keep_canonical_identity() {
        for (model, board_target, device_model, asic_count, chip_id) in PUBLIC_TARGETS {
            assert_eq!(model.board_target(), board_target);
            assert_eq!(model.canonical_key(), device_model);
            assert_eq!(BitAxeModel::from_device_model(device_model), Some(model));
            assert_eq!(model.asic_count(), asic_count);
            assert_eq!(model.expected_chip_id(), chip_id);

            let board = BoardConfig::for_model(model);
            assert_eq!(board.model, model);
            assert_eq!(
                board.device_model, device_model,
                "{board_target} BoardConfig device_model must be canonical"
            );
            assert_eq!(board.asic_count, asic_count);
            assert!(board.validate().is_ok(), "{board_target} must validate");
            assert!(
                board.mining_capable(),
                "{board_target} must be mining-capable"
            );
        }
    }
}

#[cfg(test)]
mod dcent_axe_bm1397_variants {
    use super::*;

    // Canonical `9###` registry versions (BOARD_VERSION_REGISTRY.md §5); the
    // legacy 3-digit aliases are pinned separately below.
    const DCENT_AXE_BM1397: [(BitAxeModel, &str, u8, &str); 3] = [
        (BitAxeModel::DcentAxeBm1397, "dcentaxe_bm1397", 1, "9010"),
        (
            BitAxeModel::DcentAxeQuadBm1397,
            "dcentaxe_quad_bm1397",
            4,
            "9040",
        ),
        (
            BitAxeModel::DcentAxeHexBm1397,
            "dcentaxe_hex_bm1397",
            6,
            "9060",
        ),
    ];

    // ── Legacy 900/910/920 aliases stay resolvable and byte-identical to the
    // canonical 9### rows (same board, Phase-0 rename — a pre-migration NVS
    // blob must keep resolving to the exact same hardware profile). ──
    #[test]
    fn legacy_900_family_aliases_resolve_identically_to_canonical_rows() {
        for (legacy, canonical) in [("900", "9010"), ("910", "9040"), ("920", "9060")] {
            let l = BoardVersionProfile::find(legacy)
                .unwrap_or_else(|| panic!("legacy row {legacy} must stay resolvable"));
            let c = BoardVersionProfile::find(canonical)
                .unwrap_or_else(|| panic!("canonical row {canonical} must exist"));
            assert_eq!(l.model, c.model, "{legacy}/{canonical}: model");
            assert_eq!(
                l.device_model, c.device_model,
                "{legacy}/{canonical}: device_model"
            );
            assert_eq!(l.asic_model, c.asic_model, "{legacy}/{canonical}: asic");
            assert_eq!(
                l.fan_controller, c.fan_controller,
                "{legacy}/{canonical}: fan"
            );
            assert_eq!(l.temp_sensor, c.temp_sensor, "{legacy}/{canonical}: temp");
            assert_eq!(
                l.power_controller, c.power_controller,
                "{legacy}/{canonical}: power"
            );
            assert_eq!(l.has_ina260, c.has_ina260, "{legacy}/{canonical}: ina260");
            assert_eq!(
                l.emc_internal_temp, c.emc_internal_temp,
                "{legacy}/{canonical}: emc_internal_temp"
            );
            assert_eq!(
                l.emc_ideality_factor, c.emc_ideality_factor,
                "{legacy}/{canonical}: ideality"
            );
            assert_eq!(
                l.temp_offset_c, c.temp_offset_c,
                "{legacy}/{canonical}: offset"
            );
            assert_eq!(
                l.power_consumption_target_w, c.power_consumption_target_w,
                "{legacy}/{canonical}: power target"
            );
            assert_eq!(l.temp_flip, c.temp_flip, "{legacy}/{canonical}: temp_flip");
            assert_eq!(l.live_proof, c.live_proof, "{legacy}/{canonical}: proof");
        }
        // The model default is the CANONICAL row, not the legacy alias.
        assert_eq!(
            BoardVersionProfile::default_for_model(BitAxeModel::DcentAxeBm1397).board_version,
            "9010"
        );
        assert_eq!(
            BoardVersionProfile::default_for_model(BitAxeModel::DcentAxeQuadBm1397).board_version,
            "9040"
        );
        assert_eq!(
            BoardVersionProfile::default_for_model(BitAxeModel::DcentAxeHexBm1397).board_version,
            "9060"
        );
    }

    // ── fan_pwm_pin honesty (FULL_PREFAB_REVIEW_2026-07-11 LOW): DCENT_axe has
    // no ESP-driven fan-PWM line (I2C fan controller generates PWM; GPIO11 is
    // unconnected on the BM1397 netlist). Other boards keep their stock pin. ──
    #[test]
    fn dcentaxe_fan_pwm_pin_is_honest_minus_one() {
        for (model, _key, _count, _ver) in DCENT_AXE_BM1397 {
            let cfg = BoardConfig::for_model(model);
            assert_eq!(
                cfg.fan_pwm_pin, -1,
                "{model:?}: fan PWM is generated by the I2C fan controller, \
                 not an ESP GPIO — the pin claim must be -1"
            );
            // The tach input IS wired (FAN_TACH net / GPIO14) — unchanged.
            assert_eq!(cfg.fan_tach_pin, 14, "{model:?}: tach pin unchanged");
        }
        // No collateral change to any other family's arm.
        assert_eq!(BoardConfig::for_model(BitAxeModel::Max).fan_pwm_pin, 11);
        assert_eq!(BoardConfig::for_model(BitAxeModel::Gamma).fan_pwm_pin, 11);
        assert_eq!(
            BoardConfig::for_model(BitAxeModel::HexUltra).fan_pwm_pin,
            11
        );
        assert_eq!(BoardConfig::for_model(BitAxeModel::NerdNOS).fan_pwm_pin, -1);
    }

    // ── M-7 (FULL_PREFAB_REVIEW_2026-07-11): status-LED kind metadata. Only
    // the netlist-confirmed BM1397 single carries the SK6812; everything else
    // (including Quad/Hex, whose netlists don't exist yet) stays PlainGpio. ──
    #[test]
    fn status_led_kind_is_sk6812_only_for_the_netlist_confirmed_single() {
        assert_eq!(
            BitAxeModel::DcentAxeBm1397.status_led_kind(),
            StatusLedKind::Sk6812
        );
        for model in [
            BitAxeModel::Max,
            BitAxeModel::Ultra,
            BitAxeModel::Supra,
            BitAxeModel::Gamma,
            BitAxeModel::GammaTurbo,
            BitAxeModel::NerdNOS,
            BitAxeModel::DcentAxeQuadBm1397,
            BitAxeModel::DcentAxeHexBm1397,
        ] {
            assert_eq!(
                model.status_led_kind(),
                StatusLedKind::PlainGpio,
                "{model:?}: must stay PlainGpio (no speculative SK6812 claim)"
            );
        }
    }

    // ── The three DCENT_axe SKUs resolve from their canonical device-model key ──
    #[test]
    fn from_device_model_resolves_the_three_skus() {
        for (model, key, _count, _ver) in DCENT_AXE_BM1397 {
            assert_eq!(
                BitAxeModel::from_device_model(key),
                Some(model),
                "device_model '{key}' must resolve to {model:?}"
            );
            // canonical_key round-trips back to the same model.
            assert_eq!(
                BitAxeModel::from_device_model(model.canonical_key()),
                Some(model),
                "canonical_key '{}' must round-trip to {model:?}",
                model.canonical_key()
            );
        }
        // A couple of human/marketing aliases also resolve.
        assert_eq!(
            BitAxeModel::from_device_model("quad_bm1397"),
            Some(BitAxeModel::DcentAxeQuadBm1397)
        );
        assert_eq!(
            BitAxeModel::from_device_model("DCENT_axe Hex BM1397"),
            Some(BitAxeModel::DcentAxeHexBm1397)
        );
    }

    // ── All three drive the BM1397 chip id (detection register 0x00) ──
    #[test]
    fn expected_chip_id_is_bm1397() {
        for (model, _key, _count, _ver) in DCENT_AXE_BM1397 {
            assert_eq!(
                model.expected_chip_id(),
                0x1397,
                "{model:?} must expect chip id 0x1397"
            );
        }
    }

    // ── BAP is populated on every DCENT_axe board ──
    #[test]
    fn has_bap_is_true_for_all_three() {
        for (model, _key, _count, _ver) in DCENT_AXE_BM1397 {
            assert!(model.has_bap(), "{model:?} must report has_bap()==true");
            // Accessory mode follows has_bap and validates against itself.
            let cfg = BoardConfig::for_model(model);
            assert_eq!(cfg.accessory_mode(), AccessoryMode::BapTouch);
            assert!(cfg.validate_accessory_mode(cfg.accessory_mode()).is_ok());
        }
    }

    // ── Chip counts: 1 / 4 / 6 ──
    #[test]
    fn asic_counts_are_one_four_six() {
        for (model, _key, count, _ver) in DCENT_AXE_BM1397 {
            assert_eq!(model.asic_count(), count, "{model:?} chip count");
            assert_eq!(BoardConfig::for_model(model).asic_count, count);
        }
    }

    // ── Each SKU has a profile row that resolves and builds a valid board ──
    #[test]
    fn profiles_resolve_and_build_valid_boards() {
        for (model, key, count, ver) in DCENT_AXE_BM1397 {
            let profile = BoardVersionProfile::find(ver)
                .unwrap_or_else(|| panic!("profile {ver} for {model:?} must exist"));
            assert_eq!(profile.model, model);
            assert_eq!(profile.asic_model, "BM1397");
            assert_eq!(profile.device_model, key);
            assert_eq!(BoardVersionProfile::default_for_model(model).model, model);

            let board = BoardConfig::for_model(model);
            assert_eq!(board.model, model);
            assert_eq!(board.asic_count, count);
            assert!(
                board.validate().is_ok(),
                "{model:?} board config must validate: {:?}",
                board.validate()
            );
            assert!(board.mining_capable(), "{model:?} must be mining-capable");
        }
    }

    // ── ESP-Miner board-version parity (bitaxeorg/ESP-Miner main/device_config.h) ──
    // Every board_version ESP-Miner recognizes MUST resolve here to the correct
    // family + ASIC, so DCENT_OS can be flashed onto ANY existing Bitaxe and
    // auto-identify it by its stored board_version instead of falling back to a
    // default (a wrong family = wrong sensors / power target = a safety risk).
    // When ESP-Miner adds a new board_version, this test flags the gap — that is
    // how the 603 (Gamma) gap was caught during the ESP-Miner cross-check.
    const ESP_MINER_DEVICE_CONFIG_H: &str =
        include_str!("../../");
    const ESP_MINER_FIXTURE_MANIFEST: &str =
        include_str!("../../");

    #[derive(Clone, Copy, Debug)]
    struct UpstreamBoardVersion<'a> {
        version: &'a str,
        model: BitAxeModel,
        asic: &'static str,
    }

    fn c_initializer_field<'a>(row: &'a str, field: &str) -> Option<&'a str> {
        let marker = format!(".{field} = ");
        let value = row.split_once(&marker)?.1;
        let raw = value
            .split(|c| c == ',' || c == '}')
            .next()
            .map(str::trim)?;
        if let Some(quoted) = raw.strip_prefix('"') {
            Some(match quoted.strip_suffix('"') {
                Some(unquoted) => unquoted,
                None => quoted,
            })
        } else {
            Some(raw)
        }
    }

    fn upstream_family_to_model_and_asic(family: &str) -> (BitAxeModel, &'static str) {
        match family {
            "FAMILY_MAX" => (BitAxeModel::Max, "BM1397"),
            "FAMILY_ULTRA" => (BitAxeModel::Ultra, "BM1366"),
            "FAMILY_HEX" => (BitAxeModel::HexUltra, "BM1366"),
            "FAMILY_SUPRA" => (BitAxeModel::Supra, "BM1368"),
            "FAMILY_GAMMA" => (BitAxeModel::Gamma, "BM1370"),
            "FAMILY_GAMMA_DUO" => (BitAxeModel::GammaDuo, "BM1370"),
            "FAMILY_SUPRA_HEX" => (BitAxeModel::HexSupra, "BM1368"),
            "FAMILY_GAMMA_TURBO" => (BitAxeModel::GammaTurbo, "BM1370"),
            other => panic!("unknown ESP-Miner family constant in fixture: {other}"),
        }
    }

    fn parse_esp_miner_default_configs(header: &str) -> Vec<UpstreamBoardVersion<'_>> {
        header
            .lines()
            .filter_map(|line| {
                let row = line.trim();
                if !row.starts_with("{ .board_version = ") {
                    return None;
                }
                let version = c_initializer_field(row, "board_version")
                    .unwrap_or_else(|| panic!("missing board_version field in {row}"));
                let family = c_initializer_field(row, "family")
                    .unwrap_or_else(|| panic!("missing family field in {row}"));
                let (model, asic) = upstream_family_to_model_and_asic(family);
                Some(UpstreamBoardVersion {
                    version,
                    model,
                    asic,
                })
            })
            .collect()
    }

    fn expected_default_board_version_for_model(model: BitAxeModel) -> &'static str {
        match model {
            BitAxeModel::Max => "102",
            BitAxeModel::Ultra => "207",
            BitAxeModel::HexUltra => "302",
            BitAxeModel::Supra => "402",
            BitAxeModel::HexSupra => "701",
            BitAxeModel::Gamma => "601",
            BitAxeModel::GammaDuo => "650",
            BitAxeModel::GammaTurbo => "801",
            BitAxeModel::Touch => "601",
            BitAxeModel::GtTouch => "801",
            BitAxeModel::NerdNOS => "102",
            BitAxeModel::NerdAxe => "601",
            BitAxeModel::NerdQaxePlus => "402",
            BitAxeModel::NerdQaxePP => "601",
            BitAxeModel::DcentAxeBm1397 => "9010",
            BitAxeModel::DcentAxeQuadBm1397 => "9040",
            BitAxeModel::DcentAxeHexBm1397 => "9060",
        }
    }

    #[test]
    fn every_model_has_explicit_default_board_version() {
        const MODELS: &[BitAxeModel] = &[
            BitAxeModel::Max,
            BitAxeModel::Ultra,
            BitAxeModel::HexUltra,
            BitAxeModel::Supra,
            BitAxeModel::HexSupra,
            BitAxeModel::Gamma,
            BitAxeModel::GammaDuo,
            BitAxeModel::GammaTurbo,
            BitAxeModel::Touch,
            BitAxeModel::GtTouch,
            BitAxeModel::NerdNOS,
            BitAxeModel::NerdAxe,
            BitAxeModel::NerdQaxePlus,
            BitAxeModel::NerdQaxePP,
            BitAxeModel::DcentAxeBm1397,
            BitAxeModel::DcentAxeQuadBm1397,
            BitAxeModel::DcentAxeHexBm1397,
        ];

        for &model in MODELS {
            let expected = expected_default_board_version_for_model(model);
            let profile = BoardVersionProfile::default_for_model(model);
            assert_eq!(
                profile.board_version, expected,
                "{model:?} default board_version"
            );
            assert!(
                BoardVersionProfile::find(expected).is_some(),
                "{model:?} default board_version {expected} must resolve"
            );

            let board = BoardConfig::for_model(model);
            assert_eq!(board.model, model, "{model:?} default board model");
            assert_eq!(
                board.board_version, expected,
                "{model:?} default BoardConfig board_version"
            );
        }
    }

    #[test]
    fn nerd_family_defaults_preserve_model_specific_display_and_power_metadata() {
        let nerdaxe = BoardConfig::for_model(BitAxeModel::NerdAxe);
        assert_eq!(nerdaxe.board_version, "601");
        assert_eq!(nerdaxe.power_controller, PowerControllerKind::Tps546);
        assert_eq!(nerdaxe.display_kind, DisplayKind::TDisplayS3);
        assert!(nerdaxe.has_display());

        let qaxe_plus = BoardConfig::for_model(BitAxeModel::NerdQaxePlus);
        assert_eq!(qaxe_plus.board_version, "402");
        assert_eq!(qaxe_plus.asic_model, "BM1368");
        assert_eq!(
            qaxe_plus.power_controller,
            PowerControllerKind::Tps546,
            "NerdQaxe+ must not inherit the DS4432U Supra 400 profile"
        );
        assert_eq!(qaxe_plus.display_kind, DisplayKind::TDisplayS3);
        assert!(qaxe_plus.has_display());

        let qaxe_pp = BoardConfig::for_model(BitAxeModel::NerdQaxePP);
        assert_eq!(qaxe_pp.board_version, "601");
        assert_eq!(qaxe_pp.power_controller, PowerControllerKind::Tps546);
        assert_eq!(qaxe_pp.display_kind, DisplayKind::TDisplayS3);
        assert!(qaxe_pp.has_display());

        let nerdnos = BoardConfig::for_model(BitAxeModel::NerdNOS);
        assert_eq!(nerdnos.display_kind, DisplayKind::None);
        assert!(!nerdnos.has_display());
    }

    #[test]
    fn esp_miner_fixture_manifest_records_last_sync_without_network_fetch() {
        let manifest = ESP_MINER_FIXTURE_MANIFEST;
        for required in [
            "\"schema\": \"dcentos-esp.upstream-fixture.v1\"",
            "\"source_repository\": \"https://github.com/bitaxeorg/ESP-Miner\"",
            "\"source_file\": \"main/device_config.h\"",
            "\"local_file\": \"device_config.h\"",
            "\"upstream_commit\": \"b4c3dcbb9ed36c2a0eb9ae7d57a4132e8c52c14b\"",
            "\"upstream_commit_date\": \"2026-07-03\"",
            "\"last_synced_on\": \"2026-07-04\"",
            "\"network_ci_policy\": \"no_network_fetch_in_ci\"",
        ] {
            assert!(
                manifest.contains(required),
                "fixture manifest missing required marker: {required}"
            );
        }
    }

    #[test]
    fn board_version_profiles_pin_display_metadata() {
        let expected = &[
            ("2.2", DisplayKind::Ssd1306),
            ("102", DisplayKind::Ssd1306),
            ("0.11", DisplayKind::Ssd1306),
            ("201", DisplayKind::Ssd1306),
            ("202", DisplayKind::Ssd1306),
            ("203", DisplayKind::Ssd1306),
            ("204", DisplayKind::Ssd1306),
            ("205", DisplayKind::Ssd1306),
            ("207", DisplayKind::Ssd1306),
            ("302", DisplayKind::Ssd1306),
            ("303", DisplayKind::Ssd1306),
            ("400", DisplayKind::Ssd1306),
            ("401", DisplayKind::Ssd1306),
            ("402", DisplayKind::Ssd1306),
            ("403", DisplayKind::Ssd1306),
            ("600", DisplayKind::Ssd1306),
            ("601", DisplayKind::Ssd1306),
            ("602", DisplayKind::Ssd1306),
            ("603", DisplayKind::Ssd1306),
            ("650", DisplayKind::Ssd1306),
            ("701", DisplayKind::Ssd1306),
            ("702", DisplayKind::Ssd1306),
            ("801", DisplayKind::Ssd1306),
            ("900", DisplayKind::Ssd1306),
            ("910", DisplayKind::Ssd1306),
            ("920", DisplayKind::Ssd1306),
            ("9010", DisplayKind::Ssd1306),
            ("9040", DisplayKind::Ssd1306),
            ("9060", DisplayKind::Ssd1306),
        ];

        assert_eq!(expected.len(), BoardVersionProfile::ALL.len());
        for (version, display_kind) in expected {
            let profile = BoardVersionProfile::find(version)
                .unwrap_or_else(|| panic!("missing board_version {version}"));
            assert_eq!(
                profile.display_kind(),
                *display_kind,
                "board {version}: display metadata"
            );
            assert_eq!(
                BoardConfig::for_profile(profile).display_kind,
                *display_kind,
                "board {version}: BoardConfig display metadata"
            );
        }
    }

    #[test]
    fn esp_miner_board_version_parity() {
        let esp_miner = parse_esp_miner_default_configs(ESP_MINER_DEVICE_CONFIG_H);
        assert!(
            esp_miner.len() >= 23,
            "fixture parser missed upstream default_configs rows"
        );
        assert!(
            esp_miner.iter().any(|entry| entry.version == "603"),
            "fixture must include upstream Gamma board_version 603"
        );
        for expected in esp_miner {
            let ver = expected.version;
            let model = &expected.model;
            let asic = &expected.asic;
            let p = BoardVersionProfile::find(ver).unwrap_or_else(|| {
                panic!(
                    "ESP-Miner board_version {ver} does not resolve — parity gap vs device_config.h"
                )
            });
            assert_eq!(p.model, *model, "board {ver}: family mismatch");
            assert_eq!(p.asic_model, *asic, "board {ver}: ASIC mismatch");
        }
    }

    // ── Fan IC: EMC2101 (single) vs EMC2302 (Quad/Hex dual-fan) ──
    #[test]
    fn board_version_deep_parity_pins_power_and_support_attributes() {
        #[derive(Clone, Copy)]
        struct Expected {
            ver: &'static str,
            model: BitAxeModel,
            asic: &'static str,
            fan: FanControllerKind,
            temp: TempSensorKind,
            power: PowerControllerKind,
            has_ina260: bool,
            temp_flip: bool,
            temp_offset_c: i8,
            power_target_w: u16,
            live_proof: LiveProof,
            support: &'static str,
        }

        let expected: &[Expected] = &[
            Expected {
                ver: "2.2",
                model: BitAxeModel::Max,
                asic: "BM1397",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "102",
                model: BitAxeModel::Max,
                asic: "BM1397",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "0.11",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "201",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "202",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "203",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "204",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "205",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "207",
                model: BitAxeModel::Ultra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "302",
                model: BitAxeModel::HexUltra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 40,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "303",
                model: BitAxeModel::HexUltra,
                asic: "BM1366",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 40,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "400",
                model: BitAxeModel::Supra,
                asic: "BM1368",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "401",
                model: BitAxeModel::Supra,
                asic: "BM1368",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Ds4432u,
                has_ina260: true,
                temp_flip: false,
                temp_offset_c: 5,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "402",
                model: BitAxeModel::Supra,
                asic: "BM1368",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 8,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "403",
                model: BitAxeModel::Supra,
                asic: "BM1368",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 8,
                live_proof: LiveProof::Host,
                support: "supported",
            },
            Expected {
                ver: "600",
                model: BitAxeModel::Gamma,
                asic: "BM1370",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 19,
                live_proof: LiveProof::FocusedRun,
                support: "supported",
            },
            Expected {
                ver: "601",
                model: BitAxeModel::Gamma,
                asic: "BM1370",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 19,
                live_proof: LiveProof::FocusedRun,
                support: "supported",
            },
            Expected {
                ver: "602",
                model: BitAxeModel::Gamma,
                asic: "BM1370",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 22,
                live_proof: LiveProof::FocusedRun,
                support: "supported",
            },
            Expected {
                ver: "603",
                model: BitAxeModel::Gamma,
                asic: "BM1370",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 22,
                live_proof: LiveProof::FocusedRun,
                support: "supported",
            },
            Expected {
                ver: "650",
                model: BitAxeModel::GammaDuo,
                asic: "BM1370",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 35,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "701",
                model: BitAxeModel::HexSupra,
                asic: "BM1368",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 90,
                live_proof: LiveProof::FocusedRun,
                support: "experimental",
            },
            Expected {
                ver: "702",
                model: BitAxeModel::HexSupra,
                asic: "BM1368",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 90,
                live_proof: LiveProof::FocusedRun,
                support: "experimental",
            },
            Expected {
                ver: "801",
                model: BitAxeModel::GammaTurbo,
                asic: "BM1370",
                fan: FanControllerKind::Emc2103,
                temp: TempSensorKind::Emc2103,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: true,
                temp_offset_c: 0,
                power_target_w: 36,
                live_proof: LiveProof::FocusedRun,
                support: "experimental",
            },
            Expected {
                ver: "900",
                model: BitAxeModel::DcentAxeBm1397,
                asic: "BM1397",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                // R-10: TPS546D24A VRM, no DS4432U / no INA260 on the real board.
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "910",
                model: BitAxeModel::DcentAxeQuadBm1397,
                asic: "BM1397",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 48,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "920",
                model: BitAxeModel::DcentAxeHexBm1397,
                asic: "BM1397",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 72,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            // Canonical `9###` registry rows — byte-identical to 900/910/920.
            Expected {
                ver: "9010",
                model: BitAxeModel::DcentAxeBm1397,
                asic: "BM1397",
                fan: FanControllerKind::Emc2101,
                temp: TempSensorKind::Emc2101,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 0,
                power_target_w: 12,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "9040",
                model: BitAxeModel::DcentAxeQuadBm1397,
                asic: "BM1397",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 48,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
            Expected {
                ver: "9060",
                model: BitAxeModel::DcentAxeHexBm1397,
                asic: "BM1397",
                fan: FanControllerKind::Emc2302,
                temp: TempSensorKind::Tmp1075,
                power: PowerControllerKind::Tps546,
                has_ina260: false,
                temp_flip: false,
                temp_offset_c: 10,
                power_target_w: 72,
                live_proof: LiveProof::Host,
                support: "experimental",
            },
        ];

        for expected in expected {
            let p = BoardVersionProfile::find(expected.ver)
                .unwrap_or_else(|| panic!("missing board_version {}", expected.ver));
            assert_eq!(p.model, expected.model, "board {}: model", expected.ver);
            assert_eq!(p.asic_model, expected.asic, "board {}: ASIC", expected.ver);
            assert_eq!(
                p.fan_controller, expected.fan,
                "board {}: fan",
                expected.ver
            );
            assert_eq!(p.temp_sensor, expected.temp, "board {}: temp", expected.ver);
            assert_eq!(
                p.power_controller, expected.power,
                "board {}: power",
                expected.ver
            );
            assert_eq!(
                p.has_ina260, expected.has_ina260,
                "board {}: INA260",
                expected.ver
            );
            assert_eq!(
                p.temp_flip, expected.temp_flip,
                "board {}: temp_flip",
                expected.ver
            );
            assert_eq!(
                p.temp_offset_c, expected.temp_offset_c,
                "board {}: temp offset",
                expected.ver
            );
            assert_eq!(
                p.power_consumption_target_w, expected.power_target_w,
                "board {}: power target",
                expected.ver
            );
            assert_eq!(
                p.live_proof, expected.live_proof,
                "board {}: live proof",
                expected.ver
            );
            assert_eq!(
                p.model.support_status(),
                expected.support,
                "board {}: support",
                expected.ver
            );
        }
    }

    #[test]
    fn live_proof_is_explicit_and_support_status_does_not_imply_soak() {
        for profile in BoardVersionProfile::ALL {
            assert_ne!(
                profile.live_proof,
                LiveProof::None,
                "board {} must carry an explicit retained proof level",
                profile.board_version
            );
            if profile.model.support_status() == "supported" {
                assert_ne!(
                    profile.live_proof,
                    LiveProof::SustainedSoak,
                    "board {} must not imply sustained soak from support_status alone",
                    profile.board_version
                );
            }
        }
    }

    #[test]
    fn ds4432u_profiles_stay_hardware_gated_for_live_promotion() {
        for profile in BoardVersionProfile::ALL {
            if profile.power_controller == PowerControllerKind::Ds4432u {
                assert_eq!(
                    profile.live_proof,
                    LiveProof::Host,
                    "board {} uses DS4432U and must stay host-proof only until the ignored meter-log bench gate passes",
                    profile.board_version
                );
            }
        }
    }

    // ── R-10 (PREFAB_DESIGN_REVIEW_2026-07-08): DCENT_axe resolved power path ──
    // The dcent-axe-BM1397 schematic netlist wires the TPS546D24A EN pin to
    // GPIO10, ACTIVE-HIGH; there is no DS4432U and no INA260 on the board. The
    // old profile (Ds4432u + has_ina260) resolved GPIO10 as active-LOW, so the
    // fail-closed "power OFF" drive turned the VRM rail ON, and the generic
    // Tps546 arm would have picked GPIO46 (unconnected). Pin the fully-resolved
    // power path for every DCENT_axe SKU so neither regression can come back.
    #[test]
    fn dcentaxe_power_path_resolves_tps546_gpio10_active_high() {
        for model in [
            BitAxeModel::DcentAxeBm1397,
            BitAxeModel::DcentAxeQuadBm1397,
            BitAxeModel::DcentAxeHexBm1397,
        ] {
            let cfg = BoardConfig::for_model(model);
            assert_eq!(
                cfg.power_controller,
                PowerControllerKind::Tps546,
                "{model:?}: power controller must be the TPS546 PMBus VRM"
            );
            assert!(
                !cfg.has_ina260,
                "{model:?}: no INA260 is populated on DCENT_axe boards"
            );
            assert_eq!(
                cfg.buck_enable_pin, 10,
                "{model:?}: TPS546 EN is wired to GPIO10 (GPIO46 is unconnected)"
            );
            assert!(
                !cfg.buck_enable_active_low,
                "{model:?}: EN is ACTIVE-HIGH — an active-low resolution inverts \
                 fail-closed power-off into driving the VRM rail ON"
            );
            // The fail-closed OFF level for this polarity must actually cut the
            // rail: active-high ⇒ OFF == drive LOW (gpio_set_level 0).
            assert_eq!(
                crate::safety::buck_off_level(cfg.buck_enable_active_low),
                0,
                "{model:?}: fail-closed OFF must drive the EN pin LOW"
            );
        }
        // The board-version rows themselves must agree with the resolved config
        // (legacy 3-digit aliases AND the canonical 9### registry rows).
        for ver in ["900", "910", "920", "9010", "9040", "9060"] {
            let p = BoardVersionProfile::find(ver).unwrap();
            assert_eq!(
                p.power_controller,
                PowerControllerKind::Tps546,
                "board {ver}: profile row must declare Tps546"
            );
            assert!(
                !p.has_ina260,
                "board {ver}: profile row must not claim INA260"
            );
        }
    }

    #[test]
    fn fan_controllers_match_topology() {
        assert_eq!(
            BoardConfig::for_model(BitAxeModel::DcentAxeBm1397).fan_controller,
            FanControllerKind::Emc2101
        );
        assert_eq!(
            BoardConfig::for_model(BitAxeModel::DcentAxeQuadBm1397).fan_controller,
            FanControllerKind::Emc2302
        );
        assert_eq!(
            BoardConfig::for_model(BitAxeModel::DcentAxeHexBm1397).fan_controller,
            FanControllerKind::Emc2302
        );
    }
}
