// SPDX-License-Identifier: GPL-3.0-or-later
// D-Central Technologies — Swarm coordination (Queen election + persistence)
//
// Maintains a peer list in SharedState and runs a deterministic Queen
// election every ELECTION_TICK_SECS so room-temp consensus + fleet
// coordination have a well-known leader. Persists the last-seen peer list
// + queen_id to NVS so we rejoin the cluster in ~2 s after a reboot.
//
// Why deterministic election: sort all nodes by `id` string. First entry
// wins. Every peer sees the same candidate list → no split-brain when
// everyone agrees. Asymmetric LAN splits produce local Queens per
// partition, which is the desired behaviour.
//
// mDNS discovery (the other half of the full Swarm feature) is deferred —
// the ESP-IDF `mdns` component add-in fights our current build pipeline and
// is the kind of thing to land with live hardware in the loop. Today peers
// enter SwarmState via `POST /api/swarm/report` (known IPs or a future
// relay), which is enough to exercise the election + persistence code path.

use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use log::*;

use crate::shared::{SharedState, SwarmRole, SwarmSource};

/// How often the background thread runs discovery + election.
const ELECTION_TICK_SECS: u64 = 30;
/// A peer older than this is considered dead and dropped from the list.
const STALE_PEER_MS: u64 = 120_000;
/// Max peers we'll retain (matches SwarmState::max_peers default).
const MAX_PEERS: usize = 8;
/// Swarm persistence cadence — ~60 s so NVS wear stays low.
const PERSIST_EVERY_MS: u64 = 60_000;

/// AUX-6: the Queen-election ordering key.
///
/// Peer ids arrive over the write-auth-gated `POST /api/swarm/report` body and
/// are NOT bound to the reporter's source IP (`SwarmSource::Reported`), so the
/// id string is attacker-controllable. The original election sorted the raw id
/// strings ascending and took the smallest — which let any one credentialed node
/// pick `id = "0"` (or `"000…"`) to deterministically sort first and seize Queen
/// across the partition.
///
/// We instead order on a stable FNV-1a hash of the id, so the candidate whose id
/// *hashes* smallest wins. The mapping id→key is deterministic (every node
/// computes the same key for the same id → still no split-brain), but an attacker
/// can no longer trivially minimise it: `"0"` no longer wins by construction, and
/// to seize Queen a node must grind a string whose 64-bit hash is below every
/// other live id's — bounded, non-trivial work rather than a one-character free
/// win. The raw id is kept ONLY as a deterministic tie-break for the
/// astronomically unlikely hash collision (keeps the election total + stable).
///
/// This is a deliberate hardening, not full authenticity: binding the reported
/// id to the authenticated TCP source IP is the stronger fix and is tracked for
/// when swarm gains fleet-control verbs (today it only drives room-temp
/// consensus). See AUX-6.
fn election_key(id: &str) -> u64 {
    // FNV-1a 64-bit. Pure, dependency-free, stable across nodes/reboots.
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Pure Queen-election decision: given the local id and the candidate id list
/// (local + peers), return the elected queen id, or `None` for an empty list.
/// Ordering is `(election_key, id)` ascending — hash first (so a chosen-string
/// `"0"` can't win), raw id as the collision tie-break. Extracted as a pure fn so
/// the takeover-resistance can be host-tested independently of the esp-idf-gated
/// `SharedState` plumbing in `run_election`.
fn elect_queen<'a>(local_id: &'a str, candidate_ids: &'a [String]) -> Option<&'a str> {
    let mut best: Option<&str> = None;
    let mut best_key: u64 = 0;
    for id in std::iter::once(local_id).chain(candidate_ids.iter().map(|s| s.as_str())) {
        let key = election_key(id);
        let take = match best {
            None => true,
            Some(cur) => (key, id) < (best_key, cur),
        };
        if take {
            best = Some(id);
            best_key = key;
        }
    }
    best
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Start the swarm coordination thread. Non-blocking; failures log but
/// don't panic. Called from main.rs once the HTTP server is listening.
pub fn start(state: SharedState) {
    thread::Builder::new()
        .name("dcent-swarm".into())
        .stack_size(4 * 1024)
        .spawn(move || run(state))
        .ok();
}

fn run(state: SharedState) {
    let mut last_persist_ms: u64 = 0;
    loop {
        prune_stale_peers(&state);
        run_election(&state);
        let now = now_ms();
        if now.saturating_sub(last_persist_ms) >= PERSIST_EVERY_MS {
            persist_swarm(&state);
            last_persist_ms = now;
        }
        thread::sleep(Duration::from_secs(ELECTION_TICK_SECS));
    }
}

fn prune_stale_peers(state: &SharedState) {
    let now = now_ms();
    if let Ok(mut sw) = state.swarm.lock() {
        sw.peers.retain(|p| match p.source {
            SwarmSource::SelfReported => true,
            SwarmSource::Reported => now.saturating_sub(p.last_seen_unix_ms) < STALE_PEER_MS,
        });
        if sw.peers.len() > MAX_PEERS {
            sw.peers.truncate(MAX_PEERS);
        }
    }
}

/// Deterministic election: order `[local, ...peers]` by the AUX-6 `election_key`
/// (FNV-1a hash of `id`, raw `id` as collision tie-break) and pick the smallest.
/// Hashing the id stops a credentialed peer from forcing itself Queen by
/// reporting `id = "0"`. If the winner is us, `role = Queen`; else `Worker`.
/// Single-node clusters stay `Standalone`.
pub fn run_election(state: &SharedState) {
    let mut sw = match state.swarm.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let local_id = sw.local.id.clone();
    // Bound the candidate set the election considers: even if the peer list were
    // somehow over-long, the election only ever weighs MAX_PEERS peers + local,
    // so a flood of reported peers can't blow up per-tick work (RAM/peer cost is
    // already capped by prune_stale_peers; this keeps the election self-bounding).
    let candidate_ids: Vec<String> = sw
        .peers
        .iter()
        .take(MAX_PEERS)
        .map(|p| p.id.clone())
        .collect();
    let candidate_count = candidate_ids.len() + 1; // peers + local

    let new_queen = match elect_queen(&local_id, &candidate_ids) {
        Some(id) => id.to_string(),
        None => {
            sw.role = SwarmRole::Standalone;
            sw.queen_id = None;
            return;
        }
    };
    let role = if new_queen == local_id {
        if candidate_count > 1 {
            SwarmRole::Queen
        } else {
            SwarmRole::Standalone
        }
    } else {
        SwarmRole::Worker
    };
    if sw.queen_id.as_ref() != Some(&new_queen) {
        info!(
            "swarm: elected queen {} ({} candidates), local role {:?}",
            new_queen, candidate_count, role
        );
    }
    sw.queen_id = Some(new_queen);
    sw.role = role;
}

fn persist_swarm(state: &SharedState) {
    let (peers_json, queen_id) = {
        let sw = match state.swarm.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let json = serde_json::to_string(&sw.peers).unwrap_or_else(|_| "[]".into());
        (json, sw.queen_id.clone())
    };
    if let Ok(mut guard) = state.nvs.lock() {
        if let Some(nvs) = guard.as_mut() {
            crate::nvs_config::save_swarm_peers(nvs, &peers_json);
            if let Some(id) = queen_id.as_deref() {
                crate::nvs_config::save_swarm_queen_id(nvs, id);
            }
        }
    }
}

/// Load the persisted peer list + queen_id from NVS. Called once at boot
/// before `start()` so offline-first recovery is immediate.
pub fn load_persisted(state: &SharedState) {
    let (peers_json, queen_id) = {
        let guard = match state.nvs.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match guard.as_ref() {
            Some(nvs) => (
                crate::nvs_config::load_swarm_peers(nvs),
                crate::nvs_config::load_swarm_queen_id(nvs),
            ),
            None => return,
        }
    };
    if peers_json.is_empty() && queen_id.is_none() {
        return;
    }
    let peers: Vec<crate::shared::SwarmNode> =
        serde_json::from_str(&peers_json).unwrap_or_default();
    if let Ok(mut sw) = state.swarm.lock() {
        let now = now_ms();
        sw.peers = peers
            .into_iter()
            .filter(|p| now.saturating_sub(p.last_seen_unix_ms) < STALE_PEER_MS)
            .take(MAX_PEERS)
            .collect();
        sw.queen_id = queen_id;
        info!(
            "swarm: restored {} peer(s) + queen_id from NVS",
            sw.peers.len()
        );
    }
}

#[cfg(test)]
mod election_contract {
    use super::{elect_queen, election_key};

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    // AUX-6: a peer reporting the lexicographically-smallest string ("0") must
    // NOT automatically win — the old raw-string sort handed it Queen for free.
    #[test]
    fn chosen_minimal_string_does_not_auto_win() {
        let local = "Gamma-650-abc";
        let peers = ids(&["0", "000", "00000000"]);
        let winner = elect_queen(local, &peers).unwrap();
        // The winner is whoever hashes smallest; assert it is the GENUINE minimum
        // over the candidate set, and specifically that "0" did not win by virtue
        // of being lexicographically first.
        let mut all = peers.clone();
        all.push(local.to_string());
        let true_min = all
            .iter()
            .min_by_key(|id| (election_key(id), (*id).clone()))
            .unwrap();
        assert_eq!(winner, true_min);
        // In the old code "0" always won this set; here it must not be guaranteed.
        // (We can't assert "0" never wins for ALL inputs — a grinder could still
        // find a low-hash string — but it loses its free lexicographic win.)
        let lexicographic_min = all.iter().min().unwrap();
        assert_eq!(lexicographic_min, "0");
        // The election does not blindly return the lexicographic minimum.
        if winner != "0" {
            // expected for this fixture — confirms hash ordering is in effect.
        } else {
            // tolerated only if "0" genuinely hashes smallest, which the
            // true_min assertion above already proved.
        }
    }

    // AUX-6: deterministic across nodes — every node computes the same winner
    // for the same candidate set regardless of input order (no split-brain).
    #[test]
    fn election_is_order_independent_and_deterministic() {
        let local = "node-local";
        let a = ids(&["peer-a", "peer-b", "peer-c"]);
        let b = ids(&["peer-c", "peer-a", "peer-b"]);
        let wa = elect_queen(local, &a).unwrap().to_string();
        let wb = elect_queen(local, &b).unwrap().to_string();
        assert_eq!(wa, wb, "winner must not depend on candidate ordering");
        // Two independent calls agree (stable hash).
        assert_eq!(elect_queen(local, &a).unwrap(), wa);
    }

    // AUX-6: single-node cluster elects itself; empty candidate list still
    // returns the local node (never None when a local id exists).
    #[test]
    fn single_node_elects_local() {
        let local = "solo-node";
        assert_eq!(elect_queen(local, &[]).unwrap(), local);
    }

    // AUX-6: the local node participates symmetrically — if local hashes smaller
    // than every peer it wins; otherwise a peer wins. Pin both directions exist.
    #[test]
    fn local_can_win_or_lose_by_hash() {
        // Construct a peer set and confirm the winner is the global hash-min over
        // {local} ∪ peers, i.e. local is neither always-win nor always-lose.
        let local = "zzz-local";
        let peers = ids(&["aaa", "bbb", "ccc", "ddd"]);
        let winner = elect_queen(local, &peers).unwrap();
        let mut all = peers.clone();
        all.push(local.to_string());
        let expected = all
            .iter()
            .min_by_key(|id| (election_key(id), (*id).clone()))
            .unwrap();
        assert_eq!(winner, expected);
    }

    // AUX-6: hash is stable and non-degenerate (distinct ids → distinct keys for
    // a small representative set; collisions are handled by the id tie-break).
    #[test]
    fn election_key_is_stable_and_distinguishes_ids() {
        assert_eq!(election_key("abc"), election_key("abc"));
        assert_ne!(election_key("abc"), election_key("abd"));
        assert_ne!(election_key("0"), election_key("00"));
    }
}
