use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::mantle::{
    Value, ledger,
    ledger::Operation as _,
    ops::channel::{
        ChannelId, ChannelKeyIndex, Ed25519PublicKey as PublicKey, MsgId, inscribe::InscriptionOp,
    },
    tx::MantleTxGasContext,
};

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum Error {
    #[error("Invalid parent {parent:?} for channel {channel_id:?}, expected {actual:?}")]
    InvalidParent {
        channel_id: ChannelId,
        parent: [u8; 32],
        actual: [u8; 32],
    },
    #[error("Unauthorized signer {signer:?} for channel {channel_id:?}")]
    UnauthorizedSigner {
        channel_id: ChannelId,
        signer: String,
    },
    #[error("Invalid signature")]
    InvalidSignature,
    #[error("Invalid keys for channel {channel_id:?}")]
    EmptyKeys { channel_id: ChannelId },
    #[error("Channel {channel_id:?} not found")]
    ChannelNotFound { channel_id: ChannelId },
    #[error("Insufficient funds")]
    InsufficientFunds,
    #[error("Balance overflow")]
    BalanceOverflow,
    #[error("The withdraw nonce doesn't correspond to the channel state")]
    InvalidWithdrawNonce,
    #[error("Withdraw Nonce overflow")]
    WithdrawNonceOverflow,
    #[error("Inputs error: {0}")]
    Inputs(#[from] ledger::InputsError),
    #[error("Outputs error: {0}")]
    Outputs(#[from] ledger::OutputsError),
    #[error(
        "Invalid number of signatures (treshold:?) for channel {channel_id:?}, expected {actual:?}"
    )]
    WithdrawThresholdUnmet {
        channel_id: ChannelId,
        threshold: u16,
        actual: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub channels: rpds::HashTrieMapSync<ChannelId, ChannelState>,
}

impl From<&Channels> for MantleTxGasContext {
    fn from(value: &Channels) -> Self {
        let withdraw_thresholds = value
            .channels
            .iter()
            .map(|(channel_id, channel)| (*channel_id, channel.withdraw_threshold))
            .collect();
        Self::new(withdraw_thresholds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelState {
    pub tip: MsgId,
    // avoid cloning the keys every new message
    #[serde(with = "arc_slice")]
    pub keys: Arc<[PublicKey]>, // keys.len() <= ChannelKeyIndex::MAX
    pub balance: Value,
    // Indicating how many accredited keys are required to withdraw
    // funds from the channel.
    pub withdraw_threshold: ChannelKeyIndex,
    pub withdrawal_nonce: u32,
}

pub(crate) const DEFAULT_WITHDRAW_THRESHOLD: ChannelKeyIndex = 1;

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl Channels {
    pub fn from_genesis(op: &InscriptionOp) -> Result<Self, Error> {
        let channels = op.execute(Self::default())?;
        Ok(channels)
    }

    #[must_use]
    pub fn new() -> Self {
        Self {
            channels: rpds::HashTrieMapSync::new_sync(),
        }
    }

    #[must_use]
    pub fn channel_state(&self, channel_id: &ChannelId) -> Option<&ChannelState> {
        self.channels.get(channel_id)
    }
}

mod arc_slice {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(v: &Arc<[T]>, s: S) -> Result<S::Ok, S::Error> {
        v.as_ref().serialize(s)
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Arc<[T]>, D::Error> {
        Vec::<T>::deserialize(d).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use ark_ff::Field as _;
    use lb_groth16::Fr;
    use lb_key_management_system_keys::keys::{Ed25519Key, ZkKey, ZkPublicKey};
    use lb_utils::blake_rng::RngCore as _;
    use rand::thread_rng;

    use super::*;
    use crate::{
        mantle::{
            Note, Utxo,
            ledger::{Inputs, Outputs, Utxos},
            ops::channel::{
                deposit::{DepositExecutionContext, DepositOp},
                withdraw::{ChannelWithdrawOp, WithdrawExecutionContext},
            },
        },
        sdp::locked_notes::LockedNotes,
    };

    fn test_public_key(seed: u8) -> PublicKey {
        Ed25519Key::from_bytes(&[seed; 32]).public_key()
    }

    fn utxo(value: Value) -> (ZkKey, Utxo) {
        let mut op_id = [0u8; 32];
        thread_rng().fill_bytes(&mut op_id);
        let zk_sk = ZkKey::from(Fr::ZERO);
        let utxo = Utxo {
            op_id,
            output_index: 0,
            note: Note::new(value, zk_sk.to_public_key()),
        };
        (zk_sk, utxo)
    }

    fn utxo_tree(utxos: Vec<Utxo>) -> Utxos {
        let mut utxo_tree = Utxos::new();
        for utxo in utxos {
            (utxo_tree, _) = utxo_tree.insert(utxo.id(), utxo);
        }
        utxo_tree
    }

    impl Channels {
        #[must_use]
        pub fn with_balance(channel_id: ChannelId, balance: Value) -> Self {
            Self {
                channels: rpds::HashTrieMapSync::new_sync().insert(
                    channel_id,
                    ChannelState {
                        tip: MsgId::root(),
                        keys: vec![test_public_key(7)].into(),
                        balance,
                        withdraw_threshold: 1,
                        withdrawal_nonce: 0,
                    },
                ),
            }
        }
    }

    #[test]
    fn channels_to_gas_context_tracks_withdraw_thresholds() {
        let first_id = ChannelId::from([1u8; 32]);
        let second_id = ChannelId::from([2u8; 32]);
        let missing_id = ChannelId::from([0u8; 32]);

        let channels = Channels {
            channels: rpds::HashTrieMapSync::new_sync()
                .insert(
                    first_id,
                    ChannelState {
                        tip: MsgId::root(),
                        keys: vec![test_public_key(11)].into(),
                        balance: 5,
                        withdraw_threshold: 1,
                        withdrawal_nonce: 0,
                    },
                )
                .insert(
                    second_id,
                    ChannelState {
                        tip: MsgId::root(),
                        keys: vec![test_public_key(22), test_public_key(23)].into(),
                        balance: 9,
                        withdraw_threshold: 2,
                        withdrawal_nonce: 0,
                    },
                ),
        };

        let gas_context = MantleTxGasContext::from(&channels);

        assert_eq!(gas_context.withdraw_threshold(&first_id), Some(1));
        assert_eq!(gas_context.withdraw_threshold(&second_id), Some(2));
        assert_eq!(gas_context.withdraw_threshold(&missing_id), None);
    }

    #[test]
    fn deposit_increases_channel_balance() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::with_balance(channel_id, 10);

        let (_, utxo) = utxo(6u64);

        let deposit_op = DepositOp {
            channel_id,
            inputs: Inputs::new(vec![utxo.id()]),
            metadata: vec![],
        };

        let utxo_tree = utxo_tree(vec![utxo]);

        let updated = deposit_op
            .execute(DepositExecutionContext {
                channels,
                locked_notes: LockedNotes::new(),
                utxos: utxo_tree,
            })
            .expect("execution should succeed");

        assert_eq!(
            updated.channels.channel_state(&channel_id).unwrap().balance,
            16
        );
    }

    #[test]
    fn withdraw_decreases_channel_balance() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::with_balance(channel_id, 10);

        let (_, utxo) = utxo(6u64);

        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new(vec![Note {
                value: 6,
                pk: ZkPublicKey::zero(),
            }]),
            withdraw_nonce: 0,
        };

        let utxo_tree = utxo_tree(vec![utxo]);

        let updated = withdraw_op
            .execute(WithdrawExecutionContext {
                channels,
                utxos: utxo_tree,
            })
            .expect("execution should succeed");

        assert_eq!(
            updated.channels.channel_state(&channel_id).unwrap().balance,
            4
        );
    }

    #[test]
    fn withdraw_fails_with_insufficient_funds() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::with_balance(channel_id, 3);

        let (_, utxo) = utxo(6u64);

        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new(vec![Note {
                value: 6,
                pk: ZkPublicKey::zero(),
            }]),
            withdraw_nonce: 0,
        };

        let utxo_tree = utxo_tree(vec![utxo]);

        let result = withdraw_op.execute(WithdrawExecutionContext {
            channels,
            utxos: utxo_tree,
        });

        assert!(matches!(result, Err(Error::InsufficientFunds)));
    }

    #[test]
    fn withdraw_fails_for_missing_channel() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::new();
        let (_, utxo) = utxo(6u64);

        let withdraw_op = ChannelWithdrawOp {
            channel_id,
            outputs: Outputs::new(vec![Note {
                value: 6,
                pk: ZkPublicKey::zero(),
            }]),
            withdraw_nonce: 0,
        };

        let utxo_tree = utxo_tree(vec![utxo]);

        let result = withdraw_op.execute(WithdrawExecutionContext {
            channels,
            utxos: utxo_tree,
        });

        assert!(matches!(result, Err(Error::ChannelNotFound { .. })));
    }
}
