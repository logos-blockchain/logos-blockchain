use std::sync::Arc;

use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
#[cfg(any(test, feature = "samples"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        Value,
        channel::{ChannelState, Channels, Error},
        gas::{Gas, MainnetGasProfile, OperationGas, SignedOperationExecutionGas},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{SignedOperation, channel::config::Keys},
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, VerificationState, Verified},
        },
    },
};

/// The maximum number of bytes that can be inscribed in a single inscription
/// operation. This is derived from the maximum block transactions size,
/// allowing for some overhead.
pub const MAX_BYTES: usize = MAX_BLOCK_TRANSACTIONS_SIZE * 7 / 8;
pub type Inscription = UpperBoundedVec<u8, MAX_BYTES>;

mod serde_inscription {
    use serde::{Deserializer, Serializer};

    use super::{Inscription, MAX_BYTES};

    pub fn serialize<S>(inscription: &Inscription, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        lb_utils::serde::serde_bytes_slice::serialize(inscription, serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Inscription, D::Error>
    where
        D: Deserializer<'de>,
    {
        lb_utils::serde::serde_bytes_slice::deserialize_bounded::<Inscription, MAX_BYTES, D>(
            deserializer,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct InscriptionOp {
    pub channel_id: ChannelId,
    /// Message to be written in the blockchain
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
    /// Enforce that this inscription comes after this tx
    pub parent: MsgId,
    pub signer: Ed25519PublicKey,
}

impl InscriptionOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode().as_ref());
        MsgId(hasher.finalize().into())
    }

    #[cfg(any(test, feature = "samples"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([14u8; 32]),
            inscription: b"hello logos".into(),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[15; 32]).public_key(),
        }
    }
}

pub struct InscriptionPreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct InscriptionValidationContext<'a> {
    pub channels: &'a Channels,
    pub block_slot: Slot,
}

pub struct InscriptionExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl ProvableOperation for InscriptionOp {
    type Proof = Ed25519Signature;
    const CODE: u8 = 0x11;
}

impl OperationGas<MainnetGasProfile> for InscriptionOp {
    const GAS_COST: Gas = Gas::new(56);
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Unverified, StandardMode>
{
    type Context<'a> = InscriptionPreverificationContext<'a>;
    type Error = Error;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Check the signature
        self.operation()
            .signer
            .verify(context.tx_hash_view.as_bytes(), self.proof())
            .map_err(|_error| Error::InvalidSignature)?;

        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Preverified, StandardMode>
{
    type Context<'a> = InscriptionValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        let operation = self.operation();

        // Check if the channel exist otherwise the inscription is valid only if and
        // only if parent == ZERO
        if let Some(channel) = context
            .channels
            .channels
            .get(&operation.channel_id)
            .cloned()
        {
            // Check the parent corresponds to the payload
            if operation.parent != channel.tip_message {
                return Err(Error::InvalidParent {
                    channel_id: operation.channel_id,
                    parent: operation.parent.into(),
                    actual: channel.tip_message.into(),
                });
            }

            // Check that the signer is the authorized one
            if operation.signer
                != channel.accredited_keys[channel.round_robin(context.block_slot).0 as usize]
            {
                return Err(Error::UnauthorizedSigner {
                    channel_id: operation.channel_id,
                    signer: format!("{signer:?}", signer = operation.signer),
                });
            }
        } else if operation.parent != MsgId::root() {
            // Checked that the parent is ZERO because channel doesn't exist
            return Err(Error::InvalidParent {
                channel_id: operation.channel_id,
                parent: operation.parent.into(),
                actual: MsgId::root().into(),
            });
        }

        Ok(())
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<InscriptionOp, Verified, Mode>
{
    type Context<'a> = InscriptionExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // if the channel doesn't exist, create it
        let channel = context
            .channels
            .channels
            .get(&operation.channel_id)
            .cloned()
            .unwrap_or_else(|| ChannelState {
                accredited_keys: Keys::from(operation.signer).into(),
                configuration_threshold: 1,
                tip_message: MsgId::root(),
                tip_slot: context.block_slot,
                tip_sequencer: 0,
                tip_sequencer_starting_slot: context.block_slot,
                posting_timeframe: 0.into(),
                transfer_threshold: crate::mantle::channel::DEFAULT_TRANSFER_THRESHOLD,
                posting_timeout: 0.into(),
            });

        // Update the channel sequencer, its starting slot, the tip message and the tip
        // slot
        let (new_sequencer, new_starting_slot) = channel.round_robin(context.block_slot);
        context.channels.channels = context.channels.channels.insert(
            operation.channel_id,
            ChannelState {
                tip_message: operation.id(),
                accredited_keys: Arc::clone(&channel.accredited_keys),
                tip_sequencer: new_sequencer,
                tip_sequencer_starting_slot: new_starting_slot,
                tip_slot: context.block_slot,
                ..channel
            },
        );
        Ok((context, Vec::new()))
    }
}

impl<State: VerificationState, Mode: VerificationMode> SignedOperationExecutionGas
    for SignedOperation<InscriptionOp, State, Mode>
{
    fn gas_multiplier(&self) -> Value {
        1
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedError;

    use super::*;
    use crate::{
        crypto::Hash,
        mantle::{
            TxHash, ops::op_proof::samples::SampleProof as _,
            transactions::tx_list::signed_ops::test_utils::make_channel_state,
        },
    };

    fn preverified(
        operation: InscriptionOp,
    ) -> SignedOperation<InscriptionOp, Preverified, StandardMode> {
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            Ed25519Key::from_bytes(&[15; 32]).sign_payload(tx_hash_view.as_bytes().as_ref());

        SignedOperation::<_, Unverified, StandardMode>::new(operation, proof)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view,
            })
            .expect("the sample signer signed this transaction hash")
    }

    fn channels(channel_id: ChannelId, state: ChannelState) -> Channels {
        let mut channels = Channels::new();
        channels.channels.insert_mut(channel_id, state);

        channels
    }

    fn sample() -> InscriptionOp {
        InscriptionOp {
            channel_id: ChannelId([0u8; 32]),
            inscription: b"genesis".into(),
            parent: MsgId([0u8; 32]),
            signer: Ed25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
        }
    }

    #[test]
    fn oversized_inscription_rejected_at_construction() {
        let oversized = vec![0u8; MAX_BYTES + 1];
        let err = Inscription::try_from(oversized).unwrap_err();
        assert!(
            matches!(err, BoundedError::TooManyItems { count, max } if count == MAX_BYTES + 1 && max == MAX_BYTES)
        );
    }

    #[test]
    fn oversized_inscription_rejected_on_deserialize() {
        let oversized = vec![0u8; MAX_BYTES + 1];
        let bytes = bincode::serialize(&oversized).unwrap();
        let err = bincode::deserialize::<Inscription>(&bytes).unwrap_err();
        assert!(
            format!("{err}").contains(
                format!(
                    "Item count {} exceeds static maximum of {MAX_BYTES}",
                    MAX_BYTES + 1,
                )
                .as_str()
            ),
            "{err:?}",
        );
    }

    #[test]
    fn json_round_trip() {
        let op = sample();
        let json = serde_json::to_string(&op).unwrap();
        assert!(
            json.contains("\"67656e65736973\""),
            "inscription should be hex in JSON"
        );
        let recovered: InscriptionOp = serde_json::from_str(&json).unwrap();
        assert_eq!(op, recovered);
    }

    #[test]
    fn bincode_round_trip() {
        let op = sample();
        let bytes = bincode::serialize(&op).unwrap();
        let recovered: InscriptionOp = bincode::deserialize(&bytes).unwrap();
        assert_eq!(op, recovered);
    }

    #[test]
    fn preverify_rejects_a_signature_over_another_transaction() {
        let signing_key = Ed25519Key::from_bytes(&[15; 32]);
        let signed_hash: Hash = Hasher::digest(b"signed").into();
        let other_hash: Hash = Hasher::digest(b"other").into();
        let signed_view = TxHashView::new(TxHash::from(signed_hash));
        let other_view = TxHashView::new(TxHash::from(other_hash));

        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            InscriptionOp::sample(),
            signing_key.sign_payload(signed_view.as_bytes().as_ref()),
        );

        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &signed_view
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &other_view
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn preverify_rejects_a_signature_unrelated_to_the_op() {
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof =
            Ed25519Key::from_bytes(&[16; 32]).sign_payload(tx_hash_view.as_bytes().as_ref());

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(InscriptionOp::sample(), proof);

        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_accepts_an_unregistered_channel_rooted_inscription() {
        let signed_operation = preverified(InscriptionOp::sample());

        assert_eq!(
            signed_operation.verify(&InscriptionValidationContext {
                channels: &Channels::new(),
                block_slot: Slot::from(0),
            }),
            Ok(())
        );
    }

    #[test]
    fn verify_rejects_an_unrooted_inscription_on_an_unregistered_channel() {
        let operation = InscriptionOp {
            parent: MsgId([1u8; 32]),
            ..InscriptionOp::sample()
        };
        let channel_id = operation.channel_id;
        let parent = operation.parent;
        let signed_operation = preverified(operation);

        assert_eq!(
            signed_operation.verify(&InscriptionValidationContext {
                channels: &Channels::new(),
                block_slot: Slot::from(0),
            }),
            Err(Error::InvalidParent {
                channel_id,
                parent: parent.into(),
                actual: MsgId::root().into(),
            })
        );
    }

    #[test]
    fn verify_rejects_an_inscription_that_does_not_follow_the_channel_tip() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let parent = operation.parent;
        let signed_operation = preverified(operation);

        let tip_message = MsgId([2u8; 32]);
        let channels = channels(
            channel_id,
            ChannelState {
                tip_message,
                ..make_channel_state(
                    1,
                    Some(
                        Keys::try_from(vec![Ed25519Key::from_bytes(&[15; 32]).public_key()])
                            .expect("one key is within bounds"),
                    ),
                )
            },
        );

        assert_eq!(
            signed_operation.verify(&InscriptionValidationContext {
                channels: &channels,
                block_slot: Slot::from(0),
            }),
            Err(Error::InvalidParent {
                channel_id,
                parent: parent.into(),
                actual: tip_message.into(),
            })
        );
    }

    #[test]
    fn verify_rejects_an_inscription_from_a_key_the_channel_does_not_accredit() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let signer = operation.signer;
        let signed_operation = preverified(operation);

        let channels = channels(
            channel_id,
            make_channel_state(
                1,
                Some(
                    Keys::try_from(vec![Ed25519Key::from_bytes(&[16; 32]).public_key()])
                        .expect("one key is within bounds"),
                ),
            ),
        );

        assert_eq!(
            signed_operation.verify(&InscriptionValidationContext {
                channels: &channels,
                block_slot: Slot::from(0),
            }),
            Err(Error::UnauthorizedSigner {
                channel_id,
                signer: format!("{signer:?}"),
            })
        );
    }

    #[test]
    fn verify_accepts_an_inscription_from_the_sequencer_on_duty() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let signer = operation.signer;
        let signed_operation = preverified(operation);

        let channels = channels(
            channel_id,
            ChannelState {
                tip_sequencer: 1,
                ..make_channel_state(
                    1,
                    Some(
                        Keys::try_from(vec![
                            Ed25519Key::from_bytes(&[16; 32]).public_key(),
                            signer,
                        ])
                        .expect("two keys are within bounds"),
                    ),
                )
            },
        );

        assert_eq!(
            signed_operation.verify(&InscriptionValidationContext {
                channels: &channels,
                block_slot: Slot::from(0),
            }),
            Ok(())
        );
    }

    #[test]
    fn execute_bootstraps_a_channel_the_ledger_does_not_hold_yet() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let signer = operation.signer;
        let message_id = operation.id();
        let block_slot = Slot::from(7u64);

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                block_slot,
            })
            .expect("inscribing never fails");

        assert_eq!(
            context.channels.channel_state(&channel_id),
            Some(&ChannelState {
                accredited_keys: Arc::new(Keys::from(signer)),
                configuration_threshold: 1,
                tip_message: message_id,
                tip_slot: block_slot,
                tip_sequencer: 0,
                tip_sequencer_starting_slot: block_slot,
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
                transfer_threshold: crate::mantle::channel::DEFAULT_TRANSFER_THRESHOLD,
            })
        );
        assert!(events.is_empty());
    }

    #[test]
    fn execute_rotates_the_sequencer_of_an_existing_channel() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let message_id = operation.id();
        let keys = Keys::try_from(vec![
            Ed25519Key::from_bytes(&[16; 32]).public_key(),
            operation.signer,
        ])
        .expect("two keys are within bounds");

        let channels = channels(
            channel_id,
            ChannelState {
                configuration_threshold: 2,
                posting_timeframe: 1.into(),
                ..make_channel_state(3, Some(keys.clone()))
            },
        );

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                block_slot: Slot::from(1u64),
            })
            .expect("inscribing never fails");

        assert_eq!(
            context.channels.channel_state(&channel_id),
            Some(&ChannelState {
                accredited_keys: Arc::new(keys),
                configuration_threshold: 2,
                tip_message: message_id,
                tip_slot: Slot::from(1u64),
                tip_sequencer: 1,
                tip_sequencer_starting_slot: Slot::from(1u64),
                posting_timeframe: 1.into(),
                posting_timeout: 0.into(),
                transfer_threshold: 3,
            })
        );
        assert!(events.is_empty());
    }

    #[test]
    fn inscription_op_execution_gas() {
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(
            InscriptionOp::sample(),
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        );

        assert_eq!(
            signed_operation.execution_gas::<MainnetGasProfile>(),
            Ok(Gas::new(56))
        );
    }
}
