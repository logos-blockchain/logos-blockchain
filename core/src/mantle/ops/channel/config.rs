use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
use lb_utils::bounded::NonEmptyBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        Value,
        batch::DeferredZkpVerification,
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        gas::{
            Gas, GasOverflow, MainnetGasProfile, OpGasCalculator, OperationGas, ThresholdSource,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::SignedOperation,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

pub const CHANNEL_MAX_KEYS: usize = u16::MAX as usize;
pub type Keys = NonEmptyBoundedVec<Ed25519PublicKey, CHANNEL_MAX_KEYS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelConfigOp {
    pub channel: ChannelId,
    pub parent: MsgId,
    pub keys: Keys,
    pub posting_timeframe: SlotTimeframe,
    pub posting_timeout: SlotTimeout,
    pub configuration_threshold: u16,
    pub transfer_threshold: u16,
}

impl ChannelConfigOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode());
        MsgId(hasher.finalize().into())
    }
}

pub struct ChannelConfigValidationContext<'a> {
    pub channels: &'a Channels,
    pub tx_hash_view: &'a TxHashView,
}

pub struct ChannelConfigExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl ProvableOperation for ChannelConfigOp {
    type Proof = ChannelMultiSigProof;
    const CODE: u8 = 0x10;
}

impl OperationGas<MainnetGasProfile> for ChannelConfigOp {
    const GAS_COST: Gas = Gas::new(56);
}

impl OpGasCalculator<MainnetGasProfile> for ChannelConfigOp {
    fn execution_gas(&self, thresholds: &impl ThresholdSource) -> Result<Gas, GasOverflow> {
        Self::GAS_COST.checked_mul(Value::from(
            thresholds.configuration_threshold(&self.channel),
        ))
    }
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<ChannelConfigOp, Unverified, StandardMode>
{
    type Context<'a> = ();
    type Error = Error;

    fn preverify(&self, _context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check config is well-formed
        if operation.configuration_threshold == 0
            || operation.transfer_threshold == 0
            || operation.keys.is_empty()
        {
            return Err(Error::InvalidChannelConfig);
        }

        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ChannelConfigOp, Preverified, StandardMode>
{
    type Context<'a> = ChannelConfigValidationContext<'a>;
    type Error = Error;

    fn verify(
        &self,
        context: &Self::Context<'_>,
    ) -> Result<Option<DeferredZkpVerification>, Self::Error> {
        let operation = self.operation();
        let proof = self.proof();

        // Check that the indexes are unique and there is the same number of proof and
        // index. This is enforced by the proof structure that enforces it.

        if let Some(channel) = context.channels.channels.get(&operation.channel).cloned() {
            // Check the configuration extends the last configuration of the channel
            if operation.parent != channel.config_tip_hash {
                return Err(Error::InvalidParent {
                    channel_id: operation.channel,
                    parent: operation.parent.into(),
                    actual: channel.config_tip_hash.into(),
                });
            }

            // Check there is enough signatures
            let signatures = proof.signatures();
            if signatures.len() != channel.configuration_threshold as usize {
                return Err(Error::ThresholdUnmet {
                    channel_id: operation.channel,
                    threshold: channel.configuration_threshold,
                    actual: proof.signatures().len(),
                });
            }

            // Check the signatures. Don't defer this because ed25519 verification is cheap.
            for signature in signatures {
                if channel
                    .accredited_keys
                    .get(signature.channel_key_index as usize)
                    .ok_or_else(|| Error::InvalidSignatureIndex {
                        channel_id: operation.channel,
                        sequencers: channel.accredited_keys.len(),
                        index: signature.channel_key_index,
                    })?
                    .verify(context.tx_hash_view.as_bytes(), &signature.signature)
                    .is_err()
                {
                    return Err(Error::InvalidSignature);
                }
            }
        } else {
            // Checked that the parent is ZERO because channel doesn't exist
            if operation.parent != MsgId::root() {
                return Err(Error::InvalidParent {
                    channel_id: operation.channel,
                    parent: operation.parent.into(),
                    actual: MsgId::root().into(),
                });
            }

            // No key is accredited yet, so the threshold to verify against is 0
            let signatures = proof.signatures();
            if !signatures.is_empty() {
                return Err(Error::ThresholdUnmet {
                    channel_id: operation.channel,
                    threshold: 0,
                    actual: signatures.len(),
                });
            }
        }

        Ok(None)
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<ChannelConfigOp, Verified, Mode>
{
    type Context<'a> = ChannelConfigExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // if the channel doesn't exist, create it otherwise just update the config
        if let Some(channel) = context.channels.channels.get_mut(&operation.channel) {
            channel.accredited_keys = operation.keys.clone().into();
            channel.configuration_threshold = operation.configuration_threshold;
            channel.tip_sequencer = 0;
            channel.tip_sequencer_starting_slot = context.block_slot;
            channel.posting_timeframe = operation.posting_timeframe.clone();
            channel.posting_timeout = operation.posting_timeout.clone();
            channel.transfer_threshold = operation.transfer_threshold;
            channel.tip_slot = context.block_slot;
            channel.config_tip_hash = operation.id();
        } else {
            context.channels.channels = context.channels.channels.insert(
                operation.channel,
                ChannelState {
                    accredited_keys: operation.keys.clone().into(),
                    configuration_threshold: operation.configuration_threshold,
                    tip_message: MsgId::root(),
                    config_tip_hash: operation.id(),
                    tip_slot: context.block_slot,
                    tip_sequencer: 0,
                    tip_sequencer_starting_slot: context.block_slot,
                    posting_timeframe: operation.posting_timeframe.clone(),
                    transfer_threshold: operation.transfer_threshold,
                    posting_timeout: operation.posting_timeout.clone(),
                },
            );
        }
        Ok((context, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_key_management_system_keys::keys::Ed25519Key;

    use super::*;
    use crate::mantle::{
        ops::channel::verification::test_utils::create_channel_multi_sig_proof,
        transactions::hash::TxHash,
    };

    fn genesis_config_op(channel: ChannelId) -> ChannelConfigOp {
        ChannelConfigOp {
            channel,
            parent: MsgId::root(),
            keys: Ed25519Key::from_bytes(&[1; 32]).public_key().into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: 1,
            transfer_threshold: 1,
        }
    }

    // Audit #136: nothing constrained this proof, so the same block id could
    // carry two bodies priced differently.
    #[test]
    fn genesis_config_rejects_a_non_empty_proof() {
        let op = genesis_config_op(ChannelId::from([0u8; 32]));
        let tx_hash = TxHash::from([7u8; 32]);
        let tx_hash_view = TxHashView::new(tx_hash);
        let context = ChannelConfigValidationContext {
            channels: &Channels::new(),
            tx_hash_view: &tx_hash_view,
        };
        let signer = Ed25519Key::from_bytes(&[2; 32]);
        let proof = create_channel_multi_sig_proof(&tx_hash, &[&signer]);
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(op, proof)
            .into_preverified(&())
            .unwrap();

        assert!(matches!(
            signed_operation.verify(&context),
            Err(Error::ThresholdUnmet {
                threshold: 0,
                actual: 1,
                ..
            })
        ));
    }

    #[test]
    fn genesis_config_accepts_an_empty_proof() {
        let op = genesis_config_op(ChannelId::from([0u8; 32]));
        let tx_hash_view = TxHashView::new(TxHash::from([7u8; 32]));
        let context = ChannelConfigValidationContext {
            channels: &Channels::new(),
            tx_hash_view: &tx_hash_view,
        };
        let proof = ChannelMultiSigProof::try_new([].into()).unwrap();
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(op, proof)
            .into_preverified(&())
            .unwrap();

        assert!(signed_operation.verify(&context).unwrap().is_none());
    }
}
