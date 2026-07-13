//! AM2 serial-chain topology plan (Codex Phase 2 handoff, 2026-05-26).
//!
//! Pure, host-testable abstraction that converts an operator-declared list of
//! chain UART devices (`MiningConfig::resolved_serial_devices`) into a vector
//! of `Am2ChainContext` records — one per planned chain.
//!
//! Each context carries everything the S19j hybrid runtime needs to drive a
//! single chain end-to-end:
//!
//! - `serial_device`     — the `/dev/ttyS*` path the `SerialChainBackend` opens
//! - `am2_slot`          — 0..=3, the AM2 hashboard slot (PL UART 0/1/2/3)
//! - `dspic_addr`        — the I²C address of the slot's dsPIC voltage MCU
//! - `chain_id`          — sequential index in the planned list (0..=N-1)
//!
//! The plan is config-driven: there is **no hardcoded `a lab unit` behavior**. A
//! single-device legacy config (`serial_device = "/dev/ttyS1"`,
//! `serial_devices = None`) produces a one-element plan exactly as the
//! existing runtime expects. A multi-device config
//! (`serial_devices = ["/dev/ttyS1", "/dev/ttyS3"]`) produces a two-element
//! plan that the vectorized runtime (Phase 2 step 2) will consume.
//!
//! ## Failure mode
//!
//! Any invalid `/dev/ttyS*` path returns `Err` so the daemon refuses to start
//! rather than silently dropping a planned chain. Duplicate paths fail too
//! (config-validation also catches them, but defense-in-depth here is cheap).
//!
//! ## Why this is in `dcentrald` and not `dcentrald-hal`
//!
//! No HAL deps — pure data transform. Host-testable on Windows + Linux + WSL.
//! Lives next to `s19j_hybrid_mining.rs` so the runtime can `use crate::am2_chain_plan`.

use anyhow::{Context, Result};
use dcentrald_common::am2_topology::{dspic_address_for_slot, slot_for_uart};

/// Canonical AM2 dsPIC I²C address table, indexed by hashboard slot 0..=3.
/// Matches `s19j_hybrid_mining::S19_DSPIC_ADDRS` (kept in lockstep — single
/// source of truth is `dcentrald_common::am2_topology` and is consumed below.
///
/// Slot 0 = PL UART 0 = `/dev/ttyS1` → dsPIC 0x20
/// Slot 1 = PL UART 1 = `/dev/ttyS2` → dsPIC 0x21
/// Slot 2 = PL UART 2 = `/dev/ttyS3` → dsPIC 0x22
/// Slot 3 = PL UART 3 = `/dev/ttyS4` → dsPIC 0x23

/// One planned chain — the runtime opens one `SerialChainBackend`,
/// orchestrates one PIC's voltage path, and dispatches/polls work per
/// instance of this record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Am2ChainContext {
    /// The `/dev/ttyS*` path the runtime opens.
    pub serial_device: String,
    /// AM2 hashboard slot (0..=3) — index into the canonical slot table.
    pub am2_slot: u8,
    /// I²C address of this slot's dsPIC voltage MCU.
    pub dspic_addr: u8,
    /// Sequential chain index in the planned list (0..=N-1).
    pub chain_id: u8,
}

/// Build the AM2 serial-chain plan from a list of operator-declared tty paths.
///
/// Preserves Phase 1 single-chain behavior: pass a one-element list and you
/// get a one-element plan. Invalid paths fail-closed.
pub fn build_am2_chain_plan(serial_devices: &[String]) -> Result<Vec<Am2ChainContext>> {
    if serial_devices.is_empty() {
        anyhow::bail!(
            "AM2 chain plan: serial_devices list is empty — at least one chain UART must be declared"
        );
    }
    let mut seen: Vec<&str> = Vec::with_capacity(serial_devices.len());
    let mut plan = Vec::with_capacity(serial_devices.len());
    for (chain_id, dev) in serial_devices.iter().enumerate() {
        if seen.contains(&dev.as_str()) {
            anyhow::bail!(
                "AM2 chain plan: duplicate serial_device '{}' (chain_id={})",
                dev,
                chain_id
            );
        }
        seen.push(dev.as_str());

        let am2_slot = slot_for_uart(dev).with_context(|| {
            format!(
                "AM2 chain plan: '{}' is not a recognized PL UART path \
                 (expected /dev/ttyS1..ttyS4 for slots 0..=3)",
                dev
            )
        })?;
        let dspic_addr = dspic_address_for_slot(am2_slot).with_context(|| {
            format!(
                "AM2 chain plan: slot {} for '{}' has no dsPIC address \
                         (canonical table has 4 slots — indexable 0..=3)",
                am2_slot, dev
            )
        })?;
        let chain_id_u8 = u8::try_from(chain_id).with_context(|| {
            format!(
                "AM2 chain plan: chain_id {} exceeds u8 range (would-overflow plan size)",
                chain_id
            )
        })?;
        plan.push(Am2ChainContext {
            serial_device: dev.clone(),
            am2_slot,
            dspic_addr,
            chain_id: chain_id_u8,
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttys1_maps_to_slot_0_pic_0x20() {
        let plan = build_am2_chain_plan(&["/dev/ttyS1".to_string()]).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].serial_device, "/dev/ttyS1");
        assert_eq!(plan[0].am2_slot, 0);
        assert_eq!(plan[0].dspic_addr, 0x20);
        assert_eq!(plan[0].chain_id, 0);
    }

    #[test]
    fn ttys3_maps_to_slot_2_pic_0x22() {
        let plan = build_am2_chain_plan(&["/dev/ttyS3".to_string()]).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].serial_device, "/dev/ttyS3");
        assert_eq!(plan[0].am2_slot, 2);
        assert_eq!(plan[0].dspic_addr, 0x22);
        assert_eq!(plan[0].chain_id, 0);
    }

    #[test]
    fn ttys2_maps_to_slot_1_pic_0x21() {
        let plan = build_am2_chain_plan(&["/dev/ttyS2".to_string()]).unwrap();
        assert_eq!(plan[0].am2_slot, 1);
        assert_eq!(plan[0].dspic_addr, 0x21);
    }

    #[test]
    fn ttys4_maps_to_slot_3_pic_0x23() {
        let plan = build_am2_chain_plan(&["/dev/ttyS4".to_string()]).unwrap();
        assert_eq!(plan[0].am2_slot, 3);
        assert_eq!(plan[0].dspic_addr, 0x23);
    }

    #[test]
    fn dual_device_list_builds_two_chain_contexts() {
        // The `a lab unit` canonical topology — slot-1 chain + slot-3 chain.
        let plan =
            build_am2_chain_plan(&["/dev/ttyS1".to_string(), "/dev/ttyS3".to_string()]).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].dspic_addr, 0x20);
        assert_eq!(plan[0].chain_id, 0);
        assert_eq!(plan[1].dspic_addr, 0x22);
        assert_eq!(plan[1].chain_id, 1);
    }

    #[test]
    fn dual_device_list_preserves_operator_declared_order() {
        // Reverse order produces reverse plan — first declared is chain_id=0.
        let plan =
            build_am2_chain_plan(&["/dev/ttyS3".to_string(), "/dev/ttyS1".to_string()]).unwrap();
        assert_eq!(plan[0].dspic_addr, 0x22);
        assert_eq!(plan[0].chain_id, 0);
        assert_eq!(plan[1].dspic_addr, 0x20);
        assert_eq!(plan[1].chain_id, 1);
    }

    #[test]
    fn legacy_single_device_builds_exactly_one_context() {
        // Phase 1 legacy: only one tty declared → one chain context. This is
        // the safety contract Codex's handoff explicitly requires.
        let plan = build_am2_chain_plan(&["/dev/ttyS1".to_string()]).unwrap();
        assert_eq!(plan.len(), 1);
    }

    #[test]
    fn empty_list_fails_closed() {
        let err = build_am2_chain_plan(&[]).expect_err("empty list must fail");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("empty"),
            "error message should mention empty: {}",
            msg
        );
    }

    #[test]
    fn invalid_tty_path_fails_closed() {
        let err = build_am2_chain_plan(&["/dev/ttyZ9".to_string()])
            .expect_err("invalid tty path must fail");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("not a recognized PL UART path"),
            "error message should explain the invalid path: {}",
            msg
        );
    }

    #[test]
    fn duplicate_tty_paths_fail_closed() {
        let err = build_am2_chain_plan(&["/dev/ttyS1".to_string(), "/dev/ttyS1".to_string()])
            .expect_err("duplicate paths must fail");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("duplicate"),
            "error message should mention duplicate: {}",
            msg
        );
    }

    #[test]
    fn all_canonical_slots_round_trip() {
        // Pin the full 0x20..=0x23 canonical mapping.
        let plan = build_am2_chain_plan(&[
            "/dev/ttyS1".to_string(),
            "/dev/ttyS2".to_string(),
            "/dev/ttyS3".to_string(),
            "/dev/ttyS4".to_string(),
        ])
        .unwrap();
        assert_eq!(
            plan.iter()
                .map(|c| (c.am2_slot, c.dspic_addr))
                .collect::<Vec<_>>(),
            vec![(0, 0x20), (1, 0x21), (2, 0x22), (3, 0x23)]
        );
    }

    #[test]
    fn chain_id_is_sequential_starting_at_zero() {
        let plan = build_am2_chain_plan(&[
            "/dev/ttyS1".to_string(),
            "/dev/ttyS3".to_string(),
            "/dev/ttyS4".to_string(),
        ])
        .unwrap();
        for (idx, ctx) in plan.iter().enumerate() {
            assert_eq!(ctx.chain_id, idx as u8);
        }
    }
}
