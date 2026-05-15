use std::sync::Arc;

use lb_cryptarchia_engine::Slot;
use lb_key_management_system_keys::keys::Ed25519Signature;
use lb_utils::bounded_vec::BoundedVec;
use nom::IResult;
use serde::{Deserialize, Serialize};

use super::{ChannelId, Ed25519PublicKey, MsgId};
use crate::{
    block::MAX_BLOCK_SIZE,
    crypto::{Digest as _, Hasher},
    events::Events,
    mantle::{
        TxHash,
        channel::{ChannelState, Channels, Error},
        ledger::Operation,
        nom::{NomBoundedVec, NomDecode, NomEncode},
    },
};

pub const MAX_BYTES: usize = MAX_BLOCK_SIZE * 7 / 8;
pub type Inscription = BoundedVec<u8, MAX_BYTES>;
type NomInscription<'a> = NomBoundedVec<'a, u8, MAX_BYTES, 4>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
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
        hasher.update(self.encode().as_slice());
        MsgId(hasher.finalize().into())
    }
}

// ChannelInscribe = ChannelId Inscription Parent Signer
impl NomEncode for InscriptionOp {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.channel_id.encode();
        bytes.extend(NomInscription::from(&self.inscription).encode());
        bytes.extend(self.parent.encode());
        bytes.extend(self.signer.encode());
        bytes
    }
}

impl NomDecode for InscriptionOp {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (input, channel_id) = ChannelId::decode(bytes)?;
        let (input, inscription) = NomInscription::decode(input)?;
        let (input, parent) = MsgId::decode(input)?;
        let (input, signer) = Ed25519PublicKey::decode(input)?;
        Ok((
            input,
            Self {
                channel_id,
                inscription,
                parent,
                signer,
            },
        ))
    }
}

pub struct InscriptionValidationContext<'a> {
    pub channels: &'a Channels,
    pub tx_hash: &'a TxHash,
    pub inscribe_sig: &'a Ed25519Signature,
    pub block_slot: Slot,
}

pub struct InscriptionExecutionContext {
    pub channels: Channels,
    pub block_slot: Slot,
}

impl Operation<InscriptionValidationContext<'_>> for InscriptionOp {
    type ExecutionContext<'a>
        = InscriptionExecutionContext
    where
        Self: 'a;
    type Error = Error;

    fn validate(&self, ctx: &InscriptionValidationContext<'_>) -> Result<(), Self::Error> {
        // Check if the channel exist otherwise the inscription is valid only if and
        // only if parent == ZERO
        if let Some(channel) = ctx.channels.channels.get(&self.channel_id).cloned() {
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
                != channel.accredited_keys[channel.round_robin(ctx.block_slot).0 as usize]
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

        // Check the signature
        if self
            .signer
            .verify(ctx.tx_hash.as_signing_bytes().as_ref(), ctx.inscribe_sig)
            .is_err()
        {
            return Err(Error::InvalidSignature);
        }

        Ok(())
    }

    fn execute(
        &self,
        mut ctx: Self::ExecutionContext<'_>,
    ) -> Result<(Self::ExecutionContext<'_>, Events), Self::Error> {
        // if the channel doesn't exist, create it
        let channel = ctx
            .channels
            .channels
            .get(&self.channel_id)
            .cloned()
            .unwrap_or_else(|| ChannelState {
                accredited_keys: vec![self.signer].into(),
                configuration_threshold: 1,
                tip_message: MsgId::root(),
                tip_slot: ctx.block_slot,
                tip_sequencer: 0,
                tip_sequencer_starting_slot: ctx.block_slot,
                posting_timeframe: 0.into(),
                balance: 0,
                withdraw_threshold: crate::mantle::channel::DEFAULT_WITHDRAW_THRESHOLD,
                withdrawal_nonce: 0,
                posting_timeout: 0.into(),
            });

        // Update the channel sequencer, its starting slot, the tip message and the tip
        // slot
        let (new_sequencer, new_starting_slot) = channel.round_robin(ctx.block_slot);
        ctx.channels.channels = ctx.channels.channels.insert(
            self.channel_id,
            ChannelState {
                tip_message: self.id(),
                accredited_keys: Arc::clone(&channel.accredited_keys),
                tip_sequencer: new_sequencer,
                tip_sequencer_starting_slot: new_starting_slot,
                tip_slot: ctx.block_slot,
                ..channel
            },
        );
        Ok((ctx, Events::new()))
    }
}

#[cfg(test)]
mod tests {
    use lb_utils::bounded_vec::BoundedError;

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
            matches!(err, BoundedError::TooLong { actual, max } if actual == MAX_BYTES + 1 && max == MAX_BYTES)
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
                    "Length {} exceeds static maximum of {MAX_BYTES}",
                    MAX_BYTES + 1,
                )
                .as_str()
            )
        );
    }

    #[test]
    fn encode_decode_round_trip() {
        let op = sample();
        let encoded = op.encode();
        let decoded = InscriptionOp::decode(&encoded).unwrap().1;
        assert_eq!(op, decoded);
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
