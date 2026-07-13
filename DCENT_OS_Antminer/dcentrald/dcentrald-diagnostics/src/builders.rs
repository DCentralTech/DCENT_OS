use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::board_health::BoardHealthResult;
use crate::chip_health::{
    ChipColor, ChipHealthChainSnapshot, ChipHealthSnapshot, ChipMap, ChipMapCell,
};
use crate::evidence::DiagnosticEvidence;
use crate::hashreport::{
    calculate_unit_grade, score_to_grade, BaselineSnapshot, BoardResult, ChipHealthScore,
    HashReport, SystemInfo, WindowData,
};
use crate::snapshot::{
    SnapshotChain, SnapshotChipHealth, SnapshotContext, SnapshotHistorySample, SnapshotProfile,
};

/// Cores-per-chip for the RE-010 LOW-1 `expected_nonce_rate_hz` derivation.
/// Keyed on `SnapshotContext::chip_type` (canonical strings: "BM1387",
/// "BM1397", "BM1398", "BM1362", "BM1366", "BM1368", …).
///
/// Source provenance — all values cite the canonical `dcentrald-silicon-profiles`
/// crate, which has its own live/RE-confirmed citations:
///
/// | family | cores_per_chip | source                                      |
/// |--------|----------------|---------------------------------------------|
/// | BM1387 | 114            | bm1387.rs:6 (S9 standard; BM1387P=128)      |
/// | BM1397 | 672            | bm1397.rs:7,89                              |
/// | BM1398 | 672            | bm1398.rs:87 (was 894 = BM1366's value)     |
/// | BM1362 | 672            | bm1362.rs:265 (per-die small cores; conservative until live cross-check) |
/// | BM1366 | 894            | bm1366.rs:15,97                             |
/// | BM1368 | 1280           | bm1368.rs:7 (S21 fixture-RE confirmed)      |
/// | BM1370 | (unknown)      | bm1370.rs:7 says "1024+"; conservative None |
/// | BM1360 | (unknown)      | bm1360.rs:31 — no literal anywhere          |
/// | BM1491 | (unknown)      | bm1491.rs:33 — no literal anywhere          |
///
/// Returns `None` for families where `dcentrald-silicon-profiles` has no
/// confirmed value — keeping `expected_nonce_rate_hz` honest rather than
/// fabricating numbers.
fn cores_per_chip_for_family(chip_type: &str) -> Option<u32> {
    match chip_type {
        "BM1387" => Some(114),
        "BM1387P" => Some(128),
        "BM1397" => Some(672),
        "BM1397+" | "BM1397plus" => Some(672),
        // 672 not 894: BM1398 (S19 Pro) matches BM1397 at 672 cores per
        // BM1398_CORES_PER_CHIP (silicon-profiles bm1398.rs:87) + ;
        // 894 is BM1366's value (bm1366.rs:115) — the prior 894 was a copy-paste.
        "BM1398" => Some(672),
        "BM1362" => Some(672),
        "BM1366" => Some(894),
        "BM1368" => Some(1280),
        // BM1370 ("1024+"), BM1360 (no literal), BM1491 (no literal),
        // BM1373, BM1485, BM1489 — return None so `expected_nonce_rate_hz`
        // stays None rather than reporting a fabricated rate.
        _ => None,
    }
}

/// Compute the expected nonce-rate (nonces/second) for a healthy chip at
/// the given frequency and core count.
///
/// Math: nonces/sec = `freq_mhz × 1e6 × cores / 2^32`. SHA-256 nonces are
/// uniformly distributed over `2^32` possibilities; each core evaluates one
/// candidate per clock tick. Returns `None` when `freq_mhz == 0` or
/// `cores == 0` (no meaningful rate).
fn expected_nonce_rate_hz(freq_mhz: u16, cores: u32) -> Option<f32> {
    if freq_mhz == 0 || cores == 0 {
        return None;
    }
    // f64 intermediate to avoid u64 overflow on very high freq × cores
    // (e.g. BM1368: 500 × 1e6 × 1280 = 6.4e11, still well under u64::MAX
    // but f64 keeps the math clean and the cast to f32 well-defined).
    let nonces_per_sec = (freq_mhz as f64 * 1_000_000.0 * cores as f64) / 4_294_967_296.0;
    Some(nonces_per_sec as f32)
}

/// Snapshot wall-clock timestamp (Unix epoch seconds) for the RE-010 LOW-2
/// `health_ts` field. Returns `None` only when the system clock is before
/// 1970 (should not happen on running miners).
fn snapshot_health_ts() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

pub fn build_chip_health_snapshot(
    context: &SnapshotContext,
    chain_filter: Option<u8>,
) -> ChipHealthSnapshot {
    let mut chains: Vec<&SnapshotChain> = context
        .chain_states
        .iter()
        .filter(|chain| {
            chain_filter
                .map(|wanted| chain.chain_id == wanted)
                .unwrap_or(true)
        })
        .collect();
    chains.sort_by_key(|chain| chain.chain_id);

    let mut warnings = Vec::new();
    let mut recommendations = Vec::new();
    let mut report_chains = Vec::new();
    let mut total_chips = 0u16;
    let mut overall_source = "runtime_chain_summary".to_string();

    // RE-010 LOW-1/LOW-2 (2026-05-21): chip-family cores_per_chip + snapshot
    // wall-clock are shared across every cell in this snapshot. Resolve once
    // up front rather than per-cell.
    let cores = cores_per_chip_for_family(&context.chip_type);
    let now_ts = snapshot_health_ts();

    for chain in chains {
        let live: Vec<&SnapshotChipHealth> = context
            .live_chip_health
            .iter()
            .filter(|chip| chip.chain_id == chain.chain_id)
            .collect();
        let profile = context.profile(chain.chain_id);
        let expected_chip_count = context
            .expected_chip_count(chain.chain_id)
            .max(chain.chips as u16);
        total_chips += expected_chip_count;

        let mut chipmap = chipmap_layout(chain.chain_id, expected_chip_count);
        let source = if !live.is_empty() {
            overall_source = "runtime_chip_health".to_string();
            for chip in sorted_runtime_health(&live) {
                let score = chip.health_score.clamp(0.0, 1.25) as f32;
                let grade = score_to_grade(score);
                chipmap.add_cell(ChipMapCell {
                    index: chip.chip_index as u16,
                    address: chip.chip_index.saturating_mul(4),
                    health_score: score,
                    grade,
                    color: ChipColor::from_score(score),
                    frequency_mhz: chip.freq_mhz,
                    nonce_count: 0,
                    crc_errors: chip.error_rate_pct.max(0.0).round() as u32,
                    expected_nonce_rate_hz: cores
                        .and_then(|c| expected_nonce_rate_hz(chip.freq_mhz, c)),
                    health_ts: now_ts,
                    die_temp_c: None,
                    anomaly_gradient: None,
                    anomaly_cross_slot_zscore: None,
                    anomaly_nonce_deficit: None,
                });
            }
            if let Some(profile) = profile {
                fill_missing_profile_cells(&mut chipmap, profile, cores, now_ts);
            }
            "runtime_chip_health".to_string()
        } else if let Some(profile) = profile {
            if overall_source != "runtime_chip_health" {
                overall_source = "saved_profile_baseline".to_string();
            }
            for chip in &profile.chips {
                let score = profile_score(chip.grade, chip.error_rate) as f32;
                let grade = score_to_grade(score);
                chipmap.add_cell(ChipMapCell {
                    index: chip.chip_index as u16,
                    address: chip.chip_index.saturating_mul(4),
                    health_score: score,
                    grade,
                    color: ChipColor::from_score(score),
                    frequency_mhz: chip.operating_mhz,
                    nonce_count: chip.nonces_counted,
                    crc_errors: (chip.error_rate * 1000.0).round() as u32,
                    expected_nonce_rate_hz: cores
                        .and_then(|c| expected_nonce_rate_hz(chip.operating_mhz, c)),
                    health_ts: now_ts,
                    die_temp_c: None,
                    anomaly_gradient: None,
                    anomaly_cross_slot_zscore: None,
                    anomaly_nonce_deficit: None,
                });
            }
            // Profile baselines carry per-chip nonce counts — enrich deficit now.
            chipmap.enrich_nonce_deficits();
            "saved_profile_baseline".to_string()
        } else {
            let inferred_score = inferred_chain_score(chain, expected_chip_count) as f32;
            for chip_index in 0..expected_chip_count {
                let is_responding = chip_index < chain.chips as u16;
                let score = if is_responding { inferred_score } else { 0.0 };
                chipmap.add_cell(ChipMapCell {
                    index: chip_index,
                    address: (chip_index as u8).saturating_mul(4),
                    health_score: score,
                    grade: score_to_grade(score),
                    color: ChipColor::from_score(score),
                    frequency_mhz: chain.frequency_mhz,
                    nonce_count: 0,
                    crc_errors: chain.errors,
                    expected_nonce_rate_hz: cores
                        .and_then(|c| expected_nonce_rate_hz(chain.frequency_mhz, c)),
                    health_ts: now_ts,
                    die_temp_c: None,
                    anomaly_gradient: None,
                    anomaly_cross_slot_zscore: None,
                    anomaly_nonce_deficit: None,
                });
            }
            warnings.push(format!(
                "Chain {} has no live autotuner chip-health or saved profile data; per-chip cells are inferred from current board state.",
                chain.chain_id
            ));
            "runtime_chain_summary".to_string()
        };

        let average_score = if chipmap.cells.is_empty() {
            0.0
        } else {
            chipmap
                .cells
                .iter()
                .map(|cell| cell.health_score as f64)
                .sum::<f64>()
                / chipmap.cells.len() as f64
        };
        if average_score < 0.7 {
            recommendations.push(format!(
                "Inspect chain {} for weak silicon, thermal imbalance, or communication faults before increasing frequency.",
                chain.chain_id
            ));
        }

        report_chains.push(ChipHealthChainSnapshot {
            chain_id: chain.chain_id,
            source,
            chip_count: expected_chip_count,
            responding_chips: chain.chips,
            board_temp_c: chain.temp_c,
            board_hashrate_ghs: chain.hashrate_ghs,
            board_health_score: average_score,
            frequency_mhz: chain.frequency_mhz,
            voltage_mv: chain.voltage_mv,
            errors: chain.errors,
            status: chain.status.clone(),
            chipmap,
        });
    }

    recommendations.sort();
    recommendations.dedup();
    warnings.sort();
    warnings.dedup();

    ChipHealthSnapshot {
        report_id: context.report_id,
        generated_at: context.generated_at.clone(),
        report_type: "chip_health_snapshot".to_string(),
        source: overall_source,
        total_boards: report_chains.len() as u8,
        total_chips,
        warnings,
        recommendations,
        chains: report_chains,
    }
}

pub fn build_board_health_snapshot(
    context: &SnapshotContext,
    chain_filter: Option<u8>,
) -> Vec<BoardHealthResult> {
    let chip_health = build_chip_health_snapshot(context, chain_filter);
    let chain_map: HashMap<u8, &ChipHealthChainSnapshot> = chip_health
        .chains
        .iter()
        .map(|chain| (chain.chain_id, chain))
        .collect();

    let mut boards: Vec<BoardHealthResult> = context
        .chain_states
        .iter()
        .filter(|chain| chain_filter.map(|wanted| chain.chain_id == wanted).unwrap_or(true))
        .map(|chain| {
            let expected = context.expected_chip_count(chain.chain_id).max(chain.chips as u16) as u8;
            let dead_chip_addresses = chain_map
                .get(&chain.chain_id)
                .map(|chip_chain| {
                    chip_chain
                        .chipmap
                        .cells
                        .iter()
                        .filter(|cell| cell.health_score <= 0.01)
                        .map(|cell| cell.address)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let voltage_v = chain.voltage_mv as f32 / 1000.0;
            let crc_ok = chain.errors == 0;
            let temp_ok = chain.temp_c < 75.0;
            let mut result = BoardHealthResult {
                chain_id: chain.chain_id,
                data_source: chain_map
                    .get(&chain.chain_id)
                    .map(|chip_chain| chip_chain.source.clone())
                    .unwrap_or_else(|| "runtime_chain_summary".to_string()),
                measurement_type: "live_snapshot".to_string(),
                status: chain.status.clone(),
                estimated_hashrate_ghs: chain.hashrate_ghs,
                notes: vec![
                    "Board-health values are derived from the miner's current runtime snapshot; no standalone stress pass was launched.".to_string(),
                    "Voltage shown is the commanded/last-known setpoint — the rail was NOT measured in snapshot mode (no PIC/dsPIC set/get readback); deviation is not available. A collapsed rail can still show a non-zero commanded value.".to_string(),
                    "CRC status is inferred from a cumulative runtime counter; no bounded command window was measured, so zero errors cannot prove a measured PASS.".to_string(),
                    "Board model/serial metadata is not EEPROM checksum or schema validation; EEPROM validity is unavailable in snapshot mode.".to_string(),
                ],
                chips_expected: expected,
                chips_responding: chain.chips,
                dead_chip_addresses,
                // NOTE: snapshot mode performs no PIC/dsPIC readback. readback ==
                // setpoint and deviation == 0.0 are NOT measurements; report.rs
                // renders this honestly (gated on measurement_type=="live_snapshot")
                // and the note above discloses it. voltage_ok here means "a non-zero
                // voltage is configured", NOT "the rail was verified".
                voltage_setpoint_v: voltage_v,
                voltage_readback_v: voltage_v,
                voltage_deviation_pct: 0.0,
                voltage_ok: chain.voltage_mv > 0,
                voltage_evidence: DiagnosticEvidence::commanded(
                    voltage_v,
                    "runtime_voltage_setpoint",
                    None,
                ),
                crc_commands_sent: 0,
                crc_errors_received: chain.errors,
                crc_error_rate_pct: 0.0,
                crc_ok,
                crc_evidence: DiagnosticEvidence::inferred(
                    chain.errors,
                    "runtime_cumulative_error_counter_without_test_window",
                    None,
                ),
                temperature_c: chain.temp_c,
                temperature_ok: temp_ok,
                // Runtime model metadata does not prove that EEPROM bytes were
                // read or that their checksum/schema was validated.
                eeprom_present: false,
                eeprom_valid: false,
                eeprom_model: context.board_type.clone(),
                eeprom_serial: context.serial.clone(),
                eeprom_evidence: context
                    .board_type
                    .as_ref()
                    .map(|_| {
                        DiagnosticEvidence::inferred(
                            false,
                            "runtime_board_model_metadata_not_eeprom_validation",
                            None,
                        )
                    })
                    .unwrap_or_else(|| {
                        DiagnosticEvidence::unavailable("eeprom_not_observed_in_snapshot")
                    }),
                required_evidence_measured: false,
                grade: 'F',
                grade_explanation: String::new(),
            };
            if !crc_ok {
                result.notes.push(format!(
                    "Chain {} already shows {} cumulative CRC/communication errors in runtime counters.",
                    chain.chain_id, chain.errors
                ));
            }
            result.calculate_grade();
            result
        })
        .collect();
    boards.sort_by_key(|board| board.chain_id);
    boards
}

pub fn build_hashreport_snapshot(
    context: &SnapshotContext,
    chain_filter: Option<u8>,
) -> HashReport {
    let chip_health = build_chip_health_snapshot(context, chain_filter);
    let boards = build_board_results(&chip_health);
    let warnings = build_hashreport_warnings(context, &boards, &chip_health);
    let recommendations = build_hashreport_recommendations(&boards, &chip_health);
    let duration_seconds = history_duration_s(&context.history);
    let windows = build_windows(context, chain_filter);
    let unit_grade = calculate_unit_grade(&boards);
    let unit_grade_explanation = format!(
        "{} snapshot board(s) analyzed using current runtime state{}. Passing grade withheld because snapshot voltage is commanded and CRC validity is inferred rather than measured in a bounded test window.",
        boards.len(),
        if duration_seconds > 0 {
            format!(" and {}s of retained history", duration_seconds)
        } else {
            String::new()
        }
    );

    HashReport {
        report_id: context.report_id,
        report_version: "snapshot-v1".to_string(),
        generated_at: context.generated_at.clone(),
        duration_seconds,
        report_kind: "snapshot".to_string(),
        source: if context.history.is_empty() {
            "live_runtime".to_string()
        } else {
            "live_runtime_plus_history".to_string()
        },
        firmware_version: context.firmware_version.clone(),
        system: SystemInfo {
            serial: context
                .serial
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            mac: context.mac.clone().unwrap_or_else(|| "unknown".to_string()),
            model: context
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            chip_type: context.chip_type.clone(),
            chip_id: context
                .chip_id
                .map(|chip_id| format!("0x{chip_id:04X}"))
                .unwrap_or_else(|| "unknown".to_string()),
            fpga_version: "runtime-unavailable".to_string(),
            board_count: boards.len() as u8,
            total_chips: boards.iter().map(|board| board.chips_expected as u16).sum(),
            control_board: context.control_board.clone(),
        },
        baseline: BaselineSnapshot {
            temperatures_c: context
                .chain_states
                .iter()
                .map(|chain| chain.temp_c)
                .collect(),
            fan_rpm: context.fan_rpm,
            fan_pwm: context.fan_pwm,
            voltages_v: context
                .chain_states
                .iter()
                .map(|chain| chain.voltage_mv as f32 / 1000.0)
                .collect(),
            crc_baseline: context
                .chain_states
                .iter()
                .map(|chain| chain.errors)
                .collect(),
        },
        windows,
        boards,
        unit_grade,
        unit_grade_explanation,
        warnings,
        recommendations,
    }
}

fn build_board_results(chip_health: &ChipHealthSnapshot) -> Vec<BoardResult> {
    chip_health
        .chains
        .iter()
        .map(|chain| {
            let chips: Vec<ChipHealthScore> = chain
                .chipmap
                .cells
                .iter()
                .map(|cell| ChipHealthScore {
                    index: cell.index,
                    address: cell.address,
                    grade: cell.grade,
                    health_score: cell.health_score,
                    nonce_count: cell.nonce_count,
                    expected_nonces: 0,
                    crc_errors: cell.crc_errors,
                    frequency_mhz: cell.frequency_mhz,
                    hashrate_ghs: if cell.frequency_mhz > 0 {
                        chain.board_hashrate_ghs as f32 / chain.chipmap.cells.len().max(1) as f32
                    } else {
                        0.0
                    },
                })
                .collect();
            let chips_dead = chain
                .chipmap
                .cells
                .iter()
                .filter(|cell| cell.health_score <= 0.01)
                .count() as u8;
            let health_grade = score_to_grade(chain.board_health_score as f32);
            BoardResult {
                chain_id: chain.chain_id,
                chips_expected: chain.chip_count as u8,
                chips_responding: chain.responding_chips,
                chips_dead,
                hashrate_ghs: chain.board_hashrate_ghs as f32,
                voltage_v: chain.voltage_mv as f32 / 1000.0,
                voltage_evidence: DiagnosticEvidence::commanded(
                    chain.voltage_mv as f32 / 1000.0,
                    "runtime_voltage_setpoint",
                    None,
                ),
                temp_c: chain.board_temp_c,
                crc_errors: chain.errors,
                crc_evidence: DiagnosticEvidence::inferred(
                    chain.errors,
                    "runtime_cumulative_error_counter_without_test_window",
                    None,
                ),
                grade: if matches!(health_grade, 'A' | 'B') {
                    'C'
                } else {
                    health_grade
                },
                chips,
            }
        })
        .collect::<Vec<_>>()
}

fn build_windows(context: &SnapshotContext, chain_filter: Option<u8>) -> Vec<WindowData> {
    if context.history.is_empty() {
        return vec![WindowData {
            window_index: 0,
            chip_nonces: Vec::new(),
            chain_crc_errors: context
                .chain_states
                .iter()
                .map(|chain| chain.errors)
                .collect(),
            chain_temps_c: context
                .chain_states
                .iter()
                .map(|chain| chain.temp_c)
                .collect(),
            total_nonces: context
                .accepted_shares
                .saturating_add(context.rejected_shares),
        }];
    }

    let mut samples = context.history.clone();
    samples.sort_by_key(|sample| sample.timestamp_s);
    samples
        .into_iter()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .enumerate()
        .map(|(index, sample)| WindowData {
            window_index: index as u8,
            chip_nonces: Vec::new(),
            chain_crc_errors: context
                .chain_states
                .iter()
                .filter(|chain| {
                    chain_filter
                        .map(|wanted| chain.chain_id == wanted)
                        .unwrap_or(true)
                })
                .map(|chain| chain.errors)
                .collect(),
            chain_temps_c: context
                .chain_states
                .iter()
                .filter(|chain| {
                    chain_filter
                        .map(|wanted| chain.chain_id == wanted)
                        .unwrap_or(true)
                })
                .map(|chain| chain.temp_c.max(sample.temp_c))
                .collect(),
            total_nonces: sample.accepted.saturating_add(sample.rejected),
        })
        .collect()
}

fn build_hashreport_warnings(
    context: &SnapshotContext,
    boards: &[BoardResult],
    chip_health: &ChipHealthSnapshot,
) -> Vec<String> {
    let mut warnings = chip_health.warnings.clone();
    if context.history.is_empty() {
        warnings.push(
            "Historical window data is not available yet; this hashreport is a live snapshot, not a timed drive.".to_string(),
        );
    }
    for board in boards {
        if board.crc_errors > 0 {
            warnings.push(format!(
                "Chain {} currently reports {} CRC/communication errors.",
                board.chain_id, board.crc_errors
            ));
        }
        if board.chips_responding < board.chips_expected {
            warnings.push(format!(
                "Chain {} has only {}/{} responding chips in the current runtime snapshot.",
                board.chain_id, board.chips_responding, board.chips_expected
            ));
        }
    }
    warnings.sort();
    warnings.dedup();
    warnings
}

fn build_hashreport_recommendations(
    boards: &[BoardResult],
    chip_health: &ChipHealthSnapshot,
) -> Vec<String> {
    let mut recommendations = chip_health.recommendations.clone();
    for board in boards {
        if board.temp_c >= 70.0 {
            recommendations.push(format!(
                "Reduce frequency or improve airflow on chain {} before running a longer stress test.",
                board.chain_id
            ));
        }
        if matches!(board.grade, 'C' | 'D' | 'F') {
            recommendations.push(format!(
                "Run a full timed diagnostic on chain {} once the dedicated engine is wired, because this snapshot already shows degraded health.",
                board.chain_id
            ));
        }
    }
    recommendations.sort();
    recommendations.dedup();
    recommendations
}

fn history_duration_s(history: &[SnapshotHistorySample]) -> u32 {
    let min = history.iter().map(|sample| sample.timestamp_s).min();
    let max = history.iter().map(|sample| sample.timestamp_s).max();
    min.zip(max)
        .map(|(start, end)| end.saturating_sub(start) as u32)
        .unwrap_or(0)
}

fn sorted_runtime_health<'a>(chips: &'a [&'a SnapshotChipHealth]) -> Vec<&'a SnapshotChipHealth> {
    let mut sorted = chips.to_vec();
    sorted.sort_by_key(|chip| chip.chip_index);
    sorted
}

fn fill_missing_profile_cells(
    chipmap: &mut ChipMap,
    profile: &SnapshotProfile,
    cores: Option<u32>,
    now_ts: Option<u64>,
) {
    let mut present: HashMap<u16, bool> = chipmap
        .cells
        .iter()
        .map(|cell| (cell.index, true))
        .collect();
    for chip in &profile.chips {
        let index = chip.chip_index as u16;
        if present.contains_key(&index) {
            continue;
        }
        let score = profile_score(chip.grade, chip.error_rate) as f32;
        chipmap.add_cell(ChipMapCell {
            index,
            address: chip.chip_index.saturating_mul(4),
            health_score: score,
            grade: score_to_grade(score),
            color: ChipColor::from_score(score),
            frequency_mhz: chip.operating_mhz,
            nonce_count: chip.nonces_counted,
            crc_errors: (chip.error_rate * 1000.0).round() as u32,
            expected_nonce_rate_hz: cores
                .and_then(|c| expected_nonce_rate_hz(chip.operating_mhz, c)),
            health_ts: now_ts,
            die_temp_c: None,
            anomaly_gradient: None,
            anomaly_cross_slot_zscore: None,
            anomaly_nonce_deficit: None,
        });
        present.insert(index, true);
    }
    chipmap.cells.sort_by_key(|cell| cell.index);
}

fn inferred_chain_score(chain: &SnapshotChain, expected_chip_count: u16) -> f64 {
    if expected_chip_count == 0 || chain.frequency_mhz == 0 {
        return 0.0;
    }
    let responding_ratio = chain.chips as f64 / expected_chip_count as f64;
    let error_penalty = (chain.errors as f64 / 100.0).min(0.5);
    (responding_ratio - error_penalty).clamp(0.0, 1.0)
}

fn chipmap_layout(chain_id: u8, chip_count: u16) -> ChipMap {
    match chip_count {
        0 => ChipMap {
            chain_id,
            chip_count: 0,
            columns: 0,
            rows: 0,
            cells: Vec::new(),
        },
        1..=63 => ChipMap::bm1387_layout(chain_id),
        64..=108 => ChipMap::bm1368_layout(chain_id),
        _ => ChipMap {
            chain_id,
            chip_count,
            columns: 12,
            rows: ((chip_count as f32) / 12.0).ceil() as u8,
            cells: Vec::with_capacity(chip_count as usize),
        },
    }
}

fn profile_score(grade: char, error_rate: f64) -> f64 {
    let base = match grade {
        'A' => 0.98,
        'B' => 0.85,
        'C' => 0.65,
        'D' => 0.35,
        _ => 0.5,
    };
    (base - (error_rate * 5.0)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod re010_helpers_tests {
    //! RE-010 LOW-1 follow-up (2026-05-21): pin the cores_per_chip lookup
    //! table + the expected_nonce_rate_hz formula. The chip-family table is
    //! load-bearing (a stale entry would inject fabricated rate numbers
    //! into the per-chip telemetry, violating  truth-contract).
    //! Every entry cites its `dcentrald-silicon-profiles` source in the
    //! `cores_per_chip_for_family` doc-comment.
    use super::{
        cores_per_chip_for_family, expected_nonce_rate_hz, inferred_chain_score, snapshot_health_ts,
    };

    #[test]
    fn cores_per_chip_lookup_table_matches_silicon_profiles() {
        // Canonical values — must match dcentrald-silicon-profiles literals.
        assert_eq!(cores_per_chip_for_family("BM1387"), Some(114));
        assert_eq!(cores_per_chip_for_family("BM1387P"), Some(128));
        assert_eq!(cores_per_chip_for_family("BM1397"), Some(672));
        assert_eq!(cores_per_chip_for_family("BM1397+"), Some(672));
        assert_eq!(cores_per_chip_for_family("BM1397plus"), Some(672));
        assert_eq!(cores_per_chip_for_family("BM1398"), Some(672));
        assert_eq!(cores_per_chip_for_family("BM1362"), Some(672));
        assert_eq!(cores_per_chip_for_family("BM1366"), Some(894));
        assert_eq!(cores_per_chip_for_family("BM1368"), Some(1280));
    }

    #[test]
    fn cores_per_chip_lookup_returns_none_for_unconfirmed_families() {
        // BM1370 ("1024+" per bm1370.rs:7 — not a confirmed literal),
        // BM1360 (no literal anywhere — bm1360.rs:31), BM1491 (no literal
        // anywhere — bm1491.rs:33), and any other family must return None
        // so expected_nonce_rate_hz stays None rather than reporting a
        // fabricated rate.
        assert_eq!(cores_per_chip_for_family("BM1370"), None);
        assert_eq!(cores_per_chip_for_family("BM1360"), None);
        assert_eq!(cores_per_chip_for_family("BM1491"), None);
        assert_eq!(cores_per_chip_for_family("BM1373"), None);
        assert_eq!(cores_per_chip_for_family("BM1485"), None);
        assert_eq!(cores_per_chip_for_family("BM1489"), None);
        assert_eq!(cores_per_chip_for_family(""), None);
        assert_eq!(cores_per_chip_for_family("unknown"), None);
    }

    #[test]
    fn expected_nonce_rate_hz_matches_canonical_formula() {
        // Math: nonces/sec = freq_mhz × 1e6 × cores / 2^32.
        //
        // BM1387 (S9) @ 650 MHz × 114 cores:
        //   650 × 1e6 × 114 / 4_294_967_296 ≈ 17.249 nonces/sec
        let bm1387 = expected_nonce_rate_hz(650, 114).expect("BM1387 @ 650MHz");
        assert!(
            (bm1387 - 17.249).abs() < 0.01,
            "BM1387 @ 650 MHz × 114 cores should be ~17.25 nonces/sec, got {bm1387}"
        );

        // BM1398 (S19 Pro) @ 525 MHz × 672 cores:
        //   525 × 1e6 × 672 / 4_294_967_296 ≈ 82.14 nonces/sec
        let bm1398 = expected_nonce_rate_hz(525, 672).expect("BM1398 @ 525MHz");
        assert!(
            (bm1398 - 82.14).abs() < 0.05,
            "BM1398 @ 525 MHz × 672 cores should be ~82.14 nonces/sec, got {bm1398}"
        );

        // BM1368 (S21) @ 500 MHz × 1280 cores:
        //   500 × 1e6 × 1280 / 4_294_967_296 ≈ 149.01 nonces/sec
        let bm1368 = expected_nonce_rate_hz(500, 1280).expect("BM1368 @ 500MHz");
        assert!(
            (bm1368 - 149.01).abs() < 0.1,
            "BM1368 @ 500 MHz × 1280 cores should be ~149.01 nonces/sec, got {bm1368}"
        );
    }

    #[test]
    fn expected_nonce_rate_hz_is_none_at_zero_freq_or_zero_cores() {
        assert_eq!(expected_nonce_rate_hz(0, 114), None);
        assert_eq!(expected_nonce_rate_hz(650, 0), None);
        assert_eq!(expected_nonce_rate_hz(0, 0), None);
    }

    fn snap_chain(chips: u8, frequency_mhz: u16, errors: u32) -> crate::snapshot::SnapshotChain {
        crate::snapshot::SnapshotChain {
            chain_id: 0,
            chips,
            frequency_mhz,
            voltage_mv: 13_000,
            temp_c: 55.0,
            hashrate_ghs: 1000.0,
            errors,
            status: String::new(),
        }
    }

    #[test]
    fn inferred_chain_score_is_bounded_and_fails_safe_on_zero_expected_or_freq() {
        // inferred_chain_score divides chips / expected_chip_count. A zero expected
        // count (or zero frequency = not running) must short-circuit to 0.0 — never
        // divide into NaN/inf — and the score is always clamped to [0, 1] so a bad
        // input can't inject an out-of-range health score into the diagnostics.
        assert_eq!(inferred_chain_score(&snap_chain(100, 500, 0), 0), 0.0);
        assert_eq!(inferred_chain_score(&snap_chain(100, 0, 0), 100), 0.0);
        let full = inferred_chain_score(&snap_chain(126, 500, 0), 126);
        assert!(
            (full - 1.0).abs() < 1e-9,
            "full-responding, no errors -> 1.0, got {full}"
        );
        assert_eq!(inferred_chain_score(&snap_chain(0, 500, 0), 126), 0.0);

        // Always finite and in [0, 1] across chips/expected/errors, including
        // over-report (chips > expected) and huge error counts.
        for chips in [0u8, 1, 63, 126, 255] {
            for expected in [1u16, 63, 126, 255] {
                for errors in [0u32, 50, 100, 100_000] {
                    let s = inferred_chain_score(&snap_chain(chips, 500, errors), expected);
                    assert!(
                        s.is_finite() && (0.0..=1.0).contains(&s),
                        "score {s} out of [0,1] for chips={chips} expected={expected} errors={errors}"
                    );
                }
            }
        }
    }

    #[test]
    fn snapshot_health_ts_is_populated_on_running_system() {
        // System clock should produce a Some(unix_seconds) > 1.7e9 (year 2023+).
        let ts = snapshot_health_ts().expect("running system has a clock");
        assert!(
            ts > 1_700_000_000,
            "Unix timestamp {ts} looks pre-2023 — system clock unset?"
        );
    }
}

#[cfg(test)]
mod evidence_producer_tests {
    use super::{build_board_health_snapshot, build_hashreport_snapshot};
    use crate::evidence::EvidenceKind;
    use crate::snapshot::{SnapshotChain, SnapshotContext};
    use uuid::Uuid;

    fn snapshot(board_type: Option<&str>) -> SnapshotContext {
        SnapshotContext {
            report_id: Uuid::nil(),
            generated_at: "2026-07-12T00:00:00Z".into(),
            firmware_version: "test".into(),
            serial: Some("runtime-serial".into()),
            mac: None,
            model: Some("test-miner".into()),
            chip_type: "BM1397".into(),
            chip_id: Some(0x1397),
            control_board: "test-control-board".into(),
            board_type: board_type.map(str::to_owned),
            chain_states: vec![SnapshotChain {
                chain_id: 0,
                chips: 100,
                frequency_mhz: 500,
                voltage_mv: 13_700,
                temp_c: 55.0,
                hashrate_ghs: 1_000.0,
                errors: 0,
                status: "running".into(),
            }],
            fan_pwm: 50,
            fan_rpm: 4_000,
            accepted_shares: 1,
            rejected_shares: 0,
            pool_url: String::new(),
            pool_status: "connected".into(),
            pool_difficulty: 1.0,
            uptime_s: 60,
            history: Vec::new(),
            live_chip_health: Vec::new(),
            saved_profiles: Vec::new(),
        }
    }

    #[test]
    fn runtime_snapshot_producer_cannot_emit_a_measured_board_or_unit_pass() {
        let context = snapshot(Some("BHB42"));
        let boards = build_board_health_snapshot(&context, None);
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].grade, 'C');
        assert_eq!(boards[0].voltage_evidence.kind(), EvidenceKind::Commanded);
        assert_eq!(boards[0].crc_evidence.kind(), EvidenceKind::Inferred);
        assert!(!boards[0].eeprom_present);
        assert!(!boards[0].eeprom_valid);
        assert_eq!(boards[0].eeprom_evidence.kind(), EvidenceKind::Inferred);
        assert!(!boards[0].required_evidence_measured);

        let report = build_hashreport_snapshot(&context, None);
        assert_eq!(report.boards.len(), 1);
        assert_eq!(report.boards[0].grade, 'C');
        assert_eq!(
            report.boards[0].voltage_evidence.kind(),
            EvidenceKind::Commanded
        );
        assert_eq!(report.boards[0].crc_evidence.kind(), EvidenceKind::Inferred);
        assert_eq!(report.unit_grade, 'C');
    }

    #[test]
    fn missing_eeprom_observation_is_explicitly_unavailable() {
        let boards = build_board_health_snapshot(&snapshot(None), None);
        assert_eq!(boards[0].eeprom_evidence.kind(), EvidenceKind::Unavailable);
        assert_eq!(boards[0].eeprom_evidence.value(), None);
        assert!(!boards[0].eeprom_present);
        assert!(!boards[0].eeprom_valid);
    }
}
