use std::cmp::Ordering;

use lb_core::{
    crypto::ZkHasher,
    mantle::{
        Value,
        ops::leader_claim::{LeaderClaimOp, RewardsRoot, VoucherCm, VoucherNullifier},
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
    /// current epoch
    epoch: Epoch,
    /// A snapshot of voucher commitments, updated once at each epoch start.
    vouchers_snapshot: VouchersSnapshot,
    /// nullifiers of vouchers that have been claimed since genesis
    nfs: rpds::HashTrieSetSync<VoucherNullifier>,
    /// rewards to be distributed
    /// at the start of each epoch this is increased by the amount of rewards
    /// that have been collected in the previous epoch.
    /// unclaimed rewards are carried over to the next epoch.
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
    /// Number of voucher commitments in the tree
    /// This includes voucher commitments that have been already claimed
    /// because claiming is done by nullifier that is transparently coupled
    /// with commitment.
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

    /// Claim the reward associated with a voucher.
    /// Any cryptographic proof of correct derivation of the voucher nullifier
    /// and membership proof in the merkle tree is expected to happen
    /// outside of this function.
    pub fn claim(&self, op: &LeaderClaimOp) -> Result<(Self, Value), Error> {
        if self.nfs.contains(&op.voucher_nullifier) {
            return Err(Error::DuplicatedVoucherNullifier);
        }

        if self.vouchers_snapshot_root() != op.rewards_root {
            return Err(Error::VoucherNotFound);
        }

        let reward_amount = self.reward_amount();
        let nfs = self.nfs.insert(op.voucher_nullifier);
        let claimable_rewards = self.claimable_rewards - reward_amount;
        Ok((
            Self {
                nfs,
                claimable_rewards,
                ..self.clone()
            },
            reward_amount,
        ))
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{Field as _, Fr};
    use lb_key_management_system_keys::keys::ZkPublicKey;

    use super::*;

    impl LeaderState {
        #[cfg(test)]
        #[must_use]
        pub fn get_pending_rewards(&self) -> Value {
            self.pending_rewards
        }
    }

    #[test]
    fn test_reward_amounts() {
        let state = LeaderState::new();
        let state = state.try_apply_header(1.into(), Fr::ZERO.into()).unwrap();
        let state = state.try_apply_header(1.into(), Fr::ONE.into()).unwrap();
        let state = state
            .try_apply_header(1.into(), Fr::from(2u64).into())
            .unwrap();
        let state = state
            .try_apply_header(2.into(), Fr::from(3u64).into())
            .unwrap();
        let state = LeaderState {
            claimable_rewards: 300,
            ..state
        };
        let op1 = LeaderClaimOp {
            rewards_root: state.vouchers_snapshot_root(),
            voucher_nullifier: Fr::ZERO.into(),
            pk: ZkPublicKey::zero(),
        };
        let (state, bal) = state.claim(&op1).unwrap();
        assert_eq!(bal, 100);
        assert_eq!(state.claimable_rewards, 200);
        let op2 = LeaderClaimOp {
            rewards_root: state.vouchers_snapshot_root(),
            voucher_nullifier: Fr::ONE.into(),
            pk: ZkPublicKey::zero(),
        };
        let (state, bal) = state.claim(&op2).unwrap();
        assert_eq!(bal, 100);
        assert_eq!(state.claimable_rewards, 100);
        let op3 = LeaderClaimOp {
            rewards_root: state.vouchers_snapshot_root(),
            voucher_nullifier: Fr::from(2u64).into(),
            pk: ZkPublicKey::zero(),
        };
        let (state, bal) = state.claim(&op3).unwrap();
        assert_eq!(bal, 100);
        assert_eq!(state.claimable_rewards, 0);
    }

    #[test]
    fn test_epoch_transition() {
        let state = LeaderState::new();
        let state = state.try_apply_header(1.into(), Fr::ZERO.into()).unwrap();
        assert_eq!(state.epoch, 1.into());
        assert_eq!(state.vouchers_snapshot.count, 0);
        let state = state.try_apply_header(2.into(), Fr::ONE.into()).unwrap();
        assert_eq!(state.epoch, 2.into());
        assert_eq!(state.vouchers_snapshot.count, 1);
        let state = state
            .try_apply_header(2.into(), Fr::from(2u64).into())
            .unwrap();
        assert_eq!(state.epoch, 2.into());
        assert_eq!(state.vouchers_snapshot.count, 1);
        let state = state
            .try_apply_header(3.into(), Fr::from(3u64).into())
            .unwrap();
        assert_eq!(state.epoch, 3.into());
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
        assert_eq!(state.epoch, 4.into());
        assert_eq!(state.vouchers_snapshot.count, 4);
    }

    #[test]
    fn test_cannot_claim_reward_twice() {
        let state = LeaderState::new();
        let op = LeaderClaimOp {
            voucher_nullifier: Fr::ZERO.into(),
            rewards_root: state.vouchers_snapshot_root(),
            pk: ZkPublicKey::zero(),
        };
        let (state, balance) = state.claim(&op).unwrap();
        assert_eq!(balance, 0);
        let err = state.claim(&op).unwrap_err();
        assert_eq!(err, Error::DuplicatedVoucherNullifier);
    }
}
