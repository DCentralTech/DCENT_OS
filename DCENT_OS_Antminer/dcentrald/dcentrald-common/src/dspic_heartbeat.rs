//! Pure (no-HAL) helper deciding which dsPIC addresses the post-ENABLE 1 Hz
//! keepalive heartbeat should target, beyond the always-heartbeated selected
//! dsPIC.
//!
//! Context (am2 `a lab unit` standalone — swarm `wf_7b37bed4` adversarial verify,
//! 2026-05-29): the `DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS` path originally built
//! its "extras" set from a chain-bitmask enumeration
//! (`active_dspic_addrs(0b111)` => `[0x20, 0x21, 0x22]`). On `a lab unit` the middle
//! slot (dsPIC `0x21`) is PHYSICALLY ABSENT — only slots 1+3 are populated — so
//! heartbeating `0x21` NACKs every tick, which trips the I2C service's
//! recover-and-reopen of the shared `/dev/i2c-0` fd roughly once per second and
//! destabilises the bus right when the chain is trying to enumerate.
//!
//! The correct keepalive target beyond `selected` is the EFFECTIVE chain dsPIC
//! only (the controller the chain UART actually routes to — `0x22` on the
//! slot-3 path), never a blanket bitmask that can name an empty slot. This pure
//! fn encodes that rule and is pinned by host tests so a future refactor cannot
//! silently re-introduce the absent-slot heartbeat.

/// Return the EXTRA dsPIC addresses to heartbeat in addition to `selected`
/// (which the caller always heartbeats on its own).
///
/// Returns at most one address: the `effective` chain dsPIC, and only when it
/// is present (`Some`) and distinct from `selected`. An absent middle slot can
/// therefore never be heartbeated — that is the whole point. When the caller's
/// `DCENT_AM2_HEARTBEAT_ALL_ACTIVE_PICS` gate is unset it passes nothing through
/// here (so only `selected` is heartbeated — byte-for-byte the proven-fleet /
///  bosminer-handoff behaviour).
pub fn heartbeat_extra_addrs(selected: u8, effective: Option<u8>) -> Vec<u8> {
    match effective {
        Some(eff) if eff != selected => vec![eff],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot25_topology_targets_effective_only_never_absent_middle() {
        // `a lab unit`: selected = 0x20 (slot 1), effective chain dsPIC = 0x22
        // (slot 3); the middle slot 0x21 is physically absent.
        let extras = heartbeat_extra_addrs(0x20, Some(0x22));
        assert_eq!(extras, vec![0x22]);
        // The regression this pins: 0x21 must NEVER be a heartbeat target,
        // because NACKing it tears down + reopens the shared i2c-0 fd each tick.
        assert!(!extras.contains(&0x21));
    }

    #[test]
    fn effective_equal_selected_yields_no_extras() {
        // The effective chain dsPIC IS the selected one → nothing extra.
        assert_eq!(heartbeat_extra_addrs(0x20, Some(0x20)), Vec::<u8>::new());
    }

    #[test]
    fn no_effective_yields_no_extras() {
        assert_eq!(heartbeat_extra_addrs(0x20, None), Vec::<u8>::new());
    }

    #[test]
    fn single_hashboard_unit_only_selected() {
        // A single-chain unit routes the chain UART to its only dsPIC, so
        // effective == selected and no extra keepalive target is produced.
        assert_eq!(heartbeat_extra_addrs(0x22, Some(0x22)), Vec::<u8>::new());
    }
}
