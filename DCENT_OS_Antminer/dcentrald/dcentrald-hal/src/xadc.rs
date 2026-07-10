//! XADC (Xilinx Analog-to-Digital Converter) reader.
//!
//! Reads Zynq die temperature, VCCINT, and VCCAUX via the IIO sysfs interface.
//! The XADC is built into the Zynq FPGA and provides internal system monitoring.
//!
//! Typical values from live S9 probe:
//!   Die temperature: ~41 C
//!   VCCINT: 0.99V (nominal 1.0V)
//!   VCCAUX: 1.78V (nominal 1.8V)
//!
//! IIO sysfs files:
//!   in_temp0_raw      - Raw ADC value (e.g., 36564)
//!   in_temp0_offset   - Offset for conversion (e.g., -2219)
//!   in_temp0_scale    - Scale factor in milli-degrees (e.g., 123.040771484)
//!   in_voltage0_vccint_raw  - Raw ADC value
//!   in_voltage0_vccint_scale - Scale factor in mV (e.g., 0.732421875)
//!   in_voltage1_vccaux_raw  - Raw ADC value
//!   in_voltage1_vccaux_scale - Scale factor in mV (e.g., 0.732421875)

use crate::{HalError, Result};

/// IIO sysfs base path for XADC.
pub const XADC_IIO_PATH: &str = "/sys/bus/iio/devices/iio:device0";

/// XADC readings from the Zynq SoC.
#[derive(Debug, Clone, Copy)]
pub struct XadcReadings {
    /// Zynq die temperature in degrees Celsius.
    pub die_temp_c: f32,
    /// VCCINT voltage (nominal 1.0V).
    pub vccint_v: f32,
    /// VCCAUX voltage (nominal 1.8V).
    pub vccaux_v: f32,
}

/// XADC reader.
pub struct Xadc;

impl Xadc {
    fn ensure_finite(attr: &str, value: f32) -> Result<f32> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(HalError::Xadc(format!(
                "{} produced non-finite value ({})",
                attr, value
            )))
        }
    }

    fn parse_iio_f32(attr: &str, content: &str) -> Result<f32> {
        let value = content.trim().parse::<f32>().map_err(|e| {
            HalError::Xadc(format!(
                "failed to parse {} ('{}'): {}",
                attr,
                content.trim(),
                e
            ))
        })?;
        Self::ensure_finite(attr, value)
    }

    /// Read a single IIO sysfs attribute as a float.
    fn read_iio_f32(attr: &str) -> Result<f32> {
        let path = format!("{}/{}", XADC_IIO_PATH, attr);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| HalError::Xadc(format!("failed to read {}: {}", attr, e)))?;
        Self::parse_iio_f32(attr, &content)
    }

    fn temp_c_from_iio(raw: f32, offset: f32, scale: f32) -> Result<f32> {
        Self::ensure_finite("in_temp0_celsius", (raw + offset) * scale / 1000.0)
    }

    fn voltage_v_from_iio(attr: &str, raw: f32, scale: f32) -> Result<f32> {
        Self::ensure_finite(attr, raw * scale / 1000.0)
    }

    /// Read all XADC channels.
    ///
    /// Temperature: (raw + offset) * scale / 1000.0 = degrees C
    /// Voltage: raw * scale / 1000.0 = volts
    pub fn read_all() -> Result<XadcReadings> {
        // Temperature: (raw + offset) * scale / 1000
        let temp_raw = Self::read_iio_f32("in_temp0_raw")?;
        let temp_offset = Self::read_iio_f32("in_temp0_offset")?;
        let temp_scale = Self::read_iio_f32("in_temp0_scale")?;
        let die_temp_c = Self::temp_c_from_iio(temp_raw, temp_offset, temp_scale)?;

        // VCCINT: raw * scale / 1000
        let vccint_raw = Self::read_iio_f32("in_voltage0_vccint_raw")?;
        let vccint_scale = Self::read_iio_f32("in_voltage0_vccint_scale")?;
        let vccint_v =
            Self::voltage_v_from_iio("in_voltage0_vccint_volts", vccint_raw, vccint_scale)?;

        // VCCAUX: raw * scale / 1000
        let vccaux_raw = Self::read_iio_f32("in_voltage1_vccaux_raw")?;
        let vccaux_scale = Self::read_iio_f32("in_voltage1_vccaux_scale")?;
        let vccaux_v =
            Self::voltage_v_from_iio("in_voltage1_vccaux_volts", vccaux_raw, vccaux_scale)?;

        Ok(XadcReadings {
            die_temp_c,
            vccint_v,
            vccaux_v,
        })
    }

    /// Read only the die temperature (celsius).
    ///
    /// This is the Zynq SoC die temperature, NOT the hash board chip temperature.
    /// It serves as a proxy for ambient temperature inside the miner enclosure.
    pub fn read_temp() -> Result<f32> {
        let temp_raw = Self::read_iio_f32("in_temp0_raw")?;
        let temp_offset = Self::read_iio_f32("in_temp0_offset")?;
        let temp_scale = Self::read_iio_f32("in_temp0_scale")?;
        Self::temp_c_from_iio(temp_raw, temp_offset, temp_scale)
    }
}

#[cfg(test)]
mod tests {
    use super::Xadc;

    #[test]
    fn iio_float_parser_rejects_non_finite_values() {
        for sample in ["NaN", "inf", "-inf", "Infinity", "-Infinity"] {
            let err = Xadc::parse_iio_f32("probe", sample)
                .expect_err("XADC sysfs parser must reject non-finite floats");
            assert!(
                err.to_string().contains("non-finite"),
                "unexpected parser error for {sample}: {err}"
            );
        }
    }

    #[test]
    fn xadc_conversions_reject_non_finite_results() {
        let err = Xadc::temp_c_from_iio(f32::MAX, f32::MAX, f32::MAX)
            .expect_err("overflowing XADC temperature conversion must fail");
        assert!(err.to_string().contains("non-finite"));

        let err = Xadc::voltage_v_from_iio("vccint", f32::MAX, f32::MAX)
            .expect_err("overflowing XADC voltage conversion must fail");
        assert!(err.to_string().contains("non-finite"));
    }
}
