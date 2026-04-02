use std::sync::Arc;

use lb_core::mantle::{
    TxHash, Value,
    ops::channel::{
        ChannelId, ChannelKeyIndex, Ed25519PublicKey as PublicKey, MsgId, deposit::DepositOp,
        inscribe::InscriptionOp, set_keys::SetKeysOp, withdraw::ChannelWithdrawOp,
    },
    tx::MantleTxGasContext,
};
use lb_key_management_system_keys::keys::Ed25519Signature;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

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
}

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelState {
    pub tip: MsgId,
    // avoid cloning the keys every new message
    pub keys: Arc<[PublicKey]>, // keys.len() <= ChannelKeyIndex::MAX
    pub balance: Value,
    // Indicating how many accredited keys are required to withdraw
    // funds from the channel.
    pub withdraw_threshold: ChannelKeyIndex,
}

const DEFAULT_WITHDRAW_THRESHOLD: ChannelKeyIndex = 1;

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl Channels {
    pub fn from_genesis(op: &InscriptionOp) -> Result<Self, Error> {
        Self::default().apply_msg(op.channel_id, &op.parent, op.id(), &op.signer)
    }

    pub fn apply_msg(
        mut self,
        channel_id: ChannelId,
        parent: &MsgId,
        msg: MsgId,
        signer: &PublicKey,
    ) -> Result<Self, Error> {
        let channel = self
            .channels
            .get(&channel_id)
            .cloned()
            .unwrap_or_else(|| ChannelState {
                tip: MsgId::root(),
                keys: vec![*signer].into(),
                balance: 0,
                withdraw_threshold: DEFAULT_WITHDRAW_THRESHOLD,
            });

        if *parent != channel.tip {
            return Err(Error::InvalidParent {
                channel_id,
                parent: (*parent).into(),
                actual: channel.tip.into(),
            });
        }

        if !channel.keys.contains(signer) {
            return Err(Error::UnauthorizedSigner {
                channel_id,
                signer: format!("{signer:?}"),
            });
        }

        self.channels = self.channels.insert(
            channel_id,
            ChannelState {
                tip: msg,
                keys: Arc::clone(&channel.keys),
                balance: channel.balance,
                withdraw_threshold: channel.withdraw_threshold,
            },
        );
        Ok(self)
    }

    // TODO: Replace with CHANNEL_CONFIG op: https://github.com/logos-blockchain/logos-blockchain/issues/2461
    pub fn set_keys(
        mut self,
        channel_id: ChannelId,
        op: &SetKeysOp,
        sig: &Ed25519Signature,
        tx_hash: &TxHash,
    ) -> Result<Self, Error> {
        if op.keys.is_empty() {
            return Err(Error::EmptyKeys { channel_id });
        }

        if let Some(channel) = self.channels.get_mut(&channel_id) {
            if channel.keys[0]
                .verify(tx_hash.as_signing_bytes().as_ref(), sig)
                .is_err()
            {
                return Err(Error::InvalidSignature);
            }
            channel.keys = op.keys.clone().into();
        } else {
            self.channels = self.channels.insert(
                channel_id,
                ChannelState {
                    tip: MsgId::root(),
                    keys: op.keys.clone().into(),
                    balance: 0,
                    // TODO: Replace with `ChannelConfig.withdraw_threshold`
                    // once this op is replaced with CHANNEL_CONFIG op: https://github.com/logos-blockchain/logos-blockchain/issues/2461
                    withdraw_threshold: DEFAULT_WITHDRAW_THRESHOLD,
                },
            );
        }

        Ok(self)
    }

    pub fn deposit(mut self, op: &DepositOp) -> Result<Self, Error> {
        if let Some(channel) = self.channels.get_mut(&op.channel_id) {
            channel.balance = channel
                .balance
                .checked_add(op.amount)
                .ok_or(Error::BalanceOverflow)?;
            Ok(self)
        } else {
            Err(Error::ChannelNotFound {
                channel_id: op.channel_id,
            })
        }
    }

    pub fn withdraw(mut self, op: &ChannelWithdrawOp) -> Result<Self, Error> {
        if let Some(channel) = self.channels.get_mut(&op.channel_id) {
            channel.balance = channel
                .balance
                .checked_sub(op.amount)
                .ok_or(Error::InsufficientFunds)?;
            Ok(self)
        } else {
            Err(Error::ChannelNotFound {
                channel_id: op.channel_id,
            })
        }
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

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;

    fn test_public_key(seed: u8) -> PublicKey {
        Ed25519Key::from_bytes(&[seed; 32]).public_key()
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
                    },
                )
                .insert(
                    second_id,
                    ChannelState {
                        tip: MsgId::root(),
                        keys: vec![test_public_key(22), test_public_key(23)].into(),
                        balance: 9,
                        withdraw_threshold: 2,
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

        let updated = channels
            .deposit(&DepositOp {
                channel_id,
                amount: 6,
                metadata: vec![],
            })
            .expect("deposit should succeed");

        assert_eq!(updated.channel_state(&channel_id).unwrap().balance, 16);
    }

    #[test]
    fn withdraw_decreases_channel_balance() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::with_balance(channel_id, 10);

        let updated = channels
            .withdraw(&ChannelWithdrawOp {
                channel_id,
                amount: 6,
            })
            .expect("withdraw should succeed");

        assert_eq!(updated.channel_state(&channel_id).unwrap().balance, 4);
    }

    #[test]
    fn withdraw_fails_with_insufficient_funds() {
        let channel_id = ChannelId::from([0u8; 32]);
        let channels = Channels::with_balance(channel_id, 3);

        let result = channels.withdraw(&ChannelWithdrawOp {
            channel_id,
            amount: 6,
        });

        assert!(matches!(result, Err(Error::InsufficientFunds)));
    }

    #[test]
    fn withdraw_fails_for_missing_channel() {
        let result = Channels::new().withdraw(&ChannelWithdrawOp {
            channel_id: ChannelId::from([0u8; 32]),
            amount: 1,
        });

        assert!(matches!(result, Err(Error::ChannelNotFound { .. })));
    }
}
