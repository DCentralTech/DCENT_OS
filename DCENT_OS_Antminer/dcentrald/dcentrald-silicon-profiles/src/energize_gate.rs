//!  B2 (2026-05-22): runtime hashboard-SKU energize-refusal gate.
//!
//! **Drive-half** of matrix §7 #15. The observe-half (Wave K) shipped
//! per-chain EEPROM-preamble telemetry/classification in `daemon.rs`. This
//! module is the load-bearing fail-closed gate the **mining paths** call
//! BEFORE any `set_voltage` / `enable_voltage` / `cold_boot_init` on a
//! chain.
//!
//! ## What this gate refuses
//!
//! §7 #15 (drive-half):
//!
//! 1. **Malformed EEPROM preamble** — the first 2 bytes are not one of
//!    the canonical hashboard-family markers (`0x04 0x11` BHB42xxx;
//!    `0x05 0x11` BHB56902 NoPic), AND not all-`0xFF` or all-`0x00`
//!    (the unpopulated / empty case, which is treated as "no board" and
//!    silently skipped — not a refusal).
//! 2. **AM2 EEPROM readiness timeout** — the per-chain EEPROM read
//!    didn't complete within the operator-supplied deadline. Fail
//!    closed: a dsPIC that doesn't surface its EEPROM in time is in an
//!    unknown state.
//! 3. **Mixed-SKU chains** — chain N reports BHB42xxx while chain M
//!    reports BHB56902 (or any other cross-family pairing). A mixed
//!    fleet's PVT envelopes don't align, so we refuse the whole
//!    platform rather than driving any chain at a wrong setpoint.
//! 4. **Per-chain profile-binding failure** — the readable preamble
//!    doesn't classify to ANY known SKU (`classify_by_eeprom_preamble`
//!    returns `None`). Treated identically to (1).
//!
//! ## What this gate does NOT do
//!
//! - **`(freq, voltage)` PVT-envelope validation** lives in
//!   [`crate::pvt_envelope::validate_freq_volt`] (W13.C3). This gate
//!   only resolves the SKU binding. Mining paths that already drove
//!   chains MUST plug the resolved [`SkuBinding`] back into
//!   `validate_freq_volt` before each setpoint write.
//! - **Live S9 / am1-s9 routing** — BHB-S9 / BHB-S11 / BHB-S17
//!   hashboards have no pinned preamble in [`crate::hashboards`]; this
//!   gate treats them as a known "non-AT24-EEPROM platform" and never
//!   blocks am1-s9. Only PicType::NoPic and PicType::DsPic33Ep paths
//!   (am2 / am3 BB / am3-aml) ever call this gate.
//!
//! ## Env gating (rollout pattern, /22 style)
//!
//! `DCENT_AM2_STRICT_SKU_REFUSE=1` flips the gate from
//! **telemetry-only** (log + record a refusal reason but proceed) to
//! **strict-refuse** (return `Err(EnergizeRefusal)` — the caller is
//! expected to tear down without energizing).
//!
//! For the **first deploy** we ship default-OFF: the gate logs every
//! refusal decision so the operator can confirm `a lab unit`'s BHB42601
//! chains classify cleanly and no false-positive refusal is recorded;
//! only AFTER that confirmation do we promote the env-default to ON in
//! a follow-up  commit.
//!
//! Lab override: `DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1` mirrors the
//! existing toolbox `--accept-degraded-hardware` pattern and lets the
//! strict-refuse branch proceed anyway (still logs LOUDLY). Use only
//! when bringing up a known-good unit with a flaky EEPROM bus.
//!
//! ## Cross-references
//!
//! -  — 0x50-0x57 are READ-OK,
//!   write-denied.
//! -  — fw=0x86 refusal at the dsPIC
//!   layer. This gate is the layer ABOVE that: refuses on EEPROM-side
//!   evidence even before any dsPIC commands fly.
//! -  — XIL `a lab unit`
//!   classifies as BHB42601 (`a lab unit`'s class). Whatever this gate
//!   accepts for `a lab unit` MUST also accept `a lab unit`.

use crate::hashboards::{classify_by_eeprom_preamble, Hashboard};

/// Result of probing one chain's EEPROM during pre-energize gating.
///
/// Mining paths construct these from per-chain reads of the
/// `/sys/bus/i2c/devices/<bus>-005<slot>/eeprom` node (or an equivalent
/// I²C one-shot) BEFORE driving voltage. The gate then folds these into
/// a platform-wide energize decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainProbe {
    /// EEPROM read completed AND the first 2 bytes classify to a known
    /// hashboard SKU. Safe to energize unless mixed-SKU detected
    /// downstream.
    Classified {
        chain_id: u8,
        preamble: [u8; 2],
        sku: Hashboard,
    },
    /// EEPROM read completed BUT the 2-byte preamble is neither
    /// BHB42xxx (`0x04 0x11`) nor BHB56902 (`0x05 0x11`) AND not
    /// "unpopulated" (all-`0xFF` / all-`0x00`). This is the
    /// "untrusted preamble" case — an EEPROM is electrically present
    /// but reports an unexpected header. **Refuse.**
    MalformedPreamble { chain_id: u8, preamble: [u8; 2] },
    /// EEPROM read returned ALL `0xFF` or ALL `0x00` for the entire
    /// sample — the slot is electrically unpopulated. Skip silently;
    /// not a refusal (a 2-board unit with chain 3 empty is normal).
    Unpopulated { chain_id: u8 },
    /// EEPROM read did not complete within the deadline. **Refuse**:
    /// an AM2 chain whose EEPROM doesn't surface in time is in an
    /// unknown state.
    Timeout { chain_id: u8 },
    /// Underlying I²C / sysfs read itself errored (file missing, EIO,
    /// etc.). On AM2/am3 the EEPROM node is provided by the kernel
    /// AT24 driver and a missing node means the slot doesn't have a
    /// board at all, OR the driver isn't bound. We treat this
    /// **identically to `Unpopulated`** — silently skip — UNLESS the
    /// caller knows from independent evidence that a board IS present
    /// (e.g. the dsPIC already replied to GET_VERSION), in which case
    /// the caller should map the read error to `Timeout` before
    /// passing it here.
    ReadError { chain_id: u8 },
}

impl ChainProbe {
    /// Chain slot index (0..3).
    pub fn chain_id(&self) -> u8 {
        match self {
            ChainProbe::Classified { chain_id, .. }
            | ChainProbe::MalformedPreamble { chain_id, .. }
            | ChainProbe::Unpopulated { chain_id }
            | ChainProbe::Timeout { chain_id }
            | ChainProbe::ReadError { chain_id } => *chain_id,
        }
    }
}

/// A single populated chain's resolved SKU after the gate accepts it.
/// Mining paths can plug this into `pvt_envelope::validate_freq_volt`
/// before each `set_voltage` call to enforce the per-SKU PVT envelope
/// at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkuBinding {
    pub chain_id: u8,
    pub preamble: [u8; 2],
    pub sku: Hashboard,
}

/// Per-chain reason a refusal was raised. Mining-path logging surfaces
/// this so the operator can see WHICH chain blocked WHICH way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    MalformedPreamble {
        chain_id: u8,
        preamble: [u8; 2],
    },
    EepromReadinessTimeout {
        chain_id: u8,
    },
    /// Mixed-SKU: chain `chain_id` reports `local` while another chain
    /// reports `other`. Both chains carry valid preambles individually;
    /// the platform-wide invariant ("all chains share one SKU family")
    /// is what fails.
    MixedSkuChain {
        chain_id: u8,
        local: Hashboard,
        other_chain_id: u8,
        other: Hashboard,
    },
    /// `classify_by_eeprom_preamble` returned `None` for an otherwise-
    /// readable preamble. Currently shaped identically to
    /// `MalformedPreamble`; kept as a distinct variant so future
    /// preamble routing changes can specialize the operator message.
    ProfileBindFailure {
        chain_id: u8,
        preamble: [u8; 2],
    },
}

impl RefusalReason {
    /// Short tag for the `[ENERGIZE-REFUSED] reason=…` log line.
    pub fn tag(&self) -> &'static str {
        match self {
            RefusalReason::MalformedPreamble { .. } => "malformed-preamble",
            RefusalReason::EepromReadinessTimeout { .. } => "eeprom-readiness-timeout",
            RefusalReason::MixedSkuChain { .. } => "mixed-sku",
            RefusalReason::ProfileBindFailure { .. } => "profile-bind-fail",
        }
    }

    /// Chain that triggered the refusal (for log indexing).
    pub fn chain_id(&self) -> u8 {
        match self {
            RefusalReason::MalformedPreamble { chain_id, .. }
            | RefusalReason::EepromReadinessTimeout { chain_id }
            | RefusalReason::MixedSkuChain { chain_id, .. }
            | RefusalReason::ProfileBindFailure { chain_id, .. } => *chain_id,
        }
    }
}

/// Platform-wide energize refusal. Carries every chain-level reason
/// that contributed so the operator gets a complete picture in one log
/// line instead of a fault-cascade story.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnergizeRefusal {
    pub reasons: Vec<RefusalReason>,
}

impl EnergizeRefusal {
    pub fn new(reasons: Vec<RefusalReason>) -> Self {
        Self { reasons }
    }

    pub fn is_empty(&self) -> bool {
        self.reasons.is_empty()
    }

    /// Concatenated `[ENERGIZE-REFUSED]`-style summary, suitable for a
    /// single `tracing::error!` line. The mining-path call site
    /// remains responsible for the `info!`/`error!` macro itself so
    /// the tracing span context is preserved.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for (i, r) in self.reasons.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            match r {
                RefusalReason::MalformedPreamble { chain_id, preamble } => {
                    out.push_str(&format!(
                        "chain={} reason=malformed-preamble evidence=[0x{:02X} 0x{:02X}]",
                        chain_id, preamble[0], preamble[1]
                    ));
                }
                RefusalReason::EepromReadinessTimeout { chain_id } => {
                    out.push_str(&format!(
                        "chain={} reason=eeprom-readiness-timeout",
                        chain_id
                    ));
                }
                RefusalReason::MixedSkuChain {
                    chain_id,
                    local,
                    other_chain_id,
                    other,
                } => {
                    out.push_str(&format!(
                        "chain={} reason=mixed-sku local={} other_chain={} other={}",
                        chain_id,
                        local.sku(),
                        other_chain_id,
                        other.sku(),
                    ));
                }
                RefusalReason::ProfileBindFailure { chain_id, preamble } => {
                    out.push_str(&format!(
                        "chain={} reason=profile-bind-fail evidence=[0x{:02X} 0x{:02X}]",
                        chain_id, preamble[0], preamble[1]
                    ));
                }
            }
        }
        out
    }
}

impl std::fmt::Display for EnergizeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

impl std::error::Error for EnergizeRefusal {}

/// Classify a single chain's EEPROM byte payload into a `ChainProbe`.
///
/// `eeprom_bytes`:
/// - `None` → `ReadError { chain_id }`. Caller is responsible for
///   distinguishing "no board, no read attempted" from "read attempted
///   and timed out" — pass `Some(empty_slice)` and a deadline-timeout
///   bit if you need to surface `Timeout`.
/// - `Some(bytes)` where `bytes.len() < 2` → `ReadError` (not enough
///   bytes to inspect the preamble).
/// - `Some(bytes)` all-`0x00` or all-`0xFF` → `Unpopulated` (the AT24
///   `manufactured-empty` pattern; also seen on a slot with no
///   physical board).
/// - `Some(bytes)` with a known preamble → `Classified`.
/// - `Some(bytes)` with an unknown preamble → `MalformedPreamble`.
pub fn classify_chain(chain_id: u8, eeprom_bytes: Option<&[u8]>) -> ChainProbe {
    let Some(data) = eeprom_bytes else {
        return ChainProbe::ReadError { chain_id };
    };
    if data.is_empty() {
        // Read returned nothing — an absent / missing node; skip, don't refuse.
        return ChainProbe::ReadError { chain_id };
    }
    if data.iter().all(|&b| b == 0x00) || data.iter().all(|&b| b == 0xff) {
        return ChainProbe::Unpopulated { chain_id };
    }
    if data.len() < 2 {
        // Present but TRUNCATED: a single non-blank byte is too short to carry a
        // 2-byte preamble. This is the .74-corruption class — an electrically
        // present EEPROM returning a garbled header — and MUST be refuse-eligible,
        // exactly like a >=2-byte unknown preamble (both are MalformedPreamble),
        // NOT silently skipped as ReadError. (None / empty stay ReadError: a
        // genuinely absent read. The second preamble byte is marked 0x00 = absent.)
        return ChainProbe::MalformedPreamble {
            chain_id,
            preamble: [data[0], 0x00],
        };
    }
    let preamble = [data[0], data[1]];
    match classify_by_eeprom_preamble(preamble) {
        Some(sku) => ChainProbe::Classified {
            chain_id,
            preamble,
            sku,
        },
        None => ChainProbe::MalformedPreamble { chain_id, preamble },
    }
}

/// Platform-wide gate: fold per-chain probe results into a single
/// `Ok(bindings)` (safe to energize) or `Err(EnergizeRefusal)`.
///
/// **Strictness** is controlled by `strict`:
/// - `true` — return `Err` on any refusal-class probe result. Use when
///   the operator has opted in to fail-closed mode
///   (`DCENT_AM2_STRICT_SKU_REFUSE=1`) AND has NOT set the lab override
///   `DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1`.
/// - `false` — return `Ok(bindings)` with whatever chains classified
///   cleanly, while still SURFACING the refusal reasons via the
///   `_telemetry_reasons` return slot. This is the **first-deploy
///   telemetry-only mode** the rollout plan calls for.
///
/// Even in `strict=false` mode the caller MUST log the surfaced
/// refusal reasons (the `EnergizeRefusal::summary()` line) so the
/// operator can see what WOULD have been refused.
pub fn gate_chains_for_energize(
    probes: &[ChainProbe],
    strict: bool,
) -> Result<(Vec<SkuBinding>, EnergizeRefusal), EnergizeRefusal> {
    // Thin wrapper preserving the original behavior: an EEPROM-readiness
    // timeout IS a refuse-eligible reason. am2/serial callers + all
    // existing tests use this form unchanged.
    gate_chains_for_energize_with_opts(probes, strict, false)
}

/// Like [`gate_chains_for_energize`], but with `timeout_is_skip` controlling
/// how an `eeprom-readiness-timeout` is treated.
///
/// **`timeout_is_skip = false` (default / am2 / serial):** a timeout is a
/// refuse-eligible reason (fail-closed — couldn't establish identity).
///
/// **`timeout_is_skip = true` (am3-bb):** a timeout is treated like an
/// unpopulated/read-error chain — silently skipped, NOT a refusal.
///
/// Rationale (live evidence, `a lab unit` 2026-05-22, RESULTS-5a-79.md): on the
/// AM3-BB S19J_IO_BOARD_V2_0 the hashboard EEPROM (bus 0 @ 0x50-0x52) is
/// **unpowered until the chain rail is enabled** — the same bus-0 dsPICs
/// (0x20-0x22) only answered AFTER rail-enable, and the energize gate runs
/// *before* energize by design. So a pre-energize EEPROM read on am3-bb
/// ALWAYS times out; that timeout tells us nothing about board health.
/// Treating it as refuse-eligible would FALSE-REFUSE every healthy am3-bb
/// chain under strict mode. am3-bb board-identity protection therefore
/// comes from the other gates (plug-detect GPIO, dsPIC fw=0x86 refusal,
/// chain-enum liveness), not the pre-energize EEPROM read. (A future
/// enhancement could re-read the EEPROM AFTER rail-enable for full am3-bb
/// identity coverage; that is a larger reorder, tracked separately.)
///
/// This only affects STRICT mode (`DCENT_AM2_STRICT_SKU_REFUSE=1`); the
/// default-OFF telemetry-only path is byte-identical either way.
pub fn gate_chains_for_energize_with_opts(
    probes: &[ChainProbe],
    strict: bool,
    timeout_is_skip: bool,
) -> Result<(Vec<SkuBinding>, EnergizeRefusal), EnergizeRefusal> {
    let mut bindings: Vec<SkuBinding> = Vec::new();
    let mut reasons: Vec<RefusalReason> = Vec::new();

    // First pass: per-chain shape (preamble / timeout / unpopulated).
    for probe in probes {
        match probe {
            ChainProbe::Classified {
                chain_id,
                preamble,
                sku,
            } => {
                bindings.push(SkuBinding {
                    chain_id: *chain_id,
                    preamble: *preamble,
                    sku: *sku,
                });
            }
            ChainProbe::MalformedPreamble { chain_id, preamble } => {
                reasons.push(RefusalReason::MalformedPreamble {
                    chain_id: *chain_id,
                    preamble: *preamble,
                });
            }
            ChainProbe::Timeout { chain_id } => {
                if !timeout_is_skip {
                    reasons.push(RefusalReason::EepromReadinessTimeout {
                        chain_id: *chain_id,
                    });
                }
                // timeout_is_skip == true (am3-bb): EEPROM unpowered
                // pre-energize → can't classify → skip, not refuse.
            }
            ChainProbe::Unpopulated { .. } | ChainProbe::ReadError { .. } => {
                // Silently skip. Unpopulated slots / missing-sysfs-node
                // are not refusals on their own; the platform may be
                // a 2-board unit, and a board that's actually present
                // will be caught by the dsPIC-side gates downstream.
            }
        }
    }

    // Second pass: mixed-SKU detection across the populated chains. We
    // compare each pair (deterministic, O(n²) over n≤4 chains).
    //
    // "Same family" rule: BHB42xxx variants all share the `0x04 0x11`
    // preamble. We use the preamble bytes — NOT the refined SKU
    // variant — to decide cross-family mismatch. This matches the
    // mixed-SKU semantics in the spec ("chain 0 reads BHB42601, chain 1
    // reads BHB56902 — refuse all"). A BHB42601 + BHB42801 pairing on
    // the same unit is ALLOWED (same family, same PVT clamp logic).
    for i in 0..bindings.len() {
        for j in (i + 1)..bindings.len() {
            let a = &bindings[i];
            let b = &bindings[j];
            if a.preamble != b.preamble {
                // Cross-family mismatch — refuse BOTH chains (both are
                // wrong relative to each other; we can't pick one to
                // believe). We push reasons for both so the operator
                // sees the full evidence.
                reasons.push(RefusalReason::MixedSkuChain {
                    chain_id: a.chain_id,
                    local: a.sku,
                    other_chain_id: b.chain_id,
                    other: b.sku,
                });
                reasons.push(RefusalReason::MixedSkuChain {
                    chain_id: b.chain_id,
                    local: b.sku,
                    other_chain_id: a.chain_id,
                    other: a.sku,
                });
            }
        }
    }

    let refusal = EnergizeRefusal::new(reasons);
    if strict && !refusal.is_empty() {
        return Err(refusal);
    }
    Ok((bindings, refusal))
}

/// Helper: is `DCENT_AM2_STRICT_SKU_REFUSE=1` set?
///
/// Default OFF for first-deploy rollout. Operator flips this on AFTER
/// `a lab unit` confirms no false-positive refusal is logged.
pub fn strict_sku_refuse_enabled() -> bool {
    std::env::var("DCENT_AM2_STRICT_SKU_REFUSE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Helper: is `DCENT_AM2_ACCEPT_DEGRADED_HARDWARE=1` set? Lab override
/// that lets strict-mode proceed anyway. Mirrors the toolbox flag.
pub fn accept_degraded_hardware_enabled() -> bool {
    std::env::var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- fail-closed energization property tests ----
    // EEPROM is provably corruptible (`a lab unit`'s 0x51 is untrusted; the `a lab unit`
    // incident corrupted an EEPROM), so board energization must never be
    // authorized by a garbage or wiped EEPROM. These properties pin the
    // fail-closed structure of `classify_chain` + `gate_chains_for_energize`
    // against arbitrary bytes.
    mod fail_closed_properties {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            // A chain is only ever `Classified` (energization-eligible) when its
            // preamble is an EXACT known family preamble — never for arbitrary bytes.
            #[test]
            fn classified_only_on_known_preamble(
                data in proptest::collection::vec(any::<u8>(), 0usize..40),
            ) {
                if let ChainProbe::Classified { preamble, .. } = classify_chain(0, Some(&data)) {
                    prop_assert!(
                        classify_by_eeprom_preamble(preamble).is_some(),
                        "classify_chain accepted preamble {preamble:?} the classifier rejects"
                    );
                    prop_assert!(!data.iter().all(|&b| b == 0x00) && !data.iter().all(|&b| b == 0xff));
                }
            }

            // A wiped EEPROM (all 0x00 or all 0xFF) is `Unpopulated`, never Classified.
            #[test]
            fn wiped_eeprom_is_unpopulated_never_classified(
                len in 2usize..40,
                fill in prop_oneof![Just(0x00u8), Just(0xffu8)],
            ) {
                let data = vec![fill; len];
                let is_unpopulated =
                    matches!(classify_chain(7, Some(&data)), ChainProbe::Unpopulated { chain_id: 7 });
                prop_assert!(is_unpopulated, "wiped EEPROM (len {len}) must be Unpopulated");
            }

            // Fail-closed: a garbage/unknown preamble (the `a lab unit` corruption class)
            // can NEVER energize the rail in strict mode.
            #[test]
            fn strict_gate_refuses_malformed_preamble(
                data in proptest::collection::vec(any::<u8>(), 2usize..40),
            ) {
                let probe = classify_chain(0, Some(&data));
                if matches!(probe, ChainProbe::MalformedPreamble { .. }) {
                    prop_assert!(gate_chains_for_energize(&[probe], true).is_err());
                }
            }
        }

        // Positive: an exact known preamble classifies AND a single such chain is
        // energize-safe in strict mode.
        #[test]
        fn known_preamble_classifies_and_energizes() {
            for preamble in [[0x04u8, 0x11u8], [0x05u8, 0x11u8]] {
                let mut data = vec![0u8; 32];
                data[0] = preamble[0];
                data[1] = preamble[1];
                data[2] = 0xAB; // ensure not a wiped-fill pattern
                let probe = classify_chain(0, Some(&data));
                assert!(
                    matches!(probe, ChainProbe::Classified { .. }),
                    "known preamble {preamble:?} must classify"
                );
                assert!(
                    gate_chains_for_energize(&[probe], true).is_ok(),
                    "a single classified chain is energize-safe"
                );
            }
        }
    }

    // ---- classify_chain ----

    #[test]
    fn classify_chain_none_is_read_error() {
        assert!(matches!(
            classify_chain(0, None),
            ChainProbe::ReadError { chain_id: 0 }
        ));
    }

    #[test]
    fn classify_chain_truncated_one_byte_is_malformed_not_skipped() {
        // D1: a present-but-truncated 1-byte read (electrically present EEPROM with
        // a garbled header — the .74-corruption class) must be REFUSE-eligible
        // (MalformedPreamble), not silently skipped like an absent node. This
        // inverts the prior `short_buffer_is_read_error` test, which pinned the
        // fail-open bug (a 1-byte garble was skipped while a 2-byte garble refused).
        assert!(matches!(
            classify_chain(1, Some(&[0x04])),
            ChainProbe::MalformedPreamble { chain_id: 1, .. }
        ));
        // The strict gate must refuse it — including am3-bb's timeout_is_skip mode
        // (timeout_is_skip relaxes only Timeout, never a malformed/garbled header).
        let probe = classify_chain(1, Some(&[0xDE]));
        assert!(gate_chains_for_energize(&[probe.clone()], true).is_err());
        assert!(gate_chains_for_energize_with_opts(&[probe], true, true).is_err());
        // None / empty read stay ReadError (a genuinely absent node → skip), and a
        // 1-byte blank stays Unpopulated — C6-pinned behaviors untouched.
        assert!(matches!(
            classify_chain(1, Some(&[])),
            ChainProbe::ReadError { chain_id: 1 }
        ));
        assert!(matches!(classify_chain(1, None), ChainProbe::ReadError { .. }));
        assert!(matches!(
            classify_chain(1, Some(&[0x00])),
            ChainProbe::Unpopulated { chain_id: 1 }
        ));
    }

    #[test]
    fn classify_chain_all_ff_is_unpopulated() {
        let data = [0xff; 32];
        assert!(matches!(
            classify_chain(2, Some(&data)),
            ChainProbe::Unpopulated { chain_id: 2 }
        ));
    }

    #[test]
    fn classify_chain_all_zeros_is_unpopulated() {
        let data = [0x00; 32];
        assert!(matches!(
            classify_chain(2, Some(&data)),
            ChainProbe::Unpopulated { chain_id: 2 }
        ));
    }

    #[test]
    fn classify_chain_bhb42xxx_preamble_classifies_to_bhb42601() {
        // .109's canonical BHB42601 chain class.
        let data = [0x04, 0x11, 0xaa, 0xbb];
        match classify_chain(0, Some(&data)) {
            ChainProbe::Classified {
                chain_id,
                preamble,
                sku,
            } => {
                assert_eq!(chain_id, 0);
                assert_eq!(preamble, [0x04, 0x11]);
                assert_eq!(sku, Hashboard::Bhb42601);
            }
            other => panic!("expected Classified, got {:?}", other),
        }
    }

    #[test]
    fn beta_bhb42601_energize_classifies_from_preamble_without_eeprom_body_decryption() {
        // LOAD-BEARING disposition pin (2026-07-02 production-readiness pass,
        // RE-ASK-01 / dossier §7 B3): the beta-tier drive path
        // (am2-s19jpro-zynq = BHB42601) MUST reach an energize decision from
        // the 2-byte EEPROM preamble + visible ASCII SKU alone. The XXTEA
        // body KDF is NOT required for the beta production claim — it only
        // unlocks the encrypted VF/sensor body (a tuning enhancement, and one
        // that matters more for the non-beta S21 `0x05 0x11` EDF-v5 family).
        //
        // This test simulates a REAL encrypted BHB42601 EEPROM: canonical
        // `0x04 0x11` preamble followed by a high-entropy (undecryptable-here)
        // ciphertext body. The gate must still classify + accept for energize
        // WITHOUT ever attempting to decrypt the body. If a future refactor
        // wires an EEPROM-body decode into the runtime drive path, this test
        // and the "why" comment force that decision to be deliberate.
        let mut data = vec![0x04u8, 0x11];
        // Pseudo-ciphertext: varied bytes so it is not the all-0x00/all-0xFF
        // "Unpopulated" pattern, and not ASCII-parseable as a decrypted body.
        for i in 0..254u32 {
            data.push(((i.wrapping_mul(167).wrapping_add(29)) & 0xFF) as u8);
        }
        assert_eq!(data.len(), 256, "canonical 256-byte EEPROM page");

        // 1. Preamble classification succeeds with an encrypted body.
        let probe = classify_chain(0, Some(&data));
        match probe.clone() {
            ChainProbe::Classified { sku, preamble, .. } => {
                assert_eq!(sku, Hashboard::Bhb42601);
                assert_eq!(preamble, [0x04, 0x11]);
            }
            other => panic!("expected Classified from encrypted BHB42601 body, got {other:?}"),
        }

        // 2. The full fail-closed energize gate (strict mode) ACCEPTS the
        //    encrypted-body chain — no KDF, no body decode, no refusal.
        let (bindings, refusal) = gate_chains_for_energize(&[probe], true)
            .expect("beta BHB42601 with an encrypted body must be energize-eligible");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].sku, Hashboard::Bhb42601);
        assert!(
            refusal.is_empty(),
            "an encrypted (undecrypted) BHB42601 body must not trigger a refusal: {}",
            refusal.summary()
        );
    }

    #[test]
    fn classify_chain_bhb56902_preamble_classifies_to_bhb56902() {
        // S19k Pro NoPic — distinct family preamble.
        let data = [0x05, 0x11, 0xcc, 0xdd];
        match classify_chain(0, Some(&data)) {
            ChainProbe::Classified { sku, .. } => {
                assert_eq!(sku, Hashboard::Bhb56902);
            }
            other => panic!("expected Classified, got {:?}", other),
        }
    }

    #[test]
    fn classify_chain_unknown_preamble_is_malformed() {
        // Untrusted preamble — not a known family marker.
        let data = [0xde, 0xad, 0xbe, 0xef];
        match classify_chain(0, Some(&data)) {
            ChainProbe::MalformedPreamble { preamble, .. } => {
                assert_eq!(preamble, [0xde, 0xad]);
            }
            other => panic!("expected MalformedPreamble, got {:?}", other),
        }
    }

    // ---- gate_chains_for_energize ----

    fn classified(chain: u8, sku: Hashboard, preamble: [u8; 2]) -> ChainProbe {
        ChainProbe::Classified {
            chain_id: chain,
            preamble,
            sku,
        }
    }

    #[test]
    fn gate_accepts_three_known_good_bhb42601_chains() {
        // .109's canonical 3-chain shape.
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            classified(1, Hashboard::Bhb42601, [0x04, 0x11]),
            classified(2, Hashboard::Bhb42601, [0x04, 0x11]),
        ];
        let (bindings, telemetry) =
            gate_chains_for_energize(&probes, false).expect("strict=false never errors");
        assert_eq!(bindings.len(), 3);
        assert!(telemetry.is_empty(), "expected no refusal reasons");
        for b in &bindings {
            assert_eq!(b.sku, Hashboard::Bhb42601);
            assert_eq!(b.preamble, [0x04, 0x11]);
        }
    }

    #[test]
    fn gate_strict_refuses_malformed_preamble() {
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            ChainProbe::MalformedPreamble {
                chain_id: 1,
                preamble: [0xde, 0xad],
            },
        ];
        let refusal = gate_chains_for_energize(&probes, true)
            .expect_err("strict mode must error on malformed preamble");
        assert_eq!(refusal.reasons.len(), 1);
        assert!(matches!(
            refusal.reasons[0],
            RefusalReason::MalformedPreamble {
                chain_id: 1,
                preamble: [0xde, 0xad]
            }
        ));
        let s = refusal.summary();
        assert!(s.contains("malformed-preamble"));
        assert!(s.contains("0xDE"));
    }

    #[test]
    fn gate_strict_refuses_timeout() {
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            ChainProbe::Timeout { chain_id: 2 },
        ];
        let refusal =
            gate_chains_for_energize(&probes, true).expect_err("strict mode must error on timeout");
        assert!(matches!(
            refusal.reasons[0],
            RefusalReason::EepromReadinessTimeout { chain_id: 2 }
        ));
        assert!(refusal.summary().contains("eeprom-readiness-timeout"));
    }

    #[test]
    fn gate_am3bb_timeout_is_skip_does_not_refuse() {
        // am3-bb (timeout_is_skip=true): the EEPROM is unpowered
        // pre-energize, so a timeout must NOT refuse even in strict mode —
        // it's "couldn't classify", not "bad board" (live finding .79
        // 2026-05-22). All 3 chains timing out → ACCEPTED, no refusal.
        let probes = vec![
            ChainProbe::Timeout { chain_id: 0 },
            ChainProbe::Timeout { chain_id: 1 },
            ChainProbe::Timeout { chain_id: 2 },
        ];
        let (bindings, refusal) = gate_chains_for_energize_with_opts(&probes, true, true)
            .expect("am3-bb timeout_is_skip must not refuse in strict mode");
        assert!(bindings.is_empty(), "no EEPROM read → no bindings");
        assert!(
            refusal.is_empty(),
            "timeout_is_skip=true must produce zero refusal reasons, got: {}",
            refusal.summary()
        );
    }

    #[test]
    fn gate_am3bb_timeout_skip_still_refuses_real_malformed_preamble() {
        // timeout_is_skip only relaxes TIMEOUTS. A genuinely malformed
        // preamble (an actually-read untrusted board) must STILL refuse in
        // strict mode even on am3-bb — the relaxation must not weaken the
        // real bad-board protection.
        let probes = vec![
            ChainProbe::Timeout { chain_id: 0 },
            ChainProbe::MalformedPreamble {
                chain_id: 1,
                preamble: [0xDE, 0xAD],
            },
        ];
        let refusal = gate_chains_for_energize_with_opts(&probes, true, true)
            .expect_err("a real malformed preamble must still refuse");
        assert!(refusal
            .reasons
            .iter()
            .any(|r| matches!(r, RefusalReason::MalformedPreamble { chain_id: 1, .. })));
        assert!(
            !refusal
                .reasons
                .iter()
                .any(|r| matches!(r, RefusalReason::EepromReadinessTimeout { .. })),
            "the chain-0 timeout must be skipped, not refused"
        );
    }

    #[test]
    fn gate_strict_refuses_mixed_sku_chains() {
        // BHB42xxx + BHB56902 on the same platform — cross-family
        // mismatch. Both chains must appear in the refusal.
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            classified(1, Hashboard::Bhb56902, [0x05, 0x11]),
        ];
        let refusal = gate_chains_for_energize(&probes, true)
            .expect_err("strict mode must error on mixed-SKU");
        assert_eq!(refusal.reasons.len(), 2);
        let s = refusal.summary();
        assert!(s.contains("mixed-sku"));
        assert!(s.contains("BHB42601"));
        assert!(s.contains("BHB56902"));
    }

    #[test]
    fn gate_accepts_same_family_aliases() {
        // BHB42601 + BHB42801 on different chains: same family
        // preamble `0x04 0x11`, no refusal. (Real-world high-bin /
        // standard mix on a hand-rebuilt unit.)
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            classified(1, Hashboard::Bhb42801, [0x04, 0x11]),
        ];
        let (bindings, telemetry) =
            gate_chains_for_energize(&probes, true).expect("same-family alias must not be refused");
        assert_eq!(bindings.len(), 2);
        assert!(telemetry.is_empty());
    }

    #[test]
    fn gate_telemetry_mode_returns_ok_with_reasons() {
        // First-deploy mode: strict=false. Reasons surface but the
        // caller proceeds.
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            ChainProbe::MalformedPreamble {
                chain_id: 1,
                preamble: [0xde, 0xad],
            },
        ];
        let (bindings, telemetry) =
            gate_chains_for_energize(&probes, false).expect("strict=false never errors");
        // Chain 0 still binds; chain 1's refusal surfaces in
        // telemetry.
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].chain_id, 0);
        assert_eq!(telemetry.reasons.len(), 1);
    }

    #[test]
    fn gate_unpopulated_chain_is_silently_skipped() {
        // 2-board unit with chain 2 empty — must NOT refuse.
        let probes = vec![
            classified(0, Hashboard::Bhb42601, [0x04, 0x11]),
            classified(1, Hashboard::Bhb42601, [0x04, 0x11]),
            ChainProbe::Unpopulated { chain_id: 2 },
        ];
        let (bindings, telemetry) =
            gate_chains_for_energize(&probes, true).expect("unpopulated is not a refusal");
        assert_eq!(bindings.len(), 2);
        assert!(telemetry.is_empty());
    }

    #[test]
    fn gate_zero_chains_is_not_a_refusal() {
        // Belt-and-suspenders: an empty probe list is silently OK
        // (the caller's higher-level "no chains found" path handles
        // that — this gate is about preventing energize of a
        // misclassified chain, not about presence).
        let (bindings, telemetry) =
            gate_chains_for_energize(&[], true).expect("empty input is benign");
        assert!(bindings.is_empty());
        assert!(telemetry.is_empty());
    }

    // ---- env helpers ----

    #[test]
    fn env_strict_refuse_default_off() {
        // The first-deploy rollout requires this to default OFF.
        // Other tests in this crate may set the variable; use
        // std::env::remove_var to be deterministic.
        std::env::remove_var("DCENT_AM2_STRICT_SKU_REFUSE");
        assert!(!strict_sku_refuse_enabled());
    }

    #[test]
    fn env_strict_refuse_recognizes_1() {
        std::env::set_var("DCENT_AM2_STRICT_SKU_REFUSE", "1");
        assert!(strict_sku_refuse_enabled());
        std::env::remove_var("DCENT_AM2_STRICT_SKU_REFUSE");
    }

    #[test]
    fn env_accept_degraded_default_off() {
        std::env::remove_var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE");
        assert!(!accept_degraded_hardware_enabled());
    }

    #[test]
    fn env_accept_degraded_recognizes_1() {
        std::env::set_var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE", "1");
        assert!(accept_degraded_hardware_enabled());
        std::env::remove_var("DCENT_AM2_ACCEPT_DEGRADED_HARDWARE");
    }
}
