use std::collections::HashMap;

use lb_core::{
    header::HeaderId,
    mantle::ops::leader_claim::{VoucherCm, VoucherNullifier},
};
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
            security_param,
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
        new_immutable_blocks_count: u64,
        pruned_nullifiers: impl IntoIterator<Item = VoucherNullifier>,
    ) {
        self.lib = new_lib;
        self.wallet.prune_states(pruned_blocks);
        self.wallet.prune_vouchers(pruned_nullifiers);
        self.pending_claims
            .evict_expired(new_immutable_blocks_count, self.security_param);
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
}
