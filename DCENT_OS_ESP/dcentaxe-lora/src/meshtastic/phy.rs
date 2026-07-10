// SPDX-License-Identifier: GPL-3.0-or-later
//! Meshtastic LoRa PHY: the modem presets + sync word + framing a DCENT_axe must
//! program into the [`Sx1262`](crate::sx1262::Sx1262) to sit on the same air as a
//! stock Meshtastic mesh. Two nodes only hear each other if their SF/BW/CR, sync
//! word, preamble, CRC, IQ **and centre frequency** all match.
//!
//! This module is the pure, host-testable resolver: `preset → SX126x bytes`. The
//! radio-side apply step is [`Sx1262::apply_meshtastic_phy`](crate::sx1262::Sx1262::apply_meshtastic_phy).
//!
//! ## Frequency is an explicit input, on purpose
//! Meshtastic derives the channel centre frequency from a slot plan
//! (`region band / bandwidth → N slots`, then `slot = hash(channelName) % N`).
//! We deliberately do **not** re-derive that hash here: shipping a guessed slot
//! that merely *looks* authoritative but lands one channel off would silently
//! break interop, which is worse than asking for the number. Every Meshtastic app
//! shows the exact channel frequency (Radio Config → LoRa), so the operator/config
//! supplies it. We ship the well-known **default LongFast** frequencies as named
//! constants ([`US_LONGFAST_HZ`], [`EU868_LONGFAST_HZ`]) for the common case, and
//! flag them "confirm against your app" — matching this codebase's evidence-gating
//! posture (do not present an unverified constant as ground truth).

/// The 16-bit LoRa sync word Meshtastic uses (public/`0x2B`, expanded the way
/// RadioLib expands a 1-byte sync word with control-nibble `0x44`:
/// MSB=`(0x2B & 0xF0)|0x04`=`0x24`, LSB=`((0x2B & 0x0F)<<4)|0x04`=`0xB4`).
pub const SYNC_WORD_MESHTASTIC: u16 = 0x24B4;

/// Meshtastic LoRa preamble length, in symbols.
pub const MESHTASTIC_PREAMBLE_LEN: u16 = 16;

/// Commonly-observed **US (902–928 MHz) LongFast** default centre frequency
/// (channel slot 19 → 906.875 MHz). Confirm against your Meshtastic app.
pub const US_LONGFAST_HZ: u32 = 906_875_000;

/// **EU 868 LongFast** default centre frequency. The EU_868 sub-band
/// (869.4–869.65 MHz) holds a single 250 kHz LongFast slot → 869.525 MHz, so this
/// one is unambiguous.
pub const EU868_LONGFAST_HZ: u32 = 869_525_000;

/// SX126x `SetModulationParams` bandwidth byte (datasheet Table 13-48).
pub mod bw_byte {
    pub const BW_62: u8 = 0x03; // 62.5 kHz
    pub const BW_125: u8 = 0x04;
    pub const BW_250: u8 = 0x05;
    pub const BW_500: u8 = 0x06;
}

/// SX126x `SetModulationParams` coding-rate byte (datasheet Table 13-49).
pub mod cr_byte {
    pub const CR_4_5: u8 = 0x01;
    pub const CR_4_8: u8 = 0x04;
}

/// The eight Meshtastic modem presets (VeryLongSlow is deprecated + omitted).
/// The default primary is [`LongFast`](Self::LongFast). Variant `preset_name`s
/// are the exact strings Meshtastic hashes into the channel byte for an unnamed
/// channel — they MUST match upstream spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModemPreset {
    ShortTurbo,
    ShortFast,
    ShortSlow,
    MediumFast,
    MediumSlow,
    #[default]
    LongFast,
    LongModerate,
    LongSlow,
}

impl ModemPreset {
    /// Spreading factor (7..=12).
    pub fn sf(self) -> u8 {
        match self {
            ModemPreset::ShortTurbo | ModemPreset::ShortFast => 7,
            ModemPreset::ShortSlow => 8,
            ModemPreset::MediumFast => 9,
            ModemPreset::MediumSlow => 10,
            ModemPreset::LongFast | ModemPreset::LongModerate => 11,
            ModemPreset::LongSlow => 12,
        }
    }

    /// Bandwidth in kHz (500 / 250 / 125).
    pub fn bandwidth_khz(self) -> u32 {
        match self {
            ModemPreset::ShortTurbo => 500,
            ModemPreset::ShortFast
            | ModemPreset::ShortSlow
            | ModemPreset::MediumFast
            | ModemPreset::MediumSlow
            | ModemPreset::LongFast => 250,
            ModemPreset::LongModerate | ModemPreset::LongSlow => 125,
        }
    }

    /// Coding-rate denominator (4/5 or 4/8).
    pub fn coding_rate_denom(self) -> u8 {
        match self {
            ModemPreset::LongModerate | ModemPreset::LongSlow => 8,
            _ => 5,
        }
    }

    /// SX126x bandwidth byte for this preset.
    pub fn bandwidth_byte(self) -> u8 {
        match self.bandwidth_khz() {
            500 => bw_byte::BW_500,
            250 => bw_byte::BW_250,
            _ => bw_byte::BW_125,
        }
    }

    /// SX126x coding-rate byte for this preset.
    pub fn coding_rate_byte(self) -> u8 {
        match self.coding_rate_denom() {
            8 => cr_byte::CR_4_8,
            _ => cr_byte::CR_4_5,
        }
    }

    /// Low-data-rate-optimize is enabled when a symbol lasts ≥ 16 ms (RadioLib's
    /// auto-LDRO rule) — otherwise long-SF/narrow-BW packets drift and fail CRC.
    pub fn low_data_rate_optimize(self) -> bool {
        let symbol_us = (1u64 << self.sf()) * 1_000_000 / (self.bandwidth_khz() as u64 * 1000);
        symbol_us >= 16_000
    }

    /// The upstream preset name (for the unnamed-channel hash + display).
    pub fn preset_name(self) -> &'static str {
        match self {
            ModemPreset::ShortTurbo => "ShortTurbo",
            ModemPreset::ShortFast => "ShortFast",
            ModemPreset::ShortSlow => "ShortSlow",
            ModemPreset::MediumFast => "MediumFast",
            ModemPreset::MediumSlow => "MediumSlow",
            ModemPreset::LongFast => "LongFast",
            ModemPreset::LongModerate => "LongModerate",
            ModemPreset::LongSlow => "LongSlow",
        }
    }

    /// Parse a preset name (case-insensitive, ignoring `_`/`-`).
    pub fn from_name(s: &str) -> Option<Self> {
        let norm: String = s
            .chars()
            .filter(|c| *c != '_' && *c != '-')
            .flat_map(|c| c.to_lowercase())
            .collect();
        let all = [
            ModemPreset::ShortTurbo,
            ModemPreset::ShortFast,
            ModemPreset::ShortSlow,
            ModemPreset::MediumFast,
            ModemPreset::MediumSlow,
            ModemPreset::LongFast,
            ModemPreset::LongModerate,
            ModemPreset::LongSlow,
        ];
        all.into_iter()
            .find(|p| p.preset_name().to_lowercase() == norm)
    }
}

/// A fully-resolved LoRa PHY configuration to program the SX1262 for one
/// Meshtastic channel. `freq_hz` is the caller-supplied channel centre frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshtasticPhyConfig {
    pub preset: ModemPreset,
    pub sf: u8,
    pub bandwidth_byte: u8,
    pub coding_rate_byte: u8,
    pub low_data_rate_optimize: bool,
    pub preamble_len: u16,
    pub sync_word: u16,
    pub crc_on: bool,
    pub iq_inverted: bool,
    pub freq_hz: u32,
}

impl MeshtasticPhyConfig {
    /// Resolve a `(preset, centre-frequency)` into the SX126x parameters. CRC is
    /// on and IQ is standard (Meshtastic uses both), preamble is 16 symbols, and
    /// the sync word is [`SYNC_WORD_MESHTASTIC`].
    pub fn new(preset: ModemPreset, freq_hz: u32) -> Self {
        Self {
            preset,
            sf: preset.sf(),
            bandwidth_byte: preset.bandwidth_byte(),
            coding_rate_byte: preset.coding_rate_byte(),
            low_data_rate_optimize: preset.low_data_rate_optimize(),
            preamble_len: MESHTASTIC_PREAMBLE_LEN,
            sync_word: SYNC_WORD_MESHTASTIC,
            crc_on: true,
            iq_inverted: false,
            freq_hz,
        }
    }

    /// US LongFast on the observed default frequency ([`US_LONGFAST_HZ`]).
    pub fn us_longfast() -> Self {
        Self::new(ModemPreset::LongFast, US_LONGFAST_HZ)
    }

    /// EU 868 LongFast on the (unambiguous) default frequency ([`EU868_LONGFAST_HZ`]).
    pub fn eu868_longfast() -> Self {
        Self::new(ModemPreset::LongFast, EU868_LONGFAST_HZ)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_word_is_meshtastic_2b_expansion() {
        // 0x2B with RadioLib control-nibble 0x44 → 0x24B4. This is what makes us
        // hearable by Meshtastic and deaf to public-LoRaWAN (0x3444) traffic.
        assert_eq!(SYNC_WORD_MESHTASTIC, 0x24B4);
        assert_ne!(SYNC_WORD_MESHTASTIC, crate::sx1262::SYNC_WORD_PUBLIC);
    }

    #[test]
    fn longfast_default_params_are_pinned() {
        let p = ModemPreset::LongFast;
        assert_eq!(p.sf(), 11);
        assert_eq!(p.bandwidth_khz(), 250);
        assert_eq!(p.bandwidth_byte(), bw_byte::BW_250);
        assert_eq!(p.coding_rate_denom(), 5);
        assert_eq!(p.coding_rate_byte(), cr_byte::CR_4_5);
        assert!(!p.low_data_rate_optimize(), "SF11/BW250 symbol < 16ms");
        assert_eq!(p.preset_name(), "LongFast");
        assert_eq!(ModemPreset::default(), ModemPreset::LongFast);
    }

    #[test]
    fn long_slow_enables_ldro() {
        let p = ModemPreset::LongSlow;
        assert_eq!((p.sf(), p.bandwidth_khz()), (12, 125));
        assert_eq!(p.bandwidth_byte(), bw_byte::BW_125);
        assert_eq!(p.coding_rate_byte(), cr_byte::CR_4_8);
        assert!(
            p.low_data_rate_optimize(),
            "SF12/BW125 symbol ≥ 16ms → LDRO on"
        );
    }

    #[test]
    fn short_turbo_is_the_fastest_widest() {
        let p = ModemPreset::ShortTurbo;
        assert_eq!((p.sf(), p.bandwidth_khz()), (7, 500));
        assert_eq!(p.bandwidth_byte(), bw_byte::BW_500);
        assert!(!p.low_data_rate_optimize());
    }

    #[test]
    fn ldro_threshold_across_all_presets() {
        // Only the two 125 kHz SF11/SF12 presets cross the 16 ms symbol threshold.
        for p in [
            ModemPreset::ShortTurbo,
            ModemPreset::ShortFast,
            ModemPreset::ShortSlow,
            ModemPreset::MediumFast,
            ModemPreset::MediumSlow,
            ModemPreset::LongFast,
        ] {
            assert!(!p.low_data_rate_optimize(), "{:?} should be LDRO-off", p);
        }
        assert!(ModemPreset::LongModerate.low_data_rate_optimize());
        assert!(ModemPreset::LongSlow.low_data_rate_optimize());
    }

    #[test]
    fn preset_name_round_trips_case_insensitive() {
        for p in [
            ModemPreset::ShortTurbo,
            ModemPreset::LongFast,
            ModemPreset::LongModerate,
        ] {
            assert_eq!(ModemPreset::from_name(p.preset_name()), Some(p));
        }
        assert_eq!(
            ModemPreset::from_name("long_fast"),
            Some(ModemPreset::LongFast)
        );
        assert_eq!(
            ModemPreset::from_name("LONG-MODERATE"),
            Some(ModemPreset::LongModerate)
        );
        assert_eq!(ModemPreset::from_name("nonsense"), None);
    }

    #[test]
    fn phy_config_defaults_are_correct() {
        let c = MeshtasticPhyConfig::us_longfast();
        assert_eq!(c.sf, 11);
        assert_eq!(c.bandwidth_byte, bw_byte::BW_250);
        assert_eq!(c.coding_rate_byte, cr_byte::CR_4_5);
        assert!(!c.low_data_rate_optimize);
        assert_eq!(c.preamble_len, 16);
        assert_eq!(c.sync_word, SYNC_WORD_MESHTASTIC);
        assert!(c.crc_on);
        assert!(!c.iq_inverted);
        assert_eq!(c.freq_hz, US_LONGFAST_HZ);

        let eu = MeshtasticPhyConfig::eu868_longfast();
        assert_eq!(eu.freq_hz, 869_525_000);
    }
}
