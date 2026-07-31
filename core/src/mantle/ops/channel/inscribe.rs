use std::sync::Arc;

use lb_codec::{BinaryCodec, BinaryEncode as _};
use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    block::MAX_BLOCK_TRANSACTIONS_SIZE,
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        channel::{ChannelState, Channels, Error},
        ledger::{ExecutableOperation, VerifiableOperation, verification_mode},
        ops::channel::config::Keys,
        transactions::hash::TxHashView,
    },
};

/// The maximum number of bytes that can be inscribed in a single inscription
/// operation. This is derived from the maximum block transactions size,
/// allowing for some overhead.
pub const MAX_BYTES: usize = MAX_BLOCK_TRANSACTIONS_SIZE * 7 / 8;
pub type Inscription = UpperBoundedVec<u8, MAX_BYTES>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct InscriptionOp {
    pub channel_id: ChannelId,
    /// Message to be written in the blockchain
    #[serde(with = "lb_utils::serde::serde_bytes_slice")]
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
}

pub struct InscriptionPreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
    pub proof: &'a Ed25519Signature,
}

pub struct InscriptionValidationContext<'a> {
    pub channels: &'a Channels,
    pub block_slot: Slot,
}

pub struct InscriptionExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl VerifiableOperation<verification_mode::StandardMode> for InscriptionOp {
    type PreverificationContext<'a> = InscriptionPreverificationContext<'a>;
    type VerificationContext<'a> = InscriptionValidationContext<'a>;
    type Error = Error;

    fn preverify(&self, context: &Self::PreverificationContext<'_>) -> Result<(), Self::Error> {
        // Check the signature
        self.signer
            .verify(context.tx_hash_view.as_bytes(), context.proof)
            .map_err(|_error| Error::InvalidSignature)?;

        Ok(())
    }

    fn verify(&self, context: &Self::VerificationContext<'_>) -> Result<(), Self::Error> {
        // Check if the channel exist otherwise the inscription is valid only if and
        // only if parent == ZERO
        if let Some(channel) = context.channels.channel_state(&self.channel_id) {
            // Check the parent corresponds to the payload
            if self.parent != channel.tip_message {
                return Err(Error::InvalidParent {
                    channel_id: self.channel_id,
                    parent: self.parent.into(),
                    actual: channel.tip_message.into(),
                });
            }

            // Check that the signer is the authorized one
            if self.signer
                != channel.accredited_keys[channel.round_robin(context.block_slot).0 as usize]
            {
                return Err(Error::UnauthorizedSigner {
                    channel_id: self.channel_id,
                    signer: format!("{signer:?}", signer = self.signer),
                });
            }
        } else if self.parent != MsgId::root() {
            // Checked that the parent is ZERO because channel doesn't exist
            return Err(Error::InvalidParent {
                channel_id: self.channel_id,
                parent: self.parent.into(),
                actual: MsgId::root().into(),
            });
        }

        Ok(())
    }
}

impl ExecutableOperation for InscriptionOp {
    type Context<'a> = InscriptionExecutionContext;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        // if the channel doesn't exist, create it
        let channel = context
            .channels
            .channel_state(&self.channel_id)
            .cloned()
            .unwrap_or_else(|| ChannelState {
                accredited_keys: Keys::from(self.signer).into(),
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
        let updated = ChannelState {
            tip_message: self.id(),
            accredited_keys: Arc::clone(&channel.accredited_keys),
            tip_sequencer: new_sequencer,
            tip_sequencer_starting_slot: new_starting_slot,
            tip_slot: context.block_slot,
            ..channel
        };
        context.channels = context
            .channels
            .set_channel_state(&self.channel_id, updated);
        Ok((context, Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded::BoundedError;

    use super::*;

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
}
