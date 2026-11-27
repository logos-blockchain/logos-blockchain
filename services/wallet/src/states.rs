use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use lb_core::{
    header::HeaderId,
    mantle::{
        Note, Value,
        note_set::NoteSetIndex,
        notes::{InputNullifiers, NoteNullifier},
        ops::transfer::TransferOp,
        transactions::{
            MantleTxBuilder,
            builder::{PriorityFee, ZkTransferOpening},
            tx_list::ops::OpsContext,
        },
    },
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::{PK0, ZkPublicKey};
use lb_log_targets::wallet;
use lb_storage_service::api::StorageApi;
use lb_wallet::{KnownNote, KnownNotes, NoteSetSpend, WalletBlock, WalletError, WalletState};
use overwatch::services::state::StateUpdater;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{
    KeyId, LeadershipKeyIds, LeadershipPublicKeys, ReservationHandoff, ReservationJob,
    WalletServiceError, WalletServiceSettings,
};

pub type Wallet = lb_wallet::Wallet<KeyId>;

/// Notes handed out as funding for transactions that have been built but not
/// yet observed on chain. Ephemeral: rebuilt empty on restart; entries are
/// dropped once the note is observed spent in a block, or expire after enough
/// LIB progress.
///
/// Unlike claim reservations, which must hold until the claim is finalised,
/// funded notes are released as soon as they are observed spent in a tip
/// block; eviction only covers transactions that never land. Releasing too
/// early merely reintroduces a conflicting tx, the same failure mode as
/// having no reservation at all.
#[derive(Clone, Debug, Default)]
struct PendingNotes {
    notes: HashMap<(NoteSetIndex, NoteNullifier), Reservation>,
    groups: HashMap<[u8; 32], HashSet<(NoteSetIndex, NoteNullifier)>>,
}

#[derive(Clone, Debug)]
struct Reservation {
    owner: [u8; 32],
    age: u64,
    funded: bool,
    active_jobs: usize,
    delivered: bool,
    // A queued response remains provisional until its API caller receives it
    // or drops the handoff. Each job keeps its own marker.
    handoffs: Vec<ReservationHandoff>,
}

impl Reservation {
    fn has_delivery(&self) -> bool {
        self.delivered || self.handoffs.iter().any(|handoff| !handoff.cancelled())
    }

    fn reconcile_handoffs(&mut self) {
        self.handoffs.retain(|handoff| {
            if handoff.completed() {
                self.delivered = true;
                false
            } else {
                !handoff.cancelled()
            }
        });
    }
}

impl PendingNotes {
    fn validate_claims(
        &self,
        builder: &MantleTxBuilder,
        for_funding: bool,
    ) -> Result<(), WalletError> {
        let inputs = builder.reservation_inputs().collect::<HashSet<_>>();
        let claims = builder.reservation_ids().collect::<HashSet<_>>();
        for owner in &claims {
            let Some(group) = self.groups.get(owner) else {
                return Err(WalletError::InputReserved);
            };
            if !group.iter().all(|input| {
                self.notes
                    .get(input)
                    .is_some_and(|entry| entry.owner == *owner)
            }) {
                return Err(WalletError::InputReserved);
            }
            let mut found = false;
            for (input, reservation) in &self.notes {
                if reservation.owner == *owner {
                    found = true;
                    if !inputs.contains(input)
                        || (for_funding
                            && (reservation.funded
                                || reservation.active_jobs > 0
                                || reservation.has_delivery()))
                    {
                        return Err(WalletError::InputReserved);
                    }
                }
            }
            if !found {
                return Err(WalletError::InputReserved);
            }
        }
        if inputs.iter().any(|input| {
            self.notes
                .get(input)
                .is_some_and(|entry| !claims.contains(&entry.owner))
        }) {
            return Err(WalletError::InputReserved);
        }
        Ok(())
    }

    fn transfer(
        &mut self,
        builder: &MantleTxBuilder,
        new_owner: [u8; 32],
        funded: bool,
    ) -> Result<(), WalletError> {
        self.validate_claims(builder, true)?;
        if self.notes.values().any(|entry| entry.owner == new_owner) {
            return Err(WalletError::InputReserved);
        }
        let inputs = builder.reservation_inputs().collect::<HashSet<_>>();
        for owner in builder.reservation_ids() {
            self.groups.remove(&owner);
        }
        for input in inputs.iter().copied() {
            self.notes
                .entry(input)
                .and_modify(|entry| {
                    entry.owner = new_owner;
                    entry.funded = funded;
                    if funded {
                        entry.age = 0;
                    }
                    // A funded builder remains usable after a failed signing
                    // or proving attempt. Its owner must explicitly abandon it.
                    entry.delivered = funded;
                    entry.handoffs.clear();
                })
                .or_insert(Reservation {
                    owner: new_owner,
                    age: 0,
                    funded,
                    active_jobs: 0,
                    delivered: funded,
                    handoffs: Vec::new(),
                });
        }
        if !inputs.is_empty() {
            self.groups.insert(new_owner, inputs);
        }
        Ok(())
    }

    #[expect(
        clippy::needless_collect,
        reason = "Each input is counted once per asynchronous job"
    )]
    fn begin_job(
        &mut self,
        builder: &MantleTxBuilder,
        new_owner: [u8; 32],
    ) -> Result<ReservationJob, WalletError> {
        self.validate_claims(builder, false)?;
        if self.notes.values().any(|entry| entry.owner == new_owner) {
            return Err(WalletError::InputReserved);
        }
        let mut job = Vec::new();
        let mut acquired = HashSet::new();
        for input in builder.reservation_inputs().collect::<HashSet<_>>() {
            if !self.notes.contains_key(&input) {
                acquired.insert(input);
            }
            let entry = self.notes.entry(input).or_insert(Reservation {
                owner: new_owner,
                age: 0,
                funded: true,
                active_jobs: 0,
                delivered: false,
                handoffs: Vec::new(),
            });
            entry.reconcile_handoffs();
            entry.active_jobs += 1;
            job.push((input, entry.owner));
        }
        if !acquired.is_empty() {
            self.groups.insert(new_owner, acquired);
        }
        Ok(job)
    }

    fn finish_job(&mut self, job: ReservationJob, delivered: bool) {
        for (input, owner) in job {
            let Some(entry) = self
                .notes
                .get_mut(&input)
                .filter(|entry| entry.owner == owner && entry.active_jobs > 0)
            else {
                continue;
            };
            entry.active_jobs -= 1;
            entry.reconcile_handoffs();
            entry.delivered |= delivered;
            if entry.active_jobs == 0 && !entry.has_delivery() {
                self.notes.remove(&input);
            }
        }
        self.forget_empty_groups();
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "The completion transfers its handoff marker into reservation tracking"
    )]
    fn finish_handoff(&mut self, job: ReservationJob, enqueued: bool, handoff: ReservationHandoff) {
        if !enqueued {
            if handoff.cancelled() {
                self.cancel_job_before_dispatch(job, handoff.owner);
            } else {
                self.finish_job(job, false);
            }
            return;
        }
        for (input, owner) in job {
            let Some(entry) = self
                .notes
                .get_mut(&input)
                .filter(|entry| entry.owner == owner && entry.active_jobs > 0)
            else {
                continue;
            };
            entry.active_jobs -= 1;
            entry.reconcile_handoffs();
            if !handoff.cancelled() {
                entry.handoffs.push(handoff.clone());
            } else if owner == handoff.owner && entry.active_jobs == 0 && !entry.has_delivery() {
                self.notes.remove(&input);
            }
        }
        self.forget_empty_groups();
    }

    fn cancel_job_before_dispatch(&mut self, job: ReservationJob, new_owner: [u8; 32]) {
        for (input, owner) in job {
            let Some(entry) = self
                .notes
                .get_mut(&input)
                .filter(|entry| entry.owner == owner && entry.active_jobs > 0)
            else {
                continue;
            };
            entry.active_jobs -= 1;
            if owner == new_owner && entry.active_jobs == 0 {
                self.notes.remove(&input);
            }
        }
        self.forget_empty_groups();
    }
    fn reserve(
        &mut self,
        nullifiers: impl IntoIterator<Item = (NoteSetIndex, NoteNullifier)>,
        owner: [u8; 32],
    ) -> Result<(), WalletError> {
        let inputs = nullifiers.into_iter().collect::<HashSet<_>>();
        if inputs.iter().any(|input| self.notes.contains_key(input)) {
            return Err(WalletError::InputReserved);
        }
        if self.groups.contains_key(&owner) {
            return Err(WalletError::InputReserved);
        }
        for (note_set, nullifier) in inputs.iter().copied() {
            debug!(
                target: wallet::SERVICE,
                ?nullifier,
                "Reserved pending note"
            );
            self.notes.insert(
                (note_set, nullifier),
                Reservation {
                    owner,
                    age: 0,
                    funded: false,
                    active_jobs: 0,
                    delivered: false,
                    handoffs: Vec::new(),
                },
            );
        }
        if !inputs.is_empty() {
            self.groups.insert(owner, inputs);
        }
        Ok(())
    }

    fn release(
        &mut self,
        nullifiers: impl IntoIterator<Item = (NoteSetIndex, NoteNullifier)>,
        owner: [u8; 32],
    ) {
        for (note_set, nullifier) in nullifiers {
            let input = (note_set, nullifier);
            let Some(entry) = self
                .notes
                .get_mut(&input)
                .filter(|entry| entry.owner == owner)
            else {
                continue;
            };
            if entry.active_jobs > 0 {
                // An explicit release abandons earlier results. A later
                // successful completion still keeps the input
                // reserved for that operation.
                entry.delivered = false;
                entry.handoffs.clear();
            } else {
                self.notes.remove(&input);
                debug!(
                    target: wallet::SERVICE,
                    ?nullifier,
                    "Released pending note reservation"
                );
            }
        }
        self.forget_empty_groups();
    }

    fn forget_empty_groups(&mut self) {
        self.groups
            .retain(|owner, _| self.notes.values().any(|entry| entry.owner == *owner));
    }

    fn nullifiers(&self, note_set: NoteSetIndex) -> HashSet<NoteNullifier> {
        self.notes
            .keys()
            .filter_map(|(set, nullifier)| (*set == note_set).then_some(*nullifier))
            .collect()
    }

    /// Evict reservations whose LIB-progress age reached the configured limit.
    ///
    /// Each LIB update adds `new_immutable_blocks_count` to every reservation's
    /// counter. A reservation expires once that counter reaches
    /// `max_immutable_blocks_since_reservation`.
    fn evict_expired(
        &mut self,
        new_immutable_blocks_count: u64,
        max_immutable_blocks_since_reservation: u64,
    ) {
        if new_immutable_blocks_count == 0 {
            return;
        }

        self.notes.retain(|nullifier, reservation| {
            reservation.age = reservation.age.saturating_add(new_immutable_blocks_count);

            let expired = reservation.age >= max_immutable_blocks_since_reservation;

            if expired {
                debug!(
                    target: wallet::SERVICE,
                    ?nullifier,
                    immutable_blocks_since_reservation = reservation.age,
                    max_immutable_blocks_since_reservation,
                    "Removing pending note reservation after LIB progress"
                );
            }

            !expired
        });
        self.forget_empty_groups();
    }
}

// Prevent older commitment-keyed checkpoints from being read as set-aware
// state. Their memberships may already have been conflated.
const WALLET_SNAPSHOT_FORMAT: u64 = 0x5345_5457_414c_4c32;

fn snapshot_format<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let format = u64::deserialize(deserializer)?;
    if format != WALLET_SNAPSHOT_FORMAT {
        return Err(serde::de::Error::custom(
            "unsupported wallet checkpoint format",
        ));
    }
    Ok(format)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    #[serde(deserialize_with = "snapshot_format")]
    format: u64,
    /// [`WalletState`] at the last known LIB.
    /// `None` until the first recovery state update.
    lib_wallet_state: Option<(HeaderId, WalletState, WalletState)>,
}

impl RecoveryState {
    pub const fn take_lib_wallet_state(&mut self) -> Option<(HeaderId, WalletState, WalletState)> {
        self.lib_wallet_state.take()
    }
}

/// Authoritative opening and reward records loaded from durable storage.
pub struct RecoveredOpenings {
    pub spending: KnownNotes,
    pub leadership: KnownNotes,
    pub reward_key_count: u64,
}

#[cfg(test)]
impl Default for RecoveredOpenings {
    fn default() -> Self {
        Self {
            spending: KnownNotes::new_sync(),
            leadership: KnownNotes::new_sync(),
            reward_key_count: 0,
        }
    }
}

impl overwatch::services::state::ServiceState for RecoveryState {
    type Settings = WalletServiceSettings;
    type Error = WalletServiceError;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            format: WALLET_SNAPSHOT_FORMAT,
            lib_wallet_state: None,
        })
    }
}

/// Provides operations on the states that must be synced to [`RecoveryState`].
#[derive(Clone)]
pub struct ServiceState<'u> {
    next_reward_key_index: u64,
    wallet: Wallet,
    leadership_wallet: Wallet,
    leadership_keys: HashMap<LeadershipPublicKeys, LeadershipKeyIds>,
    lib: HeaderId,
    updater: &'u StateUpdater<Option<RecoveryState>>,
    pending_notes: PendingNotes,
    pending_note_expiry_blocks: u64,
    fee_margin_percent: u64,
}

impl<'u> ServiceState<'u> {
    // Restores the wallet persisted at its LIB.
    pub fn from_lib_wallet_state(
        records: RecoveredOpenings,
        lib: HeaderId,
        mut wallet_state: WalletState,
        leadership_state: WalletState,
        settings: &WalletServiceSettings,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Self {
        let RecoveredOpenings {
            spending: mut known_notes,
            leadership: leadership_known_notes,
            reward_key_count,
        } = records;
        known_notes = known_notes
            .iter()
            .filter(|(_, known)| known.note.public_key != *PK0)
            .map(|(id, known)| (*id, *known))
            .collect();
        wallet_state.notes = wallet_state
            .notes
            .iter()
            .filter(|(_, known)| known.note.public_key != *PK0)
            .map(|(commitment, known)| (*commitment, known.clone()))
            .collect();
        let mut leadership_wallet =
            Wallet::from_lib_wallet_state([], leadership_known_notes, lib, leadership_state);
        leadership_wallet
            .forget_final_known_notes(lib)
            .expect("leadership state at LIB must exist");
        let mut wallet =
            Wallet::from_lib_wallet_state(known_keys(settings), known_notes, lib, wallet_state);
        wallet
            .forget_final_known_notes(lib)
            .expect("wallet state at LIB must exist");
        Self::with_wallet(
            wallet,
            leadership_wallet,
            lib,
            reward_key_count,
            settings,
            updater,
        )
    }

    // A fresh wallet follows the note sets from the genesis block.
    #[expect(
        clippy::too_many_arguments,
        reason = "Genesis initializes both tracked note collections"
    )]
    pub fn from_genesis(
        records: RecoveredOpenings,
        settings: &WalletServiceSettings,
        genesis: HeaderId,
        genesis_notes: &[Note],
        genesis_service_notes: &[Note],
        genesis_known_notes: impl IntoIterator<Item = KnownNote>,
        genesis_leadership_notes: impl IntoIterator<Item = KnownNote>,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Result<Self, WalletError> {
        let RecoveredOpenings {
            spending: mut known_notes,
            leadership: mut leadership_known_notes,
            reward_key_count,
        } = records;
        for known in genesis_known_notes {
            known_notes.insert_mut(known.id(), known);
        }
        known_notes = known_notes
            .iter()
            .filter(|(_, known)| known.note.public_key != *PK0)
            .map(|(id, known)| (*id, *known))
            .collect();
        for known in genesis_leadership_notes {
            leadership_known_notes.insert_mut(known.id(), known);
        }
        let leadership_wallet = Wallet::from_genesis(
            [],
            leadership_known_notes,
            genesis,
            genesis_notes,
            genesis_service_notes,
        )?;
        let wallet = Wallet::from_genesis(
            known_keys(settings),
            known_notes,
            genesis,
            genesis_notes,
            genesis_service_notes,
        )?;
        Ok(Self::with_wallet(
            wallet,
            leadership_wallet,
            genesis,
            reward_key_count,
            settings,
            updater,
        ))
    }

    fn with_wallet(
        wallet: Wallet,
        leadership_wallet: Wallet,
        lib: HeaderId,
        next_reward_key_index: u64,
        settings: &WalletServiceSettings,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Self {
        Self {
            next_reward_key_index,
            wallet,
            leadership_wallet,
            leadership_keys: settings
                .leadership_keys
                .iter()
                .map(|credential| (credential.public, credential.keys.clone()))
                .collect(),
            lib,
            updater,
            pending_notes: PendingNotes::default(),
            pending_note_expiry_blocks: settings.pending_note_expiry_blocks,
            fee_margin_percent: settings.fee_margin_percent,
        }
    }

    // the priority fee percentage of a funding that names none
    pub const fn fee_margin_percent(&self) -> u64 {
        self.fee_margin_percent
    }

    pub const fn lib(&self) -> HeaderId {
        self.lib
    }

    // the number of reward keys derived so far
    pub const fn reward_key_count(&self) -> u64 {
        self.next_reward_key_index
    }

    pub async fn store_reward_key_count<Tx>(
        &self,
        storage: &StorageApi<Tx>,
    ) -> Result<(), WalletServiceError> {
        storage
            .store_wallet_reward_key_count(self.next_reward_key_index)
            .await
            .map_err(WalletServiceError::PersistedState)
    }

    pub const fn next_reward_key_index(&mut self) -> u64 {
        let index = self.next_reward_key_index;
        self.next_reward_key_index = index.saturating_add(1);
        index
    }

    pub fn add_known_key(&mut self, pk: ZkPublicKey, key_id: KeyId) {
        self.wallet.add_known_key(pk, key_id);
    }

    /// Persist spendable openings before exposing them to callers.
    pub async fn store_known_notes<Tx>(
        &mut self,
        storage: &StorageApi<Tx>,
        notes: &[KnownNote],
    ) -> Result<(), WalletServiceError> {
        let leadership = notes
            .iter()
            .copied()
            .filter(|known| {
                self.leadership_keys.contains_key(&LeadershipPublicKeys {
                    nullifier_public_key: known.note.public_key,
                    staking_public_key: known.note.stake_public_key,
                })
            })
            .collect::<Vec<_>>();
        storage
            .store_wallet_openings(notes, &leadership)
            .await
            .map_err(WalletServiceError::PersistedState)?;
        self.add_known_notes(notes.iter().copied());
        Ok(())
    }

    pub fn add_leadership_key(&mut self, key_id: LeadershipKeyIds, public: LeadershipPublicKeys) {
        self.leadership_keys.insert(public, key_id);
    }

    pub fn add_known_notes(&mut self, notes: impl IntoIterator<Item = KnownNote>) {
        let notes: Vec<_> = notes.into_iter().collect();
        self.leadership_wallet
            .add_known_notes(notes.iter().copied().filter(|known| {
                self.leadership_keys.contains_key(&LeadershipPublicKeys {
                    nullifier_public_key: known.note.public_key,
                    staking_public_key: known.note.stake_public_key,
                })
            }));
        self.wallet.add_known_notes(
            notes
                .into_iter()
                .filter(|known| known.note.public_key != *PK0),
        );
    }

    pub fn add_leadership_notes(&mut self, notes: impl IntoIterator<Item = KnownNote>) {
        self.leadership_wallet.add_known_notes(notes);
        self.update_state();
    }

    pub const fn leadership_wallet(&self) -> &Wallet {
        &self.leadership_wallet
    }

    pub const fn leadership_keys(&self) -> &HashMap<LeadershipPublicKeys, LeadershipKeyIds> {
        &self.leadership_keys
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        self.wallet
            .apply_block_pair(&mut self.leadership_wallet, block)?;
        // Note reservations are released after `pending_note_expiry_blocks`
        // (see `evict_expired`), not on spent. The recovery state, the LIB's
        // wallet state, stays as it is.
        Ok(())
    }

    pub fn advance_lib(&mut self, new_lib: HeaderId, new_immutable_blocks_count: u64) {
        // The wallet must already hold a `WalletState` for `new_lib`, otherwise
        // `update_state` cannot persist the recovery state. The service
        // backfills up to `new_lib` before calling us; if it still
        // isn't present (e.g. the block is not yet in storage) we skip
        // advancing rather than panicking, keeping `self.lib` pointing
        // at a block whose state exists. A later block application or
        // LIB update advances the LIB once the gap is filled.
        if !self.wallet.has_processed_block(new_lib)
            || !self.leadership_wallet.has_processed_block(new_lib)
        {
            warn!(
                target: wallet::SERVICE,
                ?new_lib,
                current_lib = ?self.lib,
                "Skipping LIB advance: wallet has not applied the new LIB block yet"
            );
            return;
        }

        self.lib = new_lib;
        self.wallet.prune_states(new_lib);
        // The notes final at the new LIB are never adopted again.
        self.wallet
            .forget_final_known_notes(new_lib)
            .expect("WalletState at LIB must exist");
        self.leadership_wallet.prune_states(new_lib);
        self.leadership_wallet
            .forget_final_known_notes(new_lib)
            .expect("Leadership state at LIB must exist");
        self.pending_notes
            .evict_expired(new_immutable_blocks_count, self.pending_note_expiry_blocks);
        self.update_state();
    }

    pub const fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    /// Fund `tx_builder` from the wallet's notes at `tip`, excluding notes
    /// already reserved for in-flight transactions. `priority_fee` reserves
    /// its percentage of the final mandatory fee, including both execution and
    /// storage cost, within its maximum fee. Only the unused reserve becomes an
    /// effective priority tip, and the percentage is not capped at 100. A
    /// failed attempt preserves the builder's reservations for a retry. Stale
    /// or conflicting ownership claims cannot release another operation's
    /// reservations.
    #[expect(
        clippy::too_many_arguments,
        reason = "thin passthrough to `Wallet::fund_tx` plus the reservations"
    )]
    pub fn fund_tx(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &OpsContext,
        priority_fee: impl Into<PriorityFee>,
    ) -> Result<MantleTxBuilder, WalletError> {
        self.pending_notes.validate_claims(tx_builder, true)?;
        self.wallet.fund_tx(
            tip,
            tx_builder,
            change_pk,
            change_nonce,
            funding_pks,
            context,
            &self.pending_notes.nullifiers(NoteSetIndex::Ledger),
            priority_fee,
        )
    }

    // A Transfer giving `owner` a note of exactly `value` and the rest back to
    // `owner`, from the wallet's notes at `tip` that neither `tx_builder` nor
    // an in flight transaction spends.
    #[expect(
        clippy::too_many_arguments,
        reason = "thin passthrough to `WalletState::exact_note_split` plus the reservations"
    )]
    pub fn exact_note_split(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        value: Value,
        owner: ZkPublicKey,
        nonce: Fr,
        change_nonce: Fr,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
    ) -> Result<(TransferOp, ZkTransferOpening), WalletError> {
        let excluded = tx_builder
            .nullifiers_consumed_in(NoteSetIndex::Ledger)
            .chain(self.pending_notes.nullifiers(NoteSetIndex::Ledger))
            .collect::<HashSet<_>>();
        self.wallet.spend_state(tip)?.exact_note_split(
            value,
            owner,
            nonce,
            owner,
            change_nonce,
            funding_pks,
            &excluded,
        )
    }

    pub fn note_set_spend(
        &self,
        tip: HeaderId,
        spend: NoteSetSpend,
    ) -> Result<(InputNullifiers, ZkTransferOpening, Fr), WalletError> {
        let note_set = spend.note_set;
        self.wallet.spend_state(tip)?.note_set_spend(
            spend,
            self.wallet.known_notes(),
            &self.pending_notes.nullifiers(note_set),
        )
    }

    pub fn is_pending_note(&self, note_set: NoteSetIndex, nullifier: &NoteNullifier) -> bool {
        self.pending_notes
            .notes
            .contains_key(&(note_set, *nullifier))
    }

    pub fn reserve_pending_notes(
        &mut self,
        nullifiers: impl IntoIterator<Item = (NoteSetIndex, NoteNullifier)>,
        owner: [u8; 32],
    ) -> Result<(), WalletError> {
        self.pending_notes.reserve(nullifiers, owner)
    }

    pub fn release_pending_notes(
        &mut self,
        nullifiers: impl IntoIterator<Item = (NoteSetIndex, NoteNullifier)>,
        owner: [u8; 32],
    ) {
        self.pending_notes.release(nullifiers, owner);
    }

    /// Request release of every live input controlled by this capability.
    pub fn release_reservation(&mut self, owner: [u8; 32]) -> bool {
        let inputs = self
            .pending_notes
            .notes
            .iter()
            .filter_map(|(input, reservation)| (reservation.owner == owner).then_some(*input))
            .collect::<Vec<_>>();
        let found = !inputs.is_empty();
        self.pending_notes.release(inputs, owner);
        found
    }

    pub fn validate_funding(&self, builder: &MantleTxBuilder) -> Result<(), WalletError> {
        self.pending_notes.validate_claims(builder, true)
    }

    pub fn transfer_reservations(
        &mut self,
        builder: &MantleTxBuilder,
        owner: [u8; 32],
        funded: bool,
    ) -> Result<(), WalletError> {
        self.pending_notes.transfer(builder, owner, funded)
    }

    pub fn release_transaction_notes(&mut self, builder: &MantleTxBuilder) {
        let inputs = builder.reservation_inputs().collect::<Vec<_>>();
        for owner in builder.reservation_ids() {
            self.pending_notes.release(inputs.iter().copied(), owner);
        }
    }

    pub fn begin_transaction_job(
        &mut self,
        builder: &MantleTxBuilder,
        owner: [u8; 32],
    ) -> Result<ReservationJob, WalletError> {
        self.pending_notes.begin_job(builder, owner)
    }

    #[cfg(test)]
    pub fn finish_transaction_job(&mut self, job: ReservationJob, delivered: bool) {
        self.pending_notes.finish_job(job, delivered);
    }

    pub fn finish_transaction_handoff(
        &mut self,
        job: ReservationJob,
        enqueued: bool,
        handoff: ReservationHandoff,
    ) {
        self.pending_notes.finish_handoff(job, enqueued, handoff);
    }

    pub fn cancel_transaction_job(&mut self, job: ReservationJob, new_owner: [u8; 32]) {
        self.pending_notes
            .cancel_job_before_dispatch(job, new_owner);
    }

    fn update_state(&self) {
        self.updater.update(Some(self.snapshot()));
    }

    pub fn snapshot(&self) -> RecoveryState {
        let lib_wallet_state = self
            .wallet()
            .wallet_state_at(self.lib)
            .expect("WalletState at LIB must exist");

        RecoveryState {
            format: WALLET_SNAPSHOT_FORMAT,
            lib_wallet_state: Some((
                self.lib,
                lib_wallet_state,
                self.leadership_wallet
                    .wallet_state_at(self.lib)
                    .expect("Leadership state at LIB must exist"),
            )),
        }
    }
}

fn known_keys(settings: &WalletServiceSettings) -> impl Iterator<Item = (ZkPublicKey, KeyId)> {
    settings
        .known_keys
        .iter()
        .filter(|(_, pk)| **pk != *PK0)
        .map(|(key_id, pk)| (*pk, key_id.clone()))
}

#[cfg(test)]
mod tests {
    use lb_core::mantle::notes::NoteCommitment;
    use lb_groth16::Field as _;

    use super::*;
    use crate::{
        DEFAULT_FEE_MARGIN_PERCENT,
        tests::{GAS_PROFILE, era_schedule},
    };

    const EXPIRY_BLOCKS: u64 = 4;

    #[test]
    fn failed_leadership_transition_does_not_advance_spending_wallet() {
        use std::sync::Arc;

        use lb_services_utils::overwatch::RecoveryData;

        let settings = WalletServiceSettings {
            known_keys: HashMap::new(),
            leadership_keys: Vec::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, _) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = ServiceState::from_genesis(
            RecoveredOpenings::default(),
            &settings,
            genesis,
            &[],
            &[],
            [],
            [],
            &updater,
        )
        .unwrap();
        let first = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 0.into(),
            updates: vec![],
            revealed: vec![],
            withdrawals: vec![],
        };
        state.wallet.apply_block(&first).unwrap();
        let next = WalletBlock {
            id: HeaderId::from([2; 32]),
            parent: first.id,
            epoch: 0.into(),
            updates: vec![],
            revealed: vec![],
            withdrawals: vec![],
        };
        assert!(state.apply_block(&next).is_err());
        assert!(!state.wallet.has_processed_block(next.id));
        assert!(!state.leadership_wallet.has_processed_block(next.id));
        assert!(state.wallet.has_processed_block(first.id));
        assert!(!state.leadership_wallet.has_processed_block(first.id));
    }

    #[test]
    fn older_checkpoint_format_is_rejected() {
        use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

        #[derive(Serialize)]
        struct PreviousCheckpoint {
            lib_wallet_state: Option<(HeaderId, WalletState)>,
        }
        let old = PreviousCheckpoint {
            lib_wallet_state: None,
        }
        .to_bytes()
        .unwrap();
        assert!(RecoveryState::from_bytes(&old).is_err());
    }

    #[test]
    fn checkpoint_requires_both_wallet_states_and_excludes_opening_records() {
        use std::sync::Arc;

        use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
        use lb_services_utils::overwatch::RecoveryData;

        let settings = WalletServiceSettings {
            known_keys: HashMap::new(),
            leadership_keys: Vec::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, _) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = ServiceState::from_genesis(
            RecoveredOpenings::default(),
            &settings,
            genesis,
            &[],
            &[],
            [],
            [],
            &updater,
        )
        .unwrap();
        let before = state.snapshot().to_bytes().unwrap();
        let known = KnownNote {
            note: Note::new(7, Fr::from(9u64), ZkPublicKey::new(Fr::from(3u64))),
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(5u64)),
        };
        state.add_known_notes([known]);
        assert_eq!(before, state.snapshot().to_bytes().unwrap());
        assert!(RecoveryState::from_bytes(&before[..before.len() - 1]).is_err());
    }

    #[test]
    fn pending_note_does_not_expire_without_lib_progress() {
        let nullifier = NoteNullifier(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes
            .reserve([(NoteSetIndex::Ledger, nullifier)], [1; 32])
            .unwrap();
        pending_notes.evict_expired(0, EXPIRY_BLOCKS);

        assert!(
            pending_notes
                .nullifiers(NoteSetIndex::Ledger)
                .contains(&nullifier)
        );
    }

    #[test]
    fn pending_note_expires_after_enough_lib_progress() {
        let nullifier = NoteNullifier(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes
            .reserve([(NoteSetIndex::Ledger, nullifier)], [1; 32])
            .unwrap();
        pending_notes.evict_expired(EXPIRY_BLOCKS - 1, EXPIRY_BLOCKS);
        assert!(
            pending_notes
                .nullifiers(NoteSetIndex::Ledger)
                .contains(&nullifier)
        );

        pending_notes.evict_expired(1, EXPIRY_BLOCKS);
        assert!(
            !pending_notes
                .nullifiers(NoteSetIndex::Ledger)
                .contains(&nullifier)
        );
    }

    #[test]
    fn released_pending_note_becomes_available() {
        let nullifier = NoteNullifier(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes
            .reserve([(NoteSetIndex::Ledger, nullifier)], [1; 32])
            .unwrap();
        pending_notes.release([(NoteSetIndex::Ledger, nullifier)], [1; 32]);

        assert!(pending_notes.nullifiers(NoteSetIndex::Ledger).is_empty());
    }

    #[test]
    fn reservations_are_independent_across_note_sets() {
        let mut pending = PendingNotes::default();
        let nullifier = NoteNullifier(Fr::ONE);
        pending
            .reserve([(NoteSetIndex::Ledger, nullifier)], [1; 32])
            .unwrap();
        assert!(
            pending
                .reserve([(NoteSetIndex::Ledger, nullifier)], [9; 32])
                .is_err()
        );
        assert_eq!(
            pending.notes[&(NoteSetIndex::Ledger, nullifier)].owner,
            [1; 32]
        );
        pending
            .reserve([(NoteSetIndex::Sdp, nullifier)], [2; 32])
            .unwrap();
        pending.release([(NoteSetIndex::Sdp, nullifier)], [2; 32]);
        assert!(
            pending
                .nullifiers(NoteSetIndex::Ledger)
                .contains(&nullifier)
        );
        assert!(!pending.nullifiers(NoteSetIndex::Sdp).contains(&nullifier));
        pending
            .reserve([(NoteSetIndex::Sdp, nullifier)], [3; 32])
            .unwrap();
        pending.release([(NoteSetIndex::Ledger, nullifier)], [1; 32]);
        assert!(pending.nullifiers(NoteSetIndex::Sdp).contains(&nullifier));
    }

    #[test]
    fn stale_owners_cannot_release_a_transferred_or_reacquired_input() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let nullifier = NoteNullifier(Fr::ONE);
        let input = (NoteSetIndex::Ledger, nullifier);
        let old = [1; 32];
        let new = [2; 32];
        let builder = MantleTxBuilder::new()
            .with_reservation_id(old)
            .push_op(Op::Transfer(TransferOp::new(
                [nullifier].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending.reserve([input], old).unwrap();
        pending.transfer(&builder, new, true).unwrap();
        assert!(pending.validate_claims(&builder, true).is_err());
        pending.release([input], old);
        assert!(pending.notes.contains_key(&input));
        pending.evict_expired(EXPIRY_BLOCKS, EXPIRY_BLOCKS);
        pending.reserve([input], [3; 32]).unwrap();
        pending.release([input], new);
        assert_eq!(pending.notes[&input].owner, [3; 32]);
    }

    #[test]
    fn incomplete_owner_group_cannot_reacquire_a_released_member() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let first = NoteNullifier(Fr::ONE);
        let second = NoteNullifier(Fr::from(2u64));
        let owner = [1; 32];
        let builder = MantleTxBuilder::new()
            .with_reservation_id(owner)
            .push_op(Op::Transfer(TransferOp::new(
                [first, second].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending
            .reserve(
                [
                    (NoteSetIndex::Ledger, first),
                    (NoteSetIndex::Ledger, second),
                ],
                owner,
            )
            .unwrap();
        pending.release([(NoteSetIndex::Ledger, first)], owner);
        assert!(pending.transfer(&builder, [2; 32], true).is_err());
        assert!(!pending.notes.contains_key(&(NoteSetIndex::Ledger, first)));
        assert_eq!(pending.notes[&(NoteSetIndex::Ledger, second)].owner, owner);
    }

    #[test]
    fn multiple_claims_transfer_atomically() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let first = NoteNullifier(Fr::ONE);
        let second = NoteNullifier(Fr::from(2u64));
        let builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .with_additional_reservation_id([2; 32])
            .push_op(Op::Transfer(TransferOp::new(
                [first, second].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending
            .reserve([(NoteSetIndex::Ledger, first)], [1; 32])
            .unwrap();
        assert!(pending.transfer(&builder, [3; 32], true).is_err());
        assert_eq!(pending.notes[&(NoteSetIndex::Ledger, first)].owner, [1; 32]);
        pending
            .reserve([(NoteSetIndex::Ledger, second)], [2; 32])
            .unwrap();
        pending.transfer(&builder, [3; 32], true).unwrap();
        assert_eq!(pending.notes[&(NoteSetIndex::Ledger, first)].owner, [3; 32]);
        assert_eq!(
            pending.notes[&(NoteSetIndex::Ledger, second)].owner,
            [3; 32]
        );
        pending.release([(NoteSetIndex::Ledger, first)], [1; 32]);
        assert!(pending.notes.contains_key(&(NoteSetIndex::Ledger, first)));
    }

    #[test]
    fn funding_resets_all_transferred_reservation_ages() {
        use lb_core::mantle::Op;

        let first = NoteNullifier(Fr::ONE);
        let second = NoteNullifier(Fr::from(2u64));
        let newly_funded = NoteNullifier(Fr::from(3u64));
        let builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .with_additional_reservation_id([2; 32])
            .push_op(Op::Transfer(TransferOp::new(
                [first, second, newly_funded].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        let first_input = (NoteSetIndex::Ledger, first);
        let second_input = (NoteSetIndex::Ledger, second);
        let new_input = (NoteSetIndex::Ledger, newly_funded);
        pending.reserve([first_input], [1; 32]).unwrap();
        pending.evict_expired(1, EXPIRY_BLOCKS);
        pending.reserve([second_input], [2; 32]).unwrap();
        pending.evict_expired(2, EXPIRY_BLOCKS);

        // A conflicting target owner rejects the whole transition without
        // refreshing either existing claim or acquiring the new input.
        assert!(pending.transfer(&builder, [1; 32], true).is_err());
        assert_eq!(pending.notes[&first_input].age, 3);
        assert_eq!(pending.notes[&second_input].age, 2);
        assert!(!pending.notes.contains_key(&new_input));

        pending.transfer(&builder, [3; 32], true).unwrap();
        for input in [first_input, second_input, new_input] {
            assert_eq!(pending.notes[&input].owner, [3; 32]);
            assert_eq!(pending.notes[&input].age, 0);
        }
        assert!(pending.validate_claims(&builder, true).is_err());
        pending.evict_expired(EXPIRY_BLOCKS - 1, EXPIRY_BLOCKS);
        assert_eq!(pending.notes.len(), 3);
        pending.evict_expired(1, EXPIRY_BLOCKS);
        assert!(pending.notes.is_empty());
    }

    #[test]
    fn unfunded_transfer_does_not_refresh_reservation_age() {
        use lb_core::mantle::Op;

        let nullifier = NoteNullifier(Fr::ONE);
        let input = (NoteSetIndex::Ledger, nullifier);
        let builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .push_op(Op::Transfer(TransferOp::new(
                [nullifier].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending.reserve([input], [1; 32]).unwrap();
        pending.evict_expired(EXPIRY_BLOCKS - 1, EXPIRY_BLOCKS);
        pending.transfer(&builder, [2; 32], false).unwrap();
        assert_eq!(pending.notes[&input].age, EXPIRY_BLOCKS - 1);
        pending.evict_expired(1, EXPIRY_BLOCKS);
        assert!(pending.notes.is_empty());
    }

    #[test]
    fn overlapping_jobs_and_stale_completions_do_not_release_live_reservations() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let nullifier = NoteNullifier(Fr::ONE);
        let input = (NoteSetIndex::Ledger, nullifier);
        let builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .push_op(Op::Transfer(TransferOp::new(
                [nullifier].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending.reserve([input], [1; 32]).unwrap();
        let first = pending.begin_job(&builder, [2; 32]).unwrap();
        let second = pending.begin_job(&builder, [3; 32]).unwrap();
        pending.finish_job(first, false);
        assert!(pending.notes.contains_key(&input));
        pending.finish_job(second, true);
        assert!(pending.notes.contains_key(&input));
        let stale = pending.begin_job(&builder, [4; 32]).unwrap();
        pending.evict_expired(EXPIRY_BLOCKS, EXPIRY_BLOCKS);
        pending.reserve([input], [5; 32]).unwrap();
        pending.finish_job(stale, false);
        assert_eq!(pending.notes[&input].owner, [5; 32]);
    }

    #[test]
    fn cancelled_undispatched_job_preserves_existing_claims() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let first = NoteNullifier(Fr::ONE);
        let second = NoteNullifier(Fr::from(2u64));
        let builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .push_op(Op::Transfer(TransferOp::new(
                [first, second].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending
            .reserve([(NoteSetIndex::Ledger, first)], [1; 32])
            .unwrap();
        let job = pending.begin_job(&builder, [2; 32]).unwrap();
        pending.cancel_job_before_dispatch(job, [2; 32]);
        assert_eq!(pending.notes[&(NoteSetIndex::Ledger, first)].owner, [1; 32]);
        assert!(!pending.notes.contains_key(&(NoteSetIndex::Ledger, second)));
        assert!(pending.validate_claims(&builder, true).is_ok());
    }

    #[test]
    fn release_during_a_job_waits_and_preserves_later_success() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let nullifier = NoteNullifier(Fr::ONE);
        let input = (NoteSetIndex::Ledger, nullifier);
        let owner = [1; 32];
        let builder = MantleTxBuilder::new()
            .with_reservation_id(owner)
            .push_op(Op::Transfer(TransferOp::new(
                [nullifier].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending.reserve([input], owner).unwrap();
        let first = pending.begin_job(&builder, [2; 32]).unwrap();
        pending.finish_job(first, true);
        let second = pending.begin_job(&builder, [3; 32]).unwrap();
        pending.release([input], owner);
        assert!(pending.notes.contains_key(&input));
        pending.finish_job(second, false);
        assert!(!pending.notes.contains_key(&input));

        pending.reserve([input], owner).unwrap();
        let third = pending.begin_job(&builder, [4; 32]).unwrap();
        pending.release([input], owner);
        pending.finish_job(third, true);
        assert!(pending.notes.contains_key(&input));
    }

    #[test]
    fn delivered_proof_prevents_a_stale_prepared_builder_from_being_funded() {
        use lb_core::mantle::{Op, notes::NoteCommitment};

        let nullifier = NoteNullifier(Fr::ONE);
        let input = (NoteSetIndex::Ledger, nullifier);
        let owner = [1; 32];
        let builder = MantleTxBuilder::new()
            .with_reservation_id(owner)
            .push_op(Op::Transfer(TransferOp::new(
                [nullifier].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let mut pending = PendingNotes::default();
        pending.reserve([input], owner).unwrap();
        let job = pending.begin_job(&builder, [2; 32]).unwrap();
        pending.finish_job(job, true);
        assert!(pending.transfer(&builder, [3; 32], true).is_err());
        assert_eq!(pending.notes[&input].owner, owner);
    }

    #[test]
    fn sdp_reservation_does_not_block_ledger_fee_funding() {
        use std::sync::Arc;

        use lb_core::mantle::{
            Op,
            ops::sdp::SDPWithdrawOp,
            transactions::{GasPrices, tx_list::ops::OpsGasContext},
        };
        use lb_key_management_system_service::keys::ZkKey;
        use lb_services_utils::overwatch::RecoveryData;

        let key = ZkKey::new(Fr::from(7u64));
        let ledger = crate::tests::known_note(&key, 100_000, 1);
        let sdp = KnownNote {
            note_set: NoteSetIndex::Sdp,
            ..ledger
        };
        let owner = key.to_public_key();
        let settings = WalletServiceSettings {
            leadership_keys: Vec::new(),
            known_keys: HashMap::from([("owner".into(), owner)]),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = crate::tests::state_from_genesis(
            [(ledger.id(), ledger), (sdp.id(), sdp)]
                .into_iter()
                .collect(),
            0,
            &settings,
            genesis,
            &[ledger.note],
            &[sdp.note],
            &updater,
        )
        .unwrap();
        let returned = Note::new(ledger.note.value, Fr::from(2u64), owner);
        let builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .push_op_with_opening(
                Op::SDPWithdraw(SDPWithdrawOp {
                    service_note_nullifier: ledger.nullifier,
                    outputs: [returned.commitment()].into(),
                    excess_value: 0,
                    ..SDPWithdrawOp::sample()
                }),
                ZkTransferOpening {
                    inputs: vec![ledger.note],
                    outputs: vec![returned],
                },
            )
            .unwrap();
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(1, 1), GAS_PROFILE),
        };

        state
            .reserve_pending_notes([(NoteSetIndex::Sdp, ledger.nullifier)], [1; 32])
            .unwrap();
        let funded = state
            .fund_tx(
                genesis,
                &builder,
                owner,
                Fr::from(4u64),
                [owner],
                &context,
                0,
            )
            .unwrap();
        assert_eq!(funded.funding_inputs()[0].nullifier, ledger.nullifier);
        assert!(state.is_pending_note(NoteSetIndex::Sdp, &ledger.nullifier));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "The test follows the prepared reservation through failure, retry and abandonment"
    )]
    fn a_failed_funding_keeps_prepared_reservations_for_retry() {
        use std::sync::Arc;

        use lb_core::mantle::{
            Op,
            ops::sdp::SDPWithdrawOp,
            transactions::{GasPrices, tx_list::ops::OpsGasContext},
        };
        use lb_cryptarchia_engine::Epoch;
        use lb_key_management_system_service::keys::ZkKey;
        use lb_services_utils::overwatch::RecoveryData;

        let key = ZkKey::new(Fr::from(7u64));
        let funding_pk = key.to_public_key();
        let settings = WalletServiceSettings {
            leadership_keys: Vec::new(),
            known_keys: HashMap::from([("funding".into(), funding_pk)]),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = crate::tests::state_from_genesis(
            KnownNotes::new_sync(),
            0,
            &settings,
            genesis,
            &[],
            &[],
            &updater,
        )
        .unwrap();

        let split_input = NoteNullifier(Fr::ONE);
        let in_flight = NoteNullifier(Fr::from(2u64));
        state
            .reserve_pending_notes([(NoteSetIndex::Ledger, split_input)], [1; 32])
            .unwrap();
        state
            .reserve_pending_notes([(NoteSetIndex::Ledger, in_flight)], [2; 32])
            .unwrap();
        state.pending_notes.evict_expired(3, 10);
        let tx_builder = MantleTxBuilder::new()
            .with_reservation_id([1; 32])
            .push_op(Op::Transfer(TransferOp::new(
                [split_input].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(1, 1), GAS_PROFILE),
        };

        let error = state
            .fund_tx(
                genesis,
                &tx_builder,
                funding_pk,
                Fr::ONE,
                [funding_pk],
                &context,
                0,
            )
            .unwrap_err();

        assert_eq!(error, WalletError::InsufficientFunds { available: 0 });
        assert_eq!(
            state.pending_notes.nullifiers(NoteSetIndex::Ledger),
            HashSet::from([split_input, in_flight])
        );
        assert_eq!(
            state.pending_notes.notes[&(NoteSetIndex::Ledger, split_input)].age,
            3
        );

        let sdp = MantleTxBuilder::new()
            .with_reservation_id([3; 32])
            .push_op(Op::SDPWithdraw(SDPWithdrawOp {
                service_note_nullifier: in_flight,
                ..SDPWithdrawOp::sample()
            }))
            .unwrap();
        state
            .reserve_pending_notes([(NoteSetIndex::Sdp, in_flight)], [3; 32])
            .unwrap();
        assert!(
            state
                .fund_tx(
                    genesis,
                    &sdp,
                    funding_pk,
                    Fr::ONE,
                    [funding_pk],
                    &context,
                    0
                )
                .is_err()
        );
        assert!(state.is_pending_note(NoteSetIndex::Ledger, &in_flight));
        assert!(state.is_pending_note(NoteSetIndex::Sdp, &in_flight));
        assert_eq!(
            state.pending_notes.nullifiers(NoteSetIndex::Sdp),
            HashSet::from([in_flight])
        );

        // A later block supplies a Ledger funding note. The original builder
        // can be retried with the same reservation capability.
        let fee_note = crate::tests::known_note(&key, 100_000, 8);
        state.wallet.add_known_notes([fee_note]);
        let child = HeaderId::from([1; 32]);
        state
            .apply_block(&WalletBlock {
                id: child,
                parent: genesis,
                epoch: Epoch::new(0),
                revealed: vec![],
                withdrawals: vec![],
                updates: vec![lb_core::mantle::note_set::NoteSetUpdate {
                    note_set: NoteSetIndex::Ledger,
                    nullifiers: vec![],
                    commitments: vec![fee_note.note.commitment()],
                }],
            })
            .unwrap();
        let mut spend_tip = child;
        for number in 2..=5 {
            let next = HeaderId::from([number; 32]);
            state
                .apply_block(&WalletBlock {
                    id: next,
                    parent: spend_tip,
                    epoch: Epoch::new(0),
                    revealed: vec![],
                    withdrawals: vec![],
                    updates: vec![],
                })
                .unwrap();
            spend_tip = next;
        }
        let funded = state
            .fund_tx(
                spend_tip,
                &tx_builder,
                funding_pk,
                Fr::ONE,
                [funding_pk],
                &context,
                0,
            )
            .unwrap();
        state.transfer_reservations(&funded, [4; 32], true).unwrap();
        assert!(state.validate_funding(&tx_builder).is_err());
        assert!(state.is_pending_note(NoteSetIndex::Ledger, &split_input));
        assert!(state.is_pending_note(NoteSetIndex::Ledger, &in_flight));
        assert!(state.is_pending_note(NoteSetIndex::Sdp, &in_flight));
        assert!(state.release_reservation([3; 32]));
        assert!(matches!(
            state.fund_tx(
                spend_tip,
                &sdp,
                funding_pk,
                Fr::ONE,
                [funding_pk],
                &context,
                0
            ),
            Err(WalletError::InputReserved)
        ));
        assert!(state.is_pending_note(NoteSetIndex::Ledger, &in_flight));
    }

    /// Regression test for the wallet-service crash seen the first time a
    /// bootstrapping node switches to Online:
    ///
    /// During bootstrap the engine never advances the LIB, so no `LibUpdate`
    /// is broadcast and the wallet's `state.lib` stays at its starting point.
    /// `switch_to_online()` then jumps the chain LIB to the tip's k-th
    /// ancestor without broadcasting a `LibUpdate`. The first block processed
    /// afterwards broadcasts a `LibUpdate` whose `new_lib` can be a block the
    /// wallet has not applied yet (its frontier lags during IBD).
    ///
    /// The service backfills up to `new_lib` before advancing (see
    /// `handle_lib_update`). This test covers the `advance_lib` safety net for
    /// the case the block is still missing: it must skip the advance and keep
    /// the current LIB rather than panicking with
    /// "`WalletState` at LIB must exist: `UnknownBlock(...)`".
    #[test]
    fn lib_update_for_unapplied_block_skips_advance() {
        use std::sync::Arc;

        use lb_services_utils::overwatch::RecoveryData;

        let settings = WalletServiceSettings {
            leadership_keys: Vec::new(),
            known_keys: HashMap::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };

        // Fresh wallet: initialized from the genesis note sets.
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));

        let mut state = crate::tests::state_from_genesis(
            KnownNotes::new_sync(),
            0,
            &settings,
            genesis,
            &[],
            &[],
            &updater,
        )
        .unwrap();

        // The first post-online LibUpdate delivers a new_lib the wallet never
        // applied (the silent bootstrap->online LIB jump outran the wallet).
        let unseen_lib = HeaderId::from([42; 32]);
        state.advance_lib(unseen_lib, 1);

        // No panic: the advance is skipped and the LIB stays at a block whose
        // WalletState exists.
        assert_eq!(state.lib(), genesis);
    }
    #[test]
    #[expect(
        clippy::too_many_lines,
        clippy::cognitive_complexity,
        reason = "The lifecycle covers both forks and recovery"
    )]
    fn leadership_notes_follow_spends_forks_and_recovery_without_becoming_spendable() {
        use std::sync::Arc;

        use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};
        use lb_core::{crypto::ZkHasher, mantle::note_set::NoteSetUpdate};
        use lb_key_management_system_service::keys::kdf_public_key;
        use lb_services_utils::overwatch::RecoveryData;
        use tokio::sync::watch;

        use crate::LeadershipCredentials;
        let public = LeadershipPublicKeys {
            nullifier_public_key: kdf_public_key(&Fr::from(7u64)),
            staking_public_key: kdf_public_key(&Fr::from(9u64)),
        };
        let note = Note {
            stake_public_key: public.staking_public_key,
            ..Note::new(10, Fr::ONE, public.nullifier_public_key)
        };
        let known = KnownNote {
            note,
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(3u64)),
        };
        let service_known = KnownNote {
            note_set: NoteSetIndex::Sdp,
            ..known
        };
        let settings = WalletServiceSettings {
            known_keys: HashMap::new(),
            leadership_keys: vec![LeadershipCredentials {
                keys: LeadershipKeyIds {
                    nullifier: "nf".into(),
                    staking: "stake".into(),
                },
                public,
            }],
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, receiver) = watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = ServiceState::from_genesis(
            RecoveredOpenings::default(),
            &settings,
            genesis,
            &[note],
            &[note],
            [],
            [known, service_known],
            &updater,
        )
        .unwrap();
        assert!(
            state
                .wallet()
                .balance(genesis, note.public_key)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state
                .leadership_wallet()
                .wallet_state_at(genesis)
                .unwrap()
                .aged_notes()
                .unwrap()
                .len(),
            2
        );
        let pending = KnownNote {
            note: Note {
                nonce: Fr::from(20u64),
                ..note
            },
            nullifier: NoteNullifier(Fr::from(21u64)),
            ..known
        };
        let mutable_only = KnownNote {
            note: Note {
                nonce: Fr::from(22u64),
                ..note
            },
            nullifier: NoteNullifier(Fr::from(23u64)),
            ..known
        };
        state.add_leadership_notes([pending, mutable_only]);
        let genesis_state = state.leadership_wallet().wallet_state_at(genesis).unwrap();
        let block = |id, nullifiers| WalletBlock {
            id: HeaderId::from([id; 32]),
            parent: genesis,
            epoch: 0.into(),
            updates: vec![NoteSetUpdate {
                note_set: NoteSetIndex::Ledger,
                nullifiers,
                commitments: vec![],
            }],
            revealed: vec![],
            withdrawals: vec![],
        };
        let spent = block(1, vec![known.nullifier]);
        let mut fork = block(2, vec![]);
        fork.updates[0]
            .commitments
            .push(mutable_only.note.commitment());
        state.apply_block(&spent).unwrap();
        state.apply_block(&fork).unwrap();
        assert_eq!(
            state
                .leadership_wallet()
                .wallet_state_at(spent.id)
                .unwrap()
                .aged_notes()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            state
                .leadership_wallet()
                .wallet_state_at(fork.id)
                .unwrap()
                .aged_notes()
                .unwrap()
                .len(),
            2
        );
        let mut spent_state = state.clone();
        spent_state.advance_lib(spent.id, 1);
        assert!(
            !spent_state
                .leadership_wallet()
                .known_notes()
                .contains_key(&known.id())
        );
        assert!(
            !spent_state
                .leadership_wallet()
                .known_notes()
                .contains_key(&service_known.id())
        );
        assert!(
            spent_state
                .leadership_wallet()
                .known_notes()
                .contains_key(&pending.id())
        );
        assert!(
            spent_state
                .leadership_wallet()
                .known_notes()
                .contains_key(&mutable_only.id())
        );
        assert_eq!(
            spent_state
                .leadership_wallet()
                .wallet_state_at(spent.id)
                .unwrap()
                .aged_notes()
                .unwrap()
                .len(),
            1
        );
        let mut spent_checkpoint = spent_state.snapshot();
        let spent_records = RecoveredOpenings {
            leadership: KnownNotes::from_iter([
                (known.id(), known),
                (service_known.id(), service_known),
                (pending.id(), pending),
            ]),
            ..RecoveredOpenings::default()
        };
        let (spent_lib, spending_state, leadership_state) =
            spent_checkpoint.take_lib_wallet_state().unwrap();
        let spent_restored = ServiceState::from_lib_wallet_state(
            spent_records,
            spent_lib,
            spending_state,
            leadership_state,
            &settings,
            &updater,
        );
        assert!(
            !spent_restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&known.id())
        );
        assert!(
            !spent_restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&service_known.id())
        );
        assert!(
            spent_restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&pending.id())
        );
        let surviving = spent_restored
            .leadership_wallet()
            .wallet_state_at(spent_lib)
            .unwrap()
            .aged_notes()
            .unwrap();
        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].0.note_set, NoteSetIndex::Sdp);
        state.advance_lib(genesis, 0);
        assert!(
            !state
                .leadership_wallet()
                .known_notes()
                .contains_key(&known.id())
        );
        assert!(
            !state
                .leadership_wallet()
                .known_notes()
                .contains_key(&service_known.id())
        );
        assert!(
            state
                .leadership_wallet()
                .known_notes()
                .contains_key(&pending.id())
        );
        assert!(
            state
                .leadership_wallet()
                .known_notes()
                .contains_key(&mutable_only.id())
        );
        state.advance_lib(fork.id, 1);
        assert!(
            !state
                .leadership_wallet()
                .known_notes()
                .contains_key(&mutable_only.id())
        );
        assert!(
            state
                .leadership_wallet()
                .known_notes()
                .contains_key(&pending.id())
        );
        let saved = receiver.borrow().clone().unwrap();
        // Changing configured credentials keeps the existing LIB checkpoint;
        // it does not reconstruct previously untracked memberships.
        let mut changed_settings = settings.clone();
        changed_settings
            .leadership_keys
            .push(LeadershipCredentials {
                keys: LeadershipKeyIds {
                    nullifier: "nf2".into(),
                    staking: "stake2".into(),
                },
                public: LeadershipPublicKeys {
                    nullifier_public_key: kdf_public_key(&Fr::from(11u64)),
                    staking_public_key: kdf_public_key(&Fr::from(13u64)),
                },
            });
        let mut saved_with_new_key = saved.clone();
        assert_eq!(
            saved_with_new_key.take_lib_wallet_state().unwrap().0,
            fork.id
        );
        let restored = RecoveryState::from_bytes(&saved.to_bytes().unwrap()).unwrap();
        let mut restored = restored;
        let after_checkpoint = KnownNote {
            note: Note {
                nonce: Fr::from(31u64),
                ..note
            },
            nullifier: NoteNullifier(Fr::from(32u64)),
            ..known
        };
        let records = RecoveredOpenings {
            leadership: KnownNotes::from_iter([
                (known.id(), known),
                (service_known.id(), service_known),
                (mutable_only.id(), mutable_only),
                (pending.id(), pending),
                (after_checkpoint.id(), after_checkpoint),
            ]),
            ..RecoveredOpenings::default()
        };
        let (lib, wallet_state, leadership_state) = restored.take_lib_wallet_state().unwrap();
        let restored = ServiceState::from_lib_wallet_state(
            records,
            lib,
            wallet_state,
            leadership_state,
            &changed_settings,
            &updater,
        );
        assert!(
            !restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&known.id())
        );
        assert!(
            !restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&service_known.id())
        );
        assert!(
            !restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&mutable_only.id())
        );
        assert!(
            restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&pending.id())
        );
        assert!(
            restored
                .leadership_wallet()
                .known_notes()
                .contains_key(&after_checkpoint.id())
        );
        assert_eq!(
            restored
                .leadership_wallet()
                .wallet_state_at(lib)
                .unwrap()
                .aged_notes()
                .unwrap()
                .len(),
            2
        );
        let restored_state = restored.leadership_wallet().wallet_state_at(lib).unwrap();
        for (owned, path) in restored_state.aged_notes().unwrap() {
            assert_eq!(owned.nullifier, known.nullifier);
            assert!(
                path.verify::<ZkHasher>(note.commitment().0, genesis_state.root_of(owned.note_set))
            );
            assert!(
                owned.path.verify::<ZkHasher>(
                    note.commitment().0,
                    restored_state.root_of(owned.note_set),
                )
            );
        }
        assert!(
            restored
                .wallet()
                .balance(lib, note.public_key)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            restored.leadership_keys().get(&public).unwrap().nullifier,
            "nf"
        );
    }

    #[test]
    fn newly_created_leadership_note_ages_before_selection() {
        use std::sync::Arc;

        use lb_core::mantle::note_set::NoteSetUpdate;
        use lb_key_management_system_service::keys::kdf_public_key;
        use lb_services_utils::overwatch::RecoveryData;
        use tokio::sync::watch;

        use crate::LeadershipCredentials;

        let public = LeadershipPublicKeys {
            nullifier_public_key: kdf_public_key(&Fr::from(7u64)),
            staking_public_key: kdf_public_key(&Fr::from(9u64)),
        };
        let note = Note {
            stake_public_key: public.staking_public_key,
            ..Note::new(10, Fr::ONE, public.nullifier_public_key)
        };
        let known = KnownNote {
            note,
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(3u64)),
        };
        let settings = WalletServiceSettings {
            known_keys: HashMap::new(),
            leadership_keys: vec![LeadershipCredentials {
                keys: LeadershipKeyIds {
                    nullifier: "nf".into(),
                    staking: "stake".into(),
                },
                public,
            }],
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = ServiceState::from_genesis(
            RecoveredOpenings::default(),
            &settings,
            genesis,
            &[],
            &[],
            [],
            [],
            &updater,
        )
        .unwrap();
        let mut late = state.clone();
        state.add_leadership_notes([known]);
        let child = WalletBlock {
            id: HeaderId::from([1; 32]),
            parent: genesis,
            epoch: 0.into(),
            updates: vec![NoteSetUpdate {
                note_set: NoteSetIndex::Ledger,
                nullifiers: vec![],
                commitments: vec![note.commitment()],
            }],
            revealed: vec![],
            withdrawals: vec![],
        };
        state.apply_block(&child).unwrap();
        late.apply_block(&child).unwrap();
        late.add_leadership_notes([known]);
        assert_eq!(
            late.leadership_wallet()
                .wallet_state_at(child.id)
                .unwrap()
                .aged_notes_at(2.into())
                .unwrap()
                .len(),
            0
        );
        let tracked = state.leadership_wallet().wallet_state_at(child.id).unwrap();
        assert_eq!(tracked.aged_notes_at(1.into()).unwrap().len(), 0);
        assert_eq!(tracked.aged_notes_at(2.into()).unwrap().len(), 1);
        assert!(
            state
                .wallet()
                .balance(child.id, note.public_key)
                .unwrap()
                .is_none()
        );
    }
}
