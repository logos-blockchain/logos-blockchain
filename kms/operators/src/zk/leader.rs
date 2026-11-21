use std::fmt::{Debug, Formatter};

use lb_core::{
    mantle::{Note, Value},
    proofs::leader_proof::{LeaderPrivate, LeaderPublic, check_winning},
};
use lb_groth16::{AdditiveGroup as _, Fr};
use lb_key_management_system_keys::keys::{
    ZkKey, ZkPublicKey,
    errors::KeyError,
    secured_key::{SecureMultiKeyOperator, SecuredKey as _},
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
impl SecureMultiKeyOperator for CheckLotteryWinning {
    type Key = ZkKey;
    type Error = KeyError;

    fn validate_key_count(&self, count: usize) -> Result<(), Self::Error> {
        if count == 2 {
            Ok(())
        } else {
            Err(KeyError::UnsupportedMultisignatureSize(2, count))
        }
    }

    async fn execute(self: Box<Self>, keys: &[&Self::Key]) -> Result<(), Self::Error> {
        let Self {
            result_channel,
            note,
            public_inputs,
        } = *self;
        let [nullifier, staking] = keys else {
            return Err(KeyError::UnsupportedMultisignatureSize(2, keys.len()));
        };
        let nullifier_key = nullifier.nullifier_key();
        let matches = note.public_key == nullifier.as_public_key()
            && note.stake_public_key == staking.as_public_key()
            && note.stake_public_key != ZkPublicKey::zero();
        if result_channel
            .send(
                matches
                    && check_winning(&note, public_inputs, nullifier_key, staking.nullifier_key()),
            )
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
    note: Note,
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
        note: Note,
    ) -> Self {
        Self {
            result_channel,
            leader_public,
            wallet,
            reward_value,
            note,
        }
    }
}

#[async_trait::async_trait]
impl SecureMultiKeyOperator for BuildPrivateInputsWithLeaderKey {
    type Key = ZkKey;
    type Error = KeyError;

    fn validate_key_count(&self, count: usize) -> Result<(), Self::Error> {
        if count == 2 {
            Ok(())
        } else {
            Err(KeyError::UnsupportedMultisignatureSize(2, count))
        }
    }

    async fn execute(self: Box<Self>, keys: &[&Self::Key]) -> Result<(), Self::Error> {
        let Self {
            result_channel,
            leader_public,
            wallet,
            reward_value,
            note,
        } = *self;
        let [nullifier, staking] = keys else {
            return Err(KeyError::UnsupportedMultisignatureSize(2, keys.len()));
        };
        if note.stake_public_key == ZkPublicKey::zero()
            || note.public_key != nullifier.as_public_key()
            || note.stake_public_key != staking.as_public_key()
            || note.value != wallet.note_value
            || note.nonce != wallet.note_nonce
            || note.program != wallet.note_program
            || note.data != wallet.note_data
        {
            return Err(KeyError::UnsupportedKey(
                "leadership credentials do not match note".into(),
            ));
        }
        let nullifier_key = nullifier.nullifier_key();
        let wallet = PolWalletInputsData {
            nullifier_key,
            staking_secret_key: staking.nullifier_key(),
            ..wallet
        };
        // Not bound to a block yet: the proposer binds it with `with_msg` to
        // the block ID, which covers the entropy contribution of this
        // witness.
        let private = LeaderPrivate::new(leader_public, wallet, Fr::ZERO, reward_value);
        if result_channel.send(private).is_err() {
            debug!(target: LOG_TARGET, "Failed to send result via channel");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use lb_core::proofs::leader_proof::test_utils::note_set_leader_inputs;
    use lb_key_management_system_keys::keys::kdf_public_key;
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn same_spending_key_can_supply_both_leadership_credentials() {
        let (public, private) = note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::from(5u64),
            &Note::new(3, Fr::from(13u64), ZkPublicKey::new(Fr::from(17u64))),
        );
        let wallet = private.input().wallet.clone();
        for key in [
            ZkKey::new(Fr::from(7u64)),
            ZkKey::from_direct_secret(Fr::from(7u64)),
        ] {
            let note = Note {
                value: wallet.note_value,
                nonce: wallet.note_nonce,
                program: wallet.note_program,
                data: wallet.note_data,
                public_key: key.as_public_key(),
                stake_public_key: key.as_public_key(),
            };
            let (tx, rx) = oneshot::channel();
            ZkKey::execute_multiple(
                &[&key, &key],
                BuildPrivateInputsWithLeaderKey::new(tx, public, wallet.clone(), 3, note),
            )
            .await
            .unwrap();
            let built = rx.await.unwrap();
            assert_eq!(built.input().wallet.nullifier_key, key.nullifier_key());
            assert_eq!(built.input().wallet.staking_secret_key, key.nullifier_key());
        }
    }

    #[tokio::test]
    async fn build_private_inputs_uses_distinct_leadership_secrets() {
        let (public, private) = note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::from(5u64),
            &Note::new(3, Fr::from(13u64), ZkPublicKey::new(Fr::from(17u64))),
        );
        let mut wallet = private.input().wallet.clone();
        wallet.nullifier_key = Fr::ZERO;
        wallet.staking_secret_key = Fr::ZERO;
        let nullifier_key = ZkKey::new(Fr::from(7u64)).nullifier_key();
        let staking_secret_key = Fr::from(19u64);
        let note = Note {
            value: wallet.note_value,
            nonce: wallet.note_nonce,
            program: wallet.note_program,
            data: wallet.note_data,
            public_key: kdf_public_key(&nullifier_key),
            stake_public_key: kdf_public_key(&staking_secret_key),
        };
        for nf in [
            ZkKey::new(Fr::from(7u64)),
            ZkKey::from_direct_secret(nullifier_key),
        ] {
            let (sender, receiver) = oneshot::channel();
            ZkKey::execute_multiple(
                &[&nf, &ZkKey::from_direct_secret(staking_secret_key)],
                BuildPrivateInputsWithLeaderKey::new(sender, public, wallet.clone(), 3, note),
            )
            .await
            .unwrap();
            let built = receiver.await.unwrap();
            assert_eq!(
                (
                    built.input().wallet.nullifier_key,
                    built.input().wallet.staking_secret_key
                ),
                (nullifier_key, staking_secret_key)
            );
            assert_eq!(built.input().chain.reward_value, 3);
        }
    }

    #[tokio::test]
    async fn mismatched_credentials_cannot_win_or_build_a_witness() {
        let (public, private) = note_set_leader_inputs(
            Fr::from(7u64),
            1000,
            Fr::from(11u64),
            Fr::from(5u64),
            &Note::new(3, Fr::from(13u64), ZkPublicKey::new(Fr::from(17u64))),
        );
        let wallet = private.input().wallet.clone();
        let nf_sk = Fr::from(29u64);
        let staking_sk = Fr::from(31u64);
        let note = Note {
            value: wallet.note_value,
            nonce: wallet.note_nonce,
            program: wallet.note_program,
            data: wallet.note_data,
            public_key: kdf_public_key(&nf_sk),
            stake_public_key: kdf_public_key(&staking_sk),
        };
        for (nf, stake) in [
            (Fr::from(37u64), staking_sk),
            (nf_sk, Fr::from(41u64)),
            (staking_sk, nf_sk),
        ] {
            let nf_key = ZkKey::from_direct_secret(nf);
            let stake_key = ZkKey::from_direct_secret(stake);
            let (tx, rx) = oneshot::channel();
            ZkKey::execute_multiple(
                &[&nf_key, &stake_key],
                CheckLotteryWinning::new(tx, note, public),
            )
            .await
            .unwrap();
            assert!(!rx.await.unwrap());
            let (tx, _rx) = oneshot::channel();
            assert!(
                ZkKey::execute_multiple(
                    &[&nf_key, &stake_key],
                    BuildPrivateInputsWithLeaderKey::new(tx, public, wallet.clone(), 3, note)
                )
                .await
                .is_err()
            );
        }
    }
}
