//! STM32MP15 / Braiins BCB100 platform scaffold.
//!
//! BCB100 is a Braiins replacement control board for S19-family Antminers.
//! Public hardware docs establish the non-destructive platform identity:
//! STM32MP157-class SoC, 4 GB eMMC, microSD boot, four hashboard connectors,
//! four fan headers, and direct SoC I/O rather than a Bitmain FPGA.
//!
//! This module is intentionally conservative. The public files do not expose
//! a live-proved map for hashboard reset, plug detect, fan PWM/tach, PSU
//! control, or PIC bus ownership. Until a bench BCB100 probe captures those
//! facts, construction is gated behind `DCENT_BCB100_ACCEPT_UNVERIFIED=1`,
//! fan/GPIO control returns an error, and per-chain PIC addresses are unset.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use super::config::{
    probe_tty_chain_device, ChainConfig, ChainTransport, PlatformConfig, VoltageControllerKind,
};
use super::{BoardType, ChainAccess, FanAccess, GpioAccess, Platform};
use crate::i2c::I2cBus;
use crate::serial::SerialChain;
use crate::{HalError, Result};

/// Lab-only acceptance gate for constructing the BCB100 HAL.
pub const BCB100_ACCEPT_UNVERIFIED_ENV: &str = "DCENT_BCB100_ACCEPT_UNVERIFIED";

/// Candidate STM32MP15 Linux UART device names for the four BCB100 chain ports.
///
/// Source status: inferred from STM32MP15 Linux tty naming and Braiins binary
/// string evidence, not a DCENT live pin probe. These names are therefore used
/// only for lab discovery / passthrough, not for cold-boot reset or voltage.
pub const BCB100_CANDIDATE_CHAIN_UARTS: [&str; 4] = [
    "/dev/ttySTM0",
    "/dev/ttySTM1",
    "/dev/ttySTM2",
    "/dev/ttySTM3",
];

/// Standard hashboard EEPROM deny range used across BHB42xxx/BHB56xxx boards.
pub const BCB100_HASHBOARD_EEPROM_DENYLIST: [u8; 8] =
    [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];

/// Narrow provisional I2C surface for read-only lab probing.
const BCB100_ALLOWED_I2C_BUSES: [u8; 2] = [0, 1];

/// Returns true when a DT-compatible or CPU-info blob names STM32MP15.
pub fn compatible_bytes_look_like_stm32mp15(data: &[u8]) -> bool {
    let haystack = String::from_utf8_lossy(data).to_ascii_lowercase();
    haystack.contains("stm32mp15") || haystack.contains("stm32mp157")
}

fn read_bytes(path: &str) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// Host signature for STM32MP15-class boards, including BCB100.
pub fn looks_like_bcb100_host() -> bool {
    let compatible = read_bytes("/proc/device-tree/compatible");
    let compatible_alt = read_bytes("/sys/firmware/devicetree/base/compatible");
    let cpuinfo = read_bytes("/proc/cpuinfo");

    compatible_bytes_look_like_stm32mp15(&compatible)
        || compatible_bytes_look_like_stm32mp15(&compatible_alt)
        || compatible_bytes_look_like_stm32mp15(&cpuinfo)
        || BCB100_CANDIDATE_CHAIN_UARTS
            .iter()
            .any(|path| Path::new(path).exists())
}

fn tty_candidates_for_chain(chain: &ChainConfig) -> Vec<String> {
    let declared = match &chain.transport {
        ChainTransport::Serial { device, .. } => device.clone(),
        _ => return Vec::new(),
    };
    let Some(default) = BCB100_CANDIDATE_CHAIN_UARTS.get(chain.chain_id as usize) else {
        return vec![declared];
    };
    if declared == *default {
        vec![declared]
    } else {
        vec![declared, (*default).to_string()]
    }
}

/// Braiins BCB100 / STM32MP15 platform.
pub struct Bcb100Platform {
    config: PlatformConfig,
}

impl Bcb100Platform {
    pub fn new() -> Result<Self> {
        if !looks_like_bcb100_host() {
            return Err(HalError::Platform(
                "BCB100: no STM32MP15 or ttySTM signature found".to_string(),
            ));
        }

        if !env_flag(BCB100_ACCEPT_UNVERIFIED_ENV) {
            return Err(HalError::Platform(format!(
                "BCB100/STM32MP15 detected but disabled: set {}=1 only for lab discovery. \
                 GPIO, fan, PSU, and PIC maps are not live-verified.",
                BCB100_ACCEPT_UNVERIFIED_ENV
            )));
        }

        tracing::warn!(
            env = BCB100_ACCEPT_UNVERIFIED_ENV,
            "BCB100 platform scaffold enabled for lab discovery; cold boot is not wired"
        );
        Ok(Self {
            config: PlatformConfig::bcb100_s19_lab(),
        })
    }

    pub fn with_config(config: PlatformConfig) -> Self {
        Self { config }
    }
}

impl Platform for Bcb100Platform {
    fn board_type(&self) -> BoardType {
        BoardType::Stm32Mp15
    }

    fn chain_count(&self) -> u8 {
        self.config.chains.len() as u8
    }

    fn open_chain(&self, chain_id: u8) -> Result<Box<dyn ChainAccess>> {
        let chain_config = self
            .config
            .chains
            .iter()
            .find(|c| c.chain_id == chain_id)
            .ok_or_else(|| HalError::Platform(format!("chain {} not configured", chain_id)))?;

        match &chain_config.transport {
            ChainTransport::Serial { device, baud } => {
                let candidates = tty_candidates_for_chain(chain_config);
                let candidate_refs: Vec<&str> = candidates.iter().map(String::as_str).collect();
                let label = format!("bcb100-chain-{}", chain_id);
                let resolved =
                    probe_tty_chain_device(&candidate_refs, &label).unwrap_or_else(|| {
                        tracing::warn!(
                            chain = chain_id,
                            declared = %device,
                            "BCB100 tty probe failed; trying declared device directly"
                        );
                        device.clone()
                    });
                let serial = SerialChain::open(&resolved, *baud).map_err(|e| {
                    HalError::Platform(format!(
                        "BCB100 chain {}: open {} failed ({}). Stop bosminer first or use a read-only probe.",
                        chain_id, resolved, e
                    ))
                })?;
                Ok(Box::new(Bcb100ChainAccess {
                    serial: Mutex::new(serial),
                }))
            }
            other => Err(HalError::Platform(format!(
                "unexpected transport for BCB100 chain {}: {:?}",
                chain_id, other
            ))),
        }
    }

    fn open_i2c(&self, bus: u8) -> Result<I2cBus> {
        if !BCB100_ALLOWED_I2C_BUSES.contains(&bus) {
            return Err(HalError::Platform(format!(
                "BCB100: /dev/i2c-{} is outside the provisional lab allowlist {:?}",
                bus, BCB100_ALLOWED_I2C_BUSES
            )));
        }
        let mut handle = I2cBus::open(bus)?;
        handle.set_write_denylist(&BCB100_HASHBOARD_EEPROM_DENYLIST);
        Ok(handle)
    }

    fn open_fan(&self) -> Result<Box<dyn FanAccess>> {
        Err(HalError::Fan(
            "BCB100 fan PWM/tach map is not live-verified; refusing fan control".to_string(),
        ))
    }

    fn open_gpio(&self) -> Result<Box<dyn GpioAccess>> {
        Err(HalError::Platform(
            "BCB100 GPIO reset/plug map is not live-verified; refusing GPIO control".to_string(),
        ))
    }

    fn voltage_controller(&self) -> VoltageControllerKind {
        self.config.voltage_controller
    }
}

struct Bcb100ChainAccess {
    serial: Mutex<SerialChain>,
}

impl ChainAccess for Bcb100ChainAccess {
    fn send_command(&self, data: &[u8]) -> Result<()> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.write_bytes(data)
    }

    fn read_response(&self, buf: &mut [u8]) -> Result<usize> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.read_bytes(buf)
    }

    fn send_work(&self, data: &[u8]) -> Result<()> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.write_bytes(data)
    }

    fn read_nonce(&self, buf: &mut [u8]) -> Result<usize> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.read_bytes(buf)
    }

    fn set_baud(&self, baud: u32) -> Result<()> {
        let mut serial = self
            .serial
            .lock()
            .map_err(|_| HalError::Platform("serial mutex poisoned".into()))?;
        serial.set_baud(baud)
    }

    fn wait_for_nonce(&self) -> Result<()> {
        std::thread::yield_now();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stm32mp15_signature_handles_nul_separated_compatible() {
        assert!(compatible_bytes_look_like_stm32mp15(
            b"st,stm32mp157c-ii1\0st,stm32mp157\0"
        ));
        assert!(compatible_bytes_look_like_stm32mp15(
            b"Hardware\t: STM32MP15"
        ));
        assert!(!compatible_bytes_look_like_stm32mp15(b"am33xx"));
    }

    #[test]
    fn bcb100_with_config_reports_lab_board_type() {
        let platform = Bcb100Platform::with_config(PlatformConfig::bcb100_s19_lab());
        assert_eq!(platform.board_type(), BoardType::Stm32Mp15);
        assert_eq!(platform.chain_count(), 4);
        assert!(matches!(
            platform.voltage_controller(),
            VoltageControllerKind::Dspic33Ep
        ));
    }
}
