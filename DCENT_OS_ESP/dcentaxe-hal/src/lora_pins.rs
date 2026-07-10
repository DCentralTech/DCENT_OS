// SPDX-License-Identifier: GPL-3.0-or-later
//! DCENT_axe on-board SX1262 LoRa radio pin map + SPI3/HSPI bus builder.
//!
//! Compiled only when the `pins-lora` Cargo feature is selected (the `dcentaxe`
//! binary's default-OFF `lora` feature turns it on transitively). A non-LoRa SKU
//! never sees this module, so it pays zero image bytes — the same per-board
//! feature-gating discipline as `power-*` / `fan-*`.
//!
//! ## Coexistence contract (fork plan §4.5 / doc 05 §1.3)
//! The SX1262 lives on its **OWN dedicated SPI host (SPI3/HSPI)** — NEVER the
//! BAP/J4 header (SPI2/FSPI) — so a UART-mode BAP accessory can never contend for
//! or kill the radio bus. [`open_lora_bus`] acquires SPI3 ONLY.
//!
//! ## ⚠️ PROVISIONAL GPIO MAP — NEEDS-NETLIST-LOCK (doc 05 §1.3)
//! The fork-plan worked example collided MOSI with the stock fan-tach pin; these
//! are the corrected provisional pins. **Lock against the real DCENT_axe KiCad
//! netlist before routing.** The host [`pin_map`] table test pins these numbers so
//! a silent renumber is loud in CI (`cargo test -p dcentaxe-hal`).
//!
//! | Signal      | GPIO | Notes                                             |
//! |-------------|------|---------------------------------------------------|
//! | LORA_SCLK   | 5    | dedicated SPI3/HSPI, non-strap                     |
//! | LORA_MOSI   | 6    |                                                   |
//! | LORA_MISO   | 7    |                                                   |
//! | LORA_NSS    | 15   | active-low chip-select                            |
//! | LORA_BUSY   | 16   | readable GPIO, polled before every command        |
//! | LORA_DIO1   | 21   | IRQ-capable (TxDone/RxDone)                        |
//! | LORA_NRESET | 8    | active-low reset (or a shared RC power-on reset)   |
//! | LORA_TXEN   | 2    | E22 RF-switch TX enable (host-driven, active-high) |
//! | LORA_RXEN   | 9    | E22 RF-switch RX enable (host-driven, active-high) |
//!
//! SX1262 electricals (doc 08): SPI ≤ 16 MHz (ESP32-S3 SPI clears it > 2×), mode 0,
//! MSB-first; TCXO enabled via DIO3 in firmware (`sx1262`), DIO1 = IRQ, BUSY polled.
//!
//! ## RF switch (R-24, PREFAB_DESIGN_REVIEW_2026-07-08)
//! The E22-900M22S module's RF switch is **NOT** internal-DIO2-driven on the
//! DCENT_axe BM1397 board — the module exposes discrete TXEN/RXEN and the
//! schematic wires them to host GPIOs (`/LORA_TXEN` = GPIO2, `/LORA_RXEN` =
//! GPIO9). The earlier "RF switch driven internally via DIO2 (no host GPIO)"
//! claim was stale and is corrected here: firmware must drive TXEN/RXEN on
//! every TX/RX/standby transition (the `dcentaxe-lora` `Sx1262` driver does
//! this when the map populates the pins; DIO2-switch mode is kept only for
//! hypothetical pinless maps).

/// The SX1262 control-line GPIO numbers (7 mandatory + the 2 optional RF-switch
/// enables). `i32` to match the rest of the HAL's `board.rs` pin accessors
/// (esp-idf-hal takes GPIO peripherals by type, but the numeric map is what the
/// netlist lock and the host table test compare against).
///
/// ⚠️ PROVISIONAL — NEEDS-NETLIST-LOCK (doc 05 §1.3). Do NOT route from these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoraPinMap {
    /// SPI clock — dedicated SPI3/HSPI, non-strap.
    pub sclk: i32,
    /// SPI MOSI (controller-out).
    pub mosi: i32,
    /// SPI MISO (controller-in).
    pub miso: i32,
    /// Active-low chip-select (NSS).
    pub nss: i32,
    /// BUSY — readable input, polled before every SX1262 command.
    pub busy: i32,
    /// DIO1 — IRQ line (TxDone/RxDone).
    pub dio1: i32,
    /// Active-low hardware reset.
    pub nreset: i32,
    /// E22 RF-switch TX enable (host-driven, active-high). `None` on a map whose
    /// module drives its own switch via DIO2 (`SetDIO2AsRfSwitchCtrl`).
    pub txen: Option<i32>,
    /// E22 RF-switch RX enable (host-driven, active-high). `None` on a map whose
    /// module drives its own switch via DIO2.
    pub rxen: Option<i32>,
}

// ── Provisional GPIO constants (doc 05 §1.3) — NEEDS-NETLIST-LOCK ──
pub const LORA_SCLK_GPIO: i32 = 5;
pub const LORA_MOSI_GPIO: i32 = 6;
pub const LORA_MISO_GPIO: i32 = 7;
pub const LORA_NSS_GPIO: i32 = 15;
pub const LORA_BUSY_GPIO: i32 = 16;
pub const LORA_DIO1_GPIO: i32 = 21;
pub const LORA_NRESET_GPIO: i32 = 8;
// E22-900M22S discrete RF-switch enables (R-24): the dcent-axe-BM1397 schematic
// wires `/LORA_TXEN` to U1.38 (GPIO2) and `/LORA_RXEN` to U1.17 (GPIO9).
pub const LORA_TXEN_GPIO: i32 = 2;
pub const LORA_RXEN_GPIO: i32 = 9;

/// SX1262 SPI ceiling (datasheet doc 08). The ESP32-S3 SPI host is driven at this
/// rate; it clears the radio's ≤ 16 MHz limit with margin.
pub const LORA_SPI_HZ: u32 = 16_000_000;

/// The provisional DCENT_axe LoRa pin map. ⚠️ NEEDS-NETLIST-LOCK (doc 05 §1.3).
/// TXEN/RXEN are populated (R-24) — the DCENT_axe BM1397 board wires the E22's
/// discrete RF-switch enables to host GPIO2/GPIO9.
pub const fn lora_pin_map() -> LoraPinMap {
    LoraPinMap {
        sclk: LORA_SCLK_GPIO,
        mosi: LORA_MOSI_GPIO,
        miso: LORA_MISO_GPIO,
        nss: LORA_NSS_GPIO,
        busy: LORA_BUSY_GPIO,
        dio1: LORA_DIO1_GPIO,
        nreset: LORA_NRESET_GPIO,
        txen: Some(LORA_TXEN_GPIO),
        rxen: Some(LORA_RXEN_GPIO),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// ESP-IDF SPI3/HSPI bus builder (integration seam — NOT host-tested).
//
// Gated to the esp-idf target exactly like the peripheral driver modules. It is
// NOT exercised by host tests and cannot be host-compiled (esp-idf-hal), so treat
// it — like `dcentaxe-lora::esp_hal` — as the documented bring-up entry point,
// NEEDS-VERIFY against esp-idf-hal 0.46 at wire-up, not proven code. The pure pin
// map above IS host-proven (see the `#[cfg(test)]` table test).
// ───────────────────────────────────────────────────────────────────────────
#[cfg(target_os = "espidf")]
mod espidf_bus {
    use esp_idf_hal::gpio::{AnyIOPin, AnyInputPin, AnyOutputPin, Input, Output, PinDriver, Pull};
    use esp_idf_hal::spi::config::{Config as SpiConfig, DriverConfig};
    use esp_idf_hal::spi::{SpiAnyPins, SpiDeviceDriver, SpiDriver};
    use esp_idf_hal::sys::EspError;
    use esp_idf_hal::units::FromValueType;

    /// A fully-constructed SX1262 transport: the SPI3/HSPI device (NSS-framed,
    /// mode 0, ≤ 16 MHz) plus the three directly-driven control pins. The binary
    /// wraps these into `dcentaxe_lora::esp_hal::{EspSpiBus, EspInputPin,
    /// EspOutputPin}` and hands them to `Sx1262::new`.
    ///
    /// Pins are type-erased (`AnyIOPin`) so this bundle has one nameable type
    /// regardless of which concrete GPIOs the netlist lock picks — the caller
    /// `downgrade()`s each peripheral before passing it in.
    pub struct LoraBus<'d> {
        pub spi: SpiDeviceDriver<'d, SpiDriver<'d>>,
        // esp-idf-hal 0.46: `PinDriver<'d, MODE>` — the pin type is erased (the
        // driver owns any downgraded pin), so ONLY the direction mode is a generic.
        pub busy: PinDriver<'d, Input>,
        pub dio1: PinDriver<'d, Input>,
        pub nreset: PinDriver<'d, Output>,
        /// E22 RF-switch TX enable (R-24) — driven low (idle) at acquisition;
        /// `None` for a module whose switch is DIO2-driven.
        pub txen: Option<PinDriver<'d, Output>>,
        /// E22 RF-switch RX enable (R-24) — driven low (idle) at acquisition.
        pub rxen: Option<PinDriver<'d, Output>>,
    }

    /// Acquire the dedicated SX1262 SPI3/HSPI bus + control pins.
    ///
    /// SPI3 ONLY — never the BAP/J4 (SPI2/FSPI) bus (fork plan §4.5). Configures a
    /// single-device master at [`LORA_SPI_HZ`](super::LORA_SPI_HZ), SPI mode 0
    /// (CPOL=0/CPHA=0), NSS as the hardware chip-select; BUSY/DIO1 as inputs and
    /// NRESET as an output (idle-high, i.e. not-reset). The optional E22
    /// RF-switch enables (TXEN/RXEN, R-24) are driven as outputs, idle-low
    /// (switch de-energized) until the radio task commands a TX/RX transition.
    ///
    /// ⚠️ Integration seam — NEEDS-VERIFY against esp-idf-hal 0.46 at wire-up.
    #[allow(clippy::too_many_arguments)]
    pub fn open_lora_bus<'d, SPI: SpiAnyPins + 'd>(
        spi3: SPI,
        sclk: AnyOutputPin<'d>,
        mosi: AnyOutputPin<'d>,
        miso: AnyInputPin<'d>,
        nss: AnyOutputPin<'d>,
        busy: AnyIOPin<'d>,
        dio1: AnyIOPin<'d>,
        nreset: AnyIOPin<'d>,
        txen: Option<AnyIOPin<'d>>,
        rxen: Option<AnyIOPin<'d>>,
    ) -> Result<LoraBus<'d>, EspError> {
        // Dedicated SPI3 host + the three bus lines. `sdo` = MOSI, `sdi` = MISO.
        let driver = SpiDriver::new(spi3, sclk, mosi, Some(miso), &DriverConfig::new())?;
        // NSS-framed single device at ≤ 16 MHz. SPI mode 0 (CPOL=0/CPHA=0) is the
        // esp-idf-hal `Config` default — exactly what the SX1262 requires — so it is
        // left implicit rather than importing a version-fragile MODE_0 constant.
        let dev_cfg = SpiConfig::new().baudrate(super::LORA_SPI_HZ.Hz());
        let spi = SpiDeviceDriver::new(driver, Some(nss), &dev_cfg)?;

        // BUSY + DIO1 are read as inputs; NRESET is driven as an output, released
        // high (chip out of reset) by default — the driver's `reset()` pulses it.
        let busy = PinDriver::input(busy, Pull::Floating)?;
        let dio1 = PinDriver::input(dio1, Pull::Floating)?;
        let mut nreset = PinDriver::output(nreset)?;
        nreset.set_high()?;

        // E22 RF-switch enables (R-24): outputs, idle-low so the switch starts
        // de-energized; the Sx1262 driver drives them on TX/RX transitions.
        let txen = match txen {
            Some(pin) => {
                let mut p = PinDriver::output(pin)?;
                p.set_low()?;
                Some(p)
            }
            None => None,
        };
        let rxen = match rxen {
            Some(pin) => {
                let mut p = PinDriver::output(pin)?;
                p.set_low()?;
                Some(p)
            }
            None => None,
        };

        Ok(LoraBus {
            spi,
            busy,
            dio1,
            nreset,
            txen,
            rxen,
        })
    }
}

#[cfg(target_os = "espidf")]
pub use espidf_bus::{open_lora_bus, LoraBus};

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin-map table test: pins the 9 provisional SX1262 GPIO numbers (7 core +
    /// the 2 R-24 RF-switch enables) so a silent renumber (or a netlist-lock
    /// edit) is LOUD in CI. When the real DCENT_axe netlist locks these, update
    /// BOTH the constants and this table in the same commit — the mismatch fails
    /// here first.
    #[test]
    fn pin_map_pins_the_provisional_gpio_numbers() {
        let m = lora_pin_map();
        assert_eq!(m.sclk, 5, "LORA_SCLK");
        assert_eq!(m.mosi, 6, "LORA_MOSI");
        assert_eq!(m.miso, 7, "LORA_MISO");
        assert_eq!(m.nss, 15, "LORA_NSS");
        assert_eq!(m.busy, 16, "LORA_BUSY");
        assert_eq!(m.dio1, 21, "LORA_DIO1");
        assert_eq!(m.nreset, 8, "LORA_NRESET");
        // R-24: the E22-900M22S RF switch is HOST-driven on this board —
        // TXEN=GPIO2 / RXEN=GPIO9 per the dcent-axe-BM1397 schematic. A `None`
        // here would regress to the falsified "DIO2 drives the switch" claim
        // and leave the module deaf/mute.
        assert_eq!(m.txen, Some(2), "LORA_TXEN (E22 RF switch, host-driven)");
        assert_eq!(m.rxen, Some(9), "LORA_RXEN (E22 RF switch, host-driven)");
        // The struct accessors and the standalone constants must never diverge.
        assert_eq!(m.sclk, LORA_SCLK_GPIO);
        assert_eq!(m.mosi, LORA_MOSI_GPIO);
        assert_eq!(m.miso, LORA_MISO_GPIO);
        assert_eq!(m.nss, LORA_NSS_GPIO);
        assert_eq!(m.busy, LORA_BUSY_GPIO);
        assert_eq!(m.dio1, LORA_DIO1_GPIO);
        assert_eq!(m.nreset, LORA_NRESET_GPIO);
        assert_eq!(m.txen, Some(LORA_TXEN_GPIO));
        assert_eq!(m.rxen, Some(LORA_RXEN_GPIO));
    }

    /// Every SX1262 line maps to a DISTINCT GPIO — a duplicate would short two
    /// signals onto one pad (the exact class of error the fork-plan MOSI/fan-tach
    /// collision was). Also pins the SPI ceiling at the datasheet ≤ 16 MHz limit.
    #[test]
    fn pin_map_has_no_duplicate_gpio_and_spi_within_ceiling() {
        let m = lora_pin_map();
        let mut pins = vec![m.sclk, m.mosi, m.miso, m.nss, m.busy, m.dio1, m.nreset];
        if let Some(txen) = m.txen {
            pins.push(txen);
        }
        if let Some(rxen) = m.rxen {
            pins.push(rxen);
        }
        for i in 0..pins.len() {
            for j in (i + 1)..pins.len() {
                assert_ne!(
                    pins[i], pins[j],
                    "GPIO {} reused across two LoRa lines",
                    pins[i]
                );
            }
        }
        assert!(
            LORA_SPI_HZ <= 16_000_000,
            "SX1262 SPI ceiling is 16 MHz (doc 08)"
        );
    }
}
