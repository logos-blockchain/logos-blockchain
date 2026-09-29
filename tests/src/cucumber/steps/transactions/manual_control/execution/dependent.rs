use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use lb_core::mantle::ops::channel::{
    ChannelId, MsgId,
    inscribe::{Inscription, InscriptionOp},
};
use lb_key_management_system_service::keys::Ed25519Key;
use rand::{RngCore as _, SeedableRng as _, seq::SliceRandom as _};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;
use tracing::debug;

use crate::{common::wallet::WalletTransactionIntent, cucumber::error::StepError};

const INITIAL_SHUFFLE_SEED: u64 = 42;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InscriptionLineage {
    current_id: MsgId,
    current_counter: u64,
}

impl Default for InscriptionLineage {
    fn default() -> Self {
        Self {
            current_id: MsgId::root(),
            current_counter: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InscriptionLineageEntry {
    parent_id: MsgId,
    current_id: MsgId,
    parent_inscription_counter: u64,
    current_inscription_counter: u64,
    operation: InscriptionOp,
}

#[derive(Clone, Debug, Serialize)]
pub struct DependentBurstDiagnostics {
    pub round: usize,
    pub shuffle_seed: Option<u64>,
    pub transaction_count: usize,
    pub lineage_counter_start: Option<u64>,
    pub lineage_counter_end: Option<u64>,
}

/// Scenario-owned lineage and shuffle progression for one dependent workload.
#[derive(Clone)]
pub struct DependentTransactionLoadState {
    lineage: Arc<Mutex<InscriptionLineage>>,
    next_shuffle_seed: Arc<AtomicU64>,
    channel_id: ChannelId,
    signing_key: Ed25519Key,
    last_burst_diagnostics: Arc<Mutex<Option<DependentBurstDiagnostics>>>,
}

impl DependentTransactionLoadState {
    pub fn new() -> Self {
        let mut channel_id_bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut channel_id_bytes);
        Self {
            lineage: Arc::new(Mutex::new(InscriptionLineage::default())),
            next_shuffle_seed: Arc::new(AtomicU64::new(INITIAL_SHUFFLE_SEED)),
            channel_id: ChannelId::from(channel_id_bytes),
            signing_key: Ed25519Key::from_bytes(&[0x42; 32]),
            last_burst_diagnostics: Arc::new(Mutex::new(None)),
        }
    }

    pub fn prepare_hybrid_intent(
        &self,
        transfer_intent: WalletTransactionIntent,
    ) -> Result<WalletTransactionIntent, StepError> {
        self.prepare_after_allocation(|entry| {
            let intent = transfer_intent
                .with_leading_inscription(entry.operation, self.signing_key.clone())
                .map_err(|error| StepError::LogicalError {
                    message: error.to_string(),
                })?;
            Ok(intent)
        })
    }

    pub fn begin_burst(&self, round: usize, transaction_count: usize) {
        self.set_last_burst_diagnostics(DependentBurstDiagnostics {
            round,
            shuffle_seed: None,
            transaction_count,
            lineage_counter_start: None,
            lineage_counter_end: None,
        });
    }

    fn prepare_after_allocation<T>(
        &self,
        prepare: impl FnOnce(InscriptionLineageEntry) -> Result<T, StepError>,
    ) -> Result<T, StepError> {
        prepare(self.allocate_lineage_entry()?)
    }

    fn allocate_lineage_entry(&self) -> Result<InscriptionLineageEntry, StepError> {
        let mut lineage = self.lineage.lock().map_err(|_| StepError::LogicalError {
            message: "dependent inscription lineage mutex was poisoned".to_owned(),
        })?;
        let current_inscription_counter =
            lineage
                .current_counter
                .checked_add(1)
                .ok_or_else(|| StepError::LogicalError {
                    message: "dependent inscription lineage counter overflowed".to_owned(),
                })?;
        let parent_id = lineage.current_id;
        let parent_inscription_counter = lineage.current_counter;
        let operation = InscriptionOp {
            channel_id: self.channel_id,
            inscription: Inscription::try_from(current_inscription_counter.to_le_bytes().to_vec())
                .map_err(|error| StepError::LogicalError {
                    message: format!("dependent inscription payload is invalid: {error}"),
                })?,
            parent: parent_id,
            signer: self.signing_key.public_key().into_unverified(),
        };
        let current_id = operation.id();
        lineage.current_id = current_id;
        lineage.current_counter = current_inscription_counter;
        self.update_burst_lineage_range(current_inscription_counter);
        drop(lineage);

        let entry = InscriptionLineageEntry {
            parent_id,
            current_id,
            parent_inscription_counter,
            current_inscription_counter,
            operation,
        };
        debug!(
            parent_counter = entry.parent_inscription_counter,
            current_counter = entry.current_inscription_counter,
            parent_inscription_id = %entry.parent_id,
            current_inscription_id = %entry.current_id,
            "Allocated dependent inscription lineage entry"
        );
        Ok(entry)
    }

    fn update_burst_lineage_range(&self, counter: u64) {
        let update = |diagnostics: &mut Option<DependentBurstDiagnostics>| {
            if let Some(diagnostics) = diagnostics {
                diagnostics.lineage_counter_start.get_or_insert(counter);
                diagnostics.lineage_counter_end = Some(counter);
            }
        };
        match self.last_burst_diagnostics.lock() {
            Ok(mut diagnostics) => update(&mut diagnostics),
            Err(poisoned) => update(&mut poisoned.into_inner()),
        }
    }

    #[cfg(test)]
    fn lineage_counter(&self) -> Result<u64, StepError> {
        self.lineage
            .lock()
            .map(|lineage| lineage.current_counter)
            .map_err(|_| StepError::LogicalError {
                message: "dependent inscription lineage mutex was poisoned".to_owned(),
            })
    }

    pub fn shuffle_burst<T>(&self, items: &mut [T]) -> u64 {
        let seed = self.next_shuffle_seed.fetch_add(1, Ordering::Relaxed);
        shuffle_with_seed(items, seed);
        seed
    }

    fn set_last_burst_diagnostics(&self, diagnostics: DependentBurstDiagnostics) {
        match self.last_burst_diagnostics.lock() {
            Ok(mut last) => *last = Some(diagnostics),
            Err(poisoned) => *poisoned.into_inner() = Some(diagnostics),
        }
    }

    pub fn set_last_burst_shuffle_seed(&self, seed: u64) {
        let update = |diagnostics: &mut Option<DependentBurstDiagnostics>| {
            if let Some(diagnostics) = diagnostics {
                diagnostics.shuffle_seed = Some(seed);
            }
        };
        match self.last_burst_diagnostics.lock() {
            Ok(mut diagnostics) => update(&mut diagnostics),
            Err(poisoned) => update(&mut poisoned.into_inner()),
        }
    }

    pub fn clear_last_burst_diagnostics(&self) {
        match self.last_burst_diagnostics.lock() {
            Ok(mut last) => *last = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }

    pub fn last_burst_diagnostics(&self) -> Option<DependentBurstDiagnostics> {
        self.last_burst_diagnostics.lock().map_or_else(
            |poisoned| poisoned.into_inner().clone(),
            |last| last.clone(),
        )
    }
}

fn shuffle_with_seed<T>(items: &mut [T], seed: u64) {
    items.shuffle(&mut ChaCha8Rng::seed_from_u64(seed));
}

pub(super) fn shuffle_if_dependent<T>(
    items: &mut [T],
    dependent_state: Option<&DependentTransactionLoadState>,
) -> Option<u64> {
    dependent_state.map(|state| state.shuffle_burst(items))
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn lineage_starts_at_root_and_allocates_a_linear_sequence() {
        let state = DependentTransactionLoadState::new();
        assert_eq!(state.lineage_counter().expect("lineage lock"), 0);

        let first = state.allocate_lineage_entry().expect("first entry");
        let second = state.allocate_lineage_entry().expect("second entry");

        assert_eq!(first.parent_id, MsgId::root());
        assert_eq!(first.parent_inscription_counter, 0);
        assert_eq!(first.current_inscription_counter, 1);
        assert_eq!(first.operation.parent, MsgId::root());
        assert_eq!(first.operation.inscription.as_slice(), &1u64.to_le_bytes());
        assert_eq!(first.current_id, first.operation.id());
        assert_eq!(second.parent_id, first.current_id);
        assert_eq!(second.parent_inscription_counter, 1);
        assert_eq!(second.current_inscription_counter, 2);
        assert_eq!(second.operation.parent, first.current_id);
        assert_eq!(second.operation.inscription.as_slice(), &2u64.to_le_bytes());
        assert_eq!(second.operation.channel_id, first.operation.channel_id);
        assert_eq!(state.lineage_counter().expect("lineage lock"), 2);
    }

    #[test]
    fn separate_dependent_workloads_use_distinct_channel_ids() {
        let first = DependentTransactionLoadState::new();
        let second = DependentTransactionLoadState::new();

        assert_ne!(first.channel_id, second.channel_id);
    }

    #[test]
    fn concurrent_lineage_allocations_form_one_gap_free_chain() {
        let state = DependentTransactionLoadState::new();
        let mut workers = Vec::new();
        for _ in 0..8 {
            let state = state.clone();
            workers.push(thread::spawn(move || {
                std::iter::repeat_with(|| {
                    state.allocate_lineage_entry().expect("lineage allocation")
                })
                .take(25)
                .collect::<Vec<_>>()
            }));
        }
        let entries = workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("allocation worker"))
            .collect::<Vec<_>>();
        let mut entries = entries;
        entries.sort_by_key(|entry| entry.current_inscription_counter);

        assert_eq!(entries.len(), 200);
        assert_eq!(entries[0].parent_id, MsgId::root());
        for (index, entry) in entries.iter().enumerate() {
            assert_eq!(entry.current_inscription_counter, index as u64 + 1);
            if index > 0 {
                assert_eq!(entry.parent_id, entries[index - 1].current_id);
                assert_eq!(entry.parent_inscription_counter, index as u64);
            }
        }
    }

    #[test]
    fn shuffle_seeds_advance_and_reproduce_distinct_orders() {
        let state = DependentTransactionLoadState::new();
        let mut first = (0..200).collect::<Vec<_>>();
        let mut second = (0..200).collect::<Vec<_>>();
        let mut third = (0..200).collect::<Vec<_>>();

        assert_eq!(state.shuffle_burst(&mut first), 42);
        assert_eq!(state.shuffle_burst(&mut second), 43);
        shuffle_with_seed(&mut third, 42);

        assert_eq!(first, third);
        assert_ne!(first, second);
    }

    #[test]
    fn allocation_is_not_rolled_back_when_hybrid_preparation_fails() {
        let state = DependentTransactionLoadState::new();
        state.begin_burst(1, 4);
        let preparation: Result<(), StepError> = state.prepare_after_allocation(|allocation| {
            assert_eq!(allocation.current_inscription_counter, 1);
            Err(StepError::LogicalError {
                message: "simulated transaction preparation failure".to_owned(),
            })
        });

        assert!(
            matches!(preparation, Err(StepError::LogicalError { message }) if message == "simulated transaction preparation failure")
        );
        assert_eq!(state.lineage_counter().expect("lineage lock"), 1);
        let failed_burst = state
            .last_burst_diagnostics()
            .expect("failed burst diagnostics");
        assert_eq!(failed_burst.lineage_counter_start, Some(1));
        assert_eq!(failed_burst.lineage_counter_end, Some(1));
        assert_eq!(failed_burst.shuffle_seed, None);
        assert_eq!(
            state
                .allocate_lineage_entry()
                .expect("next lineage allocation")
                .parent_inscription_counter,
            1
        );
    }

    #[test]
    fn independent_mode_does_not_shuffle_a_burst() {
        let state = DependentTransactionLoadState::new();
        let mut independent = (0..8).collect::<Vec<_>>();
        let original = independent.clone();
        assert_eq!(shuffle_if_dependent(&mut independent, None), None);
        assert_eq!(independent, original);
        assert_eq!(state.lineage_counter().expect("lineage lock"), 0);
        let mut dependent = original.clone();
        assert_eq!(shuffle_if_dependent(&mut dependent, Some(&state)), Some(42));
        assert_ne!(dependent, original);
    }

    #[test]
    fn state_clones_share_the_same_lineage_and_seed_counter() {
        let state = DependentTransactionLoadState::new();
        let clone = state.clone();
        assert_eq!(
            clone
                .allocate_lineage_entry()
                .expect("allocation")
                .current_inscription_counter,
            1
        );
        let mut values = vec![0, 1, 2, 3, 4, 5];
        assert_eq!(clone.shuffle_burst(&mut values), 42);
        assert_eq!(state.shuffle_burst(&mut values), 43);
    }
}
