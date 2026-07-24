//! Per-chain logical work-ID ownership.
//!
//! ASIC nonce carriers echo a chain-local logical work ID. Keeping one global
//! table allows traffic from one hash chain to overwrite another chain's ID
//! namespace and incorrectly correlate a nonce with unrelated work. This
//! ledger makes that ownership explicit while retaining a globally unique
//! dispatch serial for deduplication and diagnostics.

use std::time::{Duration, Instant};

pub(crate) type DispatchSerial = u64;

#[derive(Debug)]
pub(crate) struct WorkReservation {
    ledger_key: usize,
    work_id: u16,
    dispatch_serial: DispatchSerial,
    dispatched_at: Instant,
}

impl WorkReservation {
    pub(crate) fn work_id(&self) -> u16 {
        self.work_id
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LedgerRecord<T> {
    pub(crate) payload: T,
    pub(crate) dispatch_serial: DispatchSerial,
    pub(crate) dispatched_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LedgerConfigError {
    ZeroCapacity,
    NonPowerOfTwo(usize),
    ExceedsU16Domain(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LedgerCommitError {
    ForeignReservation { expected: usize, observed: usize },
    OutOfRange { work_id: u16, capacity: usize },
}

#[derive(Debug, Clone)]
pub(crate) enum LedgerLookup<T> {
    Found(LedgerRecord<T>),
    Empty,
    OutOfRange { work_id: u16, capacity: usize },
}

pub(crate) struct ChainWorkLedger<T> {
    ledger_key: usize,
    slots: Box<[Option<LedgerRecord<T>>]>,
    next_work_id: u16,
    work_id_mask: u16,
}

impl<T> ChainWorkLedger<T> {
    pub(crate) fn new(capacity: usize, ledger_key: usize) -> Result<Self, LedgerConfigError> {
        if capacity == 0 {
            return Err(LedgerConfigError::ZeroCapacity);
        }
        if !capacity.is_power_of_two() {
            return Err(LedgerConfigError::NonPowerOfTwo(capacity));
        }
        if capacity > (u16::MAX as usize) + 1 {
            return Err(LedgerConfigError::ExceedsU16Domain(capacity));
        }

        let slots = std::iter::repeat_with(|| None)
            .take(capacity)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ok(Self {
            ledger_key,
            slots,
            next_work_id: 0,
            work_id_mask: (capacity - 1) as u16,
        })
    }

    /// Reserve the next reusable logical ID without changing its current
    /// record. The caller must synchronously send the work and then commit on
    /// success; no `.await` or second reservation may intervene.
    pub(crate) fn reserve(
        &mut self,
        dispatch_serial: DispatchSerial,
        now: Instant,
        minimum_reuse_age: Duration,
    ) -> Option<WorkReservation> {
        for _ in 0..self.slots.len() {
            let candidate = self.next_work_id & self.work_id_mask;
            self.next_work_id = candidate.wrapping_add(1) & self.work_id_mask;

            let recently_dispatched =
                self.slots[candidate as usize]
                    .as_ref()
                    .is_some_and(|record| {
                        now.saturating_duration_since(record.dispatched_at) < minimum_reuse_age
                    });
            if !recently_dispatched {
                return Some(WorkReservation {
                    ledger_key: self.ledger_key,
                    work_id: candidate,
                    dispatch_serial,
                    dispatched_at: now,
                });
            }
        }
        None
    }

    pub(crate) fn commit(
        &mut self,
        reservation: WorkReservation,
        payload: T,
    ) -> Result<(), LedgerCommitError> {
        if reservation.ledger_key != self.ledger_key {
            return Err(LedgerCommitError::ForeignReservation {
                expected: self.ledger_key,
                observed: reservation.ledger_key,
            });
        }
        let Some(slot) = self.slots.get_mut(reservation.work_id as usize) else {
            return Err(LedgerCommitError::OutOfRange {
                work_id: reservation.work_id,
                capacity: self.slots.len(),
            });
        };
        *slot = Some(LedgerRecord {
            payload,
            dispatch_serial: reservation.dispatch_serial,
            dispatched_at: reservation.dispatched_at,
        });
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        self.slots.iter_mut().for_each(|slot| *slot = None);
    }

    pub(crate) fn occupancy(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.slots.len()
    }
}

impl<T: Clone> ChainWorkLedger<T> {
    pub(crate) fn lookup(&self, work_id: u16) -> LedgerLookup<T> {
        match self.slots.get(work_id as usize) {
            Some(Some(record)) => LedgerLookup::Found(record.clone()),
            Some(None) => LedgerLookup::Empty,
            None => LedgerLookup::OutOfRange {
                work_id,
                capacity: self.slots.len(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUARD: Duration = Duration::from_secs(5);

    #[test]
    fn rejects_invalid_capacities() {
        assert!(matches!(
            ChainWorkLedger::<u8>::new(0, 0),
            Err(LedgerConfigError::ZeroCapacity)
        ));
        assert!(matches!(
            ChainWorkLedger::<u8>::new(3, 0),
            Err(LedgerConfigError::NonPowerOfTwo(3))
        ));
        assert!(matches!(
            ChainWorkLedger::<u8>::new(131_072, 0),
            Err(LedgerConfigError::ExceedsU16Domain(131_072))
        ));
        for capacity in [1, 256, 16_384, 65_536] {
            assert!(ChainWorkLedger::<u8>::new(capacity, 0).is_ok());
        }
    }

    #[test]
    fn same_logical_id_resolves_only_in_its_chain() {
        let now = Instant::now();
        let mut left = ChainWorkLedger::new(256, 0).unwrap();
        let mut right = ChainWorkLedger::new(256, 1).unwrap();
        let left_reservation = left.reserve(10, now, GUARD).unwrap();
        let right_reservation = right.reserve(11, now, GUARD).unwrap();
        assert_eq!(left_reservation.work_id(), 0);
        assert_eq!(right_reservation.work_id(), 0);
        left.commit(left_reservation, "left").unwrap();
        right.commit(right_reservation, "right").unwrap();

        match left.lookup(0) {
            LedgerLookup::Found(record) => assert_eq!(record.payload, "left"),
            _ => panic!("left record missing"),
        }
        match right.lookup(0) {
            LedgerLookup::Found(record) => assert_eq!(record.payload, "right"),
            _ => panic!("right record missing"),
        }
    }

    #[test]
    fn three_chains_have_independent_256_slot_domains() {
        let now = Instant::now();
        let mut ledgers = (0..3)
            .map(|chain| ChainWorkLedger::new(256, chain).unwrap())
            .collect::<Vec<ChainWorkLedger<u16>>>();
        let mut serial = 0u64;
        for ledger in &mut ledgers {
            for expected_id in 0..256u16 {
                let reservation = ledger.reserve(serial, now, GUARD).unwrap();
                assert_eq!(reservation.work_id(), expected_id);
                serial += 1;
                ledger.commit(reservation, expected_id).unwrap();
            }
        }
        assert_eq!(serial, 768);
        assert!(ledgers
            .iter_mut()
            .all(|ledger| ledger.reserve(serial, now, GUARD).is_none()));
        for ledger in &mut ledgers {
            let reservation = ledger.reserve(serial, now + GUARD, GUARD).unwrap();
            assert_eq!(reservation.work_id(), 0);
            serial += 1;
        }
    }

    #[test]
    fn failed_send_preserves_previous_record() {
        let now = Instant::now();
        let mut ledger = ChainWorkLedger::new(1, 7).unwrap();
        let first = ledger.reserve(1, now, Duration::ZERO).unwrap();
        ledger.commit(first, "old").unwrap();
        let _dropped_reservation = ledger
            .reserve(2, now + Duration::from_secs(1), Duration::ZERO)
            .unwrap();
        match ledger.lookup(0) {
            LedgerLookup::Found(record) => {
                assert_eq!(record.payload, "old");
                assert_eq!(record.dispatch_serial, 1);
            }
            _ => panic!("previous record changed before commit"),
        }
    }

    #[test]
    fn clear_preserves_cursor_and_lookup_is_checked() {
        let now = Instant::now();
        let mut ledger = ChainWorkLedger::new(4, 0).unwrap();
        let reservation = ledger.reserve(1, now, GUARD).unwrap();
        ledger.commit(reservation, 1u8).unwrap();
        ledger.clear();
        assert_eq!(ledger.occupancy(), 0);
        assert!(matches!(ledger.lookup(0), LedgerLookup::Empty));
        assert!(matches!(
            ledger.lookup(4),
            LedgerLookup::OutOfRange {
                work_id: 4,
                capacity: 4
            }
        ));
        assert_eq!(ledger.reserve(2, now, GUARD).unwrap().work_id(), 1);
    }

    #[test]
    fn foreign_reservation_is_refused() {
        let now = Instant::now();
        let mut left = ChainWorkLedger::<u8>::new(4, 0).unwrap();
        let mut right = ChainWorkLedger::new(4, 1).unwrap();
        let reservation = left.reserve(1, now, GUARD).unwrap();
        assert!(matches!(
            right.commit(reservation, 1u8),
            Err(LedgerCommitError::ForeignReservation {
                expected: 1,
                observed: 0
            })
        ));
    }
}
