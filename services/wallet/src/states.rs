use core::{num::NonZeroU64, ops::Range};
use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    num::NonZero,
};

use lb_core::{
    header::HeaderId,
    mantle::{
        NoteId,
        ops::leader_claim::{VoucherCm, VoucherNullifier},
        transactions::{MantleTxBuilder, tx_list::ops::OpsContext},
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_ledger::LedgerState;
use lb_log_targets::wallet;
use lb_wallet::{Voucher, VoucherState, Vouchers, WalletBlock, WalletError};
use overwatch::services::state::StateUpdater;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::{KeyId, WalletServiceError, WalletServiceSettings};

type VoucherIndex = u64;
type VoucherId = (KeyId, VoucherIndex);
pub type Wallet = lb_wallet::Wallet<KeyId, VoucherId>;

/// How many vouchers past the last one the wallet knows it issued are looked
/// for in the blocks it applies: see [`VoucherLookahead`].
///
/// It bounds the run of consecutive vouchers that are not in the chain the
/// wallet applies, those of proposals that did not make it, past which the
/// following ones are no longer recognized.
pub const VOUCHER_LOOKAHEAD: u64 = 1024;

/// The vouchers this wallet would issue next, derived ahead of their use.
///
/// A voucher is the leader's claim on the reward of a block it proposed, and
/// derives from a key and an index, the next of which is all a wallet keeps
/// of the ones it issued. A wallet that lost its state, or never had it, as
/// when the node syncs the chain again, starts over at index 0 while the chain
/// holds vouchers of later indices: it would not know them as its own, could
/// not claim them, and would issue them again, for rewards it cannot claim
/// either, a voucher being claimable once.
///
/// So the wallet recognizes the vouchers it is about to issue in the blocks it
/// applies. One found there was issued before: the wallet takes it as known,
/// tracks its path from that block on, and issues from the index after it.
#[derive(Debug, Default)]
struct VoucherLookahead {
    vouchers: HashMap<VoucherCm, (VoucherNullifier, VoucherId)>,
    /// The index after the last voucher derived.
    end: VoucherIndex,
}

impl VoucherLookahead {
    /// The indices to derive for the lookahead to reach [`VOUCHER_LOOKAHEAD`]
    /// vouchers from `next_new_voucher_index` on.
    fn missing(&self, next_new_voucher_index: VoucherIndex) -> Range<VoucherIndex> {
        self.end.max(next_new_voucher_index)
            ..next_new_voucher_index.saturating_add(VOUCHER_LOOKAHEAD)
    }

    fn insert(&mut self, cm: VoucherCm, nf: VoucherNullifier, id: VoucherId) {
        self.end = self.end.max(id.1.saturating_add(1));
        self.vouchers.insert(cm, (nf, id));
    }

    /// Drops the vouchers before `next_new_voucher_index`, which are issued.
    fn advance(&mut self, next_new_voucher_index: VoucherIndex) {
        self.vouchers
            .retain(|_, (_, (_, index))| *index >= next_new_voucher_index);
    }
}

pub struct ClaimableVouchers {
    pub available: Vec<Voucher>,
    pub pending: Vec<Voucher>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingClaim {
    immutable_blocks_since_reservation: u64,
    /// The number of new immutable blocks after which the pending claim is
    /// released, so the voucher can be claimed again.
    max_immutable_blocks_since_reservation: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PendingClaims {
    claims: HashMap<VoucherNullifier, PendingClaim>,
}

impl PendingClaims {
    fn reserve(
        &mut self,
        nullifier: VoucherNullifier,
        max_immutable_blocks_since_reservation: u64,
    ) {
        debug!(
            target: wallet::SERVICE,
            ?nullifier,
            max_immutable_blocks_since_reservation,
            "Reserved claim voucher"
        );
        self.claims.insert(
            nullifier,
            PendingClaim {
                immutable_blocks_since_reservation: 0,
                max_immutable_blocks_since_reservation,
            },
        );
    }

    fn release(&mut self, nullifier: VoucherNullifier) {
        if self.claims.remove(&nullifier).is_some() {
            debug!(
                target: wallet::SERVICE,
                ?nullifier,
                "Released pending claim reservation"
            );
        }
    }

    fn is_reserved(&self, nullifier: &VoucherNullifier) -> bool {
        self.claims.contains_key(nullifier)
    }

    /// Evict reservations whose LIB-progress age reached their limit.
    ///
    /// Each LIB update adds `new_immutable_blocks_count` to every reservation's
    /// `immutable_blocks_since_reservation` counter. A reservation expires once
    /// that counter reaches its `max_immutable_blocks_since_reservation`.
    fn evict_expired(&mut self, new_immutable_blocks_count: u64) {
        if new_immutable_blocks_count == 0 {
            return;
        }

        let before_count = self.claims.len();

        self.claims.retain(|nullifier, claim| {
            claim.immutable_blocks_since_reservation = claim
                .immutable_blocks_since_reservation
                .saturating_add(new_immutable_blocks_count);

            let expired = claim.immutable_blocks_since_reservation
                >= claim.max_immutable_blocks_since_reservation;

            if expired {
                debug!(
                    target: wallet::SERVICE,
                    ?nullifier,
                    immutable_blocks_since_reservation = claim.immutable_blocks_since_reservation,
                    max_immutable_blocks_since_reservation =
                        claim.max_immutable_blocks_since_reservation,
                    "Removing pending claim reservation after LIB progress"
                );
            }

            !expired
        });

        if before_count != self.claims.len() {
            debug!(
                target: wallet::SERVICE,
                "Cleaned {} pending claim reservation(s) after LIB progress",
                before_count - self.claims.len()
            );
        }
    }
}

/// Notes handed out as funding for transactions that have been built but not
/// yet observed on chain. Ephemeral: rebuilt empty on restart; entries are
/// dropped once the note is observed spent in a block, or expire after enough
/// LIB progress (mirroring [`PendingClaims`]).
///
/// Unlike claim reservations, which must hold until the claim is finalised,
/// funded notes are released as soon as they are observed spent in a tip
/// block; eviction only covers transactions that never land. Releasing too
/// early merely reintroduces a conflicting tx, the same failure mode as
/// having no reservation at all.
#[derive(Debug, Default)]
struct PendingNotes {
    notes: HashMap<NoteId, u64>,
}

impl PendingNotes {
    fn reserve(&mut self, note_ids: impl IntoIterator<Item = NoteId>) {
        for note_id in note_ids {
            debug!(
                target: wallet::SERVICE,
                ?note_id,
                "Reserved pending note"
            );
            self.notes.insert(note_id, 0);
        }
    }

    fn release(&mut self, note_ids: impl IntoIterator<Item = NoteId>) {
        for note_id in note_ids {
            if self.notes.remove(&note_id).is_some() {
                debug!(
                    target: wallet::SERVICE,
                    ?note_id,
                    "Released pending note reservation"
                );
            }
        }
    }

    fn note_ids(&self) -> HashSet<NoteId> {
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
            .retain(|note_id, immutable_blocks_since_reservation| {
                *immutable_blocks_since_reservation =
                    immutable_blocks_since_reservation.saturating_add(new_immutable_blocks_count);

                let expired =
                    *immutable_blocks_since_reservation >= max_immutable_blocks_since_reservation;

                if expired {
                    debug!(
                        target: wallet::SERVICE,
                        ?note_id,
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
    next_new_voucher_index: VoucherIndex,
    vouchers: Vouchers<VoucherId>,
    /// What the wallet tracks of the vouchers, and the block it tracks them
    /// at: its LIB when the state was saved. `None` until the first recovery
    /// state update.
    ///
    /// The notes of the wallet are not saved: they are read from the ledger
    /// state as the wallet starts.
    lib_voucher_state: Option<(HeaderId, VoucherState)>,
    /// Voucher reservations for claim transactions that were built/submitted
    /// but have not reached LIB yet. Stale reservations are bounded by
    /// LIB-progress expiry after recovery.
    pending_claims: PendingClaims,
}

impl overwatch::services::state::ServiceState for RecoveryState {
    type Settings = WalletServiceSettings;
    type Error = WalletServiceError;

    fn from_settings(_settings: &Self::Settings) -> Result<Self, Self::Error> {
        Ok(Self {
            next_new_voucher_index: 0,
            vouchers: Vouchers::default(),
            lib_voucher_state: None,
            pending_claims: PendingClaims::default(),
        })
    }
}

impl RecoveryState {
    /// The block the vouchers were saved at, if any were.
    #[must_use]
    pub fn saved_at(&self) -> Option<HeaderId> {
        self.lib_voucher_state.as_ref().map(|(block, _)| *block)
    }

    /// This state with its vouchers brought forward from the block they were
    /// saved at to `block`, over `blocks_since`: the blocks after the saved
    /// one up to `block`, oldest first.
    ///
    /// A ledger state does not hold the paths of vouchers, so they can only
    /// follow the chain block by block. A state without saved vouchers is
    /// returned as it is.
    pub fn with_vouchers_at<'b>(
        mut self,
        block: HeaderId,
        blocks_since: impl IntoIterator<Item = &'b WalletBlock>,
    ) -> Result<Self, WalletError> {
        if let Some((saved_at, voucher_state)) = &mut self.lib_voucher_state {
            for applied in blocks_since {
                *voucher_state =
                    voucher_state.apply_block(&self.vouchers, applied.epoch, applied.voucher_cm)?;
            }
            *saved_at = block;
        }
        Ok(self)
    }
}

/// Provides operations on the states that must be synced to [`RecoveryState`].
pub struct ServiceState<'u> {
    next_new_voucher_index: VoucherIndex,
    wallet: Wallet,
    lib: HeaderId,
    updater: &'u StateUpdater<Option<RecoveryState>>,
    pending_claims: PendingClaims,
    pending_notes: PendingNotes,
    pending_note_expiry_blocks: u64,
    /// Derived again on each start: not part of [`RecoveryState`].
    voucher_lookahead: VoucherLookahead,
}

impl<'u> ServiceState<'u> {
    /// The wallet at `lib`, the chain's LIB, whose ledger state `lib_ledger`
    /// is: its notes are those the ledger state holds for the configured
    /// keys, and its vouchers those of `state`, which has to hold them as of
    /// `lib` if it holds any (see [`RecoveryState::with_vouchers_at`]).
    pub fn new(
        state: RecoveryState,
        settings: &WalletServiceSettings,
        lib: HeaderId,
        lib_ledger: &LedgerState,
        updater: &'u StateUpdater<Option<RecoveryState>>,
    ) -> Result<Self, WalletServiceError> {
        let RecoveryState {
            next_new_voucher_index,
            vouchers,
            lib_voucher_state,
            pending_claims,
        } = state;
        let known_keys = settings
            .known_keys
            .iter()
            .map(|(key_id, pk)| (*pk, key_id.clone()));

        let wallet = match lib_voucher_state {
            Some((saved_at, _)) if saved_at != lib => {
                return Err(WalletServiceError::SavedStateNotAtLib {
                    saved: saved_at,
                    lib,
                });
            }
            Some((_, voucher_state)) => Wallet::from_lib_ledger_and_voucher_state(
                known_keys,
                vouchers,
                lib,
                lib_ledger,
                voucher_state,
            ),
            None => Wallet::from_lib_ledger_state(known_keys, vouchers, lib, lib_ledger),
        };

        Ok(Self {
            next_new_voucher_index,
            wallet,
            lib,
            updater,
            pending_claims,
            pending_notes: PendingNotes::default(),
            pending_note_expiry_blocks: settings.pending_note_expiry_blocks,
            voucher_lookahead: VoucherLookahead::default(),
        })
    }

    pub const fn lib(&self) -> HeaderId {
        self.lib
    }

    pub const fn next_new_voucher_index(&self) -> VoucherIndex {
        self.next_new_voucher_index
    }

    pub fn add_next_known_voucher(&mut self, cm: VoucherCm, nf: VoucherNullifier, key_id: KeyId) {
        let id = (key_id, self.next_new_voucher_index);
        self.next_new_voucher_index += 1;
        self.voucher_lookahead.advance(self.next_new_voucher_index);
        self.wallet.add_known_voucher(cm, nf, id);
        self.update_state();
    }

    /// The indices of the vouchers to hand to
    /// [`Self::add_voucher_to_lookahead`], in order, for the wallet to
    /// recognize the vouchers it issued before in the blocks it applies.
    pub fn missing_voucher_lookahead(&self) -> Range<VoucherIndex> {
        self.voucher_lookahead.missing(self.next_new_voucher_index)
    }

    /// Adds the voucher of `key_id` at `index`, which the wallet has not
    /// issued, to those it looks for in the blocks it applies.
    pub fn add_voucher_to_lookahead(
        &mut self,
        cm: VoucherCm,
        nf: VoucherNullifier,
        key_id: KeyId,
        index: VoucherIndex,
    ) {
        if index >= self.next_new_voucher_index {
            self.voucher_lookahead.insert(cm, nf, (key_id, index));
        }
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        // Before the block is applied, which is when the wallet starts
        // tracking the path of a voucher it knows.
        if let Some((nf, id)) = self.voucher_lookahead.vouchers.remove(&block.voucher_cm) {
            info!(
                target: wallet::SERVICE,
                block_id = ?block.id,
                voucher_index = id.1,
                "Recovered a voucher issued before from the block that carries it"
            );
            self.next_new_voucher_index = self.next_new_voucher_index.max(id.1.saturating_add(1));
            self.voucher_lookahead.advance(self.next_new_voucher_index);
            self.wallet.add_known_voucher(block.voucher_cm, nf, id);
        }
        self.wallet.apply_block(block)?;
        // Note reservations are released after `pending_note_expiry_blocks`
        // (see `evict_expired`), not on spent.
        self.update_state();
        Ok(())
    }

    pub fn advance_lib(
        &mut self,
        new_lib: HeaderId,
        pruned_blocks: impl IntoIterator<Item = HeaderId>,
        new_immutable_blocks_count: u64,
        pruned_nullifiers: impl IntoIterator<Item = VoucherNullifier>,
    ) {
        // The wallet must already hold a `WalletState` for `new_lib`, otherwise
        // `update_state` cannot persist the recovery state. The service
        // backfills up to `new_lib` before calling us; if it still
        // isn't present (e.g. the block is not yet in storage) we skip
        // advancing rather than panicking, keeping `self.lib` pointing
        // at a block whose state exists. A later block application or
        // LIB update advances the LIB once the gap is filled.
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
        self.wallet.prune_states(pruned_blocks);
        self.wallet.prune_vouchers(pruned_nullifiers);
        self.pending_claims
            .evict_expired(new_immutable_blocks_count);
        self.pending_notes
            .evict_expired(new_immutable_blocks_count, self.pending_note_expiry_blocks);
        self.update_state();
    }

    pub const fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    pub fn claimable_vouchers(
        &self,
        tip: HeaderId,
    ) -> Result<ClaimableVouchers, WalletServiceError> {
        let mut available = Vec::new();
        let mut pending = Vec::new();

        for voucher in self.wallet.voucher_commitments_and_nullifiers() {
            if self.pending_claims.is_reserved(&voucher.nullifier) {
                pending.push(voucher);
                continue;
            }

            if self
                .wallet
                .voucher_path_snapshot(tip, &voucher.commitment)?
                .is_some()
            {
                available.push(voucher);
            }
        }

        Ok(ClaimableVouchers { available, pending })
    }

    pub fn reserve_claim(&mut self, nullifier: VoucherNullifier, security_param: NonZero<u32>) {
        self.pending_claims
            .reserve(nullifier, NonZeroU64::from(security_param).get());
    }

    pub fn release_claim_reservation(&mut self, nullifier: VoucherNullifier) {
        self.pending_claims.release(nullifier);
    }

    /// Fund `tx_builder` from the wallet's UTXOs at `tip`, excluding notes
    /// already reserved for in-flight transactions. `priority_fee_percent`
    /// reserves that percentage of the final mandatory fee, including both
    /// execution and storage cost. Only the unused reserve becomes an
    /// effective priority tip, and the percentage is not capped at 100.
    pub fn fund_tx(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &OpsContext,
        priority_fee_percent: u64,
    ) -> Result<MantleTxBuilder, WalletError> {
        self.wallet.fund_tx(
            tip,
            tx_builder,
            change_pk,
            funding_pks,
            context,
            &self.pending_notes.note_ids(),
            priority_fee_percent,
        )
    }

    pub fn reserve_pending_notes(&mut self, note_ids: impl IntoIterator<Item = NoteId>) {
        self.pending_notes.reserve(note_ids);
    }

    pub fn release_pending_notes(&mut self, note_ids: impl IntoIterator<Item = NoteId>) {
        self.pending_notes.release(note_ids);
    }

    fn update_state(&self) {
        let lib_voucher_state = self
            .wallet()
            .wallet_state_at(self.lib)
            .expect("WalletState at LIB must exist")
            .vouchers;

        self.updater.update(Some(RecoveryState {
            next_new_voucher_index: self.next_new_voucher_index,
            vouchers: self.wallet.vouchers().clone(),
            lib_voucher_state: Some((self.lib, lib_voucher_state)),
            pending_claims: self.pending_claims.clone(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use lb_core::era::{AcceptedForkDigests, ForkDigest};
    use lb_groth16::{Field as _, Fr};
    use lb_ledger::ConsensusExcludedKeys;

    use super::*;

    /// The costs of the deployments' current gas profile.
    const GAS_PROFILE: lb_core::mantle::gas::GasProfile = lb_core::mantle::gas::GasProfile {
        transfer: lb_core::mantle::gas::Gas::new(590),
        channel_config: lb_core::mantle::gas::Gas::new(56),
        channel_inscribe: lb_core::mantle::gas::Gas::new(56),
        channel_deposit: lb_core::mantle::gas::Gas::new(590),
        channel_withdraw: lb_core::mantle::gas::Gas::new(56),
        channel_transfer: lb_core::mantle::gas::Gas::new(56),
        sdp_declare: lb_core::mantle::gas::Gas::new(646),
        sdp_withdraw: lb_core::mantle::gas::Gas::new(590),
        sdp_active: lb_core::mantle::gas::Gas::new(590),
        leader_claim: lb_core::mantle::gas::Gas::new(580),
        claim_pow_reward: lb_core::mantle::gas::Gas::new(1),
    };
    use crate::ServiceEraSchedule;

    const EXPIRY_BLOCKS: u64 = 4;

    /// A schedule of one era, with epochs of 100 slots and a `k` of 5.
    fn era_schedule() -> ServiceEraSchedule {
        use std::time::Duration;

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
            fork_digest: Some(ForkDigest::new([0x11; 32])),
            gas_profile: GAS_PROFILE,
        })
    }

    #[test]
    fn pending_claim_does_not_expire_without_lib_progress() {
        let nullifier = VoucherNullifier::default();
        let mut pending_claims = PendingClaims::default();

        pending_claims.reserve(nullifier, EXPIRY_BLOCKS);
        pending_claims.evict_expired(0);

        assert!(pending_claims.is_reserved(&nullifier));
    }

    #[test]
    fn pending_claim_expires_after_enough_lib_progress() {
        let nullifier = VoucherNullifier::default();
        let mut pending_claims = PendingClaims::default();

        pending_claims.reserve(nullifier, EXPIRY_BLOCKS);
        pending_claims.evict_expired(EXPIRY_BLOCKS - 1);
        assert!(pending_claims.is_reserved(&nullifier));

        pending_claims.evict_expired(1);
        assert!(!pending_claims.is_reserved(&nullifier));
    }

    #[test]
    fn each_pending_claim_expires_after_its_own_lib_progress() {
        // Reserved on tips of eras with a `k` of 2 and of 4.
        let short = VoucherNullifier::from(Fr::ONE);
        let long = VoucherNullifier::from(Fr::from(2u64));
        let mut pending_claims = PendingClaims::default();
        pending_claims.reserve(short, 2);
        pending_claims.reserve(long, 4);

        pending_claims.evict_expired(2);
        assert!(!pending_claims.is_reserved(&short));
        assert!(pending_claims.is_reserved(&long));

        pending_claims.evict_expired(2);
        assert!(!pending_claims.is_reserved(&long));
    }

    #[test]
    fn pending_note_does_not_expire_without_lib_progress() {
        let note_id = NoteId::from(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([note_id]);
        pending_notes.evict_expired(0, EXPIRY_BLOCKS);

        assert!(pending_notes.note_ids().contains(&note_id));
    }

    #[test]
    fn pending_note_expires_after_enough_lib_progress() {
        let note_id = NoteId::from(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([note_id]);
        pending_notes.evict_expired(EXPIRY_BLOCKS - 1, EXPIRY_BLOCKS);
        assert!(pending_notes.note_ids().contains(&note_id));

        pending_notes.evict_expired(1, EXPIRY_BLOCKS);
        assert!(!pending_notes.note_ids().contains(&note_id));
    }

    #[test]
    fn released_pending_note_becomes_available() {
        let note_id = NoteId::from(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([note_id]);
        pending_notes.release([note_id]);

        assert!(pending_notes.note_ids().is_empty());
    }

    fn ledger_config() -> lb_ledger::Config {
        use std::{num::NonZeroU32, sync::Arc};

        use lb_core::sdp::{MinStake, ServiceParameters, ServiceType};
        use lb_cryptarchia_engine::EpochConfig;
        use lb_ledger::{
            config::{BlendPoWConfig, ModulusShift, PoWConfig, RewardPoWConfig},
            mantle::sdp::{ServiceRewardsParameters, rewards},
        };
        use lb_utils::math::{NonNegativeRatio, PositiveRatio, ProperFraction};

        let epoch_config = EpochConfig {
            epoch_stake_distribution_stabilization: NonZero::new(1).unwrap(),
            epoch_period_nonce_buffer: NonZero::new(1).unwrap(),
            epoch_period_nonce_stabilization: NonZero::new(1).unwrap(),
        };
        let consensus_config = lb_cryptarchia_engine::Config::new(
            NonZero::new(1).unwrap(),
            ProperFraction::new(1, 10.try_into().unwrap()).unwrap(),
            1f64.try_into().expect("1 > 0"),
            12,
            4,
        );
        let epoch_length = epoch_config.epoch_length(consensus_config.base_period_length());

        lb_ledger::Config {
            epoch_config,
            consensus_config,
            sdp_config: lb_ledger::mantle::sdp::Config {
                era: lb_ledger::mantle::sdp::SdpEra::Master,
                service_params: Arc::new(
                    [(
                        ServiceType::BlendNetwork,
                        ServiceParameters {
                            inactivity_period: 20.try_into().unwrap(),
                        },
                    )]
                    .into(),
                ),
                service_rewards_params: ServiceRewardsParameters {
                    blend: rewards::blend::RewardsParameters {
                        rounds_per_epoch: epoch_length.try_into().unwrap(),
                        message_frequency_per_round: PositiveRatio::new(1, NonZeroU32::MIN)
                            .unwrap(),
                        cover_replication_factor: 0,
                        num_blend_layers: NonZeroU64::new(3).unwrap(),
                        minimum_network_size: NonZeroU64::new(1).unwrap(),
                        data_replication_factor: 0,
                        activity_threshold_sensitivity: 1,
                    },
                },
                min_stake: MinStake { threshold: 1 },
            },
            max_transactions: 1024,
            max_transactions_size: 2 * 1024 * 1024,
            max_op_data_size: 2 * 1024 * 1024 * 7 / 8,
            tx_era: lb_core::mantle::transactions::TxEra::Master,
            execution_market: lb_ledger::config::ExecutionMarketConfig {
                max_gas_per_block: lb_ledger::config::MaxGasPerBlock::new(3_193_460).unwrap(),
                fee_adjustment_rate: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(9, 10.try_into().unwrap()).unwrap(),
            },
            storage_market: lb_ledger::config::StorageMarketConfig {
                max_adjustment_factor: ProperFraction::new(1, 8.try_into().unwrap()).unwrap(),
                ema_smoothing_factor: ProperFraction::new(1, 2.try_into().unwrap()).unwrap(),
            },
            block_rewards: lb_ledger::config::BlockRewardsConfig::new(
                3_000_000_000,
                120_000_000.try_into().unwrap(),
                10_512,
                NonNegativeRatio::new(62_500, 657.try_into().unwrap()),
                ProperFraction::new(6, 10.try_into().unwrap()).unwrap(),
            )
            .unwrap(),
            gas_profile: GAS_PROFILE,
            pow_config: PoWConfig {
                blend: BlendPoWConfig {
                    base_difficulty: ModulusShift::new::<19>(),
                    damping_den_offset: 0,
                    damping_num: 1.try_into().unwrap(),
                    max_step: 1.try_into().unwrap(),
                    target_transactions_per_block: 1.try_into().unwrap(),
                },
                reward: RewardPoWConfig {
                    minimum_difficulty: ModulusShift::new::<26>(),
                    ema_smoothing_factor: 9,
                    ema_smoothing_precision: NonZeroU64::new(10).unwrap(),
                    target_claims_per_block: NonZeroU64::new(100).unwrap(),
                    rate_num: 0,
                    rate_den: NonZeroU64::MIN,
                    pow_share: 0,
                    share_den: NonZeroU64::MIN,
                    slot_window: NonZeroU64::new(100).unwrap(),
                },
            },
            accepted_fork_digests: AcceptedForkDigests::new(vec![ForkDigest::new([0x11; 32])]),
        }
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
            era_schedule: era_schedule(),
        };

        // Fresh wallet: initialized from the LIB ledger state at genesis.
        let recovery = RecoveryState {
            next_new_voucher_index: 0,
            vouchers: Vouchers::default(),
            lib_voucher_state: None,
            pending_claims: PendingClaims::default(),
        };

        let genesis = HeaderId::from([0; 32]);
        let ledger =
            LedgerState::from_utxos([], &ledger_config(), 0, &ConsensusExcludedKeys::empty());
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));

        let mut state = ServiceState::new(recovery, &settings, genesis, &ledger, &updater).unwrap();

        // The first post-online LibUpdate delivers a new_lib the wallet never
        // applied (the silent bootstrap->online LIB jump outran the wallet).
        let unseen_lib = HeaderId::from([42; 32]);
        state.advance_lib(
            unseen_lib,
            Vec::<HeaderId>::new(),
            1,
            Vec::<VoucherNullifier>::new(),
        );

        // No panic: the advance is skipped and the LIB stays at a block whose
        // WalletState exists.
        assert_eq!(state.lib(), genesis);
    }
    fn fresh_settings() -> WalletServiceSettings {
        WalletServiceSettings {
            known_keys: HashMap::new(),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: lb_services_utils::overwatch::RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            era_schedule: era_schedule(),
        }
    }

    /// The voucher of `index`, as the service derives it from the KMS.
    fn voucher(index: VoucherIndex) -> (VoucherCm, VoucherNullifier) {
        let secret = lb_core::mantle::ops::leader_claim::VoucherSecret::from(Fr::from(index + 1));
        (
            VoucherCm::from_secret(secret),
            VoucherNullifier::from_secret(secret),
        )
    }

    fn fill_voucher_lookahead(state: &mut ServiceState<'_>) {
        for index in state.missing_voucher_lookahead() {
            let (cm, nf) = voucher(index);
            state.add_voucher_to_lookahead(cm, nf, "voucher-master".into(), index);
        }
    }

    fn block(id: u8, parent: HeaderId, voucher_cm: VoucherCm) -> WalletBlock {
        WalletBlock {
            id: HeaderId::from([id; 32]),
            parent,
            epoch: 0.into(),
            voucher_cm,
            header_ops: vec![],
            txs: lb_core::block::BlockTransactions::empty(),
        }
    }

    /// A wallet that lost its state issues from index 0 again, while the chain
    /// it syncs holds the vouchers it issued before.
    #[test]
    fn a_wallet_without_state_recovers_the_vouchers_it_issued_from_the_blocks() {
        use std::sync::Arc;

        let settings = fresh_settings();
        let genesis = HeaderId::from([0; 32]);
        let ledger =
            LedgerState::from_utxos([], &ledger_config(), 0, &ConsensusExcludedKeys::empty());
        let (sender, _receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let recovery =
            <RecoveryState as overwatch::services::state::ServiceState>::from_settings(&settings)
                .unwrap();
        let mut state = ServiceState::new(recovery, &settings, genesis, &ledger, &updater).unwrap();

        assert_eq!(state.missing_voucher_lookahead(), 0..VOUCHER_LOOKAHEAD);
        fill_voucher_lookahead(&mut state);
        assert!(state.missing_voucher_lookahead().is_empty());
        assert_eq!(state.wallet().vouchers().count(), 0);

        // A block of another leader changes nothing.
        let foreign = block(1, genesis, voucher(VOUCHER_LOOKAHEAD + 7).0);
        state.apply_block(&foreign).unwrap();
        assert_eq!(state.next_new_voucher_index(), 0);
        assert_eq!(state.wallet().vouchers().count(), 0);

        // The wallet proposed with vouchers 0 to 4 before; only that of 4 made
        // it into the chain.
        let (cm, nf) = voucher(4);
        let own = block(2, foreign.id, cm);
        state.apply_block(&own).unwrap();
        assert_eq!(state.next_new_voucher_index(), 5);
        assert_eq!(
            state.wallet().get_voucher_by_nullifier(&nf),
            Some(&("voucher-master".into(), 4))
        );
        // Its path is tracked from the block that carries it.
        let wallet_state = state.wallet().wallet_state_at(own.id).unwrap();
        assert!(wallet_state.vouchers.paths.contains_key(&cm));
        // The lookahead is to be extended past the recovered voucher.
        assert_eq!(
            state.missing_voucher_lookahead(),
            VOUCHER_LOOKAHEAD..5 + VOUCHER_LOOKAHEAD
        );

        // The next voucher the wallet issues is a new one.
        let (cm, nf) = voucher(5);
        state.add_next_known_voucher(cm, nf, "voucher-master".into());
        assert_eq!(state.next_new_voucher_index(), 6);
        assert_eq!(state.wallet().vouchers().count(), 2);
    }

    /// A saved state holds what the wallet tracks of the vouchers and none of
    /// its notes. The wallet started from it holds the notes the ledger state
    /// has for the keys it is configured with, and the saved voucher paths.
    #[test]
    fn a_wallet_started_from_a_saved_state_has_the_ledgers_notes_and_the_saved_vouchers() {
        use std::sync::Arc;

        use lb_core::{
            block::BlockTransactions,
            mantle::{Note, Utxo},
        };
        use lb_services_utils::overwatch::RecoveryData;
        use overwatch::services::state::ServiceState as _;

        let alice = ZkPublicKey::new(Fr::from(1u64));
        let bob = ZkPublicKey::new(Fr::from(2u64));
        let settings = |key: ZkPublicKey| WalletServiceSettings {
            known_keys: HashMap::from([("key".into(), key)]),
            voucher_master_key_id: "voucher-master".into(),
            recovery_data: RecoveryData::default(),
            pending_note_expiry_blocks: 10,
            era_schedule: era_schedule(),
        };
        let genesis = HeaderId::from([0; 32]);
        // No block of this test changes the notes: the ledger state is that
        // of each.
        let ledger = LedgerState::from_utxos(
            [
                Utxo::new([0; 32], 0, Note::new(100, alice)),
                Utxo::new([0; 32], 1, Note::new(20, bob)),
            ],
            &ledger_config(),
            0,
            &ConsensusExcludedKeys::empty(),
        );
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let updater = StateUpdater::new(Arc::new(sender));
        let block = |id: u8, parent: HeaderId, voucher_cm: VoucherCm| WalletBlock {
            id: HeaderId::from([id; 32]),
            parent,
            epoch: 0.into(),
            voucher_cm,
            header_ops: vec![],
            txs: BlockTransactions::empty(),
        };

        // A wallet with Alice's key issues a voucher, which the next block
        // carries, and is saved at that block.
        let fresh = RecoveryState::from_settings(&settings(alice)).unwrap();
        let mut state =
            ServiceState::new(fresh, &settings(alice), genesis, &ledger, &updater).unwrap();
        let secret = Fr::from(7u64).into();
        let voucher_cm = VoucherCm::from_secret(secret);
        state.add_next_known_voucher(
            voucher_cm,
            VoucherNullifier::from_secret(secret),
            "voucher-master".into(),
        );
        let saved_at = block(1, genesis, voucher_cm);
        state.apply_block(&saved_at).unwrap();
        state.advance_lib(saved_at.id, [genesis], 1, Vec::<VoucherNullifier>::new());
        let saved: RecoveryState = receiver.borrow().clone().unwrap();
        assert_eq!(saved.saved_at(), Some(saved_at.id));

        // Started with Bob's key in place of Alice's, while the chain's LIB is
        // the block it was saved at.
        let state = ServiceState::new(
            saved.clone(),
            &settings(bob),
            saved_at.id,
            &ledger,
            &updater,
        )
        .unwrap();
        assert_eq!(state.lib(), saved_at.id);
        let started = state.wallet().wallet_state_at(saved_at.id).unwrap();
        assert_eq!(started.utxos_owned_by_pks([alice]).len(), 0);
        assert_eq!(started.utxos_owned_by_pks([bob]).len(), 1);
        assert!(started.vouchers.paths.contains_key(&voucher_cm));
        let path_when_saved = started.vouchers.paths.get(&voucher_cm).cloned();

        // Started while the chain's LIB is a later block: the saved vouchers
        // are not of that block.
        let lib = block(
            2,
            saved_at.id,
            VoucherCm::from_secret(Fr::from(8u64).into()),
        );
        assert!(matches!(
            ServiceState::new(saved.clone(), &settings(bob), lib.id, &ledger, &updater),
            Err(WalletServiceError::SavedStateNotAtLib { saved, lib: at })
                if saved == saved_at.id && at == lib.id
        ));

        // Brought forward to that block first, they are, and the voucher's
        // path has followed the block in between.
        let saved = saved.with_vouchers_at(lib.id, [&lib]).unwrap();
        assert_eq!(saved.saved_at(), Some(lib.id));
        let state = ServiceState::new(saved, &settings(bob), lib.id, &ledger, &updater).unwrap();
        assert_eq!(state.lib(), lib.id);
        let started = state.wallet().wallet_state_at(lib.id).unwrap();
        assert_eq!(started.utxos_owned_by_pks([bob]).len(), 1);
        assert!(started.vouchers.paths.contains_key(&voucher_cm));
        assert_ne!(
            started.vouchers.paths.get(&voucher_cm).cloned(),
            path_when_saved
        );
    }
}
