//! CE-011 (2026-07-08) — source-pin for the BM1362 SKU -> autotuner PVT
//! ceiling wiring.
//!
//! ## Context
//!
//! The per-SKU PVT envelope clamp in `dcentrald-autotuner` is only
//! reachable if a production path registers the live hashboard SKU via
//! `AutoTuner::set_chain_sku`. Before CE-011, `set_chain_sku` had ZERO
//! production call sites, so the SKU-envelope clamp was DEAD on every live
//! path (mitigated only by the independent am2 hard pins). CE-011 wires two
//! registration paths and adds a CEILING-ONLY `apply_sku_freq_ceilings`:
//!
//!   * am2 hybrid (`s19j_hybrid_mining.rs`): persist the accepted Phase-0s
//!     energize-gate `SkuBinding`s (Ok arm only; the accept_degraded
//!     override arm leaves them empty — fail-closed) and, at the freq-only
//!     tuner spawn, register the uniform BM1362 SKU on synthetic chain 0.
//!   * daemon (`daemon.rs`): the Wave-K SKU-classification block builds a
//!     `chain_id -> Bm1362HashboardSku` map and registers it after
//!     `AutoTuner::new`.
//!
//! This source-parse test pins both wirings so a future refactor cannot
//! silently UN-wire the ceiling backstop.

const HYBRID_SRC: &str = include_str!("../src/s19j_hybrid_mining.rs");
const DAEMON_SRC: &str = include_str!("../src/daemon.rs");

#[test]
fn ce011_hybrid_persists_bindings_from_ok_arm_not_accept_degraded() {
    // The struct field must exist and default to empty (fail-closed).
    assert!(
        HYBRID_SRC.contains("accepted_sku_bindings"),
        "s19j_hybrid_mining.rs must carry the `accepted_sku_bindings` field"
    );
    assert!(
        HYBRID_SRC.contains("accepted_sku_bindings: Vec::new()"),
        "`accepted_sku_bindings` must default to an empty Vec in `new()`"
    );

    // Persist happens in the energize-gate Ok arm.
    let persist = "self.accepted_sku_bindings = bindings;";
    let persist_pos = HYBRID_SRC.find(persist).expect(
        "s19j_hybrid_mining.rs must persist accepted bindings via \
         `self.accepted_sku_bindings = bindings;` in the energize-gate Ok arm",
    );

    // ...and BEFORE the accept_degraded override arm (the Err arm), so the
    // degraded lab-override path leaves the field EMPTY (fail-closed — an
    // unverified hardware set never registers a PVT ceiling).
    let degraded_pos = HYBRID_SRC
        .find("if accept_degraded {")
        .expect("energize gate must still carry the `if accept_degraded {` override arm");

    assert!(
        persist_pos < degraded_pos,
        "CE-011: bindings must be persisted in the ACCEPTED (Ok) arm, which is \
         textually before the accept_degraded override arm — the degraded arm \
         must NOT populate the SKU bindings (fail-closed)."
    );
}

#[test]
fn ce011_hybrid_registers_uniform_sku_at_tuner_spawn() {
    assert!(
        HYBRID_SRC.contains("uniform_bm1362_sku_for_bindings"),
        "s19j_hybrid_mining.rs must call `uniform_bm1362_sku_for_bindings` at the \
         freq-only tuner spawn"
    );
    assert!(
        HYBRID_SRC.contains("tuner.set_chain_sku(0, sku)"),
        "s19j_hybrid_mining.rs must register the resolved SKU on synthetic chain 0 \
         via `tuner.set_chain_sku(0, sku)`"
    );
}

#[test]
fn ce011_daemon_wavek_registers_classified_skus() {
    assert!(
        DAEMON_SRC.contains("autotuner_chain_skus"),
        "daemon.rs must build the `chain_id -> Bm1362HashboardSku` map \
         (`autotuner_chain_skus`)"
    );
    assert!(
        DAEMON_SRC.contains("hashboard_to_bm1362_sku"),
        "daemon.rs Wave-K block must map classified Hashboards via \
         `hashboard_to_bm1362_sku`"
    );
    assert!(
        DAEMON_SRC.contains("autotuner_chain_skus.insert("),
        "daemon.rs Wave-K block must insert classified SKUs into the map"
    );
    assert!(
        DAEMON_SRC.contains("tuner.set_chain_sku(chain_id, sku)"),
        "daemon.rs must register the classified SKUs after `AutoTuner::new` via \
         `tuner.set_chain_sku(chain_id, sku)`"
    );
}
