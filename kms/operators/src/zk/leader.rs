use std::fmt::{Debug, Formatter};

use lb_core::{
    mantle::Note,
    proofs::leader_proof::{LeaderPrivate, LeaderPublic, check_winning},
};
use lb_groth16::Fr;
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
        if result_channel
            .send(check_winning(
                &note,
                public_inputs,
                &key.to_public_key(),
                *key.as_fr(),
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
    msg: Fr,
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
        msg: Fr,
    ) -> Self {
        Self {
            result_channel,
            leader_public,
            wallet,
            msg,
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
            msg,
        } = *self;
        let wallet = PolWalletInputsData {
            secret_key: *key.as_fr(),
            ..wallet
        };
        if result_channel
            .send(LeaderPrivate::new(leader_public, wallet, msg))
            .is_err()
        {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::AdditiveGroup as _;
    use lb_key_management_system_keys::keys::secured_key::SecuredKey as _;
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn build_private_inputs_fills_the_secret_key() {
        let (public, private) = lb_core::proofs::leader_proof::test_utils::note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::from(5u64),
        );
        let mut wallet = private.input().wallet.clone();
        wallet.secret_key = Fr::ZERO;
        let (sender, receiver) = oneshot::channel();
        ZkKey::new(Fr::from(7u64))
            .execute(BuildPrivateInputsWithLeaderKey::new(
                sender,
                public,
                wallet,
                Fr::from(5u64),
            ))
            .await
            .unwrap();
        let built = receiver.await.unwrap();
        assert_eq!(built.input().wallet.secret_key, Fr::from(7u64));
        assert_eq!(built.input().chain.msg, Fr::from(5u64));
    }
}
