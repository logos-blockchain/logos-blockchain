use std::{num::NonZero, sync::Arc};

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::Slot;
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::{ChannelId, MsgId};
use crate::{
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        Value,
        channel::{ChannelState, Channels, Error, SlotTimeframe, SlotTimeout},
        gas::{
            Gas, GasOverflow, MainnetGasProfile, OpGasCalculator, OperationGas, ThresholdSource,
        },
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        ops::{
            SignedOperation,
            channel::{UnverifiedChannelKeys, VerifiedChannelKeys},
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
    proofs::channel_multi_sig_proof::ChannelMultiSigProof,
};

/// A channel configuration.
///
/// Only a well-formed configuration can be built: both thresholds are non-zero
/// and the configuration threshold does not exceed the number of accredited
/// keys, which also keeps the key set non-empty. [`Self::new`] checks this, and
/// so do the canonical decoding and the serde deserialization.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct ChannelConfigOp {
    channel: ChannelId,
    parent: MsgId,
    // This op is not used in genesis, so we can force channel updates to only use valid (e.g.,
    // non-weak) public keys.
    keys: VerifiedChannelKeys,
    posting_timeframe: SlotTimeframe,
    posting_timeout: SlotTimeout,
    configuration_threshold: NonZero<u16>,
    transfer_threshold: NonZero<u16>,
}

impl ChannelConfigOp {
    /// Builds a channel configuration.
    ///
    /// Returns [`Error::InvalidChannelConfig`] if the configuration threshold
    /// exceeds the number of accredited keys.
    pub fn new(
        channel: ChannelId,
        parent: MsgId,
        keys: VerifiedChannelKeys,
        posting_timeframe: SlotTimeframe,
        posting_timeout: SlotTimeout,
        configuration_threshold: NonZero<u16>,
        transfer_threshold: NonZero<u16>,
    ) -> Result<Self, Error> {
        // The configuration threshold must be reachable with the accredited
        // keys, otherwise the channel would be locked out of any future
        // reconfiguration.
        if usize::from(configuration_threshold.get()) > keys.len() {
            return Err(Error::InvalidChannelConfig);
        }
        Ok(Self {
            channel,
            parent,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        })
    }

    #[must_use]
    pub const fn channel(&self) -> ChannelId {
        self.channel
    }

    #[must_use]
    pub const fn parent(&self) -> MsgId {
        self.parent
    }

    #[must_use]
    pub const fn keys(&self) -> &VerifiedChannelKeys {
        &self.keys
    }

    #[must_use]
    pub const fn posting_timeframe(&self) -> &SlotTimeframe {
        &self.posting_timeframe
    }

    #[must_use]
    pub const fn posting_timeout(&self) -> &SlotTimeout {
        &self.posting_timeout
    }

    #[must_use]
    pub const fn configuration_threshold(&self) -> NonZero<u16> {
        self.configuration_threshold
    }

    #[must_use]
    pub const fn transfer_threshold(&self) -> NonZero<u16> {
        self.transfer_threshold
    }

    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode());
        MsgId(hasher.finalize().into())
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel: ChannelId::from([7u8; 32]),
            parent: MsgId::root(),
            keys: [
                Ed25519Key::from_bytes(&[8; 32]).public_key(),
                Ed25519Key::from_bytes(&[9; 32]).public_key(),
            ]
            .into(),
            posting_timeframe: SlotTimeframe::from(10u32),
            posting_timeout: SlotTimeout::from(11u32),
            configuration_threshold: NonZero::new(2).unwrap(),
            transfer_threshold: NonZero::new(13).unwrap(),
        }
    }
}

// Deserializes the fields as `#[derive(Deserialize)]` would, then builds the
// configuration with `ChannelConfigOp::new`, so it rejects what `new` rejects.
impl<'de> Deserialize<'de> for ChannelConfigOp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename = "ChannelConfigOp")]
        struct Fields {
            channel: ChannelId,
            parent: MsgId,
            keys: VerifiedChannelKeys,
            posting_timeframe: SlotTimeframe,
            posting_timeout: SlotTimeout,
            configuration_threshold: NonZero<u16>,
            transfer_threshold: NonZero<u16>,
        }

        let Fields {
            channel,
            parent,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        } = Fields::deserialize(deserializer)?;
        Self::new(
            channel,
            parent,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        )
        .map_err(D::Error::custom)
    }
}

// The fields in order, as `#[derive(BinaryCodec)]` would encode them. Decoding
// goes through `ChannelConfigOp::new`, so it rejects what `new` rejects.
impl BinaryEncode for ChannelConfigOp {
    fn encoded_length(&self) -> usize {
        let Self {
            channel,
            parent,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        } = self;
        channel.encoded_length()
            + parent.encoded_length()
            + keys.encoded_length()
            + posting_timeframe.encoded_length()
            + posting_timeout.encoded_length()
            + configuration_threshold.encoded_length()
            + transfer_threshold.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        let Self {
            channel,
            parent,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        } = self;
        channel.encode_into(out);
        parent.encode_into(out);
        keys.encode_into(out);
        posting_timeframe.encode_into(out);
        posting_timeout.encode_into(out);
        configuration_threshold.encode_into(out);
        transfer_threshold.encode_into(out);
    }
}

impl BinaryDecode for ChannelConfigOp {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, channel) = ChannelId::decode(input, &())?;
        let (input, parent) = MsgId::decode(input, &())?;
        let (input, keys) = VerifiedChannelKeys::decode(input, &())?;
        let (input, posting_timeframe) = SlotTimeframe::decode(input, &())?;
        let (input, posting_timeout) = SlotTimeout::decode(input, &())?;
        let (input, configuration_threshold) = NonZero::<u16>::decode(input, &())?;
        let (input, transfer_threshold) = NonZero::<u16>::decode(input, &())?;
        let operation = Self::new(
            channel,
            parent,
            keys,
            posting_timeframe,
            posting_timeout,
            configuration_threshold,
            transfer_threshold,
        )
        .map_err(|_| {
            DecodeError::invalid_value::<Self>("configuration threshold above the key count")
        })?;
        Ok((input, operation))
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
        // Nothing to check: an ill-formed configuration cannot be built or decoded.
        Ok(())
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<ChannelConfigOp, Preverified, StandardMode>
{
    type DeferredProof = ();
    type Context<'a> = ChannelConfigValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
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
            for (index, signature) in signatures {
                if channel
                    .accredited_keys
                    .get(*index as usize)
                    .ok_or_else(|| Error::InvalidSignatureIndex {
                        channel_id: operation.channel,
                        sequencers: channel.accredited_keys.len(),
                        index: *index,
                    })?
                    .verify(context.tx_hash_view.as_bytes(), signature)
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
        let keys = UnverifiedChannelKeys::new_unchecked(
            operation.keys.iter().map(|k| k.into_unverified()).collect(),
        );
        if let Some(channel) = context.channels.channels.get_mut(&operation.channel) {
            channel.accredited_keys = Arc::new(keys);
            channel.configuration_threshold = operation.configuration_threshold.get();
            channel.tip_sequencer = 0;
            channel.tip_sequencer_starting_slot = context.block_slot;
            channel.posting_timeframe = operation.posting_timeframe.clone();
            channel.posting_timeout = operation.posting_timeout.clone();
            channel.transfer_threshold = operation.transfer_threshold.get();
            channel.tip_slot = context.block_slot;
            channel.config_tip_hash = operation.id();
        } else {
            context.channels.channels = context.channels.channels.insert(
                operation.channel,
                ChannelState {
                    accredited_keys: Arc::new(keys),
                    configuration_threshold: operation.configuration_threshold.get(),
                    tip_message: MsgId::root(),
                    config_tip_hash: operation.id(),
                    tip_slot: context.block_slot,
                    tip_sequencer: 0,
                    tip_sequencer_starting_slot: context.block_slot,
                    posting_timeframe: operation.posting_timeframe.clone(),
                    transfer_threshold: operation.transfer_threshold.get(),
                    posting_timeout: operation.posting_timeout.clone(),
                },
            );
        }
        Ok((context, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_binary_codec::bincode::{DeserializeOp as _, SerializeOp as _};

    use super::*;
    use crate::mantle::{
        TxHash, gas::test_utils::FixedThresholds,
        ops::channel::verification::test_utils::create_channel_multi_sig_proof,
        transactions::tx_list::signed_ops::test_utils::make_channel_state,
    };

    fn channels(
        channel_id: ChannelId,
        configuration_threshold: u16,
        keys: UnverifiedChannelKeys,
    ) -> Channels {
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

    fn genesis_config_op(channel: ChannelId) -> ChannelConfigOp {
        ChannelConfigOp {
            channel,
            parent: MsgId::root(),
            keys: Ed25519Key::from_bytes(&[1; 32]).public_key().into(),
            posting_timeframe: 0.into(),
            posting_timeout: 0.into(),
            configuration_threshold: NonZero::<u16>::MIN,
            transfer_threshold: NonZero::<u16>::MIN,
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
        let proof = ChannelMultiSigProof::empty();
        let signed_operation = SignedOperation::<_, Unverified, StandardMode>::new(op, proof)
            .into_preverified(&())
            .unwrap();

        assert!(signed_operation.verify(&context).is_ok());
    }

    fn with_keys_and_configuration_threshold(
        keys: VerifiedChannelKeys,
        configuration_threshold: u16,
    ) -> Result<ChannelConfigOp, Error> {
        let sample = ChannelConfigOp::sample();
        ChannelConfigOp::new(
            sample.channel,
            sample.parent,
            keys,
            sample.posting_timeframe,
            sample.posting_timeout,
            NonZero::new(configuration_threshold).unwrap(),
            sample.transfer_threshold,
        )
    }

    fn with_configuration_threshold(
        configuration_threshold: u16,
    ) -> Result<ChannelConfigOp, Error> {
        with_keys_and_configuration_threshold(
            ChannelConfigOp::sample().keys,
            configuration_threshold,
        )
    }

    #[test]
    fn new_rejects_an_empty_accredited_key_set() {
        assert_eq!(
            with_keys_and_configuration_threshold(VerifiedChannelKeys::new_unchecked(vec![]), 1),
            Err(Error::InvalidChannelConfig)
        );
    }

    #[test]
    fn new_rejects_a_configuration_threshold_above_the_key_count() {
        assert_eq!(
            with_configuration_threshold(3),
            Err(Error::InvalidChannelConfig)
        );
    }

    #[test]
    fn new_accepts_a_configuration_threshold_up_to_the_key_count() {
        for configuration_threshold in [1, 2] {
            let operation = with_configuration_threshold(configuration_threshold).unwrap();
            assert_eq!(
                operation.configuration_threshold().get(),
                configuration_threshold
            );
        }
    }

    /// The sample encoding with its two trailing thresholds replaced.
    fn encoding_with_thresholds(configuration_threshold: u16, transfer_threshold: u16) -> Vec<u8> {
        let mut encoded = ChannelConfigOp::sample().encode_to_vec();
        let thresholds = encoded.len() - 4;
        encoded.truncate(thresholds);
        encoded.extend_from_slice(&configuration_threshold.to_le_bytes());
        encoded.extend_from_slice(&transfer_threshold.to_le_bytes());
        encoded
    }

    #[test]
    fn decode_round_trips_a_valid_configuration() {
        let operation = ChannelConfigOp::sample();
        let encoded = operation.encode();

        assert_eq!(encoded.as_ref(), encoding_with_thresholds(2, 13).as_slice());
        assert_eq!(ChannelConfigOp::decode_all(&encoded, &()), Ok(operation));
    }

    #[test]
    fn decode_rejects_a_zero_threshold() {
        for encoded in [
            encoding_with_thresholds(0, 13),
            encoding_with_thresholds(2, 0),
        ] {
            assert!(matches!(
                ChannelConfigOp::decode_all(&encoded, &()),
                Err(DecodeError::InvalidValue { .. })
            ));
        }
    }

    #[test]
    fn decode_rejects_a_configuration_threshold_above_the_key_count() {
        assert!(matches!(
            ChannelConfigOp::decode_all(&encoding_with_thresholds(3, 13), &()),
            Err(DecodeError::InvalidValue { .. })
        ));
    }

    #[test]
    fn deserialize_rejects_what_new_rejects() {
        let json = serde_json::to_value(ChannelConfigOp::sample()).unwrap();
        assert_eq!(
            serde_json::from_value::<ChannelConfigOp>(json.clone()).unwrap(),
            ChannelConfigOp::sample()
        );

        for (field, value) in [
            ("configuration_threshold", 0),
            ("transfer_threshold", 0),
            ("configuration_threshold", 3),
        ] {
            let mut invalid = json.clone();
            invalid[field] = value.into();
            assert!(serde_json::from_value::<ChannelConfigOp>(invalid).is_err());
        }
    }

    #[test]
    fn bincode_deserialize_rejects_a_configuration_threshold_above_the_key_count() {
        let operation = ChannelConfigOp::sample();
        let valid = operation.to_bytes().unwrap();
        assert_eq!(ChannelConfigOp::from_bytes(&valid).unwrap(), operation);

        let invalid = ChannelConfigOp {
            configuration_threshold: NonZero::new(3).unwrap(),
            ..operation
        }
        .to_bytes()
        .unwrap();
        assert!(ChannelConfigOp::from_bytes(&invalid).is_err());
    }

    #[test]
    fn verify_accepts_an_unregistered_channel_without_checking_signatures() {
        let signed_operation = preverified(
            ChannelConfigOp::sample(),
            ChannelMultiSigProof::sample_with_signatures(0),
        );

        assert!(
            signed_operation
                .verify(&ChannelConfigValidationContext {
                    channels: &Channels::new(),
                    tx_hash_view: &TxHashView::from(TxHash::from([9u8; 32])),
                })
                .is_ok()
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
            signed_operation
                .verify(&ChannelConfigValidationContext {
                    channels: &channels(
                        channel_id,
                        2,
                        UnverifiedChannelKeys::new_unchecked(vec![
                            key.public_key().into_unverified()
                        ])
                    ),
                    tx_hash_view: &TxHashView::from(tx_hash),
                })
                .unwrap_err(),
            Error::ThresholdUnmet {
                channel_id,
                threshold: 2,
                actual: 1,
            }
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
            signed_operation
                .verify(&ChannelConfigValidationContext {
                    channels: &channels(
                        channel_id,
                        2,
                        UnverifiedChannelKeys::new_unchecked(vec![
                            accredited.public_key().into_unverified()
                        ])
                    ),
                    tx_hash_view: &TxHashView::from(tx_hash),
                })
                .unwrap_err(),
            Error::InvalidSignatureIndex {
                channel_id,
                sequencers: 1,
                index: 1,
            }
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
            UnverifiedChannelKeys::new_unchecked(vec![
                accredited_key.public_key().into_unverified(),
            ]),
        );

        assert_eq!(
            signed_operation
                .verify(&ChannelConfigValidationContext {
                    channels: &channels,
                    tx_hash_view: &TxHashView::from(signed_hash),
                })
                .unwrap_err(),
            Error::InvalidSignature
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
        let channels = channels(
            channel_id,
            1,
            UnverifiedChannelKeys::new_unchecked(vec![key.public_key().into_unverified()]),
        );

        assert!(
            signed_operation
                .verify(&ChannelConfigValidationContext {
                    channels: &channels,
                    tx_hash_view: &TxHashView::from(signed_hash),
                })
                .is_ok()
        );
        assert_eq!(
            signed_operation
                .verify(&ChannelConfigValidationContext {
                    channels: &channels,
                    tx_hash_view: &TxHashView::from(other_hash),
                })
                .unwrap_err(),
            Error::InvalidSignature
        );
    }

    fn configured_state(operation: &ChannelConfigOp, block_slot: Slot) -> ChannelState {
        ChannelState {
            accredited_keys: Arc::new(UnverifiedChannelKeys::new_unchecked(
                operation.keys.iter().map(|k| k.into_unverified()).collect(),
            )),
            configuration_threshold: operation.configuration_threshold.get(),
            tip_message: MsgId::root(),
            config_tip_hash: operation.id(),
            tip_slot: block_slot,
            tip_sequencer: 0,
            tip_sequencer_starting_slot: block_slot,
            posting_timeframe: operation.posting_timeframe.clone(),
            posting_timeout: operation.posting_timeout.clone(),
            transfer_threshold: operation.transfer_threshold.get(),
        }
    }

    #[test]
    fn execute_creates_a_channel_the_ledger_does_not_hold_yet() {
        let operation = ChannelConfigOp::sample();
        let channel_id = operation.channel;
        let block_slot = Slot::from(7u64);
        let expected = configured_state(&operation, block_slot);

        let signed_operation: SignedOperation<_, _, StandardMode> =
            SignedOperation::new(operation, ChannelMultiSigProof::sample_with_signatures(1))
                .into_state_trusted();

        let (context, events) = signed_operation
            .execute(ChannelConfigExecutionContext {
                channels: Channels::new(),
                block_slot,
            })
            .expect("configuring a channel never fails");

        assert_eq!(context.channels.channel_state(&channel_id), Some(&expected));
        assert_eq!(events, []);
    }

    #[test]
    fn execute_replaces_every_field_of_an_existing_channel() {
        let operation = ChannelConfigOp::sample();
        let channel_id = operation.channel;
        let block_slot = Slot::from(7u64);
        let expected = configured_state(&operation, block_slot);

        let mut channels = Channels::new();
        channels.channels.insert_mut(
            channel_id,
            ChannelState {
                tip_sequencer: 5,
                tip_sequencer_starting_slot: Slot::from(1u64),
                tip_slot: Slot::from(2u64),
                configuration_threshold: 99,
                ..make_channel_state(
                    98,
                    Some(UnverifiedChannelKeys::new_unchecked(vec![
                        Ed25519Key::from_bytes(&[42; 32])
                            .public_key()
                            .into_unverified(),
                    ])),
                )
            },
        );

        let signed_operation: SignedOperation<_, _, StandardMode> =
            SignedOperation::new(operation, ChannelMultiSigProof::sample_with_signatures(1))
                .into_state_trusted();

        let (context, events) = signed_operation
            .execute(ChannelConfigExecutionContext {
                channels,
                block_slot,
            })
            .expect("configuring a channel never fails");

        assert_eq!(context.channels.channel_state(&channel_id), Some(&expected));
        assert_eq!(events, []);
    }

    #[test]
    fn channel_config_op_execution_gas_scales_with_the_threshold() {
        for (threshold, expected) in [(1, 56), (2, 112), (3, 168)] {
            assert_eq!(
                ChannelConfigOp::sample().execution_gas(&FixedThresholds(threshold)),
                Ok(Gas::new(expected))
            );
        }
    }

    #[test]
    fn channel_config_op_execution_gas_is_zero_against_an_unknown_channel() {
        assert_eq!(
            ChannelConfigOp::sample().execution_gas(&FixedThresholds(0)),
            Ok(Gas::new(0))
        );
    }
}
