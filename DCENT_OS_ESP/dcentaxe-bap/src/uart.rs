// SPDX-License-Identifier: GPL-3.0-or-later
// ESP-IDF UART transport for BAP. Binds UART_NUM_2 to GPIO40 (RX) / GPIO39
// (TX) at 115200-8N1 — the pins the mining ESP32 dedicates to the BAP header.
// See ESP-Miner Kconfig defaults (`CONFIG_GPIO_BAP_RX=40`, `GPIO_BAP_TX=39`).
//
// This module is feature-gated (`esp-idf`) so host-side unit tests in the
// rest of the crate can stay pure-Rust and fast.

use esp_idf_hal::uart::{config::Config, UartDriver};

use crate::{protocol::BapError, BapTransport};

/// How long to wait for bytes per `read()` call. Short because the caller
/// drives its own loop cadence; non-blocking semantics are preferred.
const READ_TIMEOUT_MS: u32 = 10;

pub struct EspBapTransport<'d> {
    driver: UartDriver<'d>,
}

impl<'d> EspBapTransport<'d> {
    /// Create a transport bound to a pre-configured `UartDriver`. The caller
    /// is responsible for taking `UART_NUM_2` + GPIO 40/39 from peripherals
    /// and handing off the driver. Keeping construction in the caller avoids
    /// typestate gymnastics inside this crate.
    pub fn new(driver: UartDriver<'d>) -> Self {
        Self { driver }
    }

    /// Recommended UART config for the BAP link.
    pub fn recommended_config() -> Config {
        use esp_idf_hal::units::FromValueType;
        Config::default().baudrate(115_200u32.Hz())
    }
}

impl<'d> BapTransport for EspBapTransport<'d> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, BapError> {
        match self.driver.read(buf, READ_TIMEOUT_MS) {
            Ok(n) => Ok(n),
            Err(e) => Err(BapError::TransportRead(format!("{:?}", e))),
        }
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), BapError> {
        self.driver
            .write(bytes)
            .map(|_| ())
            .map_err(|e| BapError::TransportWrite(format!("{:?}", e)))
    }
}
