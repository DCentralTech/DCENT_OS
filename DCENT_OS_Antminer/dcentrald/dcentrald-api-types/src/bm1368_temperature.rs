//! S21/BM1368 per-chip temperature readback DTOs.
//!
//! This module is intentionally HAL-free. It defines the REST/API contract and
//! the future live-observation seam, but it does not claim that DCENT_OS has
//! live-proven BM1368 per-chip temperature reads yet. Today the runtime has
//! board-level LM75-style temperature telemetry on Amlogic platforms; per-chip
//! BM1368 diode/register readback remains a separate proof item.

use serde::{Deserialize, Serialize};

pub const BM1368_CHIP_TEMPERATURE_SCHEMA: &str = "dcentos.hardware.bm1368_chip_temperatures.v1";
pub const BM1368_S21_CHIPS_PER_CHAIN: u16 = 108;
pub const BM1368_S21_ADDRESS_INTERVAL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bm1368ChipTemperatureStatus {
    /// The current ASIC family is not BM1368/S21/T21.
    Unsupported,
    /// BM1368 is the right family, but no live target readback path has been
    /// proven and wired into an API-state publisher.
    NotProven,
    /// A publisher exists but has not produced a current per-chip snapshot.
    NotWired,
    /// A service-owned snapshot supplied per-chip temperature observations.
    LiveSnapshot,
    /// Some observations were present, but at least one expected chip is
    /// missing or errored.
    PartialSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bm1368ChipAddressPlan {
    pub chip_index: u16,
    pub chip_addr: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bm1368ChipTemperatureObservation {
    pub chain_id: u8,
    pub chip_index: u16,
    pub chip_addr: u8,
    pub temp_c: f32,
    pub observed_at_ms: Option<u64>,
    pub source: String,
    pub raw_value: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bm1368ChainTemperatureReadback {
    pub chain_id: u8,
    pub expected_chip_count: u16,
    pub address_interval: Option<u8>,
    pub status: Bm1368ChipTemperatureStatus,
    pub board_temp_c: Option<f32>,
    pub board_temp_source: Option<String>,
    pub per_chip: Vec<Bm1368ChipTemperatureObservation>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bm1368ChainTemperatureInput {
    pub chain_id: u8,
    pub chip_count: u16,
    pub board_temp_c: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bm1368ChipTemperatureResponse {
    pub schema: String,
    pub status: Bm1368ChipTemperatureStatus,
    pub generated_at_ms: u64,
    pub chip_type: String,
    pub model: String,
    pub read_only: bool,
    pub rest_handler_hardware_reads: bool,
    pub rest_handler_hardware_writes: bool,
    pub control_actions: bool,
    pub live_service_handle_present: bool,
    pub live_target_read_proven: bool,
    pub chain_count: usize,
    pub per_chip_count: usize,
    pub chains: Vec<Bm1368ChainTemperatureReadback>,
    pub source: String,
    pub limitations: Vec<String>,
}

pub fn is_bm1368_chip_type(chip_type: &str) -> bool {
    chip_type.trim().eq_ignore_ascii_case("BM1368")
}

pub fn bm1368_address_interval(chip_count: u16) -> Option<u8> {
    match chip_count {
        0 => None,
        BM1368_S21_CHIPS_PER_CHAIN => Some(BM1368_S21_ADDRESS_INTERVAL),
        1..=256 => Some((256 / chip_count).max(1) as u8),
        _ => None,
    }
}

pub fn bm1368_chip_address(chip_index: u16, chip_count: u16) -> Option<u8> {
    if chip_index >= chip_count {
        return None;
    }
    let interval = bm1368_address_interval(chip_count)?;
    let addr = chip_index.checked_mul(interval as u16)?;
    u8::try_from(addr).ok()
}

pub fn bm1368_chip_address_plan(chip_count: u16) -> Vec<Bm1368ChipAddressPlan> {
    (0..chip_count)
        .filter_map(|chip_index| {
            bm1368_chip_address(chip_index, chip_count).map(|chip_addr| Bm1368ChipAddressPlan {
                chip_index,
                chip_addr,
            })
        })
        .collect()
}

impl Bm1368ChainTemperatureReadback {
    pub fn not_proven(input: Bm1368ChainTemperatureInput) -> Self {
        Self {
            chain_id: input.chain_id,
            expected_chip_count: input.chip_count,
            address_interval: bm1368_address_interval(input.chip_count),
            status: Bm1368ChipTemperatureStatus::NotProven,
            board_temp_c: input.board_temp_c,
            board_temp_source: input
                .board_temp_c
                .map(|_| "existing_chain_temperature_snapshot".to_string()),
            per_chip: Vec::new(),
            limitations: vec![
                "Per-chip BM1368 temperature readback is not live-proven on a target miner yet."
                    .to_string(),
                "Board/chain temperature is included only as existing aggregate telemetry, not as per-chip proof."
                    .to_string(),
            ],
        }
    }

    pub fn from_observations(
        chain_id: u8,
        expected_chip_count: u16,
        board_temp_c: Option<f32>,
        per_chip: Vec<Bm1368ChipTemperatureObservation>,
    ) -> Self {
        let complete = expected_chip_count > 0 && per_chip.len() == expected_chip_count as usize;
        Self {
            chain_id,
            expected_chip_count,
            address_interval: bm1368_address_interval(expected_chip_count),
            status: if complete {
                Bm1368ChipTemperatureStatus::LiveSnapshot
            } else {
                Bm1368ChipTemperatureStatus::PartialSnapshot
            },
            board_temp_c,
            board_temp_source: board_temp_c
                .map(|_| "existing_chain_temperature_snapshot".to_string()),
            per_chip,
            limitations: vec![
                "Per-chip temperatures are service-owned snapshots; REST remains read-only."
                    .to_string(),
            ],
        }
    }
}

impl Bm1368ChipTemperatureResponse {
    pub fn unsupported(
        chip_type: impl Into<String>,
        model: impl Into<String>,
        generated_at_ms: u64,
    ) -> Self {
        Self {
            schema: BM1368_CHIP_TEMPERATURE_SCHEMA.to_string(),
            status: Bm1368ChipTemperatureStatus::Unsupported,
            generated_at_ms,
            chip_type: chip_type.into(),
            model: model.into(),
            read_only: true,
            rest_handler_hardware_reads: false,
            rest_handler_hardware_writes: false,
            control_actions: false,
            live_service_handle_present: false,
            live_target_read_proven: false,
            chain_count: 0,
            per_chip_count: 0,
            chains: Vec::new(),
            source: "unsupported_chip_family".to_string(),
            limitations: vec![
                "This response shape is specific to S21/T21 BM1368 per-chip temperature readback."
                    .to_string(),
                "No hardware reads or writes were performed by REST.".to_string(),
            ],
        }
    }

    pub fn not_proven_for_bm1368(
        chip_type: impl Into<String>,
        model: impl Into<String>,
        generated_at_ms: u64,
        chains: Vec<Bm1368ChainTemperatureInput>,
    ) -> Self {
        let chains: Vec<Bm1368ChainTemperatureReadback> = chains
            .into_iter()
            .map(Bm1368ChainTemperatureReadback::not_proven)
            .collect();
        let chain_count = chains.len();

        Self {
            schema: BM1368_CHIP_TEMPERATURE_SCHEMA.to_string(),
            status: Bm1368ChipTemperatureStatus::NotProven,
            generated_at_ms,
            chip_type: chip_type.into(),
            model: model.into(),
            read_only: true,
            rest_handler_hardware_reads: false,
            rest_handler_hardware_writes: false,
            control_actions: false,
            live_service_handle_present: false,
            live_target_read_proven: false,
            chain_count,
            per_chip_count: 0,
            chains,
            source: "api_state_without_bm1368_chip_temperature_publisher".to_string(),
            limitations: vec![
                "BM1368/S21 per-chip temperature DTO shape is present, but live target readback is not proven."
                    .to_string(),
                "REST does not poll serial ASICs, issue register reads, or infer per-chip temperatures from board sensors."
                    .to_string(),
            ],
        }
    }

    pub fn from_live_observations(
        chip_type: impl Into<String>,
        model: impl Into<String>,
        generated_at_ms: u64,
        chains: Vec<Bm1368ChainTemperatureReadback>,
    ) -> Self {
        let per_chip_count = chains.iter().map(|chain| chain.per_chip.len()).sum();
        let status = if chains
            .iter()
            .all(|chain| chain.status == Bm1368ChipTemperatureStatus::LiveSnapshot)
            && per_chip_count > 0
        {
            Bm1368ChipTemperatureStatus::LiveSnapshot
        } else {
            Bm1368ChipTemperatureStatus::PartialSnapshot
        };
        let chain_count = chains.len();

        Self {
            schema: BM1368_CHIP_TEMPERATURE_SCHEMA.to_string(),
            status,
            generated_at_ms,
            chip_type: chip_type.into(),
            model: model.into(),
            read_only: true,
            rest_handler_hardware_reads: false,
            rest_handler_hardware_writes: false,
            control_actions: false,
            live_service_handle_present: true,
            live_target_read_proven: true,
            chain_count,
            per_chip_count,
            chains,
            source: "service_owned_bm1368_chip_temperature_snapshot".to_string(),
            limitations: vec![
                "REST serialized a service-owned snapshot and did not issue hardware reads."
                    .to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s21_address_plan_uses_verified_interval_two() {
        assert_eq!(
            bm1368_address_interval(BM1368_S21_CHIPS_PER_CHAIN),
            Some(BM1368_S21_ADDRESS_INTERVAL)
        );
        assert_eq!(bm1368_chip_address(0, BM1368_S21_CHIPS_PER_CHAIN), Some(0));
        assert_eq!(bm1368_chip_address(1, BM1368_S21_CHIPS_PER_CHAIN), Some(2));
        assert_eq!(
            bm1368_chip_address(107, BM1368_S21_CHIPS_PER_CHAIN),
            Some(214)
        );
        assert_eq!(bm1368_chip_address(108, BM1368_S21_CHIPS_PER_CHAIN), None);
    }

    #[test]
    fn not_proven_bm1368_response_does_not_claim_live_readback() {
        let response = Bm1368ChipTemperatureResponse::not_proven_for_bm1368(
            "BM1368",
            "Antminer S21",
            123_000,
            vec![Bm1368ChainTemperatureInput {
                chain_id: 0,
                chip_count: BM1368_S21_CHIPS_PER_CHAIN,
                board_temp_c: Some(58.5),
            }],
        );

        assert_eq!(response.schema, BM1368_CHIP_TEMPERATURE_SCHEMA);
        assert_eq!(response.status, Bm1368ChipTemperatureStatus::NotProven);
        assert!(!response.rest_handler_hardware_reads);
        assert!(!response.rest_handler_hardware_writes);
        assert!(!response.control_actions);
        assert!(!response.live_service_handle_present);
        assert!(!response.live_target_read_proven);
        assert_eq!(response.per_chip_count, 0);
        assert_eq!(response.chains[0].address_interval, Some(2));
        assert_eq!(response.chains[0].board_temp_c, Some(58.5));
        assert!(response.chains[0].per_chip.is_empty());
    }

    #[test]
    fn unsupported_response_is_explicit_for_non_bm1368_chips() {
        let response = Bm1368ChipTemperatureResponse::unsupported("BM1387", "Antminer S9", 456_000);
        assert_eq!(response.status, Bm1368ChipTemperatureStatus::Unsupported);
        assert_eq!(response.chain_count, 0);
        assert!(!response.live_target_read_proven);
        assert!(response.chains.is_empty());
    }

    #[test]
    fn live_observation_seam_reports_snapshot_without_rest_hardware_reads() {
        let observation = Bm1368ChipTemperatureObservation {
            chain_id: 0,
            chip_index: 1,
            chip_addr: 2,
            temp_c: 67.25,
            observed_at_ms: Some(999),
            source: "unit_test_publisher".to_string(),
            raw_value: Some(0x1234),
        };
        let chain =
            Bm1368ChainTemperatureReadback::from_observations(0, 1, Some(60.0), vec![observation]);
        let response = Bm1368ChipTemperatureResponse::from_live_observations(
            "BM1368",
            "Antminer S21",
            1_000,
            vec![chain],
        );

        assert_eq!(response.status, Bm1368ChipTemperatureStatus::LiveSnapshot);
        assert_eq!(response.per_chip_count, 1);
        assert!(response.live_service_handle_present);
        assert!(response.live_target_read_proven);
        assert!(!response.rest_handler_hardware_reads);
        assert_eq!(response.chains[0].per_chip[0].chip_addr, 2);
    }
}
