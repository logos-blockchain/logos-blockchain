use std::cmp::Ordering;

use lb_core::{
    crypto::ZkHasher,
    mantle::{
        Value,
        ops::leader_claim::{RewardsRoot, VoucherCm, VoucherNullifier},
    },
};
use lb_cryptarchia_engine::Epoch;
use lb_mmr::MerkleMountainRange;
use serde::{Deserialize, Serialize};

/// A leader state in the mantle ledger.
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderState {
    /// Current epoch
    epoch: Epoch,
    /// A snapshot of voucher commitments, updated once at each epoch start.
    vouchers_snapshot: VouchersSnapshot,
    /// Nullifiers of vouchers that have been claimed since genesis
    nfs: rpds::HashTrieSetSync<VoucherNullifier>,
    /// Rewards to be distributed.
    /// At the start of each epoch this is increased by the amount of rewards
    /// that have been collected in the previous epoch.
    /// Unclaimed rewards are carried over to the next epoch.
    claimable_rewards: Value,
    /// Rewards that are being collected during the current epoch.
    /// This will be added to the `claimable_rewards` when a new epoch starts.
    pending_rewards: Value,
    /// MMR of all voucher commitments included in the chain
    vouchers: MerkleMountainRange<VoucherCm, ZkHasher>,
}

/// A snapshot of voucher commitments, updated once at each epoch start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct VouchersSnapshot {
    /// Root of voucher commitment tree
    root: RewardsRoot,
    /// Number of voucher commitments in the tree.
    /// This includes voucher commitments that have been already claimed
    /// because claims are done by nullifiers that is not transparently
    /// linked to commitments.
    count: u64,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("voucher nullifier already used")]
    DuplicatedVoucherNullifier,
    #[error("voucher not found")]
    VoucherNotFound,
    #[error("Cannot time travel to the past")]
    InvalidEpoch { current: Epoch, incoming: Epoch },
}

impl Default for LeaderState {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaderState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: 0.into(),
            vouchers_snapshot: VouchersSnapshot {
                root: RewardsRoot::default(),
                count: 0,
            },
            nfs: rpds::HashTrieSetSync::new_sync(),
            pending_rewards: 0,
            claimable_rewards: 0,
            vouchers: MerkleMountainRange::new(),
        }
    }

    #[must_use]
    pub const fn nullifiers(&self) -> &rpds::HashTrieSetSync<VoucherNullifier> {
        &self.nfs
    }

    #[must_use]
    pub fn nullifiers_cloned(&self) -> rpds::HashTrieSetSync<VoucherNullifier> {
        self.nfs.clone()
    }

    pub fn update_nullifiers(&mut self, nullifiers: rpds::HashTrieSetSync<VoucherNullifier>) {
        self.nfs = nullifiers;
    }

    pub const fn update_rewards(&mut self, claimable_rewards: Value) {
        self.claimable_rewards = claimable_rewards;
    }

    #[must_use]
    pub const fn claimable_rewards(&self) -> Value {
        self.claimable_rewards
    }

    pub fn try_apply_header(self, epoch: Epoch, voucher_cm: VoucherCm) -> Result<Self, Error> {
        Ok(self.update_epoch_state(epoch)?.add_voucher(voucher_cm))
    }

    fn update_epoch_state(mut self, epoch: Epoch) -> Result<Self, Error> {
        match epoch.cmp(&self.epoch) {
            Ordering::Equal => Ok(self),
            Ordering::Less => Err(Error::InvalidEpoch {
                current: self.epoch,
                incoming: epoch,
            }),
            Ordering::Greater => {
                self = self.snapshot_vouchers();
                self = self.update_claimable_rewards();
                self.epoch = epoch;
                Ok(self)
            }
        }
    }

    /// Add a block reward to the pending rewards that are added to the pool
    /// during epoch transition
    #[must_use]
    pub const fn add_pending_rewards(mut self, rewards: Value) -> Self {
        self.pending_rewards += rewards;
        self
    }

    /// Add a voucher commitment to the MMR.
    fn add_voucher(mut self, voucher_cm: VoucherCm) -> Self {
        self.vouchers = self
            .vouchers
            .push(voucher_cm)
            .expect("Vouchers MMR shouldn't be full");
        self
    }

    /// Insert all pending vouchers into the Merkle tree,
    /// and update the Merkle root.
    fn snapshot_vouchers(mut self) -> Self {
        self.vouchers_snapshot = VouchersSnapshot {
            root: self.vouchers.frontier_root().into(),
            count: self
                .vouchers
                .len()
                .try_into()
                .expect("vouchers count must be u64"),
        };
        self
    }

    /// Insert all pending rewards into the reward pool and reset it
    fn update_claimable_rewards(mut self) -> Self {
        self.claimable_rewards += self.pending_rewards;
        self.pending_rewards = Value::default();
        self
    }

    /// Get the root of the voucher commitments snapshot.
    pub(crate) const fn vouchers_snapshot_root(&self) -> RewardsRoot {
        self.vouchers_snapshot.root
    }

    /// Get the MMR of all voucher commitments included in the chain.
    pub(crate) const fn vouchers(&self) -> &MerkleMountainRange<VoucherCm, ZkHasher> {
        &self.vouchers
    }

    /// Compute the per-voucher reward given current state.
    #[must_use]
    pub fn reward_amount(&self) -> Value {
        let n_unclaimed_vouchers = self
            .vouchers_snapshot
            .count
            .saturating_sub(self.nfs.size() as u64);
        self.claimable_rewards
            .checked_div(n_unclaimed_vouchers)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Field as _, Fr};

    use super::*;

    impl LeaderState {
        #[must_use]
        pub fn get_pending_rewards(&self) -> Value {
            self.pending_rewards
        }
    }

    #[test]
    fn test_epoch_transition() {
        let state = LeaderState::new();
        let state = state.try_apply_header(1.into(), Fr::ZERO.into()).unwrap();
        assert_eq!(state.epoch, 1);
        assert_eq!(state.vouchers_snapshot.count, 0);
        let state = state.try_apply_header(2.into(), Fr::ONE.into()).unwrap();
        assert_eq!(state.epoch, 2);
        assert_eq!(state.vouchers_snapshot.count, 1);
        let state = state
            .try_apply_header(2.into(), Fr::from(2u64).into())
            .unwrap();
        assert_eq!(state.epoch, 2);
        assert_eq!(state.vouchers_snapshot.count, 1);
        let state = state
            .try_apply_header(3.into(), Fr::from(3u64).into())
            .unwrap();
        assert_eq!(state.epoch, 3);
        assert_eq!(state.vouchers_snapshot.count, 3);
        let err = state
            .clone()
            .try_apply_header(2.into(), Fr::from(4u64).into())
            .unwrap_err();
        assert_eq!(
            err,
            Error::InvalidEpoch {
                current: 3.into(),
                incoming: 2.into()
            }
        );
        let state = state
            .try_apply_header(4.into(), Fr::from(5u64).into())
            .unwrap();
        assert_eq!(state.epoch, 4);
        assert_eq!(state.vouchers_snapshot.count, 4);
    }
}
