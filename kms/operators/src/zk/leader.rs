use std::fmt::{Debug, Formatter};

use lb_core::{
    mantle::Utxo,
    proofs::leader_proof::{LeaderPrivate, LeaderPublic, check_winning},
};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::{
    Ed25519PublicKey, ZkKey, errors::KeyError, secured_key::SecureKeyOperator,
};
use lb_utxotree::MerklePath;

pub struct CheckLotteryWinning {
    result_channel: tokio::sync::oneshot::Sender<bool>,
    utxo: Utxo,
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
        utxo: Utxo,
        public_inputs: LeaderPublic,
    ) -> Self {
        Self {
            result_channel,
            utxo,
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
            utxo,
            public_inputs,
        } = *self;
        if result_channel
            .send(check_winning(
                utxo,
                public_inputs,
                &key.to_public_key(),
                *key.as_fr(),
            ))
            .is_err()
        {
            tracing::error!("Failed to send result via channel");
        }
        Ok(())
    }
}

pub struct BuildPrivateInputsWithLeaderKey {
    result_channel: tokio::sync::oneshot::Sender<LeaderPrivate>,
    utxo: Utxo,
    leader_public: LeaderPublic,
    aged_path: MerklePath<Fr>,
    latest_path: MerklePath<Fr>,
    leader_pk: Ed25519PublicKey,
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
        utxo: Utxo,
        leader_public: LeaderPublic,
        aged_path: MerklePath<Fr>,
        latest_path: MerklePath<Fr>,
        leader_pk: Ed25519PublicKey,
    ) -> Self {
        Self {
            result_channel,
            utxo,
            leader_public,
            aged_path,
            latest_path,
            leader_pk,
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
            utxo,
            leader_public,
            aged_path,
            latest_path,
            leader_pk,
        } = *self;
        if result_channel
            .send(LeaderPrivate::new(
                leader_public,
                utxo,
                &aged_path,
                &latest_path,
                *key.as_fr(),
                &leader_pk,
            ))
            .is_err()
        {
            tracing::error!("Failed to send result via channel");
        }
        Ok(())
    }
}
