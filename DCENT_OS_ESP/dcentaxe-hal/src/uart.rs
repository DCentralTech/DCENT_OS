//! UART driver for ASIC communication.
//!
//! The ASIC chain communicates over a single UART bus at 115200 baud (default),
//! which is ramped up to 1 MHz or 3.125 MHz after initialization depending on
//! the ASIC model. The UART uses 8N1 format with no flow control.
//!
//! This module wraps `esp_idf_hal::uart::UartDriver` with a simpler interface
//! tailored for ASIC packet-based communication.

use esp_idf_hal::gpio::{InputPin, OutputPin};
use esp_idf_hal::uart::{self, UartDriver};
use log::*;

/// Default ASIC UART baud rate (used during initialization)
pub const DEFAULT_BAUD: u32 = 115_200;

/// UART RX/TX buffer size in bytes
const BUF_SIZE: usize = 1024;

/// Errors from UART operations
#[derive(Debug)]
pub enum UartError {
    /// Failed to initialize the UART peripheral
    InitFailed(String),
    /// Write operation failed or incomplete
    WriteFailed,
    /// Read timed out with no data
    ReadTimeout,
    /// Read returned an error
    ReadFailed,
    /// Failed to change baud rate
    BaudChangeFailed,
    /// Failed to flush the TX/RX buffers
    FlushFailed,
}

impl core::fmt::Display for UartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InitFailed(msg) => write!(f, "UART init failed: {}", msg),
            Self::WriteFailed => write!(f, "UART write failed"),
            Self::ReadTimeout => write!(f, "UART read timeout"),
            Self::ReadFailed => write!(f, "UART read failed"),
            Self::BaudChangeFailed => write!(f, "UART baud change failed"),
            Self::FlushFailed => write!(f, "UART flush failed"),
        }
    }
}

impl std::error::Error for UartError {}

/// UART driver wrapper for ASIC communication.
///
/// Manages a single UART peripheral connected to the ASIC chain.
/// All BitAxe models use UART1 with configurable TX/RX pins.
pub struct AsicUart<'d> {
    driver: UartDriver<'d>,
    current_baud: u32,
}

impl<'d> AsicUart<'d> {
    /// Initialize the UART peripheral for ASIC communication.
    ///
    /// Configures UART with 8N1 format, no flow control, at the default
    /// 115200 baud rate. The baud rate can be changed later after ASIC
    /// initialization completes.
    ///
    /// # Arguments
    /// * `uart` - UART peripheral instance (typically UART1)
    /// * `tx_pin` - GPIO pin for UART TX (ESP32 -> ASIC)
    /// * `rx_pin` - GPIO pin for UART RX (ASIC -> ESP32)
    pub fn new<UART: uart::Uart + 'd>(
        uart: UART,
        tx_pin: impl OutputPin + 'd,
        rx_pin: impl InputPin + 'd,
    ) -> Result<Self, UartError> {
        Self::with_baud(uart, tx_pin, rx_pin, DEFAULT_BAUD)
    }

    /// Initialize the UART peripheral with a specific baud rate.
    pub fn with_baud<UART: uart::Uart + 'd>(
        uart: UART,
        tx_pin: impl OutputPin + 'd,
        rx_pin: impl InputPin + 'd,
        baud_rate: u32,
    ) -> Result<Self, UartError> {
        let config = uart::config::Config::new()
            .baudrate(esp_idf_hal::units::Hertz(baud_rate))
            .data_bits(uart::config::DataBits::DataBits8)
            .parity_none()
            .stop_bits(uart::config::StopBits::STOP1)
            .flow_control(uart::config::FlowControl::None);

        let driver = UartDriver::new(
            uart,
            tx_pin,
            rx_pin,
            Option::<esp_idf_hal::gpio::AnyIOPin<'_>>::None, // CTS - not used
            Option::<esp_idf_hal::gpio::AnyIOPin<'_>>::None, // RTS - not used
            &config,
        )
        .map_err(|e| UartError::InitFailed(format!("{:?}", e)))?;

        info!("UART initialized at {} baud", baud_rate);

        Ok(Self {
            driver,
            current_baud: baud_rate,
        })
    }

    /// Write data to the UART TX buffer.
    ///
    /// Blocks until all bytes have been written to the hardware FIFO.
    /// For ASIC communication, typical packets are 7-88 bytes.
    pub fn write(&self, data: &[u8]) -> Result<(), UartError> {
        let written = self
            .driver
            .write(data)
            .map_err(|_| UartError::WriteFailed)?;

        if written != data.len() {
            warn!("UART write incomplete: {} of {} bytes", written, data.len());
            return Err(UartError::WriteFailed);
        }

        Ok(())
    }

    /// Read data from the UART RX buffer with a timeout.
    ///
    /// Returns the number of bytes actually read. If no data arrives within
    /// `timeout_ms`, returns 0 (not an error — the ASIC may simply have no
    /// response ready).
    ///
    /// # Arguments
    /// * `buf` - Buffer to read data into
    /// * `timeout_ms` - Maximum time to wait for data in milliseconds
    pub fn read(&self, buf: &mut [u8], timeout_ms: u32) -> Result<usize, UartError> {
        let ticks = timeout_ms / portTICK_PERIOD_MS;

        let bytes_read = self
            .driver
            .read(buf, ticks)
            .map_err(|_| UartError::ReadFailed)?;

        Ok(bytes_read)
    }

    /// Read exactly `len` bytes, blocking until all received or timeout.
    ///
    /// This is used for reading complete ASIC response packets where the
    /// expected length is known (9 bytes for BM1397, 11 bytes for BM1366/68/70).
    ///
    /// Returns `ReadTimeout` if the full packet is not received within the timeout.
    pub fn read_exact(&self, buf: &mut [u8], timeout_ms: u32) -> Result<(), UartError> {
        let mut total_read = 0;
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);

        while total_read < buf.len() {
            let remaining_ms = deadline
                .checked_duration_since(std::time::Instant::now())
                .map(|d| d.as_millis() as u32)
                .unwrap_or(0);

            if remaining_ms == 0 {
                return Err(UartError::ReadTimeout);
            }

            let n = self.read(&mut buf[total_read..], remaining_ms)?;
            if n == 0 {
                return Err(UartError::ReadTimeout);
            }
            total_read += n;
        }

        Ok(())
    }

    /// Change the UART baud rate.
    ///
    /// Called after ASIC initialization to switch from the default 115200 baud
    /// to the maximum baud rate supported by the ASIC:
    /// - BM1397: 3,125,000 bps
    /// - BM1366/68/70: 1,000,000 bps
    ///
    /// Waits for any pending TX data to complete before changing the baud rate.
    pub fn set_baud(&mut self, baud_rate: u32) -> Result<(), UartError> {
        info!("Changing UART baud: {} -> {}", self.current_baud, baud_rate);

        // Wait for TX to complete before changing baud
        self.driver
            .wait_tx_done(1000)
            .map_err(|_| UartError::BaudChangeFailed)?;

        self.driver
            .change_baudrate(esp_idf_hal::units::Hertz(baud_rate))
            .map_err(|_| UartError::BaudChangeFailed)?;

        self.current_baud = baud_rate;
        info!("UART baud rate set to {}", baud_rate);

        Ok(())
    }

    /// Flush the UART RX and TX buffers.
    ///
    /// Discards any pending data in both directions. Used during ASIC reset
    /// and error recovery to ensure a clean communication state.
    pub fn flush(&self) -> Result<(), UartError> {
        self.driver.clear_rx().map_err(|_| UartError::FlushFailed)?;
        Ok(())
    }

    /// Get the current baud rate.
    pub fn current_baud(&self) -> u32 {
        self.current_baud
    }
}

/// FreeRTOS tick period in milliseconds (ESP-IDF default is 1 ms per tick)
const portTICK_PERIOD_MS: u32 = 1;
