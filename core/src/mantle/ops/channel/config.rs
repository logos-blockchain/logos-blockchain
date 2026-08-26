use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
#[cfg(any(test, feature = "samples"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_utils::bounded::NonEmptyBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        Value,
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::SignedOperation,
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

pub const CHANNEL_MAX_KEYS: usize = u16::MAX as usize;
pub type Keys = NonEmptyBoundedVec<Ed25519PublicKey, CHANNEL_MAX_KEYS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ChannelConfigOp {
    pub channel: ChannelId,
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

    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel: ChannelId::from([7u8; 32]),
            keys: Keys::try_from(vec![
                Ed25519Key::from_bytes(&[8; 32]).public_key(),
                Ed25519Key::from_bytes(&[9; 32]).public_key(),
            ])
            .expect("Two keys are within bounds."),
            posting_timeframe: SlotTimeframe::from(10u32),
            posting_timeout: SlotTimeout::from(11u32),
            configuration_threshold: 12,
            transfer_threshold: 13,
        }
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
    // `SignedOperationExecutionGas::gas_multiplier` below reads this proof's
    // signature count. If this changes, update that too.
    type Proof = ChannelMultiSigProof;
    const CODE: u8 = 0x10;
}

impl OperationGas<MainnetGasProfile> for ChannelConfigOp {
    const GAS_COST: Gas = Gas::new(56);
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

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();
        let proof = self.proof();

        // Check that the indexes are unique and there is the same number of proof and
        // index. This is enforced by the proof structure that enforces it.

        if let Some(channel) = context.channels.channels.get(&operation.channel).cloned() {
            // Check there is enough signatures
            let signatures = proof.signatures();
            if signatures.len() != channel.configuration_threshold as usize {
                return Err(Error::ThresholdUnmet {
                    channel_id: operation.channel,
                    threshold: channel.configuration_threshold,
                    actual: proof.signatures().len(),
                });
            }

            // Check the signatures
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
        }

        Ok(())
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
            channel.tip_message = operation.id();
        } else {
            context.channels.channels = context.channels.channels.insert(
                operation.channel,
                ChannelState {
                    accredited_keys: operation.keys.clone().into(),
                    configuration_threshold: operation.configuration_threshold,
                    tip_message: operation.id(),
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

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<ChannelConfigOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        let signature_count = self.proof().signatures().len();
        Value::try_from(signature_count)
            .expect("Channel multi-signature proofs are bound to u16::MAX signatures.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mantle::{
        TxHash, ops::channel::verification::test_utils::create_channel_multi_sig_proof,
        transactions::tx_list::signed_ops::test_utils::make_channel_state,
    };

    fn channels(channel_id: ChannelId, configuration_threshold: u16, keys: Keys) -> Channels {
        let mut channels = Channels::new();
        channels.channels.insert_mut(
            channel_id,
            ChannelState {
                configuration_threshold,
                ..make_channel_state(1, Some(keys))
            },
        );
        channels
    }

    fn preverified(
        operation: ChannelConfigOp,
        proof: ChannelMultiSigProof,
    ) -> SignedOperation<ChannelConfigOp, Preverified, StandardMode> {
        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&())
            .expect("preverify accepts a well-formed configuration")
    }

    #[test]
    fn preverify_rejects_a_zero_configuration_threshold() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelConfigOp {
                configuration_threshold: 0,
                ..ChannelConfigOp::sample()
            },
            ChannelMultiSigProof::sample_with_signatures(1),
        );

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::InvalidChannelConfig)
        );
    }

    #[test]
    fn preverify_rejects_a_zero_transfer_threshold() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelConfigOp {
                transfer_threshold: 0,
                ..ChannelConfigOp::sample()
            },
            ChannelMultiSigProof::sample_with_signatures(1),
        );

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::InvalidChannelConfig)
        );
    }

    #[test]
    fn preverify_rejects_an_empty_accredited_key_set() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelConfigOp {
                keys: Keys::new_unchecked(vec![]),
                ..ChannelConfigOp::sample()
            },
            ChannelMultiSigProof::sample_with_signatures(1),
        );

        assert_eq!(
            signed_operation.preverify(&()),
            Err(Error::InvalidChannelConfig)
        );
    }

    #[test]
    fn verify_accepts_an_unregistered_channel_without_checking_signatures() {
        let signed_operation = preverified(
            ChannelConfigOp::sample(),
            ChannelMultiSigProof::sample_with_signatures(0),
        );

        assert_eq!(
            signed_operation.verify(&ChannelConfigValidationContext {
                channels: &Channels::new(),
                tx_hash_view: &TxHashView::from(TxHash::from([9u8; 32])),
            }),
            Ok(())
        );
    }

    #[test]
    fn verify_rejects_a_signature_count_below_the_threshold() {
        let operation = ChannelConfigOp::sample();
        let channel_id = operation.channel;
        let key = Ed25519Key::from_bytes(&[0; 32]);
        let tx_hash = TxHash::from([9u8; 32]);

        let signed_operation =
            preverified(operation, create_channel_multi_sig_proof(&tx_hash, &[&key]));

        assert_eq!(
            signed_operation.verify(&ChannelConfigValidationContext {
                channels: &channels(channel_id, 2, Keys::new_unchecked(vec![key.public_key()])),
                tx_hash_view: &TxHashView::from(tx_hash),
            }),
            Err(Error::ThresholdUnmet {
                channel_id,
                threshold: 2,
                actual: 1,
            })
        );
    }

    #[test]
    fn verify_rejects_a_signature_index_outside_the_accredited_keys() {
        let operation = ChannelConfigOp::sample();
        let channel_id = operation.channel;
        let accredited = Ed25519Key::from_bytes(&[0; 32]);
        let outsider = Ed25519Key::from_bytes(&[1; 32]);
        let tx_hash = TxHash::from([9u8; 32]);

        let signed_operation = preverified(
            operation,
            create_channel_multi_sig_proof(&tx_hash, &[&accredited, &outsider]),
        );

        assert_eq!(
            signed_operation.verify(&ChannelConfigValidationContext {
                channels: &channels(
                    channel_id,
                    2,
                    Keys::new_unchecked(vec![accredited.public_key()])
                ),
                tx_hash_view: &TxHashView::from(tx_hash),
            }),
            Err(Error::InvalidSignatureIndex {
                channel_id,
                sequencers: 1,
                index: 1,
            })
        );
    }

    #[test]
    fn verify_rejects_a_signature_from_a_key_the_channel_does_not_accredit() {
        let operation = ChannelConfigOp::sample();
        let channel_id = operation.channel;
        let signing_key = Ed25519Key::from_bytes(&[0; 32]);
        let accredited_key = Ed25519Key::from_bytes(&[1; 32]);
        let signed_hash = TxHash::from([9u8; 32]);

        let signed_operation = preverified(
            operation,
            create_channel_multi_sig_proof(&signed_hash, &[&signing_key]),
        );
        let channels = channels(
            channel_id,
            1,
            Keys::new_unchecked(vec![accredited_key.public_key()]),
        );

        assert_eq!(
            signed_operation.verify(&ChannelConfigValidationContext {
                channels: &channels,
                tx_hash_view: &TxHashView::from(signed_hash),
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_rejects_signatures_over_another_transaction() {
        let operation = ChannelConfigOp::sample();
        let channel_id = operation.channel;
        let key = Ed25519Key::from_bytes(&[0; 32]);
        let signed_hash = TxHash::from([9u8; 32]);
        let other_hash = TxHash::from([10u8; 32]);

        let signed_operation = preverified(
            operation,
            create_channel_multi_sig_proof(&signed_hash, &[&key]),
        );
        let channels = channels(channel_id, 1, Keys::new_unchecked(vec![key.public_key()]));

        assert_eq!(
            signed_operation.verify(&ChannelConfigValidationContext {
                channels: &channels,
                tx_hash_view: &TxHashView::from(signed_hash),
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.verify(&ChannelConfigValidationContext {
                channels: &channels,
                tx_hash_view: &TxHashView::from(other_hash),
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn channel_config_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            ChannelConfigOp::sample(),
            ChannelMultiSigProof::sample_with_signatures(3),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(168))
        );
    }
}
