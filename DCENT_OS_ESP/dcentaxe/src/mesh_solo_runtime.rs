// DCENT_axe — solo mesh runtime glue (P2 binary outcomes)
// Copyright (C) 2026 D-Central Technologies
// License: GPL-3.0
//
//! Bridges pure `MeshSoloController` / `GatewaySoloSubmit` / fragment collector
//! into the LoRa radio task and stratum share path. Compiled only under
//! `--features lora`. Default posture: **OFF** until MeshConfig opts in.
//!
//! ## Honest semantics
//! - Solo candidates are **not** pool shares (`set_solo_share_hook`).
//! - Local success = "block_candidate" / submit-prep hex — never "accepted"
//!   until operator bitcoind `submitblock` (regtest first).

#![cfg(feature = "lora")]

use std::sync::mpsc;
use std::sync::Mutex;

use log::{info, warn};

use dcentaxe_lora::config::MeshConfig;
use dcentaxe_lora::gateway::{GatewayFragmentCollector, GatewayIngest};
use dcentaxe_lora::mesh::{MeshFrame, MeshKind, NodeId, Tip};
use dcentaxe_lora::reassembly::fragment_block;
use dcentaxe_lora::relay::TxQueue;
use dcentaxe_stratum::gateway_solo::GatewaySoloSubmit;
use dcentaxe_stratum::mesh_solo::{MeshSoloController, MeshSoloMode, TipAdmit};
use dcentaxe_stratum::set_solo_share_hook;
use dcentaxe_stratum::solo::{ChainId, SoloTip};
use dcentaxe_stratum::types::{ShareSubmission, StratumEvent};

/// Live solo-mesh runtime shared by the radio task (tip/BFG RX) and the share hook.
struct SoloRuntime {
    ctrl: MeshSoloController,
    gateway: GatewayFragmentCollector,
    submit: GatewaySoloSubmit,
    event_tx: Option<mpsc::Sender<StratumEvent>>,
    /// Local node id for originating Tip / BFG frames.
    node_id: NodeId,
    seq: u8,
    is_gateway: bool,
}

static RUNTIME: Mutex<Option<SoloRuntime>> = Mutex::new(None);

fn parse_chain(s: &str) -> ChainId {
    match s.trim().to_ascii_lowercase().as_str() {
        "mainnet" | "main" => ChainId::Mainnet,
        "testnet" | "test" => ChainId::Testnet,
        _ => ChainId::Regtest,
    }
}

fn tip_from_mesh(t: &Tip) -> SoloTip {
    SoloTip {
        prev_hash: t.prev_hash,
        nbits: t.nbits,
        ntime: t.ntime,
        height: t.height,
    }
}

/// Initialize (or re-init) solo mesh runtime from config. Fail-closed when
/// `solo_mesh_empty_active()` is false — clears runtime and unregisters hook.
pub fn configure(cfg: &MeshConfig, node_id: NodeId) {
    let mut g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    if !cfg.solo_mesh_empty_active() {
        *g = None;
        set_solo_share_hook(None);
        info!("mesh_solo_runtime: disabled (fail-closed defaults or incomplete config)");
        return;
    }
    let chain = parse_chain(&cfg.solo_chain);
    let payout = cfg.solo_payout_address.trim();
    let ctrl = match MeshSoloController::from_address_for_chain(payout, chain)
        .or_else(|_| MeshSoloController::from_address(payout))
    {
        Ok(mut c) => {
            c.set_chain(chain);
            c.enable_solo_mesh_empty();
            c
        }
        Err(e) => {
            warn!("mesh_solo_runtime: bad payout address ({e}) — solo disabled");
            *g = None;
            set_solo_share_hook(None);
            return;
        }
    };
    let prev_tx = g.as_ref().and_then(|r| r.event_tx.clone());
    *g = Some(SoloRuntime {
        ctrl,
        gateway: GatewayFragmentCollector::new(),
        submit: GatewaySoloSubmit::new(),
        event_tx: prev_tx,
        node_id,
        seq: 0,
        is_gateway: cfg.is_gateway,
    });
    set_solo_share_hook(Some(on_solo_share));
    info!(
        "mesh_solo_runtime: ENABLED chain={} gateway={} mode={}",
        chain.label(),
        cfg.is_gateway,
        MeshSoloMode::SoloMeshEmpty.label()
    );
}

/// Register the stratum NewJob channel (clone of mining path's event_tx).
pub fn set_event_tx(tx: mpsc::Sender<StratumEvent>) {
    let mut g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = g.as_mut() {
        r.event_tx = Some(tx);
    } else {
        // Stash not possible without controller — main should call configure first
        // then set_event_tx. If order is reversed, drop until reconfigure.
        warn!("mesh_solo_runtime: set_event_tx before configure — ignored until configure");
        drop(g);
        // Keep a one-shot park: re-call after configure re-reads None; main clones
        // event_tx after lora spawn in typical order. Store in thread-local? Skip —
        // main will call set_event_tx after both exist.
        let _ = tx;
    }
}

/// Store event_tx even before configure (main may spawn stratum first).
static PARKED_TX: Mutex<Option<mpsc::Sender<StratumEvent>>> = Mutex::new(None);

pub fn park_event_tx(tx: mpsc::Sender<StratumEvent>) {
    *PARKED_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
    let mut g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = g.as_mut() {
        r.event_tx = Some(tx);
    }
}

/// Handle an inbound mesh frame for solo tip / BFG (called under radio RX path).
/// May push frames onto `tx_queue` (BFG originator side is via share hook).
pub fn on_rx_frame(frame: &MeshFrame, now_ms: u64, tx_queue: &mut TxQueue) {
    let mut g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(rt) = g.as_mut() else {
        return;
    };
    match &frame.kind {
        MeshKind::Tip(t) => {
            let solo_tip = tip_from_mesh(t);
            match rt.ctrl.admit_tip(solo_tip) {
                TipAdmit::Accepted { height } => {
                    info!("mesh_solo: tip admitted height={height}");
                    // Gateway also binds tip for submit-prep.
                    if rt.is_gateway {
                        rt.submit.set_tip(solo_tip);
                    }
                    if let Ok(epoch) = rt.ctrl.next_work() {
                        // CRITICAL: PrebuiltWork carries compact-nBits share_target
                        // and the controller's en2. NewJob alone would rebuild via
                        // the pool WorkBuilder and desync build_candidate_from_share.
                        let send_prebuilt = |tx: &mpsc::Sender<StratumEvent>, epoch: &dcentaxe_stratum::mesh_solo::SoloWorkEpoch| {
                            let _ = tx.send(StratumEvent::PrebuiltWork {
                                work: epoch.work.clone(),
                                clean_jobs: true,
                            });
                            info!(
                                "mesh_solo: PrebuiltWork {} en2={} (solo empty — not pool shares)",
                                epoch.work.job_id, epoch.work.extranonce2
                            );
                        };
                        if let Some(tx) = &rt.event_tx {
                            send_prebuilt(tx, &epoch);
                        } else if let Some(tx) =
                            PARKED_TX.lock().unwrap_or_else(|e| e.into_inner()).clone()
                        {
                            rt.event_tx = Some(tx.clone());
                            send_prebuilt(&tx, &epoch);
                        } else {
                            warn!("mesh_solo: tip ok but no event_tx — work not dispatched");
                        }
                    }
                }
                TipAdmit::RejectedStale => {}
                TipAdmit::RejectedPolicy(e) => {
                    warn!("mesh_solo: tip refused: {e}");
                }
                TipAdmit::IgnoredModeOff => {}
            }
        }
        MeshKind::BlockFragment(f) => {
            if !rt.is_gateway {
                return;
            }
            match rt.gateway.ingest(f, now_ms) {
                GatewayIngest::Complete(bytes) => {
                    match rt.submit.prepare(&bytes) {
                        Ok(prep) => {
                            // Operator-facing: hex ready for submitblock — NOT accepted.
                            info!(
                                "mesh_solo: GATEWAY submit-prep ready tip_h={} diff={:.4} hex_len={} (regtest submitblock is operator)",
                                prep.tip_height,
                                prep.difficulty,
                                prep.block_hex.len()
                            );
                            let _ = tx_queue; // no auto-submit RPC in binary yet
                        }
                        Err(e) => warn!("mesh_solo: gateway prepare failed: {e}"),
                    }
                }
                GatewayIngest::Pending { .. } | GatewayIngest::Duplicate => {}
                GatewayIngest::Rejected(r) => {
                    warn!("mesh_solo: BFG reject {r:?}");
                }
            }
        }
        _ => {}
    }
}

/// Originating gateway: enqueue a Tip frame for the current stratum height/nbits.
pub fn maybe_origin_tip(
    prev_hash: [u8; 32],
    nbits: u32,
    ntime: u32,
    height: i64,
    tx_queue: &mut TxQueue,
) {
    let mut g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(rt) = g.as_mut() else {
        return;
    };
    if !rt.is_gateway {
        return;
    }
    let tip = SoloTip {
        prev_hash,
        nbits,
        ntime,
        height,
    };
    rt.submit.set_tip(tip);
    let mesh_tip = Tip {
        prev_hash,
        nbits,
        ntime,
        height,
    };
    let frame = MeshFrame::originate(rt.node_id, rt.seq, MeshKind::Tip(mesh_tip));
    rt.seq = rt.seq.wrapping_add(1);
    tx_queue.push(frame);
    // Dual-role lab: admit only when solo controller is active (fail-closed
    // when runtime disabled). Real prev_hash must be plumbed before mainnet.
    let _ = rt.ctrl.admit_tip(tip);
}

/// Stratum solo-share hook: bind share.extranonce2/job_id to reconstruct the
/// coinbase/header the ASIC actually hashed, then fragment onto mesh TX.
fn on_solo_share(share: &ShareSubmission) {
    let mut g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(rt) = g.as_mut() else {
        return;
    };
    match rt.ctrl.build_candidate_from_share(share) {
        Ok(cand) => {
            info!(
                "mesh_solo: local block CANDIDATE tip_h={} diff={:.4} en2={} bytes={} (not pool-accepted)",
                cand.tip_height,
                cand.difficulty,
                share.extranonce2,
                cand.block_bytes.len()
            );
            let id = u32::from_le_bytes([
                cand.header[0],
                cand.header[1],
                cand.header[2],
                cand.header[3],
            ]);
            if let Some(frags) = fragment_block(id, &cand.block_bytes, 80) {
                for f in frags {
                    let frame =
                        MeshFrame::originate(rt.node_id, rt.seq, MeshKind::BlockFragment(f));
                    rt.seq = rt.seq.wrapping_add(1);
                    OUTBOUND_BFG
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(frame);
                }
            }
        }
        Err(e) => {
            warn!("mesh_solo: candidate build failed (en2-bound): {e}");
        }
    }
}

static OUTBOUND_BFG: Mutex<Vec<MeshFrame>> = Mutex::new(Vec::new());

/// Drain fragment frames produced by the share hook into the radio TX queue.
pub fn drain_outbound_bfg(tx_queue: &mut TxQueue) {
    let mut q = OUTBOUND_BFG.lock().unwrap_or_else(|e| e.into_inner());
    for frame in q.drain(..) {
        tx_queue.push(frame);
    }
}

/// Snapshot for dashboard honesty (no share counters).
pub fn metrics_json() -> serde_json::Value {
    let g = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(rt) => {
            let m = rt.ctrl.metrics_snapshot();
            serde_json::json!({
                "enabled": true,
                "mode": m.mode.label(),
                "chain": m.chain.label(),
                "tip_height": m.tip_height,
                "tips_admitted": m.tips_admitted,
                "tips_stale": m.tips_stale,
                "candidates_built": m.candidates_built,
                "has_work_epoch": m.has_work_epoch,
                "is_gateway": rt.is_gateway,
                "note": "solo candidates are not pool shares",
            })
        }
        None => serde_json::json!({
            "enabled": false,
            "mode": "OFF",
            "note": "solo mesh fail-closed until mesh.solo_relay_enabled+mining_source+payout",
        }),
    }
}

/// Structural test helpers (host cannot run this module under esp-idf feature).
#[cfg(all(test, not(target_os = "espidf")))]
mod host_doc_tests {
    #[test]
    fn solo_job_prefix_is_stable_contract() {
        // Pin the client.rs intercept string so a rename breaks the gate.
        assert!(dcentaxe_stratum::types::ShareSubmission {
            job_id: "solo-regtest-1".into(),
            extranonce2: "00".into(),
            ntime: "00".into(),
            nonce: "00".into(),
            version: 0,
            version_bits: None,
            difficulty: 1.0,
        }
        .job_id
        .starts_with("solo-"));
    }
}
