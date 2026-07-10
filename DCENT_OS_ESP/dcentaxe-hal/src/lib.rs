//! DCENT_axe Hardware Abstraction Layer
//!
//! Provides platform-specific hardware access for BitAxe boards:
//! - **board** — Board model detection and configuration (pin maps, voltage limits)
//! - **uart** — UART driver for ASIC chain communication (115200 -> 1M/3.125M baud)
//! - **i2c** — I2C bus driver for power ICs and temperature sensors (400 kHz)
//! - **gpio** — Discrete GPIO control (ASIC reset, buck enable, LED)
//! - **fan** — Fan PWM control (25 kHz LEDC) and tachometer RPM reading
//! - **power** — Voltage regulation (TPS546/DS4432U) and power monitoring (INA260)
//! - **temp** — Temperature sensing (EMC2101 internal + external diode, TPS546 fallback)
//!
//! # Architecture
//!
//! ```text
//! +------------------------------------------------------------------+
//! |                      Application Layer                           |
//! |  (mining daemon, thermal control, API server)                    |
//! +------------------------------------------------------------------+
//!           |           |           |          |           |
//!     +-----+---+ +----+----+ +----+----+ +---+---+ +----+----+
//!     | AsicUart| | I2cBus  | | GpioCtl | | Fan   | | PowerMgr|
//!     | (uart)  | | (i2c)   | | (gpio)  | | (fan) | | (power) |
//!     +----+----+ +----+----+ +----+----+ +---+---+ +----+----+
//!          |           |           |          |           |
//!     +----+----+ +----+----+ +----+----+ +---+---+ +----+----+
//!     | UART1   | | I2C0    | | GPIOs   | | LEDC  | | TPS546  |
//!     | TX:17   | | SDA:47  | | RST:1   | | PWM:11| | DS4432U |
//!     | RX:18   | | SCL:48  | | EN:46   | | TCH:14| | INA260  |
//!     +---------+ +---------+ | LED:4   | +-------+ | EMC2101 |
//!                             +---------+            +---------+
//! ```
//!
//! # Safety
//!
//! - Voltage settings are clamped to board-specific limits (cannot exceed max_voltage_mv);
//!   a `DRIVER_VOLTAGE_CEILING_MV` backstop in `safety` also caps the raw DAC/regulator
//!   drivers (HALPWR-3).
//! - The 20% mining fan-floor is enforced on the shipping path by `FanState::set_speed`
//!   (`pct.max(20)`) in the `dcentaxe` binary, not by the `fan::FanController` type (a
//!   reference implementation currently not on the shipping path — HALT-2). The `fan_pid`
//!   acoustic `min_pct` (30%) sits *above* the 20% floor and never undercuts it.
//! - `GpioController::power_on_sequence()` performs buck-enable → settle → reset in one
//!   fail-closed call (HALT-8); callers that drive `enable_buck`/`reset_asic` separately are
//!   responsible for that ordering.
//! - I2C operations have timeouts to prevent bus lockup.

// `board` is pure logic (pin-map tables, voltage limits, board-version
// profiles) with no ESP-IDF dependency, so it compiles on the host. The
// remaining peripheral driver modules link against esp-idf-hal/svc and are
// therefore gated to the ESP-IDF target. Host unit tests of the pure config /
// OTA logic (via the dcentaxe-core crate) need only `board`.
pub mod board;
// Pure CML fault-escalation window logic (no ESP-IDF dep) — host-testable and
// consumed by the espidf-only `power` module.
pub mod cml_escalation;
// Pure TPS546 write-protect policy (XPSAFE-2, cross-pollinated from DCENT_OS's
// HAL EEPROM write-denylist). No ESP-IDF dep — host-testable; consumed by the
// espidf-only `i2c` module's write path. Default-OFF (disarmed) so field-proven
// boards keep their exact prior behavior.
#[cfg(target_os = "espidf")]
pub mod display;
pub mod tps546_guard;
// Alloc-free fail-closed safety primitives (XPSAFE-1): buck-enable polarity +
// max-cooling fan-duty bytes. No ESP-IDF dep — host-testable and the single
// source of truth shared by the espidf-only panic hook and `gpio::enable_buck`.
pub mod safety;
// DCENT_axe on-board SX1262 LoRa radio pin map (PROVISIONAL — NEEDS-NETLIST-LOCK)
// + the esp-idf SPI3/HSPI bus builder. The pure pin map (const table + table
// test) is host-testable; the `open_lora_bus` builder inside is esp-idf-gated
// (integration seam, not host-tested). Default-OFF via the `pins-lora` feature —
// a non-LoRa SKU never compiles this module.
#[cfg(feature = "pins-lora")]
pub mod lora_pins;
// The pure decode/register-map layer of the EMC2103 driver is host-testable via
// the `dcentaxe-core` `#[path]` re-include (see emc2103.rs); the whole module is
// gated here because its I2C-backed `Emc2103` struct links esp-idf-hal.
#[cfg(target_os = "espidf")]
pub mod emc2103;
#[cfg(target_os = "espidf")]
pub mod emc2302;
#[cfg(target_os = "espidf")]
pub mod fan;
#[cfg(target_os = "espidf")]
pub mod fan_pid;
#[cfg(target_os = "espidf")]
pub mod gpio;
#[cfg(target_os = "espidf")]
pub mod i2c;
#[cfg(target_os = "espidf")]
pub mod power;
// Pure PMBus + DS4432U voltage/telemetry math (no ESP-IDF / log / serde) — split
// out of the espidf-only `power` module so the regulator-write/decode math is
// host-testable. `power.rs` re-exports the PMBus fns and calls `ds4432u_dac_code`
// so the regulator code path stays byte-identical.
pub mod power_convert;
#[cfg(target_os = "espidf")]
pub mod temp;
// Pure EMC2101 external-diode temperature decode (no ESP-IDF dep) — split out of
// the espidf-only `temp` module so the sensor-availability decision (HALT-3) is
// host-testable; consumed by `temp::Emc2101::read_external_temp`.
pub mod temp_decode;
#[cfg(target_os = "espidf")]
pub mod uart;

// Re-export key types for convenience
pub use board::{BitAxeModel, BoardConfig};
#[cfg(target_os = "espidf")]
pub use display::Ssd1306Display;
#[cfg(target_os = "espidf")]
pub use emc2103::Emc2103;
#[cfg(target_os = "espidf")]
pub use emc2302::Emc2302;
#[cfg(target_os = "espidf")]
pub use fan::FanController;
#[cfg(target_os = "espidf")]
pub use gpio::GpioController;
#[cfg(target_os = "espidf")]
pub use i2c::I2cBus;
#[cfg(target_os = "espidf")]
pub use power::{PowerManager, PowerTelemetry};
#[cfg(target_os = "espidf")]
pub use temp::{Emc2101, TempSensor, TemperatureReading, Tmp1075};
#[cfg(target_os = "espidf")]
pub use uart::AsicUart;
