// SPDX-License-Identifier: GPL-3.0-or-later
// Real ESP32-S3 transport for the SX1262 driver: the dedicated SPI3/HSPI bus +
// the BUSY / DIO1 / NRESET control pins. Feature-gated (`esp-idf`) exactly like
// dcentaxe-bap's `uart.rs`, so host unit tests stay pure-Rust and never pull in
// esp-idf-sys.
//
// ⚠️ INTEGRATION SEAM — NEEDS-VERIFY against esp-idf-hal 0.46 at wire-up, and
// against the real KiCad netlist for the pin numbers (see sx1262.rs GPIO map,
// marked NEEDS-NETLIST-LOCK). This module is NOT exercised by host tests and the
// firmware app build is currently ESP-IDF/GCC-blocked (project ), so
// treat it as the documented LoRa bring-up entry point, not proven code.
//
// Coexistence contract (fork plan §4.5): the SX1262 lives on its OWN SPI host
// (SPI3/HSPI). The BAP/W5500-LAN accessory binds SPI2/FSPI on J4 — the radio and
// a BAP accessory therefore run concurrently and must never be re-muxed onto the
// same bus.

use esp_idf_hal::gpio::{Input, Output, PinDriver};
use esp_idf_hal::spi::SpiDeviceDriver;

use crate::{GpioPin, LoraError, SpiBus};

/// SPI3/HSPI device bus for the SX1262. `T` is the borrowed `SpiDriver`. The
/// caller builds the `SpiDeviceDriver` (SPI3 host, NSS as the device CS, mode 0,
/// ≤16 MHz to clear the SX1262 ceiling) and hands it off — peripheral
/// acquisition stays in the binary (mirrors dcentaxe-bap's transport).
///
/// The `T: Borrow<SpiDriver>` bound is required on the struct itself by
/// `SpiDeviceDriver` (esp-idf-hal 0.46).
pub struct EspSpiBus<'d, T: std::borrow::Borrow<esp_idf_hal::spi::SpiDriver<'d>>> {
    dev: SpiDeviceDriver<'d, T>,
}

impl<'d, T> EspSpiBus<'d, T>
where
    T: std::borrow::Borrow<esp_idf_hal::spi::SpiDriver<'d>>,
{
    pub fn new(dev: SpiDeviceDriver<'d, T>) -> Self {
        Self { dev }
    }
}

impl<'d, T> SpiBus for EspSpiBus<'d, T>
where
    T: std::borrow::Borrow<esp_idf_hal::spi::SpiDriver<'d>>,
{
    fn transfer(&mut self, buf: &mut [u8]) -> Result<(), LoraError> {
        // Full-duplex in place: clock `write` out on MOSI while capturing MISO
        // back into `buf`. The driver frames NSS automatically per transaction.
        let write = buf.to_vec();
        self.dev
            .transfer(buf, &write)
            .map_err(|e| LoraError::Transport(format!("{e:?}")))
    }
}

/// An input control pin (BUSY or DIO1). `is_high` is the only meaningful op; the
/// setters return an error so a wiring mistake (driving an input) is loud.
pub struct EspInputPin<'d> {
    pin: PinDriver<'d, Input>,
}

impl<'d> EspInputPin<'d> {
    pub fn new(pin: PinDriver<'d, Input>) -> Self {
        Self { pin }
    }
}

impl<'d> GpioPin for EspInputPin<'d> {
    fn is_high(&self) -> Result<bool, LoraError> {
        Ok(self.pin.is_high())
    }
    fn set_high(&mut self) -> Result<(), LoraError> {
        Err(LoraError::Gpio("cannot drive an input pin".into()))
    }
    fn set_low(&mut self) -> Result<(), LoraError> {
        Err(LoraError::Gpio("cannot drive an input pin".into()))
    }
}

/// The NRESET output pin (active-low). `is_high` returns an error (an output
/// driver does not read back here).
pub struct EspOutputPin<'d> {
    pin: PinDriver<'d, Output>,
}

impl<'d> EspOutputPin<'d> {
    pub fn new(pin: PinDriver<'d, Output>) -> Self {
        Self { pin }
    }
}

impl<'d> GpioPin for EspOutputPin<'d> {
    fn is_high(&self) -> Result<bool, LoraError> {
        Err(LoraError::Gpio("cannot read an output pin".into()))
    }
    fn set_high(&mut self) -> Result<(), LoraError> {
        self.pin
            .set_high()
            .map_err(|e| LoraError::Gpio(format!("{e:?}")))
    }
    fn set_low(&mut self) -> Result<(), LoraError> {
        self.pin
            .set_low()
            .map_err(|e| LoraError::Gpio(format!("{e:?}")))
    }
}
