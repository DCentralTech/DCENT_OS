//! am2 PSU GPIO gate helper.
//!
//! S19j Pro `a lab unit` Phase 13/14 RE showed the APW121215a PSU bus is GPIO-gated:
//! bosminer asserts the `PWR_CONTROL` line before any I2C traffic reaches slave
//! `0x10`. Without that gate, every PSU opcode EIOs even when the frame bytes
//! and retry strategy are otherwise correct.
//!
//! Prefer the explicit `pwr_control_gpio` from the am2 production config.
//! Device-tree labels are useful when present, but `a lab unit` bring-up showed
//! stale or absent labels can point at the wrong line; fail closed if the
//! label cannot be resolved.

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use crate::{HalError, Result};

const DEFAULT_LABEL: &str = "PWR_CONTROL";
const GPIO_SETTLE_DELAY_MS: u64 = 50;

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// `(dt gpio-line-names path, Linux global GPIO base)` pairs for the am2 board.
///
/// Bases were live-probed on S19j Pro `a lab unit` during Phase 14 exploration:
/// - `gpio@41220000` -> 895..896
/// - `gpio@41210000` -> 897..901
/// - `gpio@41200000` -> 902..905
/// - `zynq_gpio` -> 906..
const DT_GPIO_LABEL_SOURCES: &[(&str, u32)] = &[
    (
        "/sys/firmware/devicetree/base/amba/gpio@41220000/gpio-line-names",
        895,
    ),
    (
        "/sys/firmware/devicetree/base/amba/gpio@41210000/gpio-line-names",
        897,
    ),
    (
        "/sys/firmware/devicetree/base/amba/gpio@41200000/gpio-line-names",
        902,
    ),
    (
        "/sys/firmware/devicetree/base/gpio@e000a000/gpio-line-names",
        906,
    ),
    (
        "/sys/firmware/devicetree/base/amba_ps/gpio@e000a000/gpio-line-names",
        906,
    ),
];

/// Scoped `PWR_CONTROL` assertion guard.
///
/// Records the line's prior sysfs direction/value and restores it on `Drop`.
pub struct PsuGpioGate {
    gpio: u32,
    restore_direction: String,
    restore_value: Option<bool>,
    exported_by_us: bool,
    asserted: bool,
}

/// Outcome of attempting to bring a GPIO line under sysfs control.
#[derive(Debug, Clone, Copy)]
enum ExportOutcome {
    /// A `/sys/class/gpio/gpioN/` directory was already present (either from a
    /// previous `PsuGpioGate` instance or from a boot-time init script).
    Existed,
    /// We exported it ourselves and should unexport on `Drop`.
    Created,
    /// Kernel-internal consumer holds the line (sysfs export returned EBUSY).
    /// Kernel-internal consumer holds the line (sysfs export returned EBUSY).
    /// This is not proof the line is asserted; callers must provide an
    /// explicit GPIO spec on AM2 production images so we can fail closed when
    /// sysfs cannot drive the gate.
    KernelClaimed,
}

impl PsuGpioGate {
    /// Assert the am2 PSU hardware gate before any PSU I2C access.
    ///
    /// `spec` accepts:
    /// - `None` -> lookup `label:PWR_CONTROL`
    /// - `label:PWR_CONTROL`
    /// - `gpio:901`
    /// - `901`
    pub fn assert(spec: Option<&str>) -> Result<Self> {
        let gpio = resolve_gpio(spec)?;
        let outcome = ensure_exported(gpio)?;

        if matches!(outcome, ExportOutcome::KernelClaimed) {
            return Err(HalError::Gpio(format!(
                "PWR_CONTROL GPIO {} is kernel-claimed (EBUSY); cannot verify/assert PSU gate",
                gpio
            )));
        }

        let exported_by_us = matches!(outcome, ExportOutcome::Created);
        let restore_direction = read_trimmed(&direction_path(gpio))?;
        let restore_value = read_value(gpio).ok();

        write_direction(gpio, "out")?;
        // 2026-06-07 (.25 active-LOW PWR_CONTROL): the RE-018 true-cold strace
        // proves gpio907 on `a lab unit` is ACTIVE-LOW — "0" = rail ON, "1" = rail OFF
        // (bosminer writes "1" to hold-off at cold, then "0" to energize ~55 s
        // later). DCENT historically wrote "1" to "assert", which on `a lab unit` turns
        // the rail OFF → the per-board DC-DC has no input → chips unpowered →
        // chain enum=0 even though the dsPIC ENABLE ACKs (the dsPIC runs on the
        // 3.3 V standby rail). Gate the asserted level on
        // DCENT_AM2_PWR_CONTROL_ACTIVE_LOW: default-OFF keeps the active-HIGH
        // ("1") behaviour for every other unit AND the bosminer-handoff path
        // (which uses TRUST_RAIL_FALLBACK and never asserts here) byte-identical.
        let active_low = env_flag("DCENT_AM2_PWR_CONTROL_ACTIVE_LOW");
        let active_high = env_flag("DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH");
        if gpio == crate::board_control::AM2_PSU_ENABLE_GPIO && active_low == active_high {
            return Err(HalError::Gpio(format!(
                "PWR_CONTROL gpio{} polarity unknown or conflicting; set exactly one of \
                 DCENT_AM2_PWR_CONTROL_ACTIVE_LOW=1 or DCENT_AM2_PWR_CONTROL_ACTIVE_HIGH=1",
                gpio
            )));
        }
        // ON = high("1") for active-HIGH, low("0") for active-LOW.
        let asserted_value = !active_low;
        write_value(gpio, asserted_value)?;
        let observed_value = read_value(gpio).ok();
        // P1 (2026-06-13): verify the readback on the RESOLVED gpio, identical to
        // the polarity gate above (`gpio == AM2_PSU_ENABLE_GPIO`), NOT on the
        // literal spec string. `None`, `"PWR_CONTROL"`, `"label:PWR_CONTROL"`,
        // `"907"`, and `"gpio:907"` all resolve to the PSU-enable line (see
        // `resolve_gpio` + the P2 reconcile), and a silent assert-readback
        // mismatch on that line must fail closed regardless of how the operator
        // spelled the spec. The old `explicit_gpio907` string gate skipped the
        // check for the label/None forms.
        if gpio == crate::board_control::AM2_PSU_ENABLE_GPIO {
            match observed_value {
                Some(value) if value == asserted_value => {}
                Some(value) => {
                    return Err(HalError::Gpio(format!(
                        "PWR_CONTROL gpio{} readback mismatch after assert: wrote {} \
                         (active_low={}), read {}",
                        gpio, asserted_value as u8, active_low, value as u8
                    )));
                }
                None => {
                    return Err(HalError::Gpio(format!(
                        "PWR_CONTROL gpio{} readback unavailable after assert",
                        gpio
                    )));
                }
            }
        }

        tracing::info!(
            gpio,
            spec = spec.unwrap_or("label:PWR_CONTROL"),
            active_low,
            observed_asserted = ?observed_value,
            "PWR_CONTROL asserted before PSU init (sysfs readback recorded)"
        );

        Ok(Self {
            gpio,
            restore_direction,
            restore_value,
            exported_by_us,
            asserted: true,
        })
    }

    pub fn gpio(&self) -> u32 {
        self.gpio
    }

    pub fn is_asserted(&self) -> bool {
        self.asserted
    }

    /// Test-only constructor: builds a guard for a fake line without touching
    /// `/sys`. `Drop`/`deassert` will fail their sysfs writes (swallowed by
    /// `Drop`), so don't rely on them in tests beyond "must not panic".
    #[cfg(test)]
    pub(crate) fn for_test(gpio: u32) -> Self {
        Self {
            gpio,
            restore_direction: "in".to_string(),
            restore_value: None,
            exported_by_us: false,
            asserted: true,
        }
    }

    /// Restore the line to its pre-asserted state.
    pub fn deassert(&mut self) -> Result<()> {
        if !self.asserted {
            return Ok(());
        }

        match self.restore_direction.as_str() {
            "in" => write_direction(self.gpio, "in")?,
            "out" => {
                write_direction(self.gpio, "out")?;
                if let Some(prev) = self.restore_value {
                    write_value(self.gpio, prev)?;
                }
            }
            other => {
                tracing::warn!(
                    gpio = self.gpio,
                    direction = other,
                    "Unexpected GPIO direction while restoring PWR_CONTROL; falling back to stored value"
                );
                if let Some(prev) = self.restore_value {
                    write_direction(self.gpio, "out")?;
                    write_value(self.gpio, prev)?;
                } else {
                    write_direction(self.gpio, "in")?;
                }
            }
        }

        if self.exported_by_us {
            let _ = fs::write("/sys/class/gpio/unexport", format!("{}", self.gpio));
        }

        self.asserted = false;
        tracing::info!(gpio = self.gpio, "PWR_CONTROL restored");
        Ok(())
    }
}

impl Drop for PsuGpioGate {
    fn drop(&mut self) {
        if let Err(e) = self.deassert() {
            tracing::warn!(gpio = self.gpio, error = %e, "Failed to restore PWR_CONTROL on drop");
        }
    }
}

fn resolve_gpio(spec: Option<&str>) -> Result<u32> {
    match parse_spec(spec)? {
        ParsedSpec::Gpio(gpio) => Ok(gpio),
        // P2 (2026-06-13): the canonical `PWR_CONTROL` label resolves to the
        // live-pinned am2 PSU-enable line, IDENTICAL to the teardown resolver
        // (`s19j_hybrid_mining::parse_gpio_number_spec`), so `assert` and
        // `force_pwr_control_low` can never drive different GPIOs for the same
        // spec. Previously this label went to DT `gpio-line-names`, which a unit
        // test pins to gpio901 for the 0x41210000 bank — a teardown/assert
        // divergence that could leave the rail energized. Only a NON-canonical
        // label still falls through to DT lookup (the `a lab unit` bring-up fallback).
        ParsedSpec::Label(label) if label.eq_ignore_ascii_case(DEFAULT_LABEL) => {
            Ok(crate::board_control::AM2_PSU_ENABLE_GPIO)
        }
        ParsedSpec::Label(label) => match find_gpio_by_dt_label(label)? {
            Some(gpio) => Ok(gpio),
            None => Err(HalError::Gpio(format!(
                "failed to resolve GPIO label '{}' from DT gpio-line-names",
                label
            ))),
        },
    }
}

enum ParsedSpec<'a> {
    Gpio(u32),
    Label(&'a str),
}

fn parse_spec(spec: Option<&str>) -> Result<ParsedSpec<'_>> {
    match spec.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(ParsedSpec::Label(DEFAULT_LABEL)),
        Some(raw) => {
            if let Some(rest) = raw.strip_prefix("gpio:") {
                let gpio = rest
                    .trim()
                    .parse::<u32>()
                    .map_err(|e| HalError::Gpio(format!("invalid gpio spec '{}': {}", raw, e)))?;
                return Ok(ParsedSpec::Gpio(gpio));
            }
            if let Some(rest) = raw.strip_prefix("label:") {
                let label = rest.trim();
                if label.is_empty() {
                    return Err(HalError::Gpio("empty GPIO label spec".into()));
                }
                return Ok(ParsedSpec::Label(label));
            }
            if raw.bytes().all(|b| b.is_ascii_digit()) {
                let gpio = raw
                    .parse::<u32>()
                    .map_err(|e| HalError::Gpio(format!("invalid gpio number '{}': {}", raw, e)))?;
                return Ok(ParsedSpec::Gpio(gpio));
            }
            Ok(ParsedSpec::Label(raw))
        }
    }
}

fn find_gpio_by_dt_label(label: &str) -> Result<Option<u32>> {
    for (path, base) in DT_GPIO_LABEL_SOURCES {
        let dt_path = Path::new(path);
        if !dt_path.exists() {
            continue;
        }
        let blob = fs::read(dt_path).map_err(|e| {
            HalError::Gpio(format!(
                "read DT gpio-line-names '{}': {}",
                dt_path.display(),
                e
            ))
        })?;
        if let Some(gpio) = gpio_from_dt_blob(&blob, *base, label) {
            tracing::info!(label, gpio, dt_path = %dt_path.display(), "Resolved GPIO label from DT");
            return Ok(Some(gpio));
        }
    }
    Ok(None)
}

fn gpio_from_dt_blob(blob: &[u8], base: u32, label: &str) -> Option<u32> {
    for (idx, raw_name) in blob.split(|b| *b == 0).enumerate() {
        if raw_name == label.as_bytes() {
            return Some(base + idx as u32);
        }
    }
    None
}

fn gpio_dir(gpio: u32) -> String {
    format!("/sys/class/gpio/gpio{}", gpio)
}

fn direction_path(gpio: u32) -> String {
    format!("{}/direction", gpio_dir(gpio))
}

fn value_path(gpio: u32) -> String {
    format!("{}/value", gpio_dir(gpio))
}

fn ensure_exported(gpio: u32) -> Result<ExportOutcome> {
    let dir = gpio_dir(gpio);
    if Path::new(&dir).exists() {
        return Ok(ExportOutcome::Existed);
    }

    match fs::write("/sys/class/gpio/export", format!("{}", gpio)) {
        Ok(()) => {
            sleep(Duration::from_millis(GPIO_SETTLE_DELAY_MS));
            if !Path::new(&dir).exists() {
                return Err(HalError::Gpio(format!(
                    "GPIO {} did not appear after export",
                    gpio
                )));
            }
            Ok(ExportOutcome::Created)
        }
        // EBUSY (errno 16): kernel consumer holds the line. See ExportOutcome::KernelClaimed.
        Err(e) if e.raw_os_error() == Some(16) => {
            tracing::warn!(
                gpio,
                "GPIO {} EBUSY on sysfs export — kernel already claims the line \
                 (no consumer label; likely Xilinx xps-gpio default hold)",
                gpio
            );
            Ok(ExportOutcome::KernelClaimed)
        }
        Err(e) => Err(HalError::Gpio(format!("export GPIO {}: {}", gpio, e))),
    }
}

fn read_trimmed(path: &str) -> Result<String> {
    Ok(fs::read_to_string(path)
        .map_err(|e| HalError::Gpio(format!("read {}: {}", path, e)))?
        .trim()
        .to_string())
}

fn read_value(gpio: u32) -> Result<bool> {
    Ok(read_trimmed(&value_path(gpio))? == "1")
}

fn write_direction(gpio: u32, dir: &str) -> Result<()> {
    fs::write(direction_path(gpio), dir)
        .map_err(|e| HalError::Gpio(format!("GPIO {} direction {}: {}", gpio, dir, e)))
}

fn write_value(gpio: u32, high: bool) -> Result<()> {
    fs::write(value_path(gpio), if high { "1" } else { "0" })
        .map_err(|e| HalError::Gpio(format!("GPIO {} value {}: {}", gpio, high as u8, e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_numeric_gpio_specs() {
        match parse_spec(Some("901")).unwrap() {
            ParsedSpec::Gpio(gpio) => assert_eq!(gpio, 901),
            _ => panic!("expected numeric gpio spec"),
        }
        match parse_spec(Some("gpio:907")).unwrap() {
            ParsedSpec::Gpio(gpio) => assert_eq!(gpio, 907),
            _ => panic!("expected prefixed numeric gpio spec"),
        }
    }

    #[test]
    fn parse_label_specs() {
        match parse_spec(None).unwrap() {
            ParsedSpec::Label(label) => assert_eq!(label, "PWR_CONTROL"),
            _ => panic!("expected default label spec"),
        }
        match parse_spec(Some("label:PWR_CONTROL")).unwrap() {
            ParsedSpec::Label(label) => assert_eq!(label, "PWR_CONTROL"),
            _ => panic!("expected label spec"),
        }
    }

    #[test]
    fn resolve_label_from_dt_blob() {
        let blob = b"HB0_RESET\0HB1_RESET\0HB2_RESET\0HB3_RESET\0PWR_CONTROL\0";
        assert_eq!(gpio_from_dt_blob(blob, 897, "PWR_CONTROL"), Some(901));
        assert_eq!(gpio_from_dt_blob(blob, 897, "HB1_RESET"), Some(898));
        assert_eq!(gpio_from_dt_blob(blob, 897, "missing"), None);
    }
}
