pub mod error;
pub mod transaction;

use std::{
    borrow::Borrow,
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
};

pub use error::WalletError;
use lb_core::{
    block::Block,
    events::{BlockEvent, Event, Events, HeaderEvent, TxEvent, TxEventPayload},
    header::HeaderId,
    mantle::{
        GasProfile, Note, TxHash, Value,
        gas::GasOverflow,
        ledger::verification_mode::StandardMode,
        note_set::{CommitmentMmr, LEDGER_SET, NoteSetIndex, NoteSetUpdate, SDP_SET},
        notes::{InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments},
        ops::{OpRef, transfer::TransferOp},
        traits::{MantleTx, SignedMantleTx},
        transactions::{
            builder::{FundingNote, MantleTxBuilder, ZkTransferOpening},
            states::Preverified,
            tx_list::ops::OpsContext,
        },
    },
};
use lb_cryptarchia_engine::Epoch;
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_log_targets::wallet;
use lb_mmr::{MerklePath, PushWithPathsError};
use lb_zktransfer::MAX_INPUTS;
use serde::{Deserialize, Serialize};
use tracing::info;

const LOG_TARGET: &str = wallet::CORE;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedNote {
    pub note: Note,
    pub note_set: NoteSetIndex,
    pub nullifier: NoteNullifier,
    // path to the current root of its set
    pub path: MerklePath,
}

// An opening the wallet adopts once its commitment is appended to its set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownNote {
    pub note: Note,
    pub note_set: NoteSetIndex,
    pub nullifier: NoteNullifier,
}

pub type KnownNotes = HashMap<NoteCommitment, KnownNote>;

/// A lightweight projection of a `Block<Tx>` carrying only what the wallet
/// needs to follow the note sets the block changed.
pub struct WalletBlock {
    pub id: HeaderId,
    pub parent: HeaderId,
    pub epoch: Epoch,
    pub updates: Vec<NoteSetUpdate>,
    // openings the block reveals, adopted only once registered as known notes
    pub revealed: Vec<(NoteSetIndex, Note)>,
}

impl WalletBlock {
    #[must_use]
    pub fn from_block<Tx>(block: &Block<Tx>, epoch: Epoch, events: &Events) -> Self
    where
        Tx: SignedMantleTx<Preverified, StandardMode, Hash = TxHash> + Clone,
    {
        Self {
            id: block.header().id(),
            parent: block.header().parent(),
            epoch,
            updates: events
                .iter()
                .filter_map(|event| match event {
                    Event::Block(BlockEvent::NoteSetUpdated(update)) => Some(update.clone()),
                    _ => None,
                })
                .collect(),
            revealed: revealed_notes(
                events,
                block.transactions_iter().flat_map(MantleTx::op_refs_iter),
            ),
        }
    }
}

// The openings a block reveals: the leader, PoW and Blend reward notes from
// the events, then the service note of each SDP_DECLARE.
#[must_use]
pub fn revealed_notes<'a>(
    events: &Events,
    ops: impl Iterator<Item = OpRef<'a>>,
) -> Vec<(NoteSetIndex, Note)> {
    let rewards = events.iter().filter_map(|event| match event {
        Event::Block(BlockEvent::LeaderRewardPaid { note })
        | Event::Header(HeaderEvent::SdpRewardDistributed { note, .. })
        | Event::Tx(TxEvent {
            payload: TxEventPayload::PoWRewardClaimed { note, .. },
            ..
        }) => Some((LEDGER_SET, *note)),
        _ => None,
    });
    let service_notes = ops.filter_map(|op| match op {
        OpRef::SDPDeclare(declaration) => Some((SDP_SET, declaration.service_note())),
        _ => None,
    });
    rewards.chain(service_notes).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletState {
    pub commitments: rpds::HashTrieMapSync<NoteSetIndex, CommitmentMmr>,
    pub notes: rpds::HashTrieMapSync<NoteCommitment, OwnedNote>,
    // C_LEAD snapshot of the current epoch
    pub aged_paths: rpds::HashTrieMapSync<NoteCommitment, MerklePath>,
    // snapshot the next epoch will use
    pub next_aged_paths: rpds::HashTrieMapSync<NoteCommitment, MerklePath>,
    pub epoch: Epoch,
}

impl WalletState {
    pub fn from_genesis(
        known: &KnownNotes,
        genesis_notes: &[Note],
        genesis_service_notes: &[Note],
        epoch: Epoch,
    ) -> Result<Self, WalletError> {
        let mut commitments = rpds::HashTrieMapSync::new_sync();
        let mut notes = rpds::HashTrieMapSync::new_sync();
        for (note_set, set_notes) in [
            (LEDGER_SET, genesis_notes),
            (SDP_SET, genesis_service_notes),
        ] {
            commitments.insert_mut(note_set, CommitmentMmr::new());
            let update = NoteSetUpdate {
                note_set,
                nullifiers: vec![],
                commitments: set_notes.iter().map(Note::commitment).collect(),
            };
            apply_update(&mut commitments, &mut notes, known, &update)?;
        }
        // both genesis epoch states hold the genesis sets
        let paths = current_paths(&notes);
        Ok(Self {
            commitments,
            notes,
            aged_paths: paths.clone(),
            next_aged_paths: paths,
            epoch,
        })
    }

    #[must_use]
    pub fn notes_owned_by_pks(
        &self,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
    ) -> Vec<OwnedNote> {
        let pks = pks
            .into_iter()
            .map(|pk| *pk.borrow())
            .collect::<HashSet<_>>();
        self.notes
            .values()
            .filter(|owned| owned.note_set == LEDGER_SET && pks.contains(&owned.note.public_key))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn balance(&self, pk: ZkPublicKey) -> Option<WalletBalance> {
        let notes = self
            .notes_owned_by_pks([pk])
            .into_iter()
            .map(|owned| (owned.note.commitment(), owned.note.value))
            .collect::<HashMap<_, _>>();
        if notes.is_empty() {
            return None;
        }
        Some(WalletBalance {
            balance: notes.values().sum(),
            notes,
        })
    }

    // The owned notes of every set together with their path in the aged
    // snapshot.
    #[must_use]
    pub fn aged_notes(&self) -> Vec<(OwnedNote, MerklePath)> {
        self.owned_notes_with(&self.aged_paths)
    }

    // The owned notes with their path in the aged snapshot of `epoch`, which
    // the first block of `epoch` on top of this state is proven against.
    #[must_use]
    pub fn aged_notes_at(&self, epoch: Epoch) -> Vec<(OwnedNote, MerklePath)> {
        self.owned_notes_with(&self.epoch_paths(epoch).0)
    }

    fn owned_notes_with(
        &self,
        paths: &rpds::HashTrieMapSync<NoteCommitment, MerklePath>,
    ) -> Vec<(OwnedNote, MerklePath)> {
        self.notes
            .iter()
            .filter_map(|(commitment, owned)| {
                paths
                    .get(commitment)
                    .map(|path| (owned.clone(), path.clone()))
            })
            .collect()
    }

    // The aged and next aged paths of a block of `epoch` on top of this state:
    // the ledger snapshots the sets of the parent, before any update of the
    // block.
    fn epoch_paths(
        &self,
        epoch: Epoch,
    ) -> (
        rpds::HashTrieMapSync<NoteCommitment, MerklePath>,
        rpds::HashTrieMapSync<NoteCommitment, MerklePath>,
    ) {
        match epoch.into_inner().checked_sub(self.epoch.into_inner()) {
            None | Some(0) => (self.aged_paths.clone(), self.next_aged_paths.clone()),
            Some(1) => (self.next_aged_paths.clone(), current_paths(&self.notes)),
            Some(_) => {
                let paths = current_paths(&self.notes);
                (paths.clone(), paths)
            }
        }
    }

    pub fn apply_block(
        &self,
        known: &KnownNotes,
        block: &WalletBlock,
    ) -> Result<Self, WalletError> {
        let (aged_paths, next_aged_paths) = self.epoch_paths(block.epoch);

        let mut commitments = self.commitments.clone();
        let mut notes = self.notes.clone();
        for update in &block.updates {
            apply_update(&mut commitments, &mut notes, known, update)?;
        }

        Ok(Self {
            commitments,
            notes,
            aged_paths,
            next_aged_paths,
            epoch: block.epoch,
        })
    }

    /// Funds the transaction so its excess balance is exactly the requested
    /// percentage of the final mandatory fee. Only the unused reserve becomes
    /// the transaction's effective priority tip. `0` funds to the exact
    /// minimum.
    #[expect(clippy::too_many_arguments, reason = "the funding inputs")]
    pub fn fund_tx<G: GasProfile>(
        &self,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &OpsContext,
        excluded: &HashSet<NoteNullifier>,
        priority_fee_percent: u64,
    ) -> Result<MantleTxBuilder, WalletError> {
        // Get the ledger notes owned by the provided PKs, excluding the ones
        // the tx already consumes and the ones excluded by the caller (e.g.
        // reserved for in flight txs). Service and channel notes are in
        // other sets.
        let excluded = tx_builder
            .nullifiers_consumed()
            .chain(excluded.iter().copied())
            .collect::<HashSet<_>>();
        let mut notes = self.funding_candidates(pks, &excluded);
        // A ZkTransfer spends at most four notes.
        notes.truncate(MAX_INPUTS.saturating_sub(tx_builder.funding_inputs().len()));
        let funding_root = self.ledger_root();

        // The transaction may already be funded before we add any of the
        // wallet's notes, for example when it pays no fee or the caller already
        // supplied inputs. In that case we must not pull in an extra input (and
        // the change note it would require).
        match tx_builder
            .funding_delta_with_priority_fee::<G>(context, priority_fee_percent)?
            .cmp(&0)
        {
            Ordering::Equal => return Ok(tx_builder.clone()),
            Ordering::Greater => {
                if let Some(tx_with_change) = tx_builder.clone().return_change::<G>(
                    context,
                    change_pk,
                    change_nonce,
                    priority_fee_percent,
                )? {
                    return Ok(tx_with_change);
                }
                // The change note costs more than the surplus covers, so fall
                // through and pull in more notes below.
            }
            Ordering::Less => {}
        }

        for i in 0..notes.len() {
            let funded_tx_builder = tx_builder.clone().extend_funding_inputs(
                funding_root,
                notes[..=i].iter().map(|owned| FundingNote {
                    note: owned.note,
                    nullifier: owned.nullifier,
                }),
            )?;

            let funding_delta = funded_tx_builder
                .funding_delta_with_priority_fee::<G>(context, priority_fee_percent)?;

            match funding_delta.cmp(&0) {
                Ordering::Less => {
                    // Insufficient funds, need more notes.
                }
                Ordering::Equal => {
                    // We can exactly pay the tx cost plus the tip, no change
                    // note needed.
                    return Ok(funded_tx_builder);
                }
                Ordering::Greater => {
                    // We have enough balance, but we need to introduce a change note.
                    // The change note will slightly increase the storage cost of the tx so there is
                    // a chance that we will not be able to fund the tx with the change note.
                    if let Some(tx_with_change) = funded_tx_builder.return_change::<G>(
                        context,
                        change_pk,
                        change_nonce,
                        priority_fee_percent,
                    )? {
                        // We were able to fund the tx with change note added.
                        return Ok(tx_with_change);
                    }
                    // Otherwise, need more notes.
                }
            }
        }

        Err(WalletError::InsufficientFunds {
            available: total_value(&notes)?,
        })
    }

    // A Transfer of excess 0 giving `owner` a note of exactly `value` (output
    // 0) and the rest back to `change_pk` (output 1).
    #[expect(clippy::too_many_arguments, reason = "the two notes it creates")]
    pub fn exact_note_split(
        &self,
        value: Value,
        owner: ZkPublicKey,
        nonce: Fr,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        excluded: &HashSet<NoteNullifier>,
    ) -> Result<(TransferOp, ZkTransferOpening), WalletError> {
        // The smallest note covering `value` alone, else the smallest notes that
        // do, else the largest ones, so the larger notes stay free to fund the
        // fee whenever they can.
        let candidates = self.funding_candidates(pks, excluded);
        let (inputs, total) = if let Some(owned) = candidates
            .iter()
            .rev()
            .find(|owned| owned.note.value >= value)
        {
            (vec![owned.clone()], owned.note.value)
        } else {
            let (inputs, total) = covering_prefix(candidates.iter().rev(), value)?;
            if total >= value {
                (inputs, total)
            } else {
                covering_prefix(candidates.iter(), value)?
            }
        };
        let Some(rest) = total.checked_sub(value) else {
            return Err(WalletError::InsufficientFunds { available: total });
        };

        let mut outputs = vec![Note::new(value, nonce, owner)];
        if rest > 0 {
            outputs.push(Note::new(rest, change_nonce, change_pk));
        }
        let op = TransferOp::new(
            InputNullifiers::try_from_iter(inputs.iter().map(|owned| owned.nullifier))?,
            OutputCommitments::try_from_iter(outputs.iter().map(Note::commitment))?,
            self.ledger_root(),
            0,
        );
        let opening = ZkTransferOpening {
            inputs: inputs.iter().map(|owned| owned.note).collect(),
            outputs,
        };
        Ok((op, opening))
    }

    // The spendable ledger notes of `pks`, largest first.
    fn funding_candidates(
        &self,
        pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        excluded: &HashSet<NoteNullifier>,
    ) -> Vec<OwnedNote> {
        let mut notes = self
            .notes_owned_by_pks(pks)
            .into_iter()
            .filter(|owned| !excluded.contains(&owned.nullifier))
            .collect::<Vec<_>>();
        // Consume large valued notes first to ensure we converge.
        notes.sort_by_key(|owned| -i128::from(owned.note.value));
        notes
    }

    fn ledger_root(&self) -> Fr {
        self.commitments.get(&LEDGER_SET).map_or_else(
            || CommitmentMmr::new().frontier_root(),
            CommitmentMmr::frontier_root,
        )
    }
}

fn total_value(notes: &[OwnedNote]) -> Result<Value, WalletError> {
    notes
        .iter()
        .try_fold(0, |total: Value, owned| total.checked_add(owned.note.value))
        .ok_or_else(|| GasOverflow.into())
}

// The first of `notes`, at most `MAX_INPUTS`, until they cover `value`, and
// their total.
fn covering_prefix<'a>(
    notes: impl Iterator<Item = &'a OwnedNote>,
    value: Value,
) -> Result<(Vec<OwnedNote>, Value), WalletError> {
    let mut inputs = Vec::new();
    let mut total: Value = 0;
    for owned in notes.take(MAX_INPUTS) {
        if total >= value {
            break;
        }
        total = total.checked_add(owned.note.value).ok_or(GasOverflow)?;
        inputs.push(owned.clone());
    }
    Ok((inputs, total))
}

fn current_paths(
    notes: &rpds::HashTrieMapSync<NoteCommitment, OwnedNote>,
) -> rpds::HashTrieMapSync<NoteCommitment, MerklePath> {
    notes
        .iter()
        .map(|(commitment, owned)| (*commitment, owned.path.clone()))
        .collect()
}

// Append the commitments of `update` to its set, keeping the paths of the
// owned notes of the set valid and adopting the known ones, then drop the
// owned notes whose nullifier it inserted.
fn apply_update(
    commitments: &mut rpds::HashTrieMapSync<NoteSetIndex, CommitmentMmr>,
    notes: &mut rpds::HashTrieMapSync<NoteCommitment, OwnedNote>,
    known: &KnownNotes,
    update: &NoteSetUpdate,
) -> Result<(), WalletError> {
    let note_set = update.note_set;
    let mut mmr = commitments.get(&note_set).cloned().unwrap_or_default();
    let (mut owned, mut paths): (Vec<KnownNote>, Vec<MerklePath>) = notes
        .values()
        .filter(|owned| owned.note_set == note_set)
        .map(|owned| {
            (
                KnownNote {
                    note: owned.note,
                    note_set,
                    nullifier: owned.nullifier,
                },
                owned.path.clone(),
            )
        })
        .unzip();

    for commitment in &update.commitments {
        let (next, path) =
            mmr.push_with_paths(*commitment, &mut paths)
                .map_err(|error| match error {
                    PushWithPathsError::MmrFull(_) => WalletError::MmrFull(note_set),
                    PushWithPathsError::InvalidTrackedPath(error) => error.into(),
                })?;
        mmr = next;
        if let Some(known_note) = known
            .get(commitment)
            .filter(|known| known.note_set == note_set)
        {
            owned.push(*known_note);
            paths.push(path);
        }
    }
    commitments.insert_mut(note_set, mmr);

    let mut by_nullifier = HashMap::new();
    for (known_note, path) in owned.into_iter().zip(paths) {
        let commitment = known_note.note.commitment();
        by_nullifier.insert(known_note.nullifier, commitment);
        notes.insert_mut(
            commitment,
            OwnedNote {
                note: known_note.note,
                note_set,
                nullifier: known_note.nullifier,
                path,
            },
        );
    }
    for nullifier in &update.nullifiers {
        if let Some(commitment) = by_nullifier.get(nullifier) {
            notes.remove_mut(commitment);
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct Wallet<KeyId> {
    known_keys: HashMap<ZkPublicKey, KeyId>,
    known_notes: KnownNotes,
    wallet_states: BTreeMap<HeaderId, WalletState>,
}

impl<KeyId> Wallet<KeyId> {
    pub fn from_genesis(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_notes: KnownNotes,
        genesis: HeaderId,
        genesis_notes: &[Note],
        genesis_service_notes: &[Note],
    ) -> Result<Self, WalletError> {
        let known_keys = known_keys.into_iter().collect::<HashMap<_, _>>();
        let wallet_state = WalletState::from_genesis(
            &known_notes,
            genesis_notes,
            genesis_service_notes,
            Epoch::new(0),
        )?;

        info!(
            target: LOG_TARGET,
            ?genesis,
            n_known_keys = known_keys.len(),
            n_known_notes = known_notes.len(),
            n_owned_notes = wallet_state.notes.size(),
            "initializing wallet with genesis note sets"
        );

        Ok(Self {
            known_keys,
            known_notes,
            wallet_states: [(genesis, wallet_state)].into(),
        })
    }

    /// Initialize [`Wallet`] from a given [`WalletState`] at LIB
    /// (e.g., restored from persisted state).
    ///
    /// Tracking of the paths of the owned notes starts from the paths
    /// stored in the [`WalletState`]. Persisted Merkle paths are globally
    /// bounded during deserialization. Compatibility with the production
    /// MMR height is validated when active paths are updated.
    pub fn from_lib_wallet_state(
        known_keys: impl IntoIterator<Item = (ZkPublicKey, KeyId)>,
        known_notes: KnownNotes,
        lib: HeaderId,
        wallet_state: WalletState,
    ) -> Self {
        let known_keys = known_keys.into_iter().collect::<HashMap<_, _>>();

        info!(
            target: LOG_TARGET,
            ?lib,
            n_known_keys = known_keys.len(),
            n_known_notes = known_notes.len(),
            n_owned_notes = wallet_state.notes.size(),
            n_aged_paths = wallet_state.aged_paths.size(),
            "initializing wallet with LIB wallet state"
        );

        Self {
            known_keys,
            known_notes,
            wallet_states: [(lib, wallet_state)].into(),
        }
    }

    #[must_use]
    pub const fn known_keys(&self) -> &HashMap<ZkPublicKey, KeyId> {
        &self.known_keys
    }

    pub fn add_known_key(&mut self, pk: ZkPublicKey, key_id: KeyId) {
        self.known_keys.insert(pk, key_id);
    }

    pub fn add_known_notes(&mut self, notes: impl IntoIterator<Item = KnownNote>) {
        self.known_notes.extend(
            notes
                .into_iter()
                .map(|known_note| (known_note.note.commitment(), known_note)),
        );
    }

    #[must_use]
    pub const fn known_notes(&self) -> &KnownNotes {
        &self.known_notes
    }

    #[must_use]
    pub fn has_processed_block(&self, block_id: HeaderId) -> bool {
        self.wallet_states.contains_key(&block_id)
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        if self.wallet_states.contains_key(&block.id) {
            // Already processed this block
            return Ok(());
        }

        let block_wallet_state = self
            .wallet_state_at(block.parent)?
            .apply_block(&self.known_notes, block)?;
        self.wallet_states.insert(block.id, block_wallet_state);
        Ok(())
    }

    pub fn balance(
        &self,
        tip: HeaderId,
        pk: ZkPublicKey,
    ) -> Result<Option<WalletBalance>, WalletError> {
        Ok(self.wallet_state_at(tip)?.balance(pk))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "thin passthrough to `WalletState::fund_tx` plus the tip"
    )]
    pub fn fund_tx<G: GasProfile>(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &OpsContext,
        excluded: &HashSet<NoteNullifier>,
        priority_fee_percent: u64,
    ) -> Result<MantleTxBuilder, WalletError> {
        self.wallet_state_at(tip)?.fund_tx::<G>(
            tx_builder,
            change_pk,
            change_nonce,
            funding_pks,
            context,
            excluded,
            priority_fee_percent,
        )
    }

    pub fn wallet_state_at(&self, tip: HeaderId) -> Result<WalletState, WalletError> {
        self.wallet_states
            .get(&tip)
            .cloned()
            .ok_or(WalletError::UnknownBlock(tip))
    }

    /// Prune wallet states for blocks that have been pruned from the chain.
    ///
    /// This removes wallet states for blocks that are no longer part of the
    /// chain after LIB advancement. Both stale blocks (from abandoned
    /// forks) and immutable blocks (before the new LIB) are removed.
    pub fn prune_states(&mut self, pruned_blocks: impl IntoIterator<Item = HeaderId>) {
        let mut removed_count = 0;

        for block_id in pruned_blocks {
            if self.wallet_states.remove(&block_id).is_some() {
                removed_count += 1;
            }
        }

        if removed_count > 0 {
            tracing::trace!(
                target: LOG_TARGET,
                removed_states = removed_count,
                remaining_states = self.wallet_states.len(),
                "Pruned wallet states for pruned blocks"
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalletBalance {
    pub balance: Value,
    pub notes: HashMap<NoteCommitment, Value>,
}

#[cfg(test)]
pub(crate) mod test_utils {
    use super::*;

    pub fn pk(v: u64) -> ZkPublicKey {
        ZkPublicKey::new(Fr::from(v))
    }

    pub fn nullifier(v: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(v))
    }

    pub fn known(entries: &[(Note, NoteSetIndex, NoteNullifier)]) -> KnownNotes {
        entries
            .iter()
            .map(|&(note, note_set, nullifier)| {
                (
                    note.commitment(),
                    KnownNote {
                        note,
                        note_set,
                        nullifier,
                    },
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use lb_core::{
        crypto::ZkHasher,
        mantle::{
            Op,
            gas::MainnetGasProfile,
            ops::{channel::inscribe::InscriptionOp, pow::PowNullifier, sdp::SDPDeclareOp},
            transactions::{GasPrices, tx_list::ops::OpsGasContext},
        },
    };
    use lb_groth16::{AdditiveGroup as _, Field as _};

    use super::*;
    use crate::test_utils::{known, nullifier, pk};

    fn commitment(v: u64) -> NoteCommitment {
        NoteCommitment(Fr::from(v))
    }

    fn block(id: u8, parent: u8, epoch: u32, updates: Vec<NoteSetUpdate>) -> WalletBlock {
        WalletBlock {
            id: HeaderId::from([id; 32]),
            parent: HeaderId::from([parent; 32]),
            epoch: Epoch::new(epoch),
            updates,
            revealed: vec![],
        }
    }

    fn appended(note_set: NoteSetIndex, commitments: Vec<NoteCommitment>) -> NoteSetUpdate {
        NoteSetUpdate {
            note_set,
            nullifiers: vec![],
            commitments,
        }
    }

    #[test]
    fn apply_block_keeps_the_paths_of_owned_notes_valid() {
        let genesis = Note::new(5, Fr::ZERO, pk(9));
        let owned = Note::new(100, Fr::from(1u64), pk(1));
        let known = known(&[(owned, LEDGER_SET, nullifier(1))]);
        let state = WalletState::from_genesis(&known, &[genesis], &[], Epoch::new(0)).unwrap();
        let state = state
            .apply_block(
                &known,
                &block(
                    1,
                    0,
                    0,
                    vec![appended(
                        LEDGER_SET,
                        vec![owned.commitment(), commitment(7), commitment(8)],
                    )],
                ),
            )
            .unwrap();
        let root = CommitmentMmr::new()
            .push(genesis.commitment())
            .unwrap()
            .push(owned.commitment())
            .unwrap()
            .push(commitment(7))
            .unwrap()
            .push(commitment(8))
            .unwrap()
            .frontier_root();
        assert_eq!(state.commitments[&LEDGER_SET].frontier_root(), root);
        assert!(
            state.notes[&owned.commitment()]
                .path
                .verify::<ZkHasher>(owned.commitment().0, root)
        );
        assert_eq!(
            state.balance(pk(1)),
            Some(WalletBalance {
                balance: 100,
                notes: HashMap::from([(owned.commitment(), 100)])
            })
        );
    }

    #[test]
    fn apply_block_drops_a_note_whose_nullifier_is_inserted() {
        let owned = Note::new(100, Fr::from(1u64), pk(1));
        let known = known(&[(owned, LEDGER_SET, nullifier(1))]);
        let state = WalletState::from_genesis(&known, &[owned], &[], Epoch::new(0)).unwrap();
        let spend = NoteSetUpdate {
            note_set: LEDGER_SET,
            nullifiers: vec![nullifier(1)],
            commitments: vec![commitment(7)],
        };
        let state = state
            .apply_block(&known, &block(1, 0, 0, vec![spend]))
            .unwrap();
        assert!(state.notes.is_empty());
        assert_eq!(state.balance(pk(1)), None);
    }

    #[test]
    fn epoch_change_rotates_the_aged_paths() {
        let owned = Note::new(100, Fr::from(1u64), pk(1));
        let known = known(&[(owned, LEDGER_SET, nullifier(1))]);
        // root of the ledger set holding `owned` then commitment(1)..=commitment(n)
        let root_at = |n: u64| {
            (1..=n)
                .fold(
                    CommitmentMmr::new().push(owned.commitment()).unwrap(),
                    |mmr, v| mmr.push(commitment(v)).unwrap(),
                )
                .frontier_root()
        };
        let aged_root_is = |state: &WalletState, n: u64| {
            state.aged_paths[&owned.commitment()]
                .verify::<ZkHasher>(owned.commitment().0, root_at(n))
        };
        let append = |state: &WalletState, id: u8, epoch: u32, v: u64| {
            state
                .apply_block(
                    &known,
                    &block(
                        id,
                        id - 1,
                        epoch,
                        vec![appended(LEDGER_SET, vec![commitment(v)])],
                    ),
                )
                .unwrap()
        };

        let state = WalletState::from_genesis(&known, &[owned], &[], Epoch::new(0)).unwrap();
        let state = append(&state, 1, 0, 1);
        assert!(aged_root_is(&state, 0));
        let state = append(&state, 2, 1, 2); // epoch 1 uses the genesis sets
        assert!(aged_root_is(&state, 0));
        let state = append(&state, 3, 2, 3); // epoch 2 uses the sets after the last block of epoch 0
        assert!(aged_root_is(&state, 1));
        let state = append(&state, 4, 4, 4); // epochs skipped: the sets of the parent
        assert!(aged_root_is(&state, 3));
        assert_eq!(state.aged_notes().len(), 1);
    }

    #[test]
    fn aged_notes_of_a_later_epoch_are_those_its_first_block_will_use() {
        let owned = Note::new(100, Fr::from(1u64), pk(1));
        let known = known(&[(owned, LEDGER_SET, nullifier(1))]);
        let state = WalletState::from_genesis(&known, &[owned], &[], Epoch::new(0)).unwrap();
        let state = state
            .apply_block(
                &known,
                &block(1, 0, 1, vec![appended(LEDGER_SET, vec![commitment(1)])]),
            )
            .unwrap();
        let state = state
            .apply_block(
                &known,
                &block(2, 1, 1, vec![appended(LEDGER_SET, vec![commitment(2)])]),
            )
            .unwrap();
        for epoch in 1..=3 {
            let first_block = state
                .apply_block(&known, &block(3, 2, epoch, vec![]))
                .unwrap();
            assert_eq!(
                state.aged_notes_at(Epoch::new(epoch)),
                first_block.aged_notes(),
                "epoch {epoch}"
            );
        }
    }

    #[test]
    fn notes_of_a_new_channel_set_start_from_an_empty_mmr() {
        let channel_note = Note::new(50, Fr::from(2u64), pk(1));
        let known = known(&[(channel_note, 2, nullifier(2))]);
        let state = WalletState::from_genesis(&known, &[], &[], Epoch::new(0)).unwrap();
        let state = state
            .apply_block(
                &known,
                &block(
                    1,
                    0,
                    0,
                    vec![appended(2, vec![commitment(4), channel_note.commitment()])],
                ),
            )
            .unwrap();
        let root = CommitmentMmr::new()
            .push(commitment(4))
            .unwrap()
            .push(channel_note.commitment())
            .unwrap()
            .frontier_root();
        assert!(
            state.notes[&channel_note.commitment()]
                .path
                .verify::<ZkHasher>(channel_note.commitment().0, root)
        );
        assert_eq!(state.balance(pk(1)), None);
    }

    #[test]
    fn exact_note_split_leaves_the_largest_note_to_fund_the_fee() {
        let small = Note::new(1, Fr::from(1u64), pk(1));
        let large = Note::new(1000, Fr::from(2u64), pk(1));
        let known = known(&[
            (small, LEDGER_SET, nullifier(1)),
            (large, LEDGER_SET, nullifier(2)),
        ]);
        let state = WalletState::from_genesis(&known, &[small, large], &[], Epoch::new(0)).unwrap();
        let (_, opening) = state
            .exact_note_split(
                1,
                pk(1),
                Fr::from(3u64),
                pk(1),
                Fr::from(4u64),
                [pk(1)],
                &HashSet::new(),
            )
            .unwrap();
        assert_eq!(opening.inputs, vec![small]);
    }

    #[test]
    fn exact_note_split_takes_one_larger_note_when_the_smaller_ones_fall_short() {
        let dust: Vec<Note> = (1..=4)
            .map(|nonce| Note::new(1, Fr::from(nonce), pk(1)))
            .collect();
        let large = Note::new(1000, Fr::from(5u64), pk(1));
        let known = known(
            &dust
                .iter()
                .chain([&large])
                .zip(1..)
                .map(|(note, seed)| (*note, LEDGER_SET, nullifier(seed)))
                .collect::<Vec<_>>(),
        );
        let genesis: Vec<Note> = dust.iter().copied().chain([large]).collect();
        let state = WalletState::from_genesis(&known, &genesis, &[], Epoch::new(0)).unwrap();
        let (_, opening) = state
            .exact_note_split(
                500,
                pk(1),
                Fr::from(6u64),
                pk(1),
                Fr::from(7u64),
                [pk(1)],
                &HashSet::new(),
            )
            .unwrap();
        assert_eq!(opening.inputs, vec![large]);
    }

    #[test]
    fn revealed_notes_come_from_reward_events_and_declarations() {
        let leader = Note::new(10, Fr::ZERO, pk(1));
        let pow = Note::new(20, Fr::from(3u64), pk(2));
        let events: Events = [
            Event::from(BlockEvent::LeaderRewardPaid { note: leader }),
            Event::from(TxEvent::new(
                TxHash::from([1; 32]),
                [2; 32],
                TxEventPayload::PoWRewardClaimed {
                    pow_nullifier: PowNullifier::from(Fr::ONE),
                    note: pow,
                },
            )),
        ]
        .into_iter()
        .collect();
        let declaration = SDPDeclareOp::sample();
        assert_eq!(
            revealed_notes(&events, std::iter::once(OpRef::SDPDeclare(&declaration))),
            vec![
                (LEDGER_SET, leader),
                (LEDGER_SET, pow),
                (SDP_SET, declaration.service_note())
            ]
        );
    }

    /// Two transfers within a single transaction where the second consumes
    /// the first's change. After applying the block, the intermediate note
    /// must not appear in the wallet's notes, only the final notes of the
    /// second transfer remain.
    #[test]
    fn test_apply_block_dependent_ops_within_tx() {
        let alice = pk(1);
        let bob = pk(2);
        let genesis = HeaderId::from([0; 32]);
        // Op A: 100 to alice. Op B: spends it, 30 to bob and 70 back to alice.
        let intermediate = Note::new(100, Fr::from(1u64), alice);
        let to_bob = Note::new(30, Fr::from(2u64), bob);
        let change = Note::new(70, Fr::from(3u64), alice);
        let known = known(&[
            (intermediate, LEDGER_SET, nullifier(1)),
            (to_bob, LEDGER_SET, nullifier(2)),
            (change, LEDGER_SET, nullifier(3)),
        ]);
        let mut wallet =
            Wallet::from_genesis([(alice, 1), (bob, 2)], known, genesis, &[], &[]).unwrap();

        // The ledger reports the block's appends and nullifiers in execution
        // order.
        let block = block(
            1,
            0,
            0,
            vec![NoteSetUpdate {
                note_set: LEDGER_SET,
                nullifiers: vec![nullifier(1)],
                commitments: vec![
                    intermediate.commitment(),
                    to_bob.commitment(),
                    change.commitment(),
                ],
            }],
        );
        wallet.apply_block(&block).unwrap();

        let alice_balance = wallet.balance(block.id, alice).unwrap().unwrap();
        assert_eq!(alice_balance.balance, 70);
        assert!(!alice_balance.notes.contains_key(&intermediate.commitment()));

        let bob_balance = wallet.balance(block.id, bob).unwrap().unwrap();
        assert_eq!(bob_balance.balance, 30);
    }

    fn context(gas_prices: GasPrices) -> OpsContext {
        OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), gas_prices),
        }
    }

    // `tx_builder` funded by one note of value 0, to price its funding Transfer.
    fn with_zero_input(tx_builder: &MantleTxBuilder) -> MantleTxBuilder {
        tx_builder
            .clone()
            .extend_funding_inputs(
                Fr::ZERO,
                [FundingNote {
                    note: Note::new(0, Fr::ZERO, pk(0)),
                    nullifier: nullifier(0),
                }],
            )
            .unwrap()
    }

    // The funding Transfer, which `build` appends last.
    fn funding_transfer(tx_builder: MantleTxBuilder) -> TransferOp {
        match tx_builder.build().unwrap().iter().last() {
            Some(Op::Transfer(transfer_op)) => transfer_op.clone(),
            _ => panic!("last op must be a transfer"),
        }
    }

    fn wallet_state_of(notes: &[(Note, NoteSetIndex, NoteNullifier)]) -> WalletState {
        let known = known(notes);
        let in_set = |note_set| {
            notes
                .iter()
                .filter(|(_, set, _)| *set == note_set)
                .map(|(note, _, _)| *note)
                .collect::<Vec<_>>()
        };
        WalletState::from_genesis(&known, &in_set(LEDGER_SET), &in_set(SDP_SET), Epoch::new(0))
            .unwrap()
    }

    #[test]
    fn test_fund_tx_with_change() {
        let alice = pk(1);
        let ledger_note = Note::new(5000, Fr::from(1u64), alice);
        // A service note of alice, which `fund_tx` must not spend
        let service_note = Note::new(5000, Fr::from(2u64), alice);
        let wallet_state = wallet_state_of(&[
            (ledger_note, LEDGER_SET, nullifier(1)),
            (service_note, SDP_SET, nullifier(2)),
        ]);
        let context = context(GasPrices::new(1, 1));
        let change_nonce = Fr::from(3u64);

        // Fund the transaction
        let funded_tx_builder = wallet_state
            .fund_tx::<MainnetGasProfile>(
                &MantleTxBuilder::new(),
                alice,
                change_nonce,
                [alice],
                &context,
                &HashSet::new(),
                0,
            )
            .unwrap();

        let fee = funded_tx_builder
            .minimum_gas_cost::<MainnetGasProfile>(&context)
            .unwrap()
            .into_inner();
        assert_eq!(855, fee);
        assert_eq!(i128::from(fee), funded_tx_builder.net_balance());
        assert_eq!(
            0,
            funded_tx_builder
                .funding_delta_with_priority_fee::<MainnetGasProfile>(&context, 0)
                .unwrap()
        );

        let transfer_op = funding_transfer(funded_tx_builder);
        // ensure alice's ledger note was used to pay the fee
        assert_eq!(transfer_op.inputs, [nullifier(1)].into());
        // ensure change was returned to alice
        assert_eq!(
            transfer_op.outputs,
            [Note::new(5000 - fee, change_nonce, alice).commitment()].into()
        );
    }

    #[test]
    fn test_fund_tx_with_priority_fee_percentage() {
        let alice = pk(1);
        let note = Note::new(5000, Fr::from(1u64), alice);
        let wallet_state = wallet_state_of(&[(note, LEDGER_SET, nullifier(1))]);
        let context = context(GasPrices::new(1, 1));
        let change_nonce = Fr::from(3u64);
        let tx_builder = MantleTxBuilder::new();
        let priority_fee_percent = 12;
        let mandatory_fee_without_change = with_zero_input(&tx_builder)
            .minimum_gas_cost::<MainnetGasProfile>(&context)
            .unwrap()
            .into_inner();

        let funded_tx_builder = wallet_state
            .fund_tx::<MainnetGasProfile>(
                &tx_builder,
                alice,
                change_nonce,
                [alice],
                &context,
                &HashSet::new(),
                priority_fee_percent,
            )
            .unwrap();

        // The final mandatory fee includes the change output, and the tip is
        // the rounded-up percentage of that final fee.
        let mandatory_fee = funded_tx_builder
            .minimum_gas_cost::<MainnetGasProfile>(&context)
            .unwrap()
            .into_inner();
        assert_eq!(mandatory_fee_without_change, 823);
        assert_eq!(mandatory_fee, 855);
        let priority_fee_amount = (mandatory_fee * priority_fee_percent).div_ceil(100);
        assert_eq!(
            i128::from(mandatory_fee + priority_fee_amount),
            funded_tx_builder.net_balance()
        );
        assert_eq!(
            0,
            funded_tx_builder
                .funding_delta_with_priority_fee::<MainnetGasProfile>(
                    &context,
                    priority_fee_percent
                )
                .unwrap()
        );

        // The change output shrank by the mandatory fee and percentage tip.
        assert_eq!(
            funding_transfer(funded_tx_builder).outputs,
            [Note::new(
                5000 - mandatory_fee - priority_fee_amount,
                change_nonce,
                alice
            )
            .commitment()]
            .into()
        );
    }

    #[test]
    fn test_fund_tx_no_fee_adds_no_input() {
        let alice = pk(1);
        // The wallet has a spendable note available...
        let note = Note::new(5000, Fr::from(1u64), alice);
        let wallet_state = wallet_state_of(&[(note, LEDGER_SET, nullifier(1))]);

        // ...but with zero gas prices a transfer-less transaction owes no fee,
        // so it needs no funding at all.
        let context = context(GasPrices::new(0, 0));
        let tx_builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp::sample()))
            .unwrap();

        assert_eq!(
            0,
            tx_builder
                .funding_delta::<MainnetGasProfile>(&context)
                .unwrap()
        );

        let funded_tx_builder = wallet_state
            .fund_tx::<MainnetGasProfile>(
                &tx_builder,
                alice,
                Fr::from(3u64),
                [alice],
                &context,
                &HashSet::new(),
                0,
            )
            .unwrap();

        // No input was pulled in (an added input would push the net balance to
        // 5000) and therefore no change note was added.
        assert_eq!(0, funded_tx_builder.net_balance());

        // No funding Transfer is appended, so the built tx carries no transfer op.
        let funded_tx = funded_tx_builder.build().unwrap();
        assert!(
            !funded_tx.iter().any(|op| matches!(op, Op::Transfer(_))),
            "a fee-less transaction must not contain a transfer op"
        );
    }

    #[test]
    fn test_fund_tx_insufficient_funds() {
        let alice = pk(1);
        let wallet_state = wallet_state_of(
            &(1..=4)
                .map(|i| (Note::new(100, Fr::from(i), alice), LEDGER_SET, nullifier(i)))
                .collect::<Vec<_>>(),
        );
        let context = context(GasPrices::new(1, 1));

        // Add a costly inscription
        let tx_builder = MantleTxBuilder::new()
            .push_op(Op::ChannelInscribe(InscriptionOp {
                inscription: [0xAB; 1000].into(),
                ..InscriptionOp::sample()
            }))
            .unwrap();

        // Fund the transaction
        let fund_attempt = wallet_state.fund_tx::<MainnetGasProfile>(
            &tx_builder,
            alice,
            Fr::from(5u64),
            [alice],
            &context,
            &HashSet::new(),
            0,
        );

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 400 }
        );
    }

    #[test]
    fn test_fund_tx_zero_funds() {
        let alice = pk(1);
        let wallet_state = wallet_state_of(&[]);

        // Use non-zero gas prices so the transaction actually owes a fee and
        // therefore needs funding.
        let context = context(GasPrices::new(1, 1));

        // Fund the transaction
        let fund_attempt = wallet_state.fund_tx::<MainnetGasProfile>(
            &MantleTxBuilder::new(),
            alice,
            Fr::from(1u64),
            [alice],
            &context,
            &HashSet::new(),
            0,
        );

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn test_fund_tx_all_service_notes() {
        let alice = pk(1);
        let service_note = Note::new(5000, Fr::from(1u64), alice);
        let channel_note = Note::new(5000, Fr::from(2u64), alice);
        let known = known(&[
            (service_note, SDP_SET, nullifier(1)),
            (channel_note, 2, nullifier(2)),
        ]);
        // alice only owns a service note and a channel note, which are not in
        // the ledger set, so `fund_tx` must not spend them
        let wallet_state = WalletState::from_genesis(&known, &[], &[service_note], Epoch::new(0))
            .unwrap()
            .apply_block(
                &known,
                &block(1, 0, 0, vec![appended(2, vec![channel_note.commitment()])]),
            )
            .unwrap();

        // Use non-zero gas prices so the transaction actually owes a fee and
        // therefore needs funding.
        let context = context(GasPrices::new(1, 1));

        // Fund the transaction
        let fund_attempt = wallet_state.fund_tx::<MainnetGasProfile>(
            &MantleTxBuilder::new(),
            alice,
            Fr::from(3u64),
            [alice],
            &context,
            &HashSet::new(),
            0,
        );

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );
    }

    #[test]
    fn test_fund_tx_respects_pk_list() {
        let alice = pk(1);
        let bob = pk(2);
        let wallet_state = wallet_state_of(&[(
            Note::new(1_000_000, Fr::from(1u64), bob),
            LEDGER_SET,
            nullifier(1),
        )]);

        // Use non-zero gas prices so the transaction actually owes a fee and
        // therefore needs funding.
        let context = context(GasPrices::new(1, 1));
        let tx_builder = MantleTxBuilder::new();

        // Attempt to fund the transaction with Alice's notes.
        let fund_attempt = wallet_state.fund_tx::<MainnetGasProfile>(
            &tx_builder,
            alice,
            Fr::from(2u64),
            [alice],
            &context,
            &HashSet::new(),
            0,
        );

        assert_eq!(
            fund_attempt.unwrap_err(),
            WalletError::InsufficientFunds { available: 0 }
        );

        // Fund the transaction with Bob's notes.
        wallet_state
            .fund_tx::<MainnetGasProfile>(
                &tx_builder,
                bob,
                Fr::from(2u64),
                [bob],
                &context,
                &HashSet::new(),
                0,
            )
            .unwrap(); // succesfully funded;
    }

    #[test]
    fn test_fund_tx_unfundable_region() {
        let alice = pk(1);
        let change_nonce = Fr::from(2u64);
        let context = context(GasPrices::new(1, 1));
        let tx_builder = MantleTxBuilder::new();
        let fund = |value| {
            wallet_state_of(&[(
                Note::new(value, Fr::from(1u64), alice),
                LEDGER_SET,
                nullifier(1),
            )])
            .fund_tx::<MainnetGasProfile>(
                &tx_builder,
                alice,
                change_nonce,
                [alice],
                &context,
                &HashSet::new(),
                0,
            )
        };

        // Determine gas cost without change note
        let cost_without_change = with_zero_input(&tx_builder)
            .minimum_gas_cost::<MainnetGasProfile>(&context)
            .unwrap()
            .into_inner();
        assert_eq!(823, cost_without_change);

        // We can fund the tx if the note value is exactly the gas cost without change
        // note
        let funded_tx_wo_change = fund(cost_without_change).unwrap(); // successfully funded the tx

        // verify that no change output was used.
        assert_eq!(
            funding_transfer(funded_tx_wo_change).outputs,
            OutputCommitments::default()
        );

        // Determine gas cost with change note
        let cost_with_change = with_zero_input(&tx_builder)
            .with_dummy_change_note()
            .unwrap()
            .minimum_gas_cost::<MainnetGasProfile>(&context)
            .unwrap()
            .into_inner();
        assert_eq!(855, cost_with_change);

        for value in cost_without_change + 1..=cost_with_change {
            // this region of note values will fail to fund the tx.
            assert_eq!(
                fund(value).unwrap_err(),
                WalletError::InsufficientFunds { available: value }
            );
        }

        // We can fund the tx if the note value exceeds gas cost with change note
        let funded_tx_with_change = fund(cost_with_change + 1).unwrap(); // successfully funded the tx

        // verify that indeed a change output was used.
        assert_eq!(
            funding_transfer(funded_tx_with_change).outputs,
            [Note::new(1, change_nonce, alice).commitment()].into()
        );
    }
}
