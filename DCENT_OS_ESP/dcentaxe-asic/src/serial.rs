// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — Serial/UART abstraction
// Faithful port from ESP-Miner serial.c
//
// This module wraps esp-idf-hal UART to provide the same interface
// that the C code uses (SERIAL_init, SERIAL_send, SERIAL_rx, etc.)

use crate::common::{AsicError, UART_FREQ};

/// Default TX pin (GPIO17)
pub const DEFAULT_TX_PIN: i32 = 17;
/// Default RX pin (GPIO18)
pub const DEFAULT_RX_PIN: i32 = 18;
/// UART buffer size
const BUF_SIZE: usize = 1024;

/// Serial port abstraction for ASIC communication.
///
/// On ESP32-S3, this wraps UART1 with GPIO17 (TX) and GPIO18 (RX).
/// The abstraction allows driver code to remain platform-agnostic.
pub struct SerialPort {
    /// Raw file descriptor or handle for the UART (platform-specific)
    /// For ESP-IDF: this will be replaced with `esp_idf_hal::uart::UartDriver`
    /// For now, we store the configuration and provide a trait-based interface.
    tx_buf: Vec<u8>,
    #[cfg(not(target_os = "espidf"))]
    rx_buf: Vec<u8>,
    baud_rate: u32,
    initialized: bool,
    /// Platform-specific UART handle — opaque, set by init()
    /// In ESP-IDF builds, this becomes the UartDriver.
    /// For testing/simulation, this can be a mock.
    #[cfg(target_os = "espidf")]
    uart: Option<esp_idf_hal::uart::UartDriver<'static>>,
}

impl SerialPort {
    /// Create a new uninitialized serial port
    pub fn new() -> Self {
        Self {
            tx_buf: Vec::with_capacity(BUF_SIZE),
            #[cfg(not(target_os = "espidf"))]
            rx_buf: Vec::with_capacity(BUF_SIZE),
            baud_rate: UART_FREQ,
            initialized: false,
            #[cfg(target_os = "espidf")]
            uart: None,
        }
    }

    /// Create a SerialPort from a pre-built UartDriver.
    /// Use this when peripherals are already taken (e.g., from main.rs).
    #[cfg(target_os = "espidf")]
    pub fn from_uart(uart: esp_idf_hal::uart::UartDriver<'static>) -> Self {
        Self {
            tx_buf: Vec::with_capacity(BUF_SIZE),
            baud_rate: UART_FREQ,
            initialized: true,
            uart: Some(uart),
        }
    }

    /// Initialize the UART port.
    ///
    /// On ESP32-S3: configures UART1 on GPIO17 (TX) / GPIO18 (RX), 115200 baud, 8N1.
    /// Port of SERIAL_init() from serial.c.
    #[cfg(target_os = "espidf")]
    pub fn init(&mut self) -> Result<(), AsicError> {
        use esp_idf_hal::gpio;
        use esp_idf_hal::peripherals::Peripherals;
        use esp_idf_hal::uart::{config::Config, UartDriver};

        log::info!("Initializing serial");

        let peripherals = Peripherals::take()
            .map_err(|e| AsicError::Serial(format!("Failed to take peripherals: {:?}", e)))?;

        let config = Config::new().baudrate(esp_idf_hal::units::Hertz(self.baud_rate));

        let uart = UartDriver::new(
            peripherals.uart1,
            peripherals.pins.gpio17,
            peripherals.pins.gpio18,
            Option::<gpio::AnyIOPin<'_>>::None,
            Option::<gpio::AnyIOPin<'_>>::None,
            &config,
        )
        .map_err(|e| AsicError::Serial(format!("UART init failed: {:?}", e)))?;

        self.uart = Some(uart);
        self.initialized = true;

        Ok(())
    }

    /// Initialize (non-ESP stub for compilation/testing)
    #[cfg(not(target_os = "espidf"))]
    pub fn init(&mut self) -> Result<(), AsicError> {
        log::info!("Initializing serial (stub mode)");
        self.initialized = true;
        Ok(())
    }

    /// Check if the serial port has been initialized
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Send bytes over UART.
    ///
    /// Port of SERIAL_send() from serial.c.
    /// Returns the number of bytes written.
    #[cfg(target_os = "espidf")]
    pub fn write(&mut self, data: &[u8]) -> Result<usize, AsicError> {
        if let Some(ref uart) = self.uart {
            uart.write(data)
                .map_err(|e| AsicError::Serial(format!("UART write failed: {:?}", e)))
        } else {
            Err(AsicError::Serial("UART not initialized".to_string()))
        }
    }

    #[cfg(not(target_os = "espidf"))]
    pub fn write(&mut self, data: &[u8]) -> Result<usize, AsicError> {
        if !self.initialized {
            return Err(AsicError::Serial("UART not initialized".to_string()));
        }
        // Stub: buffer the data for testing
        self.tx_buf.extend_from_slice(data);
        Ok(data.len())
    }

    /// Read bytes from UART with timeout.
    ///
    /// Port of SERIAL_rx() from serial.c.
    /// Returns the number of bytes read, or 0 on timeout.
    #[cfg(target_os = "espidf")]
    pub fn read(&mut self, buf: &mut [u8], timeout_ms: u16) -> Result<usize, AsicError> {
        if let Some(ref uart) = self.uart {
            // esp-idf-hal 0.45: read() takes timeout as TickType_t (u32 ticks)
            // FreeRTOS tick = 1ms when CONFIG_FREERTOS_HZ=1000
            let timeout_ticks = timeout_ms as u32;
            match uart.read(buf, timeout_ticks) {
                Ok(n) => Ok(n),
                Err(e) => {
                    // ESP_ERR_TIMEOUT (263) is normal — no data available within timeout
                    let err_str = format!("{:?}", e);
                    if err_str.contains("TIMEOUT") || err_str.contains("263") {
                        Ok(0)
                    } else {
                        Err(AsicError::Serial(format!("UART read failed: {:?}", e)))
                    }
                }
            }
        } else {
            Err(AsicError::Serial("UART not initialized".to_string()))
        }
    }

    #[cfg(not(target_os = "espidf"))]
    pub fn read(&mut self, buf: &mut [u8], _timeout_ms: u16) -> Result<usize, AsicError> {
        if !self.initialized {
            return Err(AsicError::Serial("UART not initialized".to_string()));
        }
        let n = buf.len().min(self.rx_buf.len());
        if n == 0 {
            return Ok(0);
        }
        buf[..n].copy_from_slice(&self.rx_buf[..n]);
        self.rx_buf.drain(..n);
        Ok(n)
    }

    /// Change the UART baud rate.
    ///
    /// Port of SERIAL_set_baud() from serial.c.
    #[cfg(target_os = "espidf")]
    pub fn set_baud(&mut self, baud: u32) -> Result<(), AsicError> {
        log::info!("Changing UART baud to {}", baud);
        if let Some(ref uart) = self.uart {
            // Wait for TX to complete before changing baud
            uart.wait_tx_done(1000) // 1000 ticks = 1 second at CONFIG_FREERTOS_HZ=1000
                .map_err(|e| AsicError::Serial(format!("Wait TX done failed: {:?}", e)))?;
            uart.change_baudrate(esp_idf_hal::units::Hertz(baud))
                .map_err(|e| AsicError::Serial(format!("Set baud failed: {:?}", e)))?;
        }
        self.baud_rate = baud;
        Ok(())
    }

    #[cfg(not(target_os = "espidf"))]
    pub fn set_baud(&mut self, baud: u32) -> Result<(), AsicError> {
        log::info!("Changing UART baud to {}", baud);
        self.baud_rate = baud;
        Ok(())
    }

    /// Clear the UART RX buffer.
    ///
    /// Port of SERIAL_clear_buffer() from serial.c.
    #[cfg(target_os = "espidf")]
    pub fn clear_buffer(&mut self) -> Result<(), AsicError> {
        if let Some(ref uart) = self.uart {
            uart.clear_rx()
                .map_err(|e| AsicError::Serial(format!("Flush failed: {:?}", e)))?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "espidf"))]
    pub fn clear_buffer(&mut self) -> Result<(), AsicError> {
        self.rx_buf.clear();
        Ok(())
    }

    /// Get current baud rate
    pub fn baud_rate(&self) -> u32 {
        self.baud_rate
    }

    #[cfg(all(test, not(target_os = "espidf")))]
    pub(crate) fn push_rx(&mut self, data: &[u8]) {
        self.rx_buf.extend_from_slice(data);
    }

    /// Test-only view of the bytes written by the driver (TX capture). Used by
    /// the frequency-ramp regression tests to count emitted PLL packets.
    #[cfg(all(test, not(target_os = "espidf")))]
    pub(crate) fn tx_bytes(&self) -> &[u8] {
        &self.tx_buf
    }

    /// Test-only reset of the TX capture buffer so a test can measure the writes
    /// produced by a single operation in isolation.
    #[cfg(all(test, not(target_os = "espidf")))]
    pub(crate) fn clear_tx(&mut self) {
        self.tx_buf.clear();
    }
}

impl Default for SerialPort {
    fn default() -> Self {
        Self::new()
    }
}
