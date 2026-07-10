//! Voltage-domain topology for multi-generation Antminer hashboards.
//!
//! This module is intentionally descriptive rather than mutating hardware. It
//! gives the autotuner a first-class representation of the voltage-control
//! granularity discovered in the reverse-engineering notes, so later runtime
//! controllers can reason about weak domains instead of treating every board as
//! one chain-level voltage rail.

use serde::{Deserialize, Serialize};

/// Known voltage-controller families seen across supported Antminer boards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoltageControllerKind {
    /// PIC16 controller used by S9-era boards.
    Pic16,
    /// dsPIC controller used by several x17/x19 boards.
    DsPic,
    /// Newer no-PIC boards where voltage is handled by board-local regulators.
    NoPic,
    /// TAS5782M/I2C DAC style control observed on S21 Pro class hardware.
    Tas5782m,
    /// PMBus-controlled PSU/board path.
    Pmbus,
    /// Unknown or not yet classified.
    Unknown,
}

impl VoltageControllerKind {
    /// Normalize config/API strings into a controller kind.
    pub fn from_voltage_control(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pic16" => Self::Pic16,
            "dspic" => Self::DsPic,
            "nopic" => Self::NoPic,
            "tas5782m" | "dac" | "i2c_dac" => Self::Tas5782m,
            "pmbus" => Self::Pmbus,
            _ => Self::Unknown,
        }
    }
}

/// A contiguous group of chips sharing one voltage-control domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoltageDomain {
    pub chain_id: u8,
    pub domain_id: u8,
    pub chip_start: u8,
    pub chip_end: u8,
    pub nominal_mv: Option<u16>,
    pub controller: VoltageControllerKind,
}

impl VoltageDomain {
    pub fn contains_chip(&self, chip_index: u8) -> bool {
        chip_index >= self.chip_start && chip_index <= self.chip_end
    }

    pub fn chip_count(&self) -> u8 {
        self.chip_end
            .saturating_sub(self.chip_start)
            .saturating_add(1)
    }
}

/// Per-chain voltage-domain layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoltageDomainTopology {
    pub profile_key: String,
    pub chip_id: u16,
    pub chips_per_chain: u8,
    pub domains_per_chain: u8,
    pub chips_per_domain: u8,
    pub controller: VoltageControllerKind,
    pub verified_from_re: bool,
}

impl VoltageDomainTopology {
    /// Expand the topology into chip-index ranges for one physical chain.
    pub fn domains_for_chain(&self, chain_id: u8) -> Vec<VoltageDomain> {
        let mut domains = Vec::with_capacity(self.domains_per_chain as usize);

        for domain_id in 0..self.domains_per_chain {
            let chip_start = domain_id.saturating_mul(self.chips_per_domain);
            if chip_start >= self.chips_per_chain {
                break;
            }
            let chip_end = chip_start
                .saturating_add(self.chips_per_domain)
                .saturating_sub(1)
                .min(self.chips_per_chain.saturating_sub(1));

            domains.push(VoltageDomain {
                chain_id,
                domain_id,
                chip_start,
                chip_end,
                nominal_mv: None,
                controller: self.controller,
            });
        }

        domains
    }

    /// Return the domain containing a chip index.
    pub fn domain_for_chip(&self, chain_id: u8, chip_index: u8) -> Option<VoltageDomain> {
        self.domains_for_chain(chain_id)
            .into_iter()
            .find(|domain| domain.contains_chip(chip_index))
    }
}

/// Build a topology from chip family, detected chain geometry, and controller.
///
/// Returns `None` when the chip count does not match a known reverse-engineered
/// production layout. That conservative behavior prevents the tuner from
/// inventing domain mappings for board variants we have not verified yet.
pub fn topology_for_chip(
    chip_id: u16,
    chips_per_chain: u8,
    voltage_control: &str,
) -> Option<VoltageDomainTopology> {
    let controller = VoltageControllerKind::from_voltage_control(voltage_control);

    match (chip_id, chips_per_chain) {
        // S9/BM1387: one chain-level PIC16 voltage domain for 63 chips.
        (0x1387, 63) => Some(VoltageDomainTopology {
            profile_key: "bm1387-s9-pic16-63x1".to_string(),
            chip_id,
            chips_per_chain,
            domains_per_chain: 1,
            chips_per_domain: 63,
            controller: if controller == VoltageControllerKind::Unknown {
                VoltageControllerKind::Pic16
            } else {
                controller
            },
            verified_from_re: true,
        }),

        // S19/BM1398: 76 chips, 38 domains, 2 chips per domain.
        (0x1398, 76) => Some(VoltageDomainTopology {
            profile_key: "bm1398-s19-76x38".to_string(),
            chip_id,
            chips_per_chain,
            domains_per_chain: 38,
            chips_per_domain: 2,
            controller,
            verified_from_re: true,
        }),

        // S19j Pro/BM1362: 126 chips, 42 domains, 3 chips per domain.
        (0x1362, 126) => Some(VoltageDomainTopology {
            profile_key: "bm1362-s19jpro-126x42".to_string(),
            chip_id,
            chips_per_chain,
            domains_per_chain: 42,
            chips_per_domain: 3,
            controller,
            verified_from_re: true,
        }),

        // S19 XP/BM1366: 110 chips, 11 domains, 10 chips per domain.
        (0x1366, 110) => Some(VoltageDomainTopology {
            profile_key: "bm1366-s19xp-110x11".to_string(),
            chip_id,
            chips_per_chain,
            domains_per_chain: 11,
            chips_per_domain: 10,
            controller,
            verified_from_re: true,
        }),

        // S21/T21/BM1368: 108 chips, 12 domains, 9 chips per domain.
        (0x1368, 108) => Some(VoltageDomainTopology {
            profile_key: "bm1368-s21-108x12".to_string(),
            chip_id,
            chips_per_chain,
            domains_per_chain: 12,
            chips_per_domain: 9,
            controller,
            verified_from_re: true,
        }),

        // S21 XP/BM1370P: 91 chips, 13 domains, 7 chips per domain.
        (0x1370, 91) => Some(VoltageDomainTopology {
            profile_key: "bm1370p-s21xp-91x13".to_string(),
            chip_id,
            chips_per_chain,
            domains_per_chain: 13,
            chips_per_domain: 7,
            controller: if controller == VoltageControllerKind::Unknown {
                VoltageControllerKind::Tas5782m
            } else {
                controller
            },
            verified_from_re: true,
        }),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_s9_chain_domain() {
        let topology = topology_for_chip(0x1387, 63, "pic16").expect("known S9 topology");
        let domains = topology.domains_for_chain(2);

        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0].chain_id, 2);
        assert_eq!(domains[0].domain_id, 0);
        assert_eq!(domains[0].chip_start, 0);
        assert_eq!(domains[0].chip_end, 62);
        assert_eq!(domains[0].chip_count(), 63);
    }

    #[test]
    fn expands_s19_voltage_domains() {
        let topology = topology_for_chip(0x1398, 76, "dspic").expect("known S19 topology");
        let domains = topology.domains_for_chain(0);

        assert_eq!(domains.len(), 38);
        assert_eq!(domains[0].chip_start, 0);
        assert_eq!(domains[0].chip_end, 1);
        assert_eq!(domains[37].chip_start, 74);
        assert_eq!(domains[37].chip_end, 75);
        assert_eq!(topology.domain_for_chip(0, 75).unwrap().domain_id, 37);
    }

    #[test]
    fn expands_s19jpro_voltage_domains() {
        let topology = topology_for_chip(0x1362, 126, "dspic").expect("known S19j topology");
        let domains = topology.domains_for_chain(4);

        assert_eq!(domains.len(), 42);
        assert_eq!(domains[0].chain_id, 4);
        assert_eq!(domains[0].chip_start, 0);
        assert_eq!(domains[0].chip_end, 2);
        assert_eq!(domains[41].chip_start, 123);
        assert_eq!(domains[41].chip_end, 125);
        assert_eq!(topology.domain_for_chip(4, 124).unwrap().domain_id, 41);
    }

    #[test]
    fn expands_s21_voltage_domains() {
        let topology = topology_for_chip(0x1368, 108, "nopic").expect("known S21 topology");
        let domains = topology.domains_for_chain(1);

        assert_eq!(domains.len(), 12);
        assert_eq!(domains[0].chip_count(), 9);
        assert_eq!(domains[11].chip_start, 99);
        assert_eq!(domains[11].chip_end, 107);
    }

    #[test]
    fn rejects_unverified_geometry() {
        assert!(topology_for_chip(0x1370, 65, "nopic").is_none());
        assert!(topology_for_chip(0x1368, 114, "nopic").is_none());
    }
}
