use std::cmp::Ordering;

use lb_core::mantle::{
    Value,
    ops::leader_claim::{LeaderClaimOp, RewardsRoot, VoucherCm, VoucherNullifier},
};
use lb_cryptarchia_engine::Epoch;
use lb_mmr::MerkleMountainRange;
use rpds::VectorSync;

/// A leader state in the mantle ledger.
///
/// NOTE: Most collection fields in this struct should use `rpds`
/// since we keep a copy of this state for each block.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaderState {
    // current epoch
    epoch: Epoch,
    // vouchers that can be claimed in this epoch
    // this is updated once at the start of each epoch
    claimable_vouchers_root: RewardsRoot,
    n_claimable_vouchers: u64,
    // nullifiers of vouchers that have been claimed since genesis
    nfs: rpds::HashTrieSetSync<VoucherNullifier>,
    // rewards to be distributed
    // at the start of each epoch this is increased by the amount of rewards
    // that have been collected in the previous epoch.
    // unclaimed rewards are carried over to the next epoch.
    claimable_rewards: Value,
    /// Rewards that are being collected during the current epoch.
    /// This will be added to the `claimable_rewards` when a new epoch starts.
    pending_rewards: Value,
    // MMR of vouchers that can be claimed in this epoch.
    // This is updated once at the start of each epoch.
    // Merkle path maintenance is handled by the wallet.
    claimable_vouchers: MerkleMountainRange<VoucherCm, lb_core::crypto::ZkHasher, 33>,
    // List of vouchers that are waiting to be added at the start of
    // the next epoch
    pending_vouchers: VectorSync<VoucherCm>,
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
            claimable_vouchers_root: RewardsRoot::default(),
            n_claimable_vouchers: 0,
            nfs: rpds::HashTrieSetSync::new_sync(),
            pending_rewards: 0,
            claimable_rewards: 0,
            claimable_vouchers: MerkleMountainRange::new(),
            pending_vouchers: VectorSync::new_sync(),
        }
    }

    /// Apply a block header, flushing pending vouchers into the MMR on epoch
    /// transition. Returns a [`ClaimableVouchersUpdate`] if an epoch
    /// transition occurred.
    pub fn try_apply_header(
        self,
        epoch: Epoch,
        voucher_cm: VoucherCm,
    ) -> Result<(Self, Option<ClaimableVouchersUpdate>), Error> {
        self.update_epoch_state(epoch)
            .map(|(s, update)| (s.add_pending_voucher(voucher_cm), update))
    }

    /// If the epoch has advanced, flush pending vouchers and rewards.
    fn update_epoch_state(
        mut self,
        epoch: Epoch,
    ) -> Result<(Self, Option<ClaimableVouchersUpdate>), Error> {
        match epoch.cmp(&self.epoch) {
            Ordering::Equal => Ok((self, None)),
            Ordering::Less => Err(Error::InvalidEpoch {
                current: self.epoch,
                incoming: epoch,
            }),
            Ordering::Greater => {
                let update = self.flush_pending_vouchers();
                self = self.update_claimable_rewards();
                self.epoch = epoch;
                Ok((self, Some(update)))
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

    /// Add a voucher to be included in the Merkle tree at the start of the
    /// next epoch
    fn add_pending_voucher(mut self, voucher_cm: VoucherCm) -> Self {
        self.pending_vouchers.push_back_mut(voucher_cm);
        self
    }

    /// Flush all pending vouchers into the MMR, returning a
    /// [`ClaimableVouchersUpdate`] with the pre-flush MMR and flushed list.
    fn flush_pending_vouchers(&mut self) -> ClaimableVouchersUpdate {
        let pre_flush_mmr = self.claimable_vouchers.clone();
        let flushed_vouchers =
            std::mem::replace(&mut self.pending_vouchers, VectorSync::new_sync());

        for &voucher_cm in &flushed_vouchers {
            self.claimable_vouchers = self
                .claimable_vouchers
                .push(voucher_cm)
                .expect("MMR should not be full");
        }

        self.claimable_vouchers_root = self.claimable_vouchers.frontier_root().into();
        self.n_claimable_vouchers = self.claimable_vouchers.len() as u64;

        ClaimableVouchersUpdate {
            pre_flush_mmr,
            flushed_vouchers,
        }
    }

    /// Insert all pending rewards into the reward pool and reset it
    fn update_claimable_rewards(mut self) -> Self {
        self.claimable_rewards += self.pending_rewards;
        self.pending_rewards = Value::default();
        self
    }

    pub(crate) const fn claimable_vouchers_root(&self) -> RewardsRoot {
        self.claimable_vouchers_root
    }

    /// Claim the reward associated with a voucher.
    /// Any cryptographic proof of correct derivation of the voucher nullifier
    /// and membership proof in the merkle tree is expected to happen
    /// outside of this function.
    pub fn claim(&self, op: &LeaderClaimOp) -> Result<(Self, Value), Error> {
        if self.nfs.contains(&op.voucher_nullifier) {
            return Err(Error::DuplicatedVoucherNullifier);
        }

        if self.claimable_vouchers_root != op.rewards_root {
            return Err(Error::VoucherNotFound);
        }

        let nfs = self.nfs.insert(op.voucher_nullifier);
        let n_unclaimed_vouchers = self
            .n_claimable_vouchers
            .checked_sub(self.nfs.size() as u64)
            .expect("more nullifiers than vouchers");
        let reward_amount = if n_unclaimed_vouchers > 0 {
            self.claimable_rewards / n_unclaimed_vouchers
        } else {
            0
        };

        let claimable_rewards = self.claimable_rewards - reward_amount;
        Ok((
            Self {
                epoch: self.epoch,
                claimable_vouchers_root: self.claimable_vouchers_root,
                n_claimable_vouchers: self.n_claimable_vouchers,
                nfs,
                pending_rewards: self.pending_rewards,
                claimable_rewards,
                claimable_vouchers: self.claimable_vouchers.clone(),
                pending_vouchers: self.pending_vouchers.clone(),
            },
            reward_amount,
        ))
    }
}

/// Emitted on epoch transition when pending vouchers are flushed into the MMR.
///
/// Contains the pre-flush MMR state and the flushed voucher list, allowing
/// the caller to replay [`lb_mmr::MerkleMountainRange::push_with_paths`]
/// locally to update its own tracked merkle paths.
#[derive(Clone, Debug)]
pub struct ClaimableVouchersUpdate {
    /// MMR state before the pending vouchers were flushed.
    pub pre_flush_mmr: MerkleMountainRange<VoucherCm, lb_core::crypto::ZkHasher, 33>,
    /// Vouchers that were flushed into the MMR, in push order.
    pub flushed_vouchers: VectorSync<VoucherCm>,
}

#[cfg(test)]
mod tests {
    use lb_core::crypto::ZkHasher;
    use lb_groth16::{Field as _, Fr};

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
        let (state, _) = state
            .try_apply_header(1.into(), Fr::ZERO.into())
            .unwrap();
        let (state, _) = state
            .try_apply_header(1.into(), Fr::ONE.into())
            .unwrap();
        let (state, _) = state
            .try_apply_header(1.into(), Fr::from(2u64).into())
            .unwrap();
        let (state, _) = state
            .try_apply_header(2.into(), Fr::from(3u64).into())
            .unwrap();
        let state = LeaderState {
            claimable_rewards: 300,
            ..state
        };
        let op1 = LeaderClaimOp {
            rewards_root: state.claimable_vouchers_root,
            voucher_nullifier: Fr::ZERO.into(),
        };
        let (state, bal) = state.claim(&op1).unwrap();
        assert_eq!(bal, 100);
        assert_eq!(state.claimable_rewards, 200);
        let op2 = LeaderClaimOp {
            rewards_root: state.claimable_vouchers_root,
            voucher_nullifier: Fr::ONE.into(),
        };
        let (state, bal) = state.claim(&op2).unwrap();
        assert_eq!(bal, 100);
        assert_eq!(state.claimable_rewards, 100);
        let op3 = LeaderClaimOp {
            rewards_root: state.claimable_vouchers_root,
            voucher_nullifier: Fr::from(2u64).into(),
        };
        let (state, bal) = state.claim(&op3).unwrap();
        assert_eq!(bal, 100);
        assert_eq!(state.claimable_rewards, 0);
    }

    #[test]
    fn test_epoch_transition() {
        let state = LeaderState::new();
        let (state, update) = state
            .try_apply_header(1.into(), Fr::ZERO.into())
            .unwrap();
        assert_eq!(state.epoch, 1.into());
        assert_eq!(state.n_claimable_vouchers, 0);
        // Epoch 0→1 transition occurs but with no pending vouchers
        assert!(update.unwrap().flushed_vouchers.is_empty());
        let (state, update) = state
            .try_apply_header(2.into(), Fr::ONE.into())
            .unwrap();
        assert_eq!(state.epoch, 2.into());
        assert_eq!(state.n_claimable_vouchers, 1);
        assert_eq!(update.unwrap().flushed_vouchers.len(), 1);
        let (state, update) = state
            .try_apply_header(2.into(), Fr::from(2u64).into())
            .unwrap();
        assert_eq!(state.epoch, 2.into());
        assert_eq!(state.n_claimable_vouchers, 1);
        assert!(update.is_none());
        let (state, update) = state
            .try_apply_header(3.into(), Fr::from(3u64).into())
            .unwrap();
        assert_eq!(state.epoch, 3.into());
        assert_eq!(state.n_claimable_vouchers, 3);
        assert_eq!(update.unwrap().flushed_vouchers.len(), 2);
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
        let (state, _) = state
            .try_apply_header(4.into(), Fr::from(5u64).into())
            .unwrap();
        assert_eq!(state.epoch, 4.into());
        assert_eq!(state.n_claimable_vouchers, 4);
    }

    /// Helper: replay push_with_paths from a [`ClaimableVouchersUpdate`],
    /// simulating what the wallet does. New paths are appended to `tracked`
    /// during replay so subsequent pushes keep them up-to-date, then
    /// drained and returned.
    fn replay_pushes(
        update: &ClaimableVouchersUpdate,
        tracked: &mut Vec<lb_mmr::MerklePath>,
    ) -> Vec<(VoucherCm, lb_mmr::MerklePath)> {
        let n_tracked = tracked.len();
        let mut mmr = update.pre_flush_mmr.clone();
        let mut cms = Vec::new();
        for &cm in &update.flushed_vouchers {
            let (new_mmr, path) = mmr.push_with_paths(cm, tracked).unwrap();
            mmr = new_mmr;
            tracked.push(path);
            cms.push(cm);
        }
        cms.into_iter()
            .zip(tracked.drain(n_tracked..))
            .collect()
    }

    #[test]
    fn test_epoch_transition_paths_via_replay() {
        let state = LeaderState::new();
        // Epoch 1: add 3 vouchers
        let (state, _) = state
            .try_apply_header(1.into(), Fr::ZERO.into())
            .unwrap();
        let (state, _) = state
            .try_apply_header(1.into(), Fr::ONE.into())
            .unwrap();
        let (state, _) = state
            .try_apply_header(1.into(), Fr::from(2u64).into())
            .unwrap();

        // Epoch 2: epoch transition flushes 3 pending vouchers
        let (state, update) = state
            .try_apply_header(2.into(), Fr::from(3u64).into())
            .unwrap();
        let update = update.unwrap();
        assert_eq!(update.flushed_vouchers.len(), 3);

        // Replay pushes to get paths (as the wallet would)
        let mut tracked = Vec::new();
        let new_paths = replay_pushes(&update, &mut tracked);
        assert_eq!(new_paths.len(), 3);

        // Each path should verify against the current frontier root
        let root: Fr = state.claimable_vouchers_root.into();
        for (cm, path) in &new_paths {
            let leaf_hash = *cm.as_ref();
            assert!(
                path.verify::<ZkHasher>(leaf_hash, root),
                "path for {cm:?} should verify",
            );
        }
    }

    #[test]
    fn test_tracked_paths_updated_across_epochs() {
        let state = LeaderState::new();
        // Epoch 1: add 2 vouchers
        let (state, _) = state
            .try_apply_header(1.into(), Fr::ZERO.into())
            .unwrap();
        let (state, _) = state
            .try_apply_header(1.into(), Fr::ONE.into())
            .unwrap();

        // Epoch 2: flush epoch 1 vouchers, replay to get paths
        let (state, update) = state
            .try_apply_header(2.into(), Fr::from(2u64).into())
            .unwrap();
        let update = update.unwrap();
        let mut tracked = Vec::new();
        let new_paths = replay_pushes(&update, &mut tracked);
        assert_eq!(new_paths.len(), 2);
        // Track the new paths (wallet would keep paths for its own vouchers)
        for (_, path) in new_paths {
            tracked.push(path);
        }

        // Epoch 3: flush epoch 2 voucher, replay with tracked paths
        let (state, update) = state
            .try_apply_header(3.into(), Fr::from(3u64).into())
            .unwrap();
        let update = update.unwrap();
        let new_paths = replay_pushes(&update, &mut tracked);
        assert_eq!(new_paths.len(), 1);

        // All paths (old tracked + new) should verify against the updated root
        let root: Fr = state.claimable_vouchers_root.into();
        for path in &tracked {
            let cm = VoucherCm::from([Fr::ZERO, Fr::ONE][path.leaf_index()]);
            let leaf_hash = *cm.as_ref();
            assert!(
                path.verify::<ZkHasher>(leaf_hash, root),
                "tracked path at index {} should verify after epoch 3",
                path.leaf_index()
            );
        }
        for (cm, path) in &new_paths {
            let leaf_hash = *cm.as_ref();
            assert!(
                path.verify::<ZkHasher>(leaf_hash, root),
                "new path should verify after epoch 3"
            );
        }
    }

    #[test]
    fn test_cannot_claim_reward_twice() {
        let state = LeaderState::new();
        let op = LeaderClaimOp {
            voucher_nullifier: Fr::ZERO.into(),
            rewards_root: state.claimable_vouchers_root,
        };
        let (state, balance) = state.claim(&op).unwrap();
        assert_eq!(balance, 0);
        let err = state.claim(&op).unwrap_err();
        assert_eq!(err, Error::DuplicatedVoucherNullifier);
    }
}
