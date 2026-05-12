use std::sync::Arc;

use lb_cryptarchia_engine::Slot;
use serde::{Deserialize, Serialize};

use crate::mantle::{
    Value, ledger,
    ledger::Operation as _,
    ops::channel::{
        ChannelId, ChannelKeyIndex, Ed25519PublicKey as PublicKey, MsgId,
        inscribe::{InscriptionExecutionContext, InscriptionOp},
    },
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct SlotTimeframe(u32);
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct SlotTimeout(u32);

impl From<u32> for SlotTimeframe {
    fn from(slot: u32) -> Self {
        Self(slot)
    }
}
impl From<u32> for SlotTimeout {
    fn from(slot: u32) -> Self {
        Self(slot)
    }
}

impl From<SlotTimeframe> for u32 {
    fn from(slot: SlotTimeframe) -> Self {
        slot.0
    }
}
impl From<SlotTimeout> for u32 {
    fn from(slot: SlotTimeout) -> Self {
        slot.0
    }
}

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
    #[error("Channel {channel_id:?} not found")]
    ChannelNotFound { channel_id: ChannelId },
    #[error("Insufficient funds")]
    InsufficientFunds,
    #[error("Balance overflow")]
    BalanceOverflow,
    #[error("The withdraw nonce doesn't correspond to the channel state")]
    InvalidWithdrawNonce,
    #[error("The Channel Config isn't well formed")]
    InvalidChannelConfig,
    #[error("Withdraw Nonce overflow")]
    WithdrawNonceOverflow,
    #[error("Inputs error: {0}")]
    Inputs(#[from] ledger::InputsError),
    #[error("Outputs error: {0}")]
    Outputs(#[from] ledger::OutputsError),
    #[error(
        "Invalid number of signatures (treshold:?) for channel {channel_id:?}, expected {actual:?}"
    )]
    ThresholdUnmet {
        channel_id: ChannelId,
        threshold: u16,
        actual: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channels {
    pub channels: rpds::HashTrieMapSync<ChannelId, ChannelState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelState {
    // Channel Configuration
    pub accredited_keys: Arc<[PublicKey]>, // keys.len() <= ChannelKeyIndex::MAX
    pub configuration_threshold: u16,      /* indicating how many keys are required to update
                                            * the
                                            * configuration */

    // Message Ordering
    pub tip_message: MsgId,

    // Decentralized Sequencing
    pub tip_slot: Slot,
    pub tip_sequencer: u16, /* indicating the actual sequencer position in the list of
                             * accredited keys */
    pub tip_sequencer_starting_slot: Slot,
    pub posting_timeframe: SlotTimeframe, // number of slots (0 = infinity)
    pub posting_timeout: SlotTimeout,     // number of slots (0 = no timeout)

    // Bridging
    pub balance: Value,
    pub withdrawal_nonce: u32,
    pub withdraw_threshold: ChannelKeyIndex, /* indicating how many keys are required to
                                              * withdraw
                                              * funds from the channel */
}

pub(crate) const DEFAULT_WITHDRAW_THRESHOLD: ChannelKeyIndex = 1;

impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}

impl Channels {
    pub fn from_genesis(op: &InscriptionOp) -> Result<Self, Error> {
        let ctx = op.execute(InscriptionExecutionContext {
            channels: Self::default(),
            block_slot: Slot::default(),
        })?;
        Ok(ctx.channels)
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

impl ChannelState {
    // Returns the new sequencer index and its starting slot
    #[must_use]
    pub fn round_robin(&self, block_slot: Slot) -> (u16, Slot) {
        let elapsed_slot_since_last_tip = (block_slot - self.tip_slot).into_inner();
        let tip_sequencer_duration = (block_slot - self.tip_sequencer_starting_slot).into_inner();
        let posting_timeframe = u64::from(self.posting_timeframe.0);
        let posting_timeout = u64::from(self.posting_timeout.0);
        let num_sequencers = self.accredited_keys.len() as u64; // bounded by ChannelKeyIndex::MAX
        let tip_sequencer = u64::from(self.tip_sequencer);

        let is_timed_out = elapsed_slot_since_last_tip >= posting_timeframe;
        let sequencers_timed_out = elapsed_slot_since_last_tip.checked_div(posting_timeout); // None if posting_timeout == 0
        let timeframe_elapsed = tip_sequencer_duration.checked_div(posting_timeframe); // None if timeframe == 0

        // Timeout-based rotation takes priority when timed out and both divisors are
        // valid. Falls back to timeframe-based rotation, then to the current
        // sequencer.
        let index = sequencers_timed_out
            .filter(|_| is_timed_out && posting_timeframe != 0)
            .or(timeframe_elapsed)
            .map_or(self.tip_sequencer, |slot| {
                ((tip_sequencer + slot) % num_sequencers) as u16
            });

        // Starting slot mirrors the same priority, but timeout-based only needs
        // timed_out
        let starting_slot = sequencers_timed_out
            .filter(|_| is_timed_out)
            .map(|sequencers_timed_out| self.tip_slot + sequencers_timed_out * posting_timeout)
            .or_else(|| {
                timeframe_elapsed.map(|timeframe_elapsed| {
                    self.tip_sequencer_starting_slot + timeframe_elapsed * posting_timeframe
                })
            })
            .unwrap_or(self.tip_sequencer_starting_slot);

        (index, starting_slot)
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
            tx::{GasPrices, MantleTxGasContext},
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
                        accredited_keys: vec![test_public_key(7)].into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0u32.into(),
                        balance,
                        withdraw_threshold: 1,
                        withdrawal_nonce: 0,
                        posting_timeout: 0u32.into(),
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
                        accredited_keys: vec![test_public_key(11)].into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0u32.into(),
                        balance: 5,
                        withdraw_threshold: 1,
                        withdrawal_nonce: 0,
                        posting_timeout: 0u32.into(),
                    },
                )
                .insert(
                    second_id,
                    ChannelState {
                        accredited_keys: vec![test_public_key(22), test_public_key(23)].into(),
                        configuration_threshold: 1,
                        tip_message: MsgId::root(),
                        tip_slot: Slot::default(),
                        tip_sequencer: 0,
                        tip_sequencer_starting_slot: Slot::default(),
                        posting_timeframe: 0.into(),
                        balance: 9,
                        withdraw_threshold: 2,
                        withdrawal_nonce: 0,
                        posting_timeout: 0.into(),
                    },
                ),
        };

        let gas_context = MantleTxGasContext::from_channels(&channels, GasPrices::new(0, 0));

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
