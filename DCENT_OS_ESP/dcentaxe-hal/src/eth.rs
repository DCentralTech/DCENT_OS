// SPDX-License-Identifier: GPL-3.0-or-later
//! W5500 SPI-Ethernet (DCENT LAN Mod) pin map + link constants — PLAN-E Phase 1.
//!
//! Compiled only when the `eth-w5500` Cargo feature is selected (the `dcentaxe`
//! binary's default-OFF `eth-w5500` feature turns it on transitively). A non-LAN
//! SKU never sees this module, so it pays zero image bytes — the same per-board
//! feature-gating discipline as `pins-lora` / `power-*` / `fan-*`.
//!
//! This module is deliberately PURE (no esp-idf imports) so the pin map, the
//! MAC-derivation rule, and the clock/poll constants are host-testable in the
//! default `cargo test -p dcentaxe-hal -p dcentaxe-core` gate. The actual
//! ESP-IDF `esp_eth` bring-up seam lives in `dcentaxe/src/eth_w5500.rs` — NOT
//! here — because esp-idf-svc's `eth` module types only exist under the
//! `esp_idf_eth_spi_ethernet_w5500` sdkconfig cfg, and only the `dcentaxe`
//! binary crate propagates ESP-IDF cfgs via its `build.rs`
//! (`embuild::espidf::sysenv::output()`); `dcentaxe-hal` has no build script.
//!
//! ## Wiring contract (PLAN-E §3.1(a) — LOCKED to ESP-Miner-LAN parity)
//! The W5500 add-on (`projects/dcent-lan-bitaxe/`) pogo-presses onto the
//! BAP/J4 header and consumes ONLY the 4 SPI lines already on J4 — zero extra
//! host GPIO. W5500 RST is a local on-board RC power-on-reset (10k + 100nF)
//! and INT is left unwired (the host polls), so the J4 "never widen" contract
//! is honored. Pin numbers are byte-identical to community ESP-Miner-LAN
//! (`Kconfig.projbuild`: MOSI 40 / MISO 39 / SCLK 41 / CS 42) so the same
//! board boots either firmware.
//!
//! | Signal | GPIO | J4 pin | Also is… |
//! |--------|------|--------|----------------------------------------|
//! | MISO   | 39   | 3      | BAP-UART **TX** (mutual-exclusion source) |
//! | MOSI   | 40   | 4      | BAP-UART **RX** (mutual-exclusion source) |
//! | SCLK   | 41   | 5      | —                                       |
//! | CS     | 42   | 6      | —                                       |
//!
//! ## Coexistence contract
//! - **BAP Touch:** GPIO39/40 double as the BAP-UART pins → W5500 LAN and a
//!   BAP-Touch accessory are mutually exclusive. `board::validate_accessory_mode`
//!   already fail-closes the illegal pair (`AccessoryMode::{BapTouch, W5500Lan}`);
//!   activation MUST go through that guard.
//! - **LoRa:** the SX1262 lives on its own dedicated SPI3/HSPI bus
//!   (`lora_pins`: 5/6/7/15/16/21/8 + 2/9) → W5500 LAN + LoRa coexist. The
//!   W5500 claims the **SPI2/FSPI** host. A table test below pins the two pin
//!   sets disjoint so a silent renumber that re-collides them is loud in CI.

/// The W5500 SPI line GPIO numbers. `i32` to match the rest of the HAL's
/// numeric pin accessors (esp-idf peripherals are taken by type in the binary;
/// this numeric map is what the parity lock and the host table tests compare
/// against).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthPinMap {
    /// SPI clock (J4 pin 5).
    pub sclk: i32,
    /// SPI MOSI, controller-out (J4 pin 4 — also BAP-UART RX).
    pub mosi: i32,
    /// SPI MISO, controller-in (J4 pin 3 — also BAP-UART TX).
    pub miso: i32,
    /// Active-low chip-select (J4 pin 6).
    pub cs: i32,
}

// ── GPIO constants — LOCKED to ESP-Miner-LAN parity (PLAN-E §3.1(a)) ────────
pub const W5500_MISO_GPIO: i32 = 39;
pub const W5500_MOSI_GPIO: i32 = 40;
pub const W5500_SCLK_GPIO: i32 = 41;
pub const W5500_CS_GPIO: i32 = 42;

/// W5500 SPI clock — conservative clone-safe default.
///
/// ESP-Miner-LAN's shipped `Kconfig.projbuild` default is 2 MHz ("2 MHz is
/// safer for clone chips") while their own `ETHERNET_WIRING.md` claims
/// 20 MHz / mode 0 — a doc-vs-Kconfig mismatch PLAN-E flags as Risk #1. We
/// ship the conservative value: mining traffic is tiny, so 2 MHz is
/// functionally sufficient (only dashboard-over-LAN feel differs).
///
/// TODO(bench, PLAN-E Risk #1): bench-confirm 16–20 MHz on genuine WIZnet
/// silicon with a first-article `dcent-lan-bitaxe` board before raising this
/// (the expansion-pack plans 16 → 20–40 MHz after measuring). Do NOT raise it
/// from a desk review — clone W5500s are known to fail above ~2 MHz.
pub const W5500_SPI_HZ: u32 = 2_000_000;

/// Emac status poll period (ms). The add-on does not wire the W5500 INT pin
/// (zero-extra-GPIO contract), so the ESP-IDF `esp_eth` MAC polls for RX/link
/// events instead of taking an interrupt. 1 ms is TNA/ESP-Miner-LAN parity
/// (`poll_period_ms = 1`); requires ESP-IDF ≥ 5.3 (this project pins v5.4).
pub const W5500_POLL_PERIOD_MS: u32 = 1;

/// The DCENT LAN Mod W5500 pin map — J4/BAP header, ESP-Miner-LAN parity.
pub const fn w5500_pin_map() -> EthPinMap {
    EthPinMap {
        sclk: W5500_SCLK_GPIO,
        mosi: W5500_MOSI_GPIO,
        miso: W5500_MISO_GPIO,
        cs: W5500_CS_GPIO,
    }
}

/// Derive the wired-Ethernet MAC from the efuse base MAC — TNA/ESP-Miner-LAN
/// parity (`generate_mac_address` in `ethernet_w5500.c`): set the
/// locally-administered bit and XOR the last byte so the Ethernet MAC is
/// unique per unit AND distinct from the Wi-Fi STA MAC derived from the same
/// base. We additionally force the I/G bit to unicast (a no-op for every real
/// efuse base MAC, but it makes "never a multicast source address" a checked
/// guarantee instead of an assumption).
pub fn derive_eth_mac(base_mac: [u8; 6]) -> [u8; 6] {
    let mut mac = base_mac;
    mac[0] |= 0x02; // locally administered
    mac[0] &= 0xFE; // unicast (I/G bit clear)
    mac[5] ^= 0x01; // differentiate from the Wi-Fi STA MAC
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin-map table test: pins the 4 W5500 SPI GPIOs to the ESP-Miner-LAN
    /// parity numbers (MOSI 40 / MISO 39 / SCLK 41 / CS 42). Parity is a
    /// product promise — the `dcent-lan-bitaxe` add-on must boot either
    /// firmware — so a silent renumber must be LOUD in CI. Any deliberate
    /// change must update `projects/dcent-lan-bitaxe/hardware/pinmap.yaml`,
    /// these constants, and this table in the same commit.
    #[test]
    fn pin_map_pins_the_esp_miner_lan_parity_numbers() {
        let m = w5500_pin_map();
        assert_eq!(m.miso, 39, "W5500_MISO (J4 pin 3)");
        assert_eq!(m.mosi, 40, "W5500_MOSI (J4 pin 4)");
        assert_eq!(m.sclk, 41, "W5500_SCLK (J4 pin 5)");
        assert_eq!(m.cs, 42, "W5500_CS (J4 pin 6)");
        // The struct accessors and the standalone constants must never diverge.
        assert_eq!(m.miso, W5500_MISO_GPIO);
        assert_eq!(m.mosi, W5500_MOSI_GPIO);
        assert_eq!(m.sclk, W5500_SCLK_GPIO);
        assert_eq!(m.cs, W5500_CS_GPIO);
    }

    /// Every W5500 line maps to a DISTINCT GPIO — a duplicate would short two
    /// signals onto one pad (the same class of error the LoRa fork-plan
    /// MOSI/fan-tach collision was).
    #[test]
    fn pin_map_has_no_duplicate_gpio() {
        let m = w5500_pin_map();
        let pins = [m.sclk, m.mosi, m.miso, m.cs];
        for i in 0..pins.len() {
            for j in (i + 1)..pins.len() {
                assert_ne!(
                    pins[i], pins[j],
                    "GPIO {} reused across two W5500 lines",
                    pins[i]
                );
            }
        }
    }

    /// The W5500 SPI pins deliberately OVERLAP the BAP-UART pins (GPIO39 =
    /// BAP TX, GPIO40 = BAP RX — see `dcentaxe-bap/src/uart.rs`). This overlap
    /// is exactly WHY `board::validate_accessory_mode` fail-closes
    /// `BapTouch` + `W5500Lan` together. Pin the numbers so the guard's
    /// rationale can never silently rot: if a future board moves the W5500
    /// off the BAP pins, this test (and possibly the guard) must be revisited
    /// deliberately.
    #[test]
    fn w5500_shares_gpio39_40_with_bap_uart_hence_the_accessory_guard() {
        const BAP_UART_TX_GPIO: i32 = 39; // dcentaxe-bap/src/uart.rs
        const BAP_UART_RX_GPIO: i32 = 40;
        let m = w5500_pin_map();
        assert_eq!(m.miso, BAP_UART_TX_GPIO, "W5500 MISO rides the BAP TX pad");
        assert_eq!(m.mosi, BAP_UART_RX_GPIO, "W5500 MOSI rides the BAP RX pad");
    }

    /// W5500-LAN + LoRa must coexist (PLAN-E §3.2 tier 2): the SX1262 owns a
    /// dedicated SPI3 bus and the W5500 the SPI2/J4 lines — the two GPIO sets
    /// must stay disjoint. Runs whenever both feature-gated modules are
    /// compiled together (the `dcentaxe-core` default host gate does exactly
    /// that via feature unification).
    #[cfg(feature = "pins-lora")]
    #[test]
    fn w5500_and_lora_pin_sets_are_disjoint() {
        use crate::lora_pins::lora_pin_map;
        let e = w5500_pin_map();
        let l = lora_pin_map();
        let eth_pins = [e.sclk, e.mosi, e.miso, e.cs];
        let mut lora_pins = vec![l.sclk, l.mosi, l.miso, l.nss, l.busy, l.dio1, l.nreset];
        if let Some(txen) = l.txen {
            lora_pins.push(txen);
        }
        if let Some(rxen) = l.rxen {
            lora_pins.push(rxen);
        }
        for ep in eth_pins {
            assert!(
                !lora_pins.contains(&ep),
                "GPIO {ep} is claimed by BOTH the W5500 LAN map and the LoRa map"
            );
        }
    }

    /// Clock + poll constants: pin the clone-safe 2 MHz default (raising it is
    /// a deliberate, bench-gated decision — PLAN-E Risk #1) and the TNA-parity
    /// 1 ms poll period, and keep the clock under the W5500's own 33.3 MHz
    /// ceiling as a sanity bound.
    #[test]
    fn spi_clock_is_clone_safe_conservative_and_poll_is_tna_parity() {
        assert_eq!(
            W5500_SPI_HZ, 2_000_000,
            "W5500 SPI clock must stay at the clone-safe 2 MHz default until a \
             first-article bench run proves a higher clock (PLAN-E Risk #1)"
        );
        assert!(W5500_SPI_HZ <= 33_300_000, "W5500 datasheet SPI ceiling");
        assert_eq!(W5500_POLL_PERIOD_MS, 1, "TNA/ESP-Miner-LAN poll parity");
    }

    /// MAC derivation — TNA parity + checked guarantees: locally administered,
    /// unicast, distinct from the base (Wi-Fi) MAC, deterministic, and OUI-
    /// stable except for the LA/IG bits.
    #[test]
    fn derive_eth_mac_is_tna_parity_locally_administered_unicast() {
        let base = [0xAC, 0x15, 0x18, 0x2A, 0x3B, 0x4C];
        let mac = derive_eth_mac(base);
        assert_ne!(mac, base, "eth MAC must differ from the Wi-Fi/base MAC");
        assert_eq!(mac[0] & 0x02, 0x02, "locally-administered bit set");
        assert_eq!(mac[0] & 0x01, 0x00, "unicast (I/G bit clear)");
        assert_eq!(mac[5], base[5] ^ 0x01, "last byte XOR 0x01 (TNA parity)");
        assert_eq!(&mac[1..5], &base[1..5], "middle bytes pass through");
        assert_eq!(mac, derive_eth_mac(base), "deterministic");

        // A (hypothetical) multicast base is forced to a valid unicast source.
        let odd_base = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
        assert_eq!(derive_eth_mac(odd_base)[0] & 0x01, 0x00);

        // Distinct bases derive distinct MACs (unique per unit).
        let other = [0xAC, 0x15, 0x18, 0x2A, 0x3B, 0x4E];
        assert_ne!(derive_eth_mac(base), derive_eth_mac(other));
    }
}
