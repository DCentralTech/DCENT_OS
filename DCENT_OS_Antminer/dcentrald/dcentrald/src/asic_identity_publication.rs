//! Generation-bound publication of measured ASIC enumeration evidence.
//!
//! The work dispatcher is the first owner that simultaneously holds the
//! dispatcher chip identity and every active mining chain. This module keeps
//! publication behind that boundary and binds it to an immutable composition
//! token. Activating a later composition revokes prior measured evidence before
//! an older dispatcher can publish again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dcentrald_api::{HardwareCompositionToken, HardwareIdentityEvidence, HardwareInfo};
use dcentrald_asic::chain::MeasuredEnumeration;
use dcentrald_asic::drivers::ChipRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExpectedMiningChain {
    pub chain_id: u8,
    pub chip_count: u8,
}

/// Receipt minted only when a chain's GetAddress enumeration succeeds.
///
/// Keeping construction in this module makes provenance explicit at daemon
/// call sites. Non-zero chain fields, model profiles, and passthrough state are
/// deliberately not interchangeable with this receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnumeratedMiningChainReceipt {
    chain_id: u8,
    chip_count: u8,
    chip_id: u16,
}

impl EnumeratedMiningChainReceipt {
    pub(crate) fn from_successful_get_address(chain_id: u8, measured: MeasuredEnumeration) -> Self {
        Self {
            chain_id,
            chip_count: measured.chip_count(),
            chip_id: measured.chip_id(),
        }
    }

    pub(crate) fn chain_id(self) -> u8 {
        self.chain_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnumerationConsensus {
    token: HardwareCompositionToken,
    chip_id: u16,
    chip_label: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum AsicIdentityPublicationError {
    #[error("dispatcher composition has no initialized mining chains")]
    ZeroChains,
    #[error("dispatcher composition repeats chain {0}")]
    DuplicateExpectedChain(u8),
    #[error("dispatcher composition chain {0} has zero enumerated chips")]
    ZeroChipCount(u8),
    #[error("dispatcher generation counter is exhausted")]
    GenerationExhausted,
    #[error("dispatcher ASIC identity 0x{0:04X} is unsupported")]
    UnsupportedChip(u16),
    #[error("enumeration snapshot has {observed} chains but composition expects {expected}")]
    Partial { expected: usize, observed: usize },
    #[error("enumeration snapshot repeats chain {0}")]
    DuplicateObservedChain(u8),
    #[error("enumeration snapshot contains unexpected chain {0}")]
    UnexpectedChain(u8),
    #[error(
        "enumeration snapshot chain {chain_id} chip count {observed} disagrees with composition {expected}"
    )]
    CompositionMismatch {
        chain_id: u8,
        expected: u8,
        observed: u8,
    },
    #[error(
        "enumeration snapshot chain {chain_id} ASIC 0x{observed:04X} disagrees with dispatcher 0x{dispatcher:04X}"
    )]
    Mixed {
        chain_id: u8,
        dispatcher: u16,
        observed: u16,
    },
    #[error("dispatcher composition generation is stale")]
    StaleGeneration,
    #[error("dispatcher identity publication state is unavailable")]
    StateUnavailable,
}

#[derive(Debug, Default)]
struct AuthorityState {
    active: Option<ActiveComposition>,
}

#[derive(Debug)]
struct ActiveComposition {
    token: HardwareCompositionToken,
    hardware_info: Arc<Mutex<HardwareInfo>>,
}

/// Daemon-owned composition generation authority.
///
/// This is process-local and is not a global singleton. A future dispatcher
/// replacement activates a new generation through the same authority, which
/// invalidates every older publication port.
#[derive(Debug, Clone, Default)]
pub(crate) struct DispatcherCompositionAuthority {
    next_generation: Arc<AtomicU64>,
    state: Arc<Mutex<AuthorityState>>,
}

impl DispatcherCompositionAuthority {
    /// Revoke the currently active composition, if any.
    ///
    /// The authority always locks its state before the hardware snapshot. The
    /// publication path and session teardown use the same order, so a lifecycle
    /// transition cannot deadlock with publication inside this module.
    pub(crate) fn invalidate_active(&self) -> Result<(), AsicIdentityPublicationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AsicIdentityPublicationError::StateUnavailable)?;
        let Some(active) = state.active.take() else {
            return Ok(());
        };
        restore_non_measured_identity(&active.hardware_info)
    }

    pub(crate) fn activate(
        &self,
        mut expected: Vec<ExpectedMiningChain>,
        receipts: Vec<EnumeratedMiningChainReceipt>,
        hardware_info: Arc<Mutex<HardwareInfo>>,
    ) -> Result<AsicIdentityPublicationPort, AsicIdentityPublicationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AsicIdentityPublicationError::StateUnavailable)?;
        // Every activation attempt is a composition boundary, including an
        // invalid replacement. Revoke the preceding measured claim before
        // validating the next composition so malformed or empty composition
        // cannot inherit confidence from an older dispatcher.
        if let Some(previous) = state.active.take() {
            restore_non_measured_identity(&previous.hardware_info)?;
            if !Arc::ptr_eq(&previous.hardware_info, &hardware_info) {
                restore_non_measured_identity(&hardware_info)?;
            }
        } else {
            restore_non_measured_identity(&hardware_info)?;
        }

        expected.sort_unstable();
        if expected.is_empty() {
            return Err(AsicIdentityPublicationError::ZeroChains);
        }
        for (index, chain) in expected.iter().enumerate() {
            if chain.chip_count == 0 {
                return Err(AsicIdentityPublicationError::ZeroChipCount(chain.chain_id));
            }
            if index > 0 && expected[index - 1].chain_id == chain.chain_id {
                return Err(AsicIdentityPublicationError::DuplicateExpectedChain(
                    chain.chain_id,
                ));
            }
        }

        let generation = self
            .next_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map_err(|_| AsicIdentityPublicationError::GenerationExhausted)?
            + 1;
        let fingerprint = expected
            .iter()
            .map(|chain| format!("{}:{}", chain.chain_id, chain.chip_count))
            .collect::<Vec<_>>()
            .join(",");
        let token = HardwareCompositionToken::new(generation, format!("chains:{fingerprint}"));

        // The authority lock has remained held throughout the transition, so
        // an old port cannot pass its stale-token check in the middle of it.
        state.active = Some(ActiveComposition {
            token: token.clone(),
            hardware_info: Arc::clone(&hardware_info),
        });

        Ok(AsicIdentityPublicationPort {
            authority: Arc::clone(&self.state),
            token,
            expected,
            receipts,
            hardware_info,
        })
    }
}

fn restore_non_measured_identity(
    hardware_info: &Arc<Mutex<HardwareInfo>>,
) -> Result<(), AsicIdentityPublicationError> {
    let mut hardware = hardware_info
        .lock()
        .map_err(|_| AsicIdentityPublicationError::StateUnavailable)?;
    hardware.identification.clear_measured_asic_evidence();
    hardware.chip_type = hardware
        .identification
        .best_non_measured_asic_resolved_value()
        .unwrap_or("Unknown")
        .to_string();
    Ok(())
}

/// Immutable dispatcher-scoped publication capability.
#[derive(Debug)]
pub(crate) struct AsicIdentityPublicationPort {
    authority: Arc<Mutex<AuthorityState>>,
    token: HardwareCompositionToken,
    expected: Vec<ExpectedMiningChain>,
    receipts: Vec<EnumeratedMiningChainReceipt>,
    hardware_info: Arc<Mutex<HardwareInfo>>,
}

impl AsicIdentityPublicationPort {
    fn evaluate(
        &self,
        dispatcher_chip_id: u16,
    ) -> Result<EnumerationConsensus, AsicIdentityPublicationError> {
        let registry = ChipRegistry::new();
        let driver = registry.detect(dispatcher_chip_id).ok_or(
            AsicIdentityPublicationError::UnsupportedChip(dispatcher_chip_id),
        )?;
        if self.receipts.len() != self.expected.len() {
            return Err(AsicIdentityPublicationError::Partial {
                expected: self.expected.len(),
                observed: self.receipts.len(),
            });
        }
        let mut observations = self.receipts.clone();
        observations.sort_unstable_by_key(|chain| chain.chain_id);
        for (index, observed) in observations.iter().enumerate() {
            if index > 0 && observations[index - 1].chain_id == observed.chain_id {
                return Err(AsicIdentityPublicationError::DuplicateObservedChain(
                    observed.chain_id,
                ));
            }
            let Some(expected) = self
                .expected
                .iter()
                .find(|expected| expected.chain_id == observed.chain_id)
            else {
                return Err(AsicIdentityPublicationError::UnexpectedChain(
                    observed.chain_id,
                ));
            };
            if observed.chip_count != expected.chip_count {
                return Err(AsicIdentityPublicationError::CompositionMismatch {
                    chain_id: observed.chain_id,
                    expected: expected.chip_count,
                    observed: observed.chip_count,
                });
            }
            if observed.chip_id != dispatcher_chip_id {
                return Err(AsicIdentityPublicationError::Mixed {
                    chain_id: observed.chain_id,
                    dispatcher: dispatcher_chip_id,
                    observed: observed.chip_id,
                });
            }
        }

        Ok(EnumerationConsensus {
            token: self.token.clone(),
            chip_id: dispatcher_chip_id,
            chip_label: driver.chip_name().to_string(),
        })
    }

    pub(crate) fn publish(
        self,
        dispatcher_chip_id: u16,
    ) -> Result<ActiveCompositionSession, AsicIdentityPublicationError> {
        let consensus = self.evaluate(dispatcher_chip_id)?;
        let state = self
            .authority
            .lock()
            .map_err(|_| AsicIdentityPublicationError::StateUnavailable)?;
        if state.active.as_ref().map(|active| &active.token) != Some(&consensus.token) {
            return Err(AsicIdentityPublicationError::StaleGeneration);
        }

        let mut hardware = self
            .hardware_info
            .lock()
            .map_err(|_| AsicIdentityPublicationError::StateUnavailable)?;
        hardware.identification.clear_measured_asic_evidence();
        hardware
            .identification
            .push_evidence(HardwareIdentityEvidence::measured_asic_enumeration(
                consensus.chip_id,
                &consensus.chip_label,
                consensus.token,
            ));
        hardware.chip_type = consensus.chip_label;
        drop(hardware);
        drop(state);

        Ok(ActiveCompositionSession {
            authority: Arc::clone(&self.authority),
            token: self.token,
            hardware_info: Arc::clone(&self.hardware_info),
            finished: false,
        })
    }
}

/// Engine-owned lease for one published composition generation.
///
/// The mining engine must retain this value for its whole dispatch lifetime.
/// Explicit revocation is deterministic on normal shutdown; `Drop` is the
/// non-panicking fallback for task cancellation and unwind. A stale lease can
/// never clear a later generation because revocation compares the exact token
/// while holding the authority lock before touching `HardwareInfo`.
#[derive(Debug)]
pub(crate) struct ActiveCompositionSession {
    authority: Arc<Mutex<AuthorityState>>,
    token: HardwareCompositionToken,
    hardware_info: Arc<Mutex<HardwareInfo>>,
    finished: bool,
}

impl ActiveCompositionSession {
    pub(crate) fn revoke(mut self) -> Result<(), AsicIdentityPublicationError> {
        let result = self.revoke_if_current();
        self.finished = true;
        result
    }

    fn revoke_if_current(&mut self) -> Result<(), AsicIdentityPublicationError> {
        let mut state = self
            .authority
            .lock()
            .map_err(|_| AsicIdentityPublicationError::StateUnavailable)?;
        if state.active.as_ref().map(|active| &active.token) != Some(&self.token) {
            return Ok(());
        }
        state.active = None;
        restore_non_measured_identity(&self.hardware_info)
    }
}

impl Drop for ActiveCompositionSession {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.revoke_if_current();
            self.finished = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_api::{HardwareIdentityConfidence, HardwareIdentityEvidenceLevel};

    fn hardware() -> Arc<Mutex<HardwareInfo>> {
        Arc::new(Mutex::new(HardwareInfo::default()))
    }

    fn expected() -> Vec<ExpectedMiningChain> {
        vec![
            ExpectedMiningChain {
                chain_id: 6,
                chip_count: 63,
            },
            ExpectedMiningChain {
                chain_id: 7,
                chip_count: 63,
            },
        ]
    }

    fn exact(chip_id: u16) -> Vec<EnumeratedMiningChainReceipt> {
        vec![test_receipt(6, 63, chip_id), test_receipt(7, 63, chip_id)]
    }

    fn test_receipt(chain_id: u8, chip_count: u8, chip_id: u16) -> EnumeratedMiningChainReceipt {
        EnumeratedMiningChainReceipt {
            chain_id,
            chip_count,
            chip_id,
        }
    }

    #[test]
    fn exact_consensus_publishes_generation_bound_measured_identity() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let port = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap();
        let _session = port.publish(0x1387).unwrap();

        let hardware = hardware.lock().unwrap();
        assert_eq!(hardware.chip_type, "BM1387");
        assert_eq!(
            hardware.identification.confidence,
            HardwareIdentityConfidence::High
        );
        assert_eq!(
            hardware.identification.strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );
        let token = hardware.identification.evidence[0]
            .composition
            .as_ref()
            .unwrap();
        assert_eq!(token.generation, 1);
        assert_eq!(token.fingerprint, "chains:6:63,7:63");
    }

    #[test]
    fn assumed_chain_fields_without_get_address_receipts_never_publish_measured_identity() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let port = authority
            .activate(expected(), Vec::new(), Arc::clone(&hardware))
            .unwrap();

        assert!(matches!(
            port.publish(0x1387),
            Err(AsicIdentityPublicationError::Partial {
                expected: 2,
                observed: 0,
            })
        ));
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            None
        );
        assert_eq!(hardware.lock().unwrap().chip_type, "Unknown");
    }

    #[test]
    fn partial_and_mixed_enumeration_never_publish_measured_identity() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let partial = authority
            .activate(expected(), vec![exact(0x1387)[0]], Arc::clone(&hardware))
            .unwrap();
        assert!(matches!(
            partial.publish(0x1387),
            Err(AsicIdentityPublicationError::Partial {
                expected: 2,
                observed: 1
            })
        ));
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            None
        );
        assert_eq!(hardware.lock().unwrap().chip_type, "Unknown");

        let mut observations = exact(0x1387);
        observations[1].chip_id = 0x1397;
        let mixed = authority
            .activate(expected(), observations, Arc::clone(&hardware))
            .unwrap();
        assert!(matches!(
            mixed.publish(0x1387),
            Err(AsicIdentityPublicationError::Mixed { .. })
        ));
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            None
        );
    }

    #[test]
    fn later_composition_revokes_and_rejects_stale_generation() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let stale = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap();
        let current = authority
            .activate(
                vec![ExpectedMiningChain {
                    chain_id: 8,
                    chip_count: 63,
                }],
                vec![test_receipt(8, 63, 0x1387)],
                Arc::clone(&hardware),
            )
            .unwrap();

        assert!(matches!(
            stale.publish(0x1387),
            Err(AsicIdentityPublicationError::StaleGeneration)
        ));
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            None
        );
        assert_eq!(hardware.lock().unwrap().chip_type, "Unknown");

        let _current_session = current.publish(0x1387).unwrap();
        let generation = hardware.lock().unwrap().identification.evidence[0]
            .composition
            .as_ref()
            .unwrap()
            .generation;
        assert_eq!(generation, 2);
    }

    #[test]
    fn invalid_compositions_revoke_measured_and_restore_declared_identity() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        {
            let mut snapshot = hardware.lock().unwrap();
            snapshot
                .identification
                .push_evidence(HardwareIdentityEvidence::declared_asic_config(
                    "s19jpro", "BM1362",
                ));
            snapshot.chip_type = "BM1362".to_string();
        }
        let _session = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );

        assert!(matches!(
            authority.activate(Vec::new(), Vec::new(), Arc::clone(&hardware)),
            Err(AsicIdentityPublicationError::ZeroChains)
        ));
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Declared)
        );
        assert_eq!(hardware.lock().unwrap().chip_type, "BM1362");
        assert!(matches!(
            authority.activate(
                vec![ExpectedMiningChain {
                    chain_id: 6,
                    chip_count: 0
                }],
                Vec::new(),
                Arc::clone(&hardware)
            ),
            Err(AsicIdentityPublicationError::ZeroChipCount(6))
        ));
        let port = authority
            .activate(expected(), exact(0xFFFF), Arc::clone(&hardware))
            .unwrap();
        assert!(matches!(
            port.publish(0xFFFF),
            Err(AsicIdentityPublicationError::UnsupportedChip(0xFFFF))
        ));
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Declared)
        );
    }

    #[test]
    fn active_session_drop_revokes_measured_and_restores_declared_identity() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        {
            let mut snapshot = hardware.lock().unwrap();
            snapshot
                .identification
                .push_evidence(HardwareIdentityEvidence::declared_asic_config(
                    "s9", "BM1387",
                ));
            snapshot.chip_type = "BM1387".to_string();
        }

        let session = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );

        drop(session);
        let snapshot = hardware.lock().unwrap();
        assert_eq!(
            snapshot.identification.strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Declared)
        );
        assert_eq!(snapshot.chip_type, "BM1387");
    }

    #[test]
    fn stale_session_drop_cannot_revoke_a_newer_generation() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let stale_session = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();
        let current_session = authority
            .activate(
                vec![ExpectedMiningChain {
                    chain_id: 8,
                    chip_count: 63,
                }],
                vec![test_receipt(8, 63, 0x1387)],
                Arc::clone(&hardware),
            )
            .unwrap()
            .publish(0x1387)
            .unwrap();

        drop(stale_session);
        let generation = hardware.lock().unwrap().identification.evidence[0]
            .composition
            .as_ref()
            .unwrap()
            .generation;
        assert_eq!(generation, 2);
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );
        drop(current_session);
    }

    #[test]
    fn authority_invalidation_revokes_now_and_later_session_drop_is_a_noop() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let session = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();

        authority.invalidate_active().unwrap();
        assert_eq!(
            hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            None
        );
        drop(session);
        assert_eq!(hardware.lock().unwrap().chip_type, "Unknown");
    }

    #[test]
    fn replacement_clears_the_previous_publication_target() {
        let first_hardware = hardware();
        let second_hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let stale_session = authority
            .activate(expected(), exact(0x1387), Arc::clone(&first_hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();

        let _replacement = authority
            .activate(expected(), exact(0x1387), Arc::clone(&second_hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();
        assert_eq!(
            first_hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            None
        );
        assert_eq!(
            second_hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );
        drop(stale_session);
        assert_eq!(
            second_hardware
                .lock()
                .unwrap()
                .identification
                .strongest_asic_evidence_level(),
            Some(HardwareIdentityEvidenceLevel::Measured)
        );
    }

    #[test]
    fn session_drop_never_panics_when_authority_state_is_poisoned() {
        let hardware = hardware();
        let authority = DispatcherCompositionAuthority::default();
        let session = authority
            .activate(expected(), exact(0x1387), Arc::clone(&hardware))
            .unwrap()
            .publish(0x1387)
            .unwrap();
        let state = Arc::clone(&session.authority);
        let _ = std::thread::spawn(move || {
            let _guard = state.lock().unwrap();
            panic!("intentional authority poison");
        })
        .join();

        assert!(std::panic::catch_unwind(|| drop(session)).is_ok());
    }
}
