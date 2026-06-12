use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use lb_core::{
    header::HeaderId,
    mantle::{
        NoteId,
        ops::leader_claim::{VoucherCm, VoucherNullifier},
    },
};
use lb_ledger::LedgerState;
use lb_log_targets::wallet;
use lb_wallet::{KnownVoucher, Vouchers, WalletBlock, WalletError, WalletState};
use overwatch::services::state::StateUpdater;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{KeyId, WalletServiceError, WalletServiceSettings};

type VoucherIndex = u64;
type VoucherId = (KeyId, VoucherIndex);
pub type Wallet = lb_wallet::Wallet<KeyId, VoucherId>;

const VOUCHER_PENDING_TIMEOUT: Duration = Duration::from_mins(1);

#[derive(Debug, Clone)]
struct PendingClaim {
    funding_notes: HashSet<NoteId>,
    reserved_at: Instant,
}

#[derive(Debug, Clone, Default)]
struct PendingClaims {
    claims: HashMap<VoucherNullifier, PendingClaim>,
}

impl PendingClaims {
    fn reserve(&mut self, nullifier: VoucherNullifier) {
        self.prune_expired();
        debug!(
            target: wallet::SERVICE,
            ?nullifier,
            "Reserved claim voucher"
        );
        self.claims.insert(
            nullifier,
            PendingClaim {
                funding_notes: HashSet::new(),
                reserved_at: Instant::now(),
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

    fn is_reserved(&mut self, nullifier: &VoucherNullifier) -> bool {
        self.prune_expired();
        self.claims.contains_key(nullifier)
    }

    fn reserve_funding_notes(
        &mut self,
        nullifier: VoucherNullifier,
        funding_notes: impl IntoIterator<Item = NoteId>,
    ) -> Result<(), WalletServiceError> {
        self.prune_expired();
        let Some(claim) = self.claims.get_mut(&nullifier) else {
            return Err(WalletServiceError::ClaimReservationNotFound(nullifier));
        };

        claim.funding_notes = funding_notes.into_iter().collect();
        debug!(
            target: wallet::SERVICE,
            ?nullifier,
            funding_notes = claim.funding_notes.len(),
            "Reserved claim funding notes"
        );

        Ok(())
    }

    fn funding_notes(&mut self) -> Vec<NoteId> {
        self.prune_expired();

        self.claims
            .values()
            .flat_map(|claim| claim.funding_notes.iter().copied())
            .collect()
    }

    fn prune_expired(&mut self) {
        let now = Instant::now();
        let before_count = self.claims.len();

        self.claims.retain(|nullifier, claim| {
            let expired = now.duration_since(claim.reserved_at) >= VOUCHER_PENDING_TIMEOUT;

            if expired {
                debug!(
                    target: wallet::SERVICE,
                    ?nullifier,
                    "Removing expired pending claim reservation (timeout: {:?})",
                    VOUCHER_PENDING_TIMEOUT
                );
            }

            !expired
        });

        if before_count != self.claims.len() {
            debug!(
                target: wallet::SERVICE,
                "Cleaned {} expired pending claim reservation(s)",
                before_count - self.claims.len()
            );
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryState {
    next_new_voucher_index: VoucherIndex,
    vouchers: Vouchers<VoucherId>,
    /// [`WalletState`] at the last known LIB.
    /// `None` on fresh start; populated after the first LIB update.
    lib_wallet_state: Option<(HeaderId, WalletState)>,
    // After restart we cannot know whether a reserved claim was submitted,
    // accepted, or dropped, so recovering it could block usable vouchers/notes.
    #[serde(skip)]
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
}

impl<'u> ServiceState<'u> {
    pub fn new(
        state: RecoveryState,
        settings: &WalletServiceSettings,
        lib: HeaderId,
        lib_ledger: &LedgerState,
        updater: &'u StateUpdater<Option<RecoveryState>>,
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
        }
    }

    pub const fn lib(&self) -> HeaderId {
        self.lib
    }

    pub fn get_and_inc_next_new_voucher_index(&mut self) -> VoucherIndex {
        let index = self.next_new_voucher_index;
        self.next_new_voucher_index += 1;
        self.update_state();
        index
    }

    pub fn add_known_voucher(&mut self, cm: VoucherCm, nf: VoucherNullifier, id: VoucherId) {
        self.wallet.add_known_voucher(cm, nf, id);
        self.update_state();
    }

    pub fn apply_block(&mut self, block: &WalletBlock) -> Result<(), WalletError> {
        self.wallet.apply_block(block)?;
        self.update_state();
        Ok(())
    }

    pub fn advance_lib(
        &mut self,
        new_lib: HeaderId,
        pruned_blocks: impl IntoIterator<Item = HeaderId>,
        pruned_nullifiers: impl IntoIterator<Item = VoucherNullifier>,
    ) {
        self.lib = new_lib;
        self.wallet.prune_states(pruned_blocks);
        self.wallet.prune_vouchers(pruned_nullifiers);
        self.update_state();
    }

    pub const fn wallet(&self) -> &Wallet {
        &self.wallet
    }

    pub fn find_available_claimable_voucher(
        &mut self,
        tip: HeaderId,
    ) -> (Option<KnownVoucher>, usize) {
        let wallet_state = &self.wallet;
        let pending_claims = &mut self.pending_claims;
        let mut pending_count = 0;

        let available_voucher = wallet_state
            .voucher_commitments_and_nullifiers()
            .find(|voucher| {
                if pending_claims.is_reserved(&voucher.nullifier) {
                    pending_count += 1;

                    return false;
                }

                matches!(
                    wallet_state.voucher_path_snapshot(tip, &voucher.commitment),
                    Ok(Some(_))
                )
            });

        (available_voucher, pending_count)
    }

    pub fn reserve_claim(&mut self, nullifier: VoucherNullifier) {
        self.pending_claims.reserve(nullifier);
    }

    pub fn release_claim_reservation(&mut self, nullifier: VoucherNullifier) {
        self.pending_claims.release(nullifier);
    }

    pub fn reserve_claim_funding_notes(
        &mut self,
        nullifier: VoucherNullifier,
        funding_notes: impl IntoIterator<Item = NoteId>,
    ) -> Result<(), WalletServiceError> {
        self.pending_claims
            .reserve_funding_notes(nullifier, funding_notes)
    }

    pub fn pending_claim_funding_notes(&mut self) -> Vec<NoteId> {
        self.pending_claims.funding_notes()
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
