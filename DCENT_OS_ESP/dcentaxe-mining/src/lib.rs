// DCENT_axe Mining Work Dispatcher
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
// Coordinates between the Stratum client and ASIC driver:
// - Converts Stratum jobs into ASIC work items (midstate computation)
// - Tracks in-flight jobs for nonce-to-share mapping
// - Manages extranonce2 incrementing
// - Validates nonces before submission
// - Tracks hashrate statistics

pub mod dispatcher;
pub mod stats;

pub use dispatcher::{MiningDispatcher, PoolSlot};
pub use stats::MiningStats;
