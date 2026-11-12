use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use lb_core::{
    header::HeaderId,
    mantle::{
        GasProfile, Note, Value,
        notes::NoteNullifier,
        ops::transfer::TransferOp,
        transactions::{
            MantleTxBuilder,
            builder::{PriorityFee, ZkTransferOpening},
            tx_list::ops::OpsContext,
        },
    },
};
use lb_groth16::Fr;
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_log_targets::wallet;
use lb_wallet::{KnownNote, KnownNotes, WalletBlock, WalletError, WalletState};
use overwatch::services::state::StateUpdater;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::{KeyId, WalletServiceError, WalletServiceSettings};

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
#[derive(Debug, Default)]
struct PendingNotes {
    notes: HashMap<NoteNullifier, u64>,
}

impl PendingNotes {
    fn reserve(&mut self, nullifiers: impl IntoIterator<Item = NoteNullifier>) {
        for nullifier in nullifiers {
            debug!(
                target: wallet::SERVICE,
                ?nullifier,
                "Reserved pending note"
            );
            self.notes.insert(nullifier, 0);
        }
    }

    fn release(&mut self, nullifiers: impl IntoIterator<Item = NoteNullifier>) {
        for nullifier in nullifiers {
            if self.notes.remove(&nullifier).is_some() {
                debug!(
                    target: wallet::SERVICE,
                    ?nullifier,
                    "Released pending note reservation"
                );
            }
        }
    }

    fn nullifiers(&self) -> HashSet<NoteNullifier> {
        self.notes.keys().copied().collect()
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

        self.notes
            .retain(|nullifier, immutable_blocks_since_reservation| {
                *immutable_blocks_since_reservation =
                    immutable_blocks_since_reservation.saturating_add(new_immutable_blocks_count);

                let expired =
                    *immutable_blocks_since_reservation >= max_immutable_blocks_since_reservation;

                if expired {
                    debug!(
                        target: wallet::SERVICE,
                        ?nullifier,
                        immutable_blocks_since_reservation,
                        max_immutable_blocks_since_reservation,
                        "Removing pending note reservation after LIB progress"
                    );
                }

                !expired
            });
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    /// [`WalletState`] at the last known LIB.
    /// `None` until the first recovery state update.
    lib_wallet_state: Option<(HeaderId, WalletState)>,
    // openings the wallet adopts once their commitments are appended
    known_notes: KnownNotes,
    // the reward keys `0..next_reward_key_index` are derived again at start
    next_reward_key_index: u64,
}

impl RecoveryState {
    pub const fn take_lib_wallet_state(&mut self) -> Option<(HeaderId, WalletState)> {
        self.lib_wallet_state.take()
    }
}

impl overwatch::services::state::ServiceState for RecoveryState {
    type Settings = WalletServiceSettings;
    type Error = WalletServiceError;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            lib_wallet_state: None,
            known_notes: KnownNotes::new(),
            next_reward_key_index: 0,
        })
    }
}

/// Provides operations on the states that must be synced to [`RecoveryState`].
pub struct ServiceState<'u> {
    next_reward_key_index: u64,
    wallet: Wallet,
    lib: HeaderId,
    updater: &'u StateUpdater<Option<RecoveryState>>,
    pending_notes: PendingNotes,
    pending_note_expiry_blocks: u64,
    fee_margin_percent: u64,
    // The leader reward notes the LIB does not hold yet. Nothing on chain
    // opens them, so the service writes them to storage itself, and confirmed,
    // before handing one out.
    reward_notes: KnownNotes,
}

impl<'u> ServiceState<'u> {
    // Restores the wallet persisted at its LIB.
    pub fn from_lib_wallet_state(
        state: RecoveryState,
        lib: HeaderId,
        wallet_state: WalletState,
        settings: &WalletServiceSettings,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Self {
        let RecoveryState {
            known_notes,
            next_reward_key_index,
            ..
        } = state;
        let wallet =
            Wallet::from_lib_wallet_state(known_keys(settings), known_notes, lib, wallet_state);
        Self::with_wallet(wallet, lib, next_reward_key_index, settings, updater)
    }

    // A fresh wallet follows the note sets from the genesis block.
    pub fn from_genesis(
        state: RecoveryState,
        settings: &WalletServiceSettings,
        genesis: HeaderId,
        genesis_notes: &[Note],
        genesis_service_notes: &[Note],
        genesis_known_notes: impl IntoIterator<Item = KnownNote>,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Result<Self, WalletError> {
        let RecoveryState {
            mut known_notes,
            next_reward_key_index,
            ..
        } = state;
        known_notes.extend(
            genesis_known_notes
                .into_iter()
                .map(|known_note| (known_note.note.commitment(), known_note)),
        );
        let wallet = Wallet::from_genesis(
            known_keys(settings),
            known_notes,
            genesis,
            genesis_notes,
            genesis_service_notes,
        )?;
        Ok(Self::with_wallet(
            wallet,
            genesis,
            next_reward_key_index,
            settings,
            updater,
        ))
    }

    fn with_wallet(
        wallet: Wallet,
        lib: HeaderId,
        next_reward_key_index: u64,
        settings: &WalletServiceSettings,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Self {
        Self {
            next_reward_key_index,
            wallet,
            lib,
            updater,
            pending_notes: PendingNotes::default(),
            pending_note_expiry_blocks: settings.pending_note_expiry_blocks,
            fee_margin_percent: settings.fee_margin_percent,
            reward_notes: KnownNotes::new(),
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

    pub fn next_reward_key_index(&mut self) -> u64 {
        let index = self.next_reward_key_index;
        self.next_reward_key_index = index.saturating_add(1);
        self.update_state();
        index
    }

    pub fn add_known_key(&mut self, pk: ZkPublicKey, key_id: KeyId) {
        self.wallet.add_known_key(pk, key_id);
    }

    pub fn add_known_notes(&mut self, notes: impl IntoIterator<Item = KnownNote>) {
        self.wallet.add_known_notes(notes);
        self.update_state();
    }

    pub const fn reward_notes(&self) -> &KnownNotes {
        &self.reward_notes
    }

    // The reward notes written before a restart, known again.
    pub fn restore_reward_notes(&mut self, notes: KnownNotes) {
        self.add_known_notes(notes.values().copied());
        self.reward_notes = notes;
    }

    pub fn add_reward_note(&mut self, note: KnownNote) {
        self.add_known_notes([note]);
        self.reward_notes.insert(note.note.commitment(), note);
    }

    // Forgets the reward notes the LIB holds, which the LIB wallet state
    // persists from then on, and tells whether there were any. A note the LIB
    // only holds once it is already spent stays.
    pub fn forget_reward_notes_at_lib(&mut self) -> bool {
        let Ok(lib_state) = self.wallet.wallet_state_at(self.lib) else {
            return false;
        };
        let count = self.reward_notes.len();
        self.reward_notes
            .retain(|commitment, _| !lib_state.notes.contains_key(commitment));
        self.reward_notes.len() != count
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        self.wallet.apply_block(block)?;
        // Note reservations are released after `pending_note_expiry_blocks`
        // (see `evict_expired`), not on spent.
        self.update_state();
        Ok(())
    }

    pub fn advance_lib(&mut self, new_lib: HeaderId, new_immutable_blocks_count: u64) {
        // The wallet must already hold a `WalletState` for `new_lib`, otherwise
        // `update_state` cannot persist the recovery state. The service backfills
        // up to `new_lib` before calling us; if it still isn't present (e.g. the
        // block is not yet in storage) we skip advancing rather than panicking,
        // keeping `self.lib` pointing at a block whose state exists. A later
        // block application or LIB update advances the LIB once the gap is filled.
        if !self.wallet.has_processed_block(new_lib) {
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
    /// funding that fails releases the notes `tx_builder` spends, such as a
    /// split reserved for it. If a builder is funded again while its first
    /// funding is in flight and this funding fails, its notes are released as
    /// well, and the ledger rejects the conflict if another transaction then
    /// spends them.
    #[expect(
        clippy::too_many_arguments,
        reason = "thin passthrough to `Wallet::fund_tx` plus the reservations"
    )]
    pub fn fund_tx<G: GasProfile>(
        &mut self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        change_nonce: Fr,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &OpsContext,
        priority_fee: impl Into<PriorityFee>,
    ) -> Result<MantleTxBuilder, WalletError> {
        let funded = self.wallet.fund_tx::<G>(
            tip,
            tx_builder,
            change_pk,
            change_nonce,
            funding_pks,
            context,
            &self.pending_notes.nullifiers(),
            priority_fee,
        );
        if funded.is_err() {
            self.release_pending_notes(tx_builder.nullifiers_consumed());
        }
        funded
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
            .nullifiers_consumed()
            .chain(self.pending_notes.nullifiers())
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

    pub fn reserve_pending_notes(&mut self, nullifiers: impl IntoIterator<Item = NoteNullifier>) {
        self.pending_notes.reserve(nullifiers);
    }

    pub fn release_pending_notes(&mut self, nullifiers: impl IntoIterator<Item = NoteNullifier>) {
        self.pending_notes.release(nullifiers);
    }

    fn update_state(&self) {
        let lib_wallet_state = self
            .wallet()
            .wallet_state_at(self.lib)
            .expect("WalletState at LIB must exist");

        self.updater.update(Some(RecoveryState {
            lib_wallet_state: Some((self.lib, lib_wallet_state)),
            known_notes: self.wallet.known_notes().clone(),
            next_reward_key_index: self.next_reward_key_index,
        }));
    }
}

fn known_keys(settings: &WalletServiceSettings) -> impl Iterator<Item = (ZkPublicKey, KeyId)> {
    settings
        .known_keys
        .iter()
        .map(|(key_id, pk)| (*pk, key_id.clone()))
}

#[cfg(test)]
mod tests {
    use core::{num::NonZero, time::Duration};

    use lb_core::mantle::{note_set::NoteSetIndex, notes::NoteCommitment};
    use lb_groth16::Field as _;

    use super::*;
    use crate::{DEFAULT_FEE_MARGIN_PERCENT, ServiceEraSchedule};

    const EXPIRY_BLOCKS: u64 = 4;

    /// A schedule of one era, with epochs of 100 slots and a `k` of 5.
    fn era_schedule() -> ServiceEraSchedule {
        use lb_time::era::{EraEntriesAfterGenesis, EraSchedule, EraTiming};
        use time::OffsetDateTime;

        use crate::WalletEraSettings;

        let timing = EraTiming {
            slot_duration: Duration::from_secs(1),
            epoch_length: NonZero::new(100).unwrap(),
        };
        EraSchedule::new(
            OffsetDateTime::UNIX_EPOCH,
            timing,
            EraEntriesAfterGenesis::empty(),
        )
        .unwrap()
        .map(|_| WalletEraSettings {
            epoch_length: timing.epoch_length,
            security_param: NonZero::new(5).unwrap(),
        })
    }

    #[test]
    fn recovery_state_keeps_the_known_notes_and_the_reward_key_index() {
        let note = KnownNote {
            note: Note::new(10, Fr::ONE, ZkPublicKey::new(Fr::from(2u64))),
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(3u64)),
        };
        let state = RecoveryState {
            lib_wallet_state: None,
            known_notes: HashMap::from([(note.note.commitment(), note)]),
            next_reward_key_index: 4,
        };
        let decoded: RecoveryState =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(decoded.known_notes, state.known_notes);
        assert_eq!(decoded.next_reward_key_index, 4);
    }

    #[test]
    fn pending_note_does_not_expire_without_lib_progress() {
        let nullifier = NoteNullifier(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([nullifier]);
        pending_notes.evict_expired(0, EXPIRY_BLOCKS);

        assert!(pending_notes.nullifiers().contains(&nullifier));
    }

    #[test]
    fn pending_note_expires_after_enough_lib_progress() {
        let nullifier = NoteNullifier(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([nullifier]);
        pending_notes.evict_expired(EXPIRY_BLOCKS - 1, EXPIRY_BLOCKS);
        assert!(pending_notes.nullifiers().contains(&nullifier));

        pending_notes.evict_expired(1, EXPIRY_BLOCKS);
        assert!(!pending_notes.nullifiers().contains(&nullifier));
    }

    #[test]
    fn released_pending_note_becomes_available() {
        let nullifier = NoteNullifier(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([nullifier]);
        pending_notes.release([nullifier]);

        assert!(pending_notes.nullifiers().is_empty());
    }

    #[test]
    fn a_failed_funding_releases_the_notes_of_its_builder_alone() {
        use std::sync::Arc;

        use lb_core::mantle::{
            Op,
            gas::MainnetGasProfile,
            transactions::{GasPrices, tx_list::ops::OpsGasContext},
        };
        use lb_services_utils::overwatch::RecoveryData;

        let settings = WalletServiceSettings {
            known_keys: HashMap::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let recovery = RecoveryState {
            lib_wallet_state: None,
            known_notes: HashMap::new(),
            next_reward_key_index: 0,
        };
        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state =
            ServiceState::from_genesis(recovery, &settings, genesis, &[], &[], [], &updater)
                .unwrap();

        let split_input = NoteNullifier(Fr::ONE);
        let in_flight = NoteNullifier(Fr::from(2u64));
        state.reserve_pending_notes([split_input, in_flight]);
        let tx_builder = MantleTxBuilder::new()
            .push_op(Op::Transfer(TransferOp::new(
                [split_input].into(),
                [NoteCommitment(Fr::ONE)].into(),
                Fr::ONE,
                0,
            )))
            .unwrap();
        let funding_pk = ZkPublicKey::new(Fr::ONE);
        let context = OpsContext {
            gas_context: OpsGasContext::new(HashMap::new(), GasPrices::new(1, 1)),
        };

        state
            .fund_tx::<MainnetGasProfile>(
                genesis,
                &tx_builder,
                funding_pk,
                Fr::ONE,
                [funding_pk],
                &context,
                0,
            )
            .unwrap_err();

        assert_eq!(state.pending_notes.nullifiers(), HashSet::from([in_flight]));
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
            known_keys: HashMap::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };

        // Fresh wallet: initialized from the genesis note sets.
        let recovery = RecoveryState {
            lib_wallet_state: None,
            known_notes: HashMap::new(),
            next_reward_key_index: 0,
        };

        let genesis = HeaderId::from([0; 32]);
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));

        let mut state =
            ServiceState::from_genesis(recovery, &settings, genesis, &[], &[], [], &updater)
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
    fn reward_notes_are_kept_until_the_lib_holds_them() {
        use std::sync::Arc;

        use lb_services_utils::overwatch::RecoveryData;

        let settings = WalletServiceSettings {
            known_keys: HashMap::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            fee_margin_percent: DEFAULT_FEE_MARGIN_PERCENT,
            era_schedule: era_schedule(),
        };
        let recovery = RecoveryState {
            lib_wallet_state: None,
            known_notes: HashMap::new(),
            next_reward_key_index: 0,
        };
        let known_note = |value| KnownNote {
            note: Note::new(value, Fr::ONE, ZkPublicKey::new(Fr::from(2u64))),
            note_set: NoteSetIndex::Ledger,
            nullifier: NoteNullifier(Fr::from(value)),
        };
        // the LIB holds `on_chain`, while `pending` is in a block not final yet
        let on_chain = known_note(10);
        let pending = known_note(20);
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let mut state = ServiceState::from_genesis(
            recovery,
            &settings,
            HeaderId::from([0; 32]),
            &[on_chain.note],
            &[],
            [on_chain],
            &updater,
        )
        .unwrap();

        state.add_reward_note(pending);
        assert!(!state.forget_reward_notes_at_lib());
        assert_eq!(
            state.reward_notes(),
            &HashMap::from([(pending.note.commitment(), pending)])
        );
        assert!(
            state
                .wallet()
                .known_notes()
                .contains_key(&pending.note.commitment())
        );

        // restored after a restart, the notes are known again, and the one the
        // LIB holds is forgotten
        state.restore_reward_notes(HashMap::from([
            (on_chain.note.commitment(), on_chain),
            (pending.note.commitment(), pending),
        ]));
        assert!(state.forget_reward_notes_at_lib());
        assert_eq!(
            state.reward_notes(),
            &HashMap::from([(pending.note.commitment(), pending)])
        );
    }
}
