// DCENT_axe Work Bridge -- MiningWork -> MiningJob conversion
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0

use dcentaxe_asic::{AsicModel, MiningJob};
use dcentaxe_stratum::MiningWork;

/// Convert a Stratum MiningWork into an ASIC-ready MiningJob.
///
/// BM1397: uses midstate-based jobs (midstates + merkle4)
/// BM1366/BM1368/BM1370: uses full header jobs (prev_hash + merkle_root)
///
/// IMPORTANT: BM1366/68/70 ASICs expect merkle_root and prev_block_hash
/// with 32-bit words in REVERSED order (word[0]↔word[7], etc.).
/// The ASIC internally reverses them back when constructing the block header.
/// This matches ESP-Miner's `reverse_32bit_words()` in construct_bm_job().
pub fn mining_work_to_job(work: &MiningWork, job_id: u8, model: AsicModel) -> MiningJob {
    match model {
        AsicModel::BM1397 => {
            // BM1397 uses midstate-based work with word-reversed midstates.
            // ESP-Miner applies reverse_32bit_words() to each midstate.
            // BM1397 doesn't have a hardware version_mask register (set_version_mask
            // is a no-op), but supports version rolling through multiple midstates.
            // The dispatcher reconstructs the rolled version from the midstate index
            // using increment_bitmask(), so nonces from all 4 midstates are valid.
            let reversed_midstates: Vec<[u8; 32]> = work
                .midstates
                .iter()
                .map(|ms| reverse_32bit_words(ms))
                .collect();
            MiningJob::new_midstate(
                job_id,
                work.version,
                work.ntime,
                work.nbits,
                0, // starting_nonce
                work.merkle4,
                reversed_midstates,
            )
        }
        AsicModel::BM1366 | AsicModel::BM1368 | AsicModel::BM1370 | AsicModel::BM1373 => {
            // Word-reverse merkle_root and prev_block_hash for the ASIC
            let prev_hash_reversed = reverse_32bit_words(&work.prev_block_hash);
            let merkle_root_reversed = reverse_32bit_words(&work.merkle_root);

            MiningJob::new_full(
                job_id,
                work.version,
                prev_hash_reversed,
                merkle_root_reversed,
                work.ntime,
                work.nbits,
                0, // starting_nonce
            )
        }
    }
}

/// Build a MiningWork directly from SV2 NewJob data.
///
/// SV2 standard channels provide pre-computed merkle_root and prev_hash,
/// so we skip coinbase construction and merkle branch computation entirely.
/// We just need to compute midstates and extract merkle4.
#[cfg(feature = "stratum-v2")]
pub fn sv2_job_to_mining_work(
    job_id: u32,
    version: u32,
    prev_hash: [u8; 32],
    merkle_root: [u8; 32],
    nbits: u32,
    ntime: u32,
    version_mask: u32,
    share_target: [u8; 32],
) -> MiningWork {
    use dcentaxe_stratum::work::compute_midstate;

    // BIP320 canonical version-rolling mask (BIP320 positions 13..28). MUST equal
    // dcentaxe_mining::dispatcher::BIP320_DEFAULT_VERSION_MASK and DCENT_OS's am2
    // BM1362 load-bearing rule (feedback_am2_serial_dispatch_bip320_version_rolling_required;
    // SERIAL_VERSION_ROLLING_FIELD_MASK = 0x1FFF_E000 in dcentrald serial_mining.rs).
    //
    // SV2 standard channels carry no negotiated version_mask today, so SV2 NewJob
    // passes mask 0. With mask 0 the midstate loop below builds exactly ONE midstate,
    // which disables ASICBoost for BM1397 (midstate_mode board: set_version_mask is a
    // no-op, version rolling happens ONLY via multiple midstates) — discarding ~75% of
    // its rolling throughput under SV2. BM1366/68/70/73 (full-header boards) are saved
    // by the dispatcher upgrading mask 0 -> canonical in effective_hardware_mask(); this
    // upgrade converges the SV2 work path to that same canonical mask so all models roll.
    const BIP320_CANONICAL_VERSION_MASK: u32 = 0x1FFFE000;
    let version_mask = if version_mask == 0 {
        BIP320_CANONICAL_VERSION_MASK
    } else {
        version_mask
    };

    // Assemble header prefix (first 64 bytes for midstate)
    let mut header_prefix = [0u8; 64];
    header_prefix[0..4].copy_from_slice(&version.to_le_bytes());
    header_prefix[4..36].copy_from_slice(&prev_hash);
    header_prefix[36..64].copy_from_slice(&merkle_root[0..28]);

    // Compute midstate(s)
    let mut midstates = Vec::with_capacity(4);
    midstates.push(compute_midstate(&header_prefix));

    if version_mask != 0 {
        let mut rolled = version;
        for _ in 0..3 {
            rolled = dcentaxe_stratum::work::increment_bitmask(rolled, version_mask);
            header_prefix[0..4].copy_from_slice(&rolled.to_le_bytes());
            midstates.push(compute_midstate(&header_prefix));
        }
    }

    // Extract merkle4 (last 4 bytes of merkle root)
    let mut merkle4 = [0u8; 4];
    merkle4.copy_from_slice(&merkle_root[28..32]);

    MiningWork {
        midstates,
        merkle4,
        ntime,
        nbits,
        version,
        version_mask,
        prev_block_hash: prev_hash,
        merkle_root,
        job_id: format!("{}", job_id),
        extranonce2: String::new(),
        share_target,
    }
}

/// Reverse the order of 32-bit words in a 32-byte array.
/// word[0]↔word[7], word[1]↔word[6], etc.
/// Port of ESP-Miner's reverse_32bit_words().
fn reverse_32bit_words(src: &[u8; 32]) -> [u8; 32] {
    let mut dest = [0u8; 32];
    for i in 0..8 {
        let j = 7 - i;
        dest[i * 4..i * 4 + 4].copy_from_slice(&src[j * 4..j * 4 + 4]);
    }
    dest
}
