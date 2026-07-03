use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use lb_core::{
    header::HeaderId,
    mantle::{
        GasConstants, NoteId,
        ops::leader_claim::{VoucherCm, VoucherNullifier},
        transactions::{MantleTxBuilder, MantleTxContext},
    },
};
use lb_key_management_system_service::keys::ZkPublicKey;
use lb_ledger::LedgerState;
use lb_log_targets::wallet;
use lb_wallet::{Voucher, Vouchers, WalletBlock, WalletError, WalletState};
use overwatch::services::state::StateUpdater;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{KeyId, WalletServiceError, WalletServiceSettings};

type VoucherIndex = u64;
type VoucherId = (KeyId, VoucherIndex);
pub type Wallet = lb_wallet::Wallet<KeyId, VoucherId>;

pub struct ClaimableVouchers {
    pub available: Vec<Voucher>,
    pub pending: Vec<Voucher>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingClaim {
    immutable_blocks_since_reservation: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PendingClaims {
    claims: HashMap<VoucherNullifier, PendingClaim>,
}

impl PendingClaims {
    fn reserve(&mut self, nullifier: VoucherNullifier) {
        debug!(
            target: wallet::SERVICE,
            ?nullifier,
            "Reserved claim voucher"
        );
        self.claims.insert(
            nullifier,
            PendingClaim {
                immutable_blocks_since_reservation: 0,
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

    /// Evict reservations whose LIB-progress age reached the configured limit.
    ///
    /// Each LIB update adds `new_immutable_blocks_count` to every reservation's
    /// `immutable_blocks_since_reservation` counter. A reservation expires once
    /// that counter reaches `max_immutable_blocks_since_reservation`.
    fn evict_expired(
        &mut self,
        new_immutable_blocks_count: u64,
        max_immutable_blocks_since_reservation: u64,
    ) {
        if new_immutable_blocks_count == 0 {
            return;
        }

        let before_count = self.claims.len();

        self.claims.retain(|nullifier, claim| {
            claim.immutable_blocks_since_reservation = claim
                .immutable_blocks_since_reservation
                .saturating_add(new_immutable_blocks_count);

            let expired =
                claim.immutable_blocks_since_reservation >= max_immutable_blocks_since_reservation;

            if expired {
                debug!(
                    target: wallet::SERVICE,
                    ?nullifier,
                    immutable_blocks_since_reservation = claim.immutable_blocks_since_reservation,
                    max_immutable_blocks_since_reservation,
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

    fn remove_spent(&mut self, spent: &HashSet<NoteId>) {
        self.notes.retain(|note_id, _| !spent.contains(note_id));
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
    /// [`WalletState`] at the last known LIB.
    /// `None` on fresh start; populated after the first LIB update.
    lib_wallet_state: Option<(HeaderId, WalletState)>,
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
            lib_wallet_state: None,
            pending_claims: PendingClaims::default(),
        })
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
    security_param: u64,
}

impl<'u> ServiceState<'u> {
    pub fn new(
        state: RecoveryState,
        settings: &WalletServiceSettings,
        lib: HeaderId,
        lib_ledger: &LedgerState,
        updater: &'u StateUpdater<Option<RecoveryState>>,
        security_param: u64,
    ) -> Self {
        let RecoveryState {
            next_new_voucher_index,
            vouchers,
            lib_wallet_state,
            pending_claims,
        } = state;
        let known_keys = settings
            .known_keys
            .iter()
            .map(|(key_id, pk)| (*pk, key_id.clone()));

        // Initialize [`Wallet`] either from the persisted [`WalletState`]
        // or from the current chain's LIB ledger state.
        let (wallet, wallet_lib) = match lib_wallet_state {
            Some((persisted_lib, wallet_state)) => (
                Wallet::from_lib_wallet_state(known_keys, vouchers, persisted_lib, wallet_state),
                persisted_lib,
            ),
            None => (
                Wallet::from_lib_ledger_state(known_keys, vouchers, lib, lib_ledger),
                lib,
            ),
        };

        Self {
            next_new_voucher_index,
            wallet,
            lib: wallet_lib,
            updater,
            pending_claims,
            pending_notes: PendingNotes::default(),
            pending_note_expiry_blocks: settings.pending_note_expiry_blocks,
            security_param,
        }
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
        self.wallet.add_known_voucher(cm, nf, id);
        self.update_state();
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        self.wallet.apply_block(block)?;
        self.pending_notes.remove_spent(&block.spent_note_ids());
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
        self.lib = new_lib;
        self.wallet.prune_states(pruned_blocks);
        self.wallet.prune_vouchers(pruned_nullifiers);
        self.pending_claims
            .evict_expired(new_immutable_blocks_count, self.security_param);
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

    pub fn reserve_claim(&mut self, nullifier: VoucherNullifier) {
        self.pending_claims.reserve(nullifier);
    }

    pub fn release_claim_reservation(&mut self, nullifier: VoucherNullifier) {
        self.pending_claims.release(nullifier);
    }

    /// Fund `tx_builder` from the wallet's UTXOs at `tip`, excluding notes
    /// already reserved for in-flight transactions.
    pub fn fund_tx<G: GasConstants>(
        &self,
        tip: HeaderId,
        tx_builder: &MantleTxBuilder,
        change_pk: ZkPublicKey,
        funding_pks: impl IntoIterator<Item = impl Borrow<ZkPublicKey>>,
        context: &MantleTxContext,
    ) -> Result<MantleTxBuilder, WalletError> {
        self.wallet.fund_tx::<G>(
            tip,
            tx_builder,
            change_pk,
            funding_pks,
            context,
            &self.pending_notes.note_ids(),
        )
    }

    pub fn reserve_pending_notes(&mut self, note_ids: impl IntoIterator<Item = NoteId>) {
        self.pending_notes.reserve(note_ids);
    }

    pub fn release_pending_notes(&mut self, note_ids: impl IntoIterator<Item = NoteId>) {
        self.pending_notes.release(note_ids);
    }

    fn update_state(&self) {
        let lib_wallet_state = self
            .wallet()
            .wallet_state_at(self.lib)
            .expect("WalletState at LIB must exist");

        self.updater.update(Some(RecoveryState {
            next_new_voucher_index: self.next_new_voucher_index,
            vouchers: self.wallet.vouchers().clone(),
            lib_wallet_state: Some((self.lib, lib_wallet_state)),
            pending_claims: self.pending_claims.clone(),
        }));
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr};

    use super::*;

    const EXPIRY_BLOCKS: u64 = 4;

    #[test]
    fn pending_claim_does_not_expire_without_lib_progress() {
        let nullifier = VoucherNullifier::default();
        let mut pending_claims = PendingClaims::default();

        pending_claims.reserve(nullifier);
        pending_claims.evict_expired(0, EXPIRY_BLOCKS);

        assert!(pending_claims.is_reserved(&nullifier));
    }

    #[test]
    fn pending_claim_expires_after_enough_lib_progress() {
        let nullifier = VoucherNullifier::default();
        let mut pending_claims = PendingClaims::default();

        pending_claims.reserve(nullifier);
        pending_claims.evict_expired(EXPIRY_BLOCKS - 1, EXPIRY_BLOCKS);
        assert!(pending_claims.is_reserved(&nullifier));

        pending_claims.evict_expired(1, EXPIRY_BLOCKS);
        assert!(!pending_claims.is_reserved(&nullifier));
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
    fn pending_note_removed_when_observed_spent() {
        let spent = NoteId::from(Fr::ZERO);
        let kept = NoteId::from(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([spent, kept]);
        pending_notes.remove_spent(&HashSet::from([spent]));

        let note_ids = pending_notes.note_ids();
        assert!(!note_ids.contains(&spent));
        assert!(note_ids.contains(&kept));
    }

    #[test]
    fn released_pending_note_becomes_available() {
        let note_id = NoteId::from(Fr::ONE);
        let mut pending_notes = PendingNotes::default();

        pending_notes.reserve([note_id]);
        pending_notes.release([note_id]);

        assert!(pending_notes.note_ids().is_empty());
    }
}
