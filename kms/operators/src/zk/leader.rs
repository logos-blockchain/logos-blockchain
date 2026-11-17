use std::fmt::{Debug, Formatter};

use lb_core::{
    mantle::{Note, Value},
    proofs::leader_proof::{LeaderPrivate, LeaderPublic, check_winning},
};
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::{
    ZkKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_log_targets::kms;
use lb_pol::PolWalletInputsData;
use tracing::debug;

const LOG_TARGET: &str = kms::operators::ZK_LEADER;

pub struct CheckLotteryWinning {
    result_channel: tokio::sync::oneshot::Sender<bool>,
    note: Note,
    public_inputs: LeaderPublic,
}

impl Debug for CheckLotteryWinning {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "CheckConditionWithLeaderKey")
    }
}

impl CheckLotteryWinning {
    #[must_use]
    pub const fn new(
        result_channel: tokio::sync::oneshot::Sender<bool>,
        note: Note,
        public_inputs: LeaderPublic,
    ) -> Self {
        Self {
            result_channel,
            note,
            public_inputs,
        }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for CheckLotteryWinning {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self {
            result_channel,
            note,
            public_inputs,
        } = *self;
        // The wallet stakes with the nullifier key of its spending secret key.
        let nullifier_key = key.nullifier_key();
        if result_channel
            .send(check_winning(
                &note,
                public_inputs,
                nullifier_key,
                nullifier_key,
            ))
            .is_err()
        {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}

pub struct BuildPrivateInputsWithLeaderKey {
    result_channel: tokio::sync::oneshot::Sender<LeaderPrivate>,
    leader_public: LeaderPublic,
    wallet: PolWalletInputsData,
    reward_value: Value,
}

impl Debug for BuildPrivateInputsWithLeaderKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "BuildWithLeaderKey")
    }
}

impl BuildPrivateInputsWithLeaderKey {
    #[must_use]
    pub const fn new(
        result_channel: tokio::sync::oneshot::Sender<LeaderPrivate>,
        leader_public: LeaderPublic,
        wallet: PolWalletInputsData,
        reward_value: Value,
    ) -> Self {
        Self {
            result_channel,
            leader_public,
            wallet,
            reward_value,
        }
    }
}

#[async_trait::async_trait]
impl SecureKeyOperator for BuildPrivateInputsWithLeaderKey {
    type Key = ZkKey;
    type Error = KeyError;

    async fn execute(self: Box<Self>, key: &Self::Key) -> Result<(), Self::Error> {
        let Self {
            result_channel,
            leader_public,
            wallet,
            reward_value,
        } = *self;
        // The proof never takes the spending secret key: the wallet stakes with
        // its nullifier key.
        let nullifier_key = key.nullifier_key();
        let wallet = PolWalletInputsData {
            nullifier_key,
            staking_secret_key: nullifier_key,
            ..wallet
        };
        // Not bound to a block yet: the proposer binds it with `with_msg` to the
        // block ID, which covers the entropy contribution of this witness.
        let private = LeaderPrivate::new(leader_public, wallet, Fr::ZERO, reward_value);
        if result_channel.send(private).is_err() {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::{ZkPublicKey, secured_key::SecuredKey as _};
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn build_private_inputs_fills_the_nullifier_key_as_staking_secret_key() {
        let (public, private) = lb_core::proofs::leader_proof::test_utils::note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::from(5u64),
            &Note::new(3, Fr::from(13u64), ZkPublicKey::new(Fr::from(17u64))),
        );
        let mut wallet = private.input().wallet.clone();
        wallet.nullifier_key = Fr::ZERO;
        wallet.staking_secret_key = Fr::ZERO;
        let (sender, receiver) = oneshot::channel();
        ZkKey::new(Fr::from(7u64))
            .execute(BuildPrivateInputsWithLeaderKey::new(
                sender, public, wallet, 3,
            ))
            .await
            .unwrap();
        let built = receiver.await.unwrap();
        let nullifier_key = ZkKey::new(Fr::from(7u64)).nullifier_key();
        assert_eq!(
            (
                built.input().wallet.nullifier_key,
                built.input().wallet.staking_secret_key
            ),
            (nullifier_key, nullifier_key)
        );
        assert_eq!(built.input().chain.reward_value, 3);
    }
}
