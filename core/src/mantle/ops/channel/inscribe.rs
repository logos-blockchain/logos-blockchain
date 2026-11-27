use std::{collections::HashSet, sync::Arc};

use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::Slot;
use lb_groth16::{Fr, serde::serde_fr};
#[cfg(any(test, feature = "test-utils"))]
use lb_key_management_system_keys::keys::Ed25519Key;
use lb_key_management_system_keys::keys::UnverifiedEd25519PublicKey;
use lb_utils::bounded::{Bounded, NonEmptyBoundedVec, UpperBoundedVec, UpperRuntimeBoundedVec};
use serde::{Deserialize, Serialize};

use super::{ChannelId, MsgId};
use crate::{
    block::BlockNumber,
    crypto::{Digest as _, Hasher},
    events::TxEvent,
    mantle::{
        batch::{DeferredZkTransfer, InscriptionDeferredProofs},
        channel::{ChannelState, Channels, Error, channel_note_set},
        gas::{Gas, GasOverflow, GasProfile, OpGasCalculator, OpGasContext},
        ledger::{
            ExecutableOperation, PreverifiableOperation, ProvableOperation, VerifiableOperation,
            verification_mode::{StandardMode, VerificationMode},
        },
        note_set::{NoteSet, NoteSetIndex, NoteSets, TxBuffer, TxNoteState},
        notes::{InputNullifiers, NoteCommitment, NoteNullifier, OutputCommitments},
        ops::{
            ChannelInscribeProof, OpId, SignedOperation, ZkTransferProof,
            channel::UnverifiedChannelKeys,
            zk_transfer::{check_zk_transfer_bounds, defer_zk_transfer},
        },
        transactions::{
            hash::TxHashView,
            states::{Preverified, Unverified, Verified},
        },
    },
};

/// The bytes an inscription writes, at most as many as the era it is decoded
/// under allows.
///
/// The length is checked against the era's maximum, a `u32`, when the
/// inscription is built or decoded, and an inscribe holds its inscription and
/// those of its steps to that maximum together. Serde reads it without an era,
/// under the largest maximum of the type.
pub type Inscription = UpperRuntimeBoundedVec<u8, u32>;

pub mod serde_inscription {
    use serde::{Deserializer, Serializer, de::Error as _};

    use super::Inscription;

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
        let bytes = lb_utils::serde::serde_bytes_slice::deserialize_bounded::<
            Vec<u8>,
            { u32::MAX as usize },
            D,
        >(deserializer)?;
        Inscription::try_new(bytes, u32::MAX).map_err(D::Error::custom)
    }
}

pub const MAX_CHANNEL_STEPS: usize = u16::MAX as usize; // StepCount = UINT16
pub const MAX_MINI_BLOCKS: usize = u16::MAX as usize; // MiniBlockCount = UINT16
// The steps of a mini-block are proven against their recent root extended with
// the buffer of the channel note set as it stands when the mini-block starts.
pub type MiniBlock = NonEmptyBoundedVec<Step, MAX_CHANNEL_STEPS>;
pub type MiniBlocks = UpperBoundedVec<MiniBlock, MAX_MINI_BLOCKS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryEncode, BinaryDecode)]
pub struct Step {
    pub inputs: InputNullifiers,
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    /// The zone message of the step, signed by its holder through `step_id`
    #[serde(with = "serde_inscription")]
    #[context]
    pub inscription: Inscription,
}

impl Step {
    fn preverify_proof(&self) -> Result<(), Error> {
        Ok(check_zk_transfer_bounds(
            self.inputs.len(),
            self.outputs.len(),
        )?)
    }
}

// The message the ZkTransfer of a step is bound to.
#[must_use]
pub fn step_id(channel_id: &ChannelId, step: &Step) -> Fr {
    super::step_id(b"STEP_V1", channel_id, step)
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryEncode)]
pub struct InscriptionOp {
    pub channel_id: ChannelId,
    /// Message to be written in the blockchain
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
    /// Enforce that this inscription comes after this tx
    pub parent: MsgId,
    // We can't use a verified Ed25519 public key here because genesis block uses a key of all
    // `0`s, which would fail validation at deserialization time. So we allow all inscription ops
    // to have such a key.
    // This is mitigated by the `verify_strict` we use during verification, but it just would not
    // give us compile-time guarantees about the validity of such a key.
    pub signer: UnverifiedEd25519PublicKey,
    /// Moves of channel notes, in the order they apply
    pub mini_blocks: MiniBlocks,
}

impl BinaryDecode for InscriptionOp {
    /// The most bytes the era the inscription is decoded under allows its
    /// inscriptions together.
    type Context = u32;

    fn decode<'input>(
        input: &'input [u8],
        max_op_data_size: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, channel_id) = ChannelId::decode(input, &())?;
        let (rest, inscription) = Inscription::decode(rest, &(*max_op_data_size, ()))?;
        let (rest, parent) = MsgId::decode(rest, &())?;
        let (rest, signer) = UnverifiedEd25519PublicKey::decode(rest, &())?;
        let (rest, mini_blocks) = MiniBlocks::decode(rest, &(*max_op_data_size, ()))?;
        let operation = Self {
            channel_id,
            inscription,
            parent,
            signer,
            mini_blocks,
        };
        if operation.inscriptions_len() > usize::try_from(*max_op_data_size).unwrap_or(usize::MAX) {
            return Err(DecodeError::invalid_value::<Self>(
                "the inscriptions of an inscribe total more than the era's max_op_data_size bytes",
            ));
        }
        Ok((rest, operation))
    }
}

impl InscriptionOp {
    #[must_use]
    pub fn id(&self) -> MsgId {
        let mut hasher = Hasher::new();
        hasher.update(self.encode().as_ref());
        MsgId(hasher.finalize().into())
    }

    /// The bytes of the inscription and of the inscriptions of the steps, which
    /// count together against the era's `max_op_data_size`.
    #[must_use]
    pub fn inscriptions_len(&self) -> usize {
        self.inscription.len()
            + self
                .steps()
                .map(|step| step.inscription.len())
                .sum::<usize>()
    }

    /// The steps of the inscription, in the order they apply.
    pub fn steps(&self) -> impl Iterator<Item = &Step> {
        self.mini_blocks.iter().flat_map(Bounded::iter)
    }

    /// The number of steps of the inscription, over all its mini-blocks.
    #[must_use]
    pub fn steps_len(&self) -> usize {
        self.mini_blocks.iter().map(MiniBlock::len).sum()
    }

    // One proof per step, each fitting the ZkTransfer circuit.
    fn preverify_step_proofs(&self, proofs: &[ZkTransferProof]) -> Result<(), Error> {
        let steps = self.steps_len();
        if proofs.len() != steps {
            return Err(Error::StepProofCount {
                steps,
                proofs: proofs.len(),
            });
        }
        self.steps().try_for_each(Step::preverify_proof)
    }

    // The note set of the channel the inscription extends, after checking the
    // inscription may extend it. A channel being created holds no note yet.
    fn verify_channel(
        &self,
        context: &InscriptionValidationContext<'_>,
    ) -> Result<Option<NoteSetIndex>, Error> {
        if let Some(channel) = context.channels.channels.get(&self.channel_id) {
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

            Ok(channel_note_set(self.channel_id))
        } else if self.parent != MsgId::root() {
            // The channel doesn't exist, so the parent must be ZERO
            Err(Error::InvalidParent {
                channel_id: self.channel_id,
                parent: self.parent.into(),
                actual: MsgId::root().into(),
            })
        } else {
            // A channel being created holds no note yet, so its inscription
            // carries no step
            Ok(None)
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            channel_id: ChannelId::from([14u8; 32]),
            inscription: Inscription::new_unchecked(b"hello logos".to_vec()),
            parent: MsgId::root(),
            signer: Ed25519Key::from_bytes(&[15; 32])
                .public_key()
                .into_unverified(),
            mini_blocks: MiniBlocks::default(),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample_with_step() -> Self {
        Self {
            // The steps of the spec vector.
            mini_blocks: [MiniBlock::from([Step {
                inputs: [NoteNullifier(Fr::from(0x23u64))].into(),
                outputs: [NoteCommitment(Fr::from(0x24u64))].into(),
                commitment_merkle_root: Fr::from(0x25u64),
                inscription: Inscription::new_unchecked(b"step-inscription".to_vec()),
            }])]
            .into(),
            ..Self::sample()
        }
    }
}

impl OpId for InscriptionOp {
    fn op_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

pub struct InscriptionPreverificationContext<'a> {
    pub tx_hash_view: &'a TxHashView,
}

pub struct InscriptionValidationContext<'a> {
    pub channels: &'a Channels,
    pub note_sets: &'a NoteSets,
    pub tx_notes: &'a TxNoteState,
    pub block_slot: Slot,
    pub block_number: BlockNumber,
}

pub struct InscriptionExecutionContext<'a> {
    pub channels: Channels,
    pub note_sets: NoteSets,
    pub tx_notes: &'a mut TxNoteState,
    pub block_slot: Slot,
}

impl ProvableOperation for InscriptionOp {
    type Proof = ChannelInscribeProof;
    const CODE: u8 = 0x11;
}

impl OpGasCalculator for InscriptionOp {
    // The Operation and its signature, plus each step beyond its spend.
    fn execution_gas(
        &self,
        profile: &GasProfile,
        _context: &impl OpGasContext,
    ) -> Result<Gas, GasOverflow> {
        let mut gas = profile.channel_inscribe.checked_add(profile.signature)?;
        for step in self.steps() {
            let step_gas = profile
                .user_step
                .checked_add(profile.spend_gas(step.inputs.len(), step.outputs.len())?)?;
            gas = gas.checked_add(step_gas)?;
        }
        Ok(gas)
    }
}

impl PreverifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Unverified, StandardMode>
{
    type Context<'a> = InscriptionPreverificationContext<'a>;
    type Error = Error;

    fn preverify(&self, context: &Self::Context<'_>) -> Result<(), Self::Error> {
        // Even if the signer weakness is also checked as part of the
        // `.verify()` function below, we still explicitly check it
        // here, in case something changes within the `.verify()`
        // implementation.
        if self.operation().signer.is_weak() {
            return Err(Error::InvalidSigner);
        }

        // Check the signature
        self.operation()
            .signer
            .verify(context.tx_hash_view.as_bytes(), &self.proof().signature)
            .map_err(|_error| Error::InvalidSignature)?;

        // One proof per step, of the kind of its step and fitting its circuit
        self.operation()
            .preverify_step_proofs(&self.proof().step_proofs)
    }
}

impl VerifiableOperation<StandardMode>
    for SignedOperation<InscriptionOp, Preverified, StandardMode>
{
    type DeferredProof = InscriptionDeferredProofs;
    type Context<'a> = InscriptionValidationContext<'a>;
    type Error = Error;

    fn verify(&self, context: &Self::Context<'_>) -> Result<Self::DeferredProof, Self::Error> {
        let operation = self.operation();
        let note_set = operation.verify_channel(context)?;

        // The signature is cheap and checked by `preverify`, only the step
        // proofs are deferred.
        if operation.mini_blocks.is_empty() {
            return Ok(InscriptionDeferredProofs::default());
        }
        let index = note_set.ok_or(Error::NoNoteSet {
            channel_id: operation.channel_id,
        })?;
        let mut step_proofs = self.proof().step_proofs.iter();
        let verifier = operation.mini_blocks.iter().try_fold(
            StepVerifier::new(operation, context, index),
            |verifier, mini_block| verifier.verify_mini_block(mini_block, &mut step_proofs),
        )?;
        Ok(verifier.deferred)
    }
}

// The verification of the steps of an inscription, in order: the buffer of the
// channel note set extended with the notes of the steps verified so far, and
// their proofs.
struct StepVerifier<'a> {
    operation: &'a InscriptionOp,
    context: &'a InscriptionValidationContext<'a>,
    note_set: &'a NoteSet,
    buffer: TxBuffer,
    // The nullifiers the steps verified so far consume
    step_nullifiers: HashSet<NoteNullifier>,
    deferred: InscriptionDeferredProofs,
}

impl<'a> StepVerifier<'a> {
    fn new(
        operation: &'a InscriptionOp,
        context: &'a InscriptionValidationContext<'a>,
        index: NoteSetIndex,
    ) -> Self {
        Self {
            operation,
            context,
            note_set: context
                .note_sets
                .get(index)
                .expect("the note set of a channel exists"),
            buffer: context.tx_notes.buffer(index).into_owned(),
            step_nullifiers: HashSet::new(),
            deferred: InscriptionDeferredProofs::default(),
        }
    }

    fn verify_mini_block<'p>(
        mut self,
        mini_block: &'a MiniBlock,
        step_proofs: &mut impl Iterator<Item = &'p ZkTransferProof>,
    ) -> Result<Self, Error> {
        // The steps of the mini-block cover the buffer as it stands now.
        let covered = self.buffer.len();
        for step in mini_block.iter() {
            let proof = step_proofs
                .next()
                .expect("preverify checked one proof per step");
            self.check_step_inputs(step)?;
            self.verify_step(step, covered, proof)?;
            // The next mini-blocks cover the notes this step creates.
            self.buffer.push(step.outputs.as_slice());
        }
        Ok(self)
    }

    // The step consumes unspent notes of the channel, which no earlier step
    // consumed.
    fn check_step_inputs(&mut self, step: &Step) -> Result<(), Error> {
        self.note_set.assert_spendable(step.inputs.as_slice())?;
        for note_nullifier in step.inputs.iter() {
            if !self.step_nullifiers.insert(*note_nullifier) {
                return Err(Error::StepDoubleSpend(*note_nullifier));
            }
        }
        Ok(())
    }

    // A step balances exactly.
    fn verify_step(
        &mut self,
        step: &Step,
        covered: usize,
        proof: &ZkTransferProof,
    ) -> Result<(), Error> {
        let msg = step_id(&self.operation.channel_id, step);
        let transfer = self.holder_transfer(step, covered, msg, proof)?;
        self.deferred.zk_transfers.push(transfer);
        Ok(())
    }

    // The ZkTransfer a holder signed for its step, against its root extended
    // with the buffer before its mini-block.
    fn holder_transfer(
        &self,
        step: &Step,
        covered: usize,
        msg: Fr,
        proof: &ZkTransferProof,
    ) -> Result<DeferredZkTransfer, Error> {
        let tx_commitment_root = self.note_set.cached_step_commitment_root(
            self.context.tx_notes.roots(),
            &self.buffer,
            &step.commitment_merkle_root,
            covered,
            self.context.block_number,
        )?;
        Ok(defer_zk_transfer(
            proof,
            step.inputs.iter(),
            step.outputs.iter(),
            0,
            tx_commitment_root,
            msg,
        )?)
    }
}

impl<Mode: VerificationMode> ExecutableOperation
    for SignedOperation<InscriptionOp, Verified, Mode>
{
    type Context<'a> = InscriptionExecutionContext<'a>;
    type Error = Error;

    fn execute<'a>(
        &self,
        mut context: Self::Context<'a>,
    ) -> Result<(Self::Context<'a>, Vec<TxEvent>), Self::Error> {
        let operation = self.operation();

        // if the channel doesn't exist, create it
        (context.channels, context.note_sets) = context.channels.create_if_not_exists(
            operation.channel_id,
            context.block_slot,
            UnverifiedChannelKeys::from(operation.signer),
            context.note_sets,
        );
        let channel = context
            .channels
            .channel_state(&operation.channel_id)
            .cloned()
            .expect("the channel exists once created");

        // Apply the steps to the channel note set, in order
        if !operation.mini_blocks.is_empty() {
            let note_set = context
                .note_sets
                .channel(&operation.channel_id)
                .ok_or(Error::NoNoteSet {
                    channel_id: operation.channel_id,
                })?
                .clone();
            let index = note_set.index();
            // The outputs of the steps extend the buffer the next Operations of
            // the transaction are proven against.
            let note_set = operation.steps().try_fold(note_set, |note_set, step| {
                let outputs = step.outputs.as_slice();
                context.tx_notes.buffer_outputs(index, outputs);
                note_set
                    .execute_spending(step.inputs.as_slice())?
                    .execute_adding(outputs)
            })?;
            context.note_sets = context.note_sets.update_channel(note_set);
        }

        // Update the channel sequencer, its starting slot, the tip message and
        // the tip slot
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

#[cfg(test)]
mod tests {
    use ark_ff::PrimeField as _;
    use lb_groth16::{AdditiveGroup as _, Field as _, Groth16Input};
    use lb_key_management_system_keys::keys::{ZkKey, ZkPublicKey};
    use lb_zktransfer::{ZkTransferInputsError, ZkTransferVerifierInput};

    use super::*;
    use crate::{
        crypto::Hash,
        mantle::{
            TxHash,
            gas::test_utils::{FixedThresholds, GAS_PROFILE},
            note_set::{CommitmentMmr, NoteSetError, TypedNoteSet},
            notes::{self, derive_note_nullifier},
            ops::{
                channel::NULL_CHANNEL_ID, op_proof::samples::SampleProof as _,
                transfer::test_utils::prove_transfer,
            },
            transactions::tx_list::signed_ops::test_utils::make_channel_state,
        },
    };

    fn preverified(
        operation: InscriptionOp,
    ) -> SignedOperation<InscriptionOp, Preverified, StandardMode> {
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof = ChannelInscribeProof::without_steps(
            Ed25519Key::from_bytes(&[15; 32]).sign_payload(tx_hash_view.as_bytes().as_ref()),
        );

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
            inscription: Inscription::new_unchecked(b"genesis".to_vec()),
            parent: MsgId([0u8; 32]),
            signer: UnverifiedEd25519PublicKey::from_bytes(&[0u8; 32]).unwrap(),
            mini_blocks: MiniBlocks::default(),
        }
    }

    #[test]
    fn the_inscriptions_of_an_inscribe_and_its_steps_count_together() {
        const MAX_OP_DATA_SIZE: usize = 64;
        let bytes = |len: usize| Inscription::new_unchecked(vec![0u8; len]);
        // One byte in a first step, `last_len` in a second one and the rest of
        // the limit in the inscribe.
        let inscribed = |last_len: usize| {
            let first = Step {
                inscription: bytes(1),
                ..step(&[nullifier(2)], &[commitment(3)], root())
            };
            let last = Step {
                inscription: bytes(last_len),
                ..step(&[nullifier(4)], &[commitment(5)], root())
            };
            InscriptionOp {
                inscription: bytes(MAX_OP_DATA_SIZE - 1),
                ..with_steps(vec![first, last])
            }
        };

        let max_op_data_size = u32::try_from(MAX_OP_DATA_SIZE).unwrap();
        let at_the_limit = inscribed(0);
        assert_eq!(at_the_limit.inscriptions_len(), MAX_OP_DATA_SIZE);
        assert_eq!(
            InscriptionOp::decode(&at_the_limit.encode(), &max_op_data_size)
                .unwrap()
                .1,
            at_the_limit
        );

        assert!(matches!(
            InscriptionOp::decode(&inscribed(1).encode(), &max_op_data_size),
            Err(DecodeError::InvalidValue { .. })
        ));
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
            ChannelInscribeProof::without_steps(
                signing_key.sign_payload(signed_view.as_bytes().as_ref()),
            ),
        );

        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &signed_view,
            }),
            Ok(())
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &other_view,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn preverify_rejects_a_signature_unrelated_to_the_op() {
        let tx_hash_view = TxHashView::from(TxHash::from([9u8; 32]));
        let proof = ChannelInscribeProof::without_steps(
            Ed25519Key::from_bytes(&[16; 32]).sign_payload(tx_hash_view.as_bytes().as_ref()),
        );

        let signed_operation =
            SignedOperation::<_, Unverified, StandardMode>::new(InscriptionOp::sample(), proof);

        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view,
            }),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn verify_accepts_an_unregistered_channel_rooted_inscription() {
        let signed_operation = preverified(InscriptionOp::sample());

        assert!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &Channels::new(),
                    note_sets: &NoteSets::new(),
                    tx_notes: &TxNoteState::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .is_ok()
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
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &Channels::new(),
                    note_sets: &NoteSets::new(),
                    tx_notes: &TxNoteState::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .unwrap_err(),
            Error::InvalidParent {
                channel_id,
                parent: parent.into(),
                actual: MsgId::root().into(),
            }
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
                ..make_channel_state(Some(
                    [Ed25519Key::from_bytes(&[15; 32])
                        .public_key()
                        .into_unverified()]
                    .into(),
                ))
            },
        );

        assert_eq!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &channels,
                    note_sets: &NoteSets::new(),
                    tx_notes: &TxNoteState::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .unwrap_err(),
            Error::InvalidParent {
                channel_id,
                parent: parent.into(),
                actual: tip_message.into(),
            }
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
            make_channel_state(Some(
                [Ed25519Key::from_bytes(&[16; 32])
                    .public_key()
                    .into_unverified()]
                .into(),
            )),
        );

        assert_eq!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &channels,
                    note_sets: &NoteSets::new(),
                    tx_notes: &TxNoteState::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .unwrap_err(),
            Error::UnauthorizedSigner {
                channel_id,
                signer: format!("{signer:?}"),
            }
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
                ..make_channel_state(Some(
                    [
                        Ed25519Key::from_bytes(&[16; 32])
                            .public_key()
                            .into_unverified(),
                        signer,
                    ]
                    .into(),
                ))
            },
        );

        assert!(
            signed_operation
                .verify(&InscriptionValidationContext {
                    channels: &channels,
                    note_sets: &NoteSets::new(),
                    tx_notes: &TxNoteState::new(),
                    block_slot: Slot::from(0),
                    block_number: 1,
                })
                .is_ok()
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

        let mut tx_notes = TxNoteState::new();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                note_sets: NoteSets::new(),
                tx_notes: &mut tx_notes,
                block_slot,
            })
            .expect("inscribing never fails");

        assert_eq!(
            context.channels.channel_state(&channel_id),
            Some(&ChannelState {
                accredited_keys: Arc::new(UnverifiedChannelKeys::from(signer)),
                configuration_threshold: 1,
                tip_message: message_id,
                config_tip_hash: MsgId::root(),
                tip_slot: block_slot,
                tip_sequencer: 0,
                tip_sequencer_starting_slot: block_slot,
                posting_timeframe: 0.into(),
                posting_timeout: 0.into(),
            })
        );
        assert_eq!(
            context
                .note_sets
                .channel(&channel_id)
                .cloned()
                .map(TypedNoteSet::into_inner),
            Some(NoteSet::empty())
        );
        assert_eq!(events, []);
    }

    #[test]
    fn execute_bootstraps_the_null_channel_without_a_note_set() {
        let operation = InscriptionOp {
            channel_id: NULL_CHANNEL_ID,
            ..InscriptionOp::sample()
        };
        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let mut tx_notes = TxNoteState::new();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels: Channels::new(),
                note_sets: NoteSets::new(),
                tx_notes: &mut tx_notes,
                block_slot: Slot::from(7u64),
            })
            .expect("inscribing never fails");

        assert!(context.channels.channel_state(&NULL_CHANNEL_ID).is_some());
        assert_eq!(context.note_sets.channel(&NULL_CHANNEL_ID), None);
        assert_eq!(context.note_sets, NoteSets::new());
    }

    #[test]
    fn execute_rotates_the_sequencer_of_an_existing_channel() {
        let operation = InscriptionOp::sample();
        let channel_id = operation.channel_id;
        let message_id = operation.id();
        let keys: UnverifiedChannelKeys = [
            Ed25519Key::from_bytes(&[16; 32])
                .public_key()
                .into_unverified(),
            operation.signer,
        ]
        .into();

        let channels = channels(
            channel_id,
            ChannelState {
                configuration_threshold: 2,
                posting_timeframe: 1.into(),
                ..make_channel_state(Some(keys.clone()))
            },
        );

        let signed_operation: SignedOperation<_, _, StandardMode> = SignedOperation::new(
            operation,
            <InscriptionOp as ProvableOperation>::Proof::sample(),
        )
        .into_state_trusted();

        let mut tx_notes = TxNoteState::new();

        let (context, events) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets: NoteSets::new(),
                tx_notes: &mut tx_notes,
                block_slot: Slot::from(1u64),
            })
            .expect("inscribing never fails");

        assert_eq!(
            context.channels.channel_state(&channel_id),
            Some(&ChannelState {
                accredited_keys: Arc::new(keys),
                configuration_threshold: 2,
                tip_message: message_id,
                config_tip_hash: MsgId::root(),
                tip_slot: Slot::from(1u64),
                tip_sequencer: 1,
                tip_sequencer_starting_slot: Slot::from(1u64),
                posting_timeframe: 1.into(),
                posting_timeout: 0.into(),
            })
        );
        assert_eq!(events, []);
    }

    #[test]
    fn inscription_op_execution_gas_does_not_scale_with_the_threshold() {
        for threshold in [0, 1, 3] {
            assert_eq!(
                InscriptionOp::sample().execution_gas(&GAS_PROFILE, &FixedThresholds(threshold)),
                // 26 + 557
                Ok(Gas::new(583))
            );
        }
    }

    #[test]
    fn inscription_op_execution_gas_charges_a_spend_per_step() {
        let step = Step {
            inputs: [NoteNullifier(Fr::from(1u64))].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::ZERO,
            inscription: Inscription::default(),
        };
        // 583 + steps * (28 + 4177 + 1758), one input, no output, nothing
        // buffered
        for (steps, expected) in [(0usize, 583u64), (1, 6_546), (3, 18_472)] {
            let operation = with_steps(vec![step.clone(); steps]);
            for threshold in [0, 3] {
                assert_eq!(
                    operation.execution_gas(&GAS_PROFILE, &FixedThresholds(threshold)),
                    Ok(Gas::new(expected))
                );
            }
        }
    }

    fn commitment(n: u64) -> NoteCommitment {
        NoteCommitment(Fr::from(n))
    }

    fn nullifier(n: u64) -> NoteNullifier {
        NoteNullifier(Fr::from(n))
    }

    fn step(
        inputs: &[NoteNullifier],
        outputs: &[NoteCommitment],
        commitment_merkle_root: Fr,
    ) -> Step {
        Step {
            inputs: InputNullifiers::try_from_iter(inputs.iter().copied()).unwrap(),
            outputs: OutputCommitments::try_from_iter(outputs.iter().copied()).unwrap(),
            commitment_merkle_root,
            inscription: Inscription::default(),
        }
    }

    // The steps in a single mini-block, none for no step.
    fn with_steps(steps: Vec<Step>) -> InscriptionOp {
        if steps.is_empty() {
            return with_mini_blocks(vec![]);
        }
        with_mini_blocks(vec![steps])
    }

    fn with_mini_blocks(mini_blocks: Vec<Vec<Step>>) -> InscriptionOp {
        InscriptionOp {
            mini_blocks: MiniBlocks::try_from(
                mini_blocks
                    .into_iter()
                    .map(|steps| MiniBlock::try_from(steps).unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            ..InscriptionOp::sample()
        }
    }

    fn signer() -> Ed25519Key {
        Ed25519Key::from_bytes(&[15; 32])
    }

    // The sample channel, accrediting the sample signer, with an empty note
    // set.
    fn empty_state() -> (Channels, NoteSets) {
        let keys = UnverifiedChannelKeys::from(signer().public_key().into_unverified());
        Channels::new().create_channel(sample_channel(), Slot::from(0u64), keys, NoteSets::new())
    }

    // The sample channel, accrediting the sample signer, owns note set 2, which
    // holds commitment(1), recorded at the end of block 0.
    fn state() -> (Channels, NoteSets) {
        let channel_id = sample_channel();
        let (channels, note_sets) = empty_state();
        let set = note_sets
            .channel(&channel_id)
            .unwrap()
            .clone()
            .execute_adding(&[commitment(1)])
            .unwrap();
        (
            channels,
            note_sets.update_channel(set).end_block(0, 1024).unwrap(),
        )
    }

    fn sample_channel() -> ChannelId {
        InscriptionOp::sample().channel_id
    }

    fn sample_set() -> NoteSetIndex {
        NoteSetIndex::Channel(sample_channel())
    }

    fn root() -> Fr {
        CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .frontier_root()
    }

    fn tx_hash_view() -> TxHashView {
        TxHashView::from(TxHash::from([9u8; 32]))
    }

    fn signed(
        operation: InscriptionOp,
        step_proofs: Vec<ZkTransferProof>,
    ) -> SignedOperation<InscriptionOp, Unverified, StandardMode> {
        let signature = signer().sign_payload(tx_hash_view().as_bytes().as_ref());
        SignedOperation::new(operation, ChannelInscribeProof::new(signature, step_proofs))
    }

    fn verify_steps(
        steps: Vec<Step>,
        channels: &Channels,
        note_sets: &NoteSets,
    ) -> Result<InscriptionDeferredProofs, Error> {
        let proofs = steps.iter().map(|_| ZkTransferProof::sample()).collect();
        signed(with_steps(steps), proofs)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .expect("the sample signer signed the transaction hash")
            .verify(&InscriptionValidationContext {
                channels,
                note_sets,
                tx_notes: &TxNoteState::new(),
                block_slot: Slot::from(0u64),
                block_number: 1,
            })
    }

    #[test]
    fn step_id_hashes_the_channel_and_the_encoded_step() {
        let channel = ChannelId::from([0x22u8; 32]);
        let step = Step {
            inscription: Inscription::new_unchecked(b"step-inscription".to_vec()),
            ..step(&[nullifier(0x23)], &[commitment(0x24)], Fr::from(0x25u64))
        };
        let mut preimage = b"STEP_V1".to_vec();
        preimage.extend_from_slice(&[0x22u8; 32]);
        preimage.extend(step.encode());
        let digest: [u8; 32] = Hasher::digest(&preimage).into();
        assert_eq!(
            step_id(&channel, &step),
            Fr::from_le_bytes_mod_order(&digest)
        );
        // every field of the step is covered, its root included
        assert_ne!(
            step_id(
                &channel,
                &Step {
                    commitment_merkle_root: Fr::ONE,
                    ..step.clone()
                }
            ),
            step_id(&channel, &step)
        );
    }

    #[test]
    fn preverify_rejects_a_step_proof_count_other_than_the_step_count() {
        let signed_operation = signed(
            with_steps(vec![step(&[nullifier(2)], &[commitment(3)], root())]),
            vec![],
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view()
            }),
            Err(Error::StepProofCount {
                steps: 1,
                proofs: 0
            })
        );
    }

    #[test]
    fn preverify_rejects_a_step_with_more_than_eight_outputs() {
        let outputs: Vec<NoteCommitment> = (1..=9).map(commitment).collect();
        let signed_operation = signed(
            with_steps(vec![step(&[nullifier(2)], &outputs, root())]),
            vec![ZkTransferProof::sample()],
        );
        assert_eq!(
            signed_operation.preverify(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view()
            }),
            Err(Error::ZkTransferInputs(
                ZkTransferInputsError::TooManyOutputs(9)
            ))
        );
    }

    #[test]
    fn verify_rejects_steps_on_a_channel_being_created_and_on_the_null_channel() {
        // Neither holds a note: the null channel has no note set, and a channel
        // being created gets its set only when the inscription
        // executes.
        for channel_id in [NULL_CHANNEL_ID, ChannelId::from([5u8; 32])] {
            let operation = InscriptionOp {
                channel_id,
                ..with_steps(vec![step(&[nullifier(2)], &[commitment(3)], root())])
            };
            let deferred = signed(operation, vec![ZkTransferProof::sample()])
                .into_preverified(&InscriptionPreverificationContext {
                    tx_hash_view: &tx_hash_view(),
                })
                .unwrap()
                .verify(&InscriptionValidationContext {
                    channels: &Channels::new(),
                    note_sets: &NoteSets::new(),
                    tx_notes: &TxNoteState::new(),
                    block_slot: Slot::from(0u64),
                    block_number: 1,
                });
            assert_eq!(deferred.unwrap_err(), Error::NoNoteSet { channel_id });
        }
    }

    #[test]
    fn verify_defers_one_balanced_transfer_per_step_bound_to_its_step_id() {
        let (channels, note_sets) = state();
        let steps = vec![
            step(&[nullifier(2)], &[commitment(3)], root()),
            step(&[nullifier(4)], &[commitment(5)], root()),
        ];
        let deferred = verify_steps(steps.clone(), &channels, &note_sets).unwrap();
        assert_eq!(deferred.zk_transfers.len(), 2);
        for (deferred, step) in deferred.zk_transfers.iter().zip(&steps) {
            assert_eq!(deferred.inputs().excess_value.into_inner(), Fr::ZERO);
            assert_eq!(
                deferred.inputs().msg.into_inner(),
                step_id(&with_steps(vec![]).channel_id, step)
            );
            assert_eq!(deferred.inputs().tx_commitment_root.into_inner(), root());
            assert_eq!(
                deferred.inputs().inputs[0].into_inner(),
                step.inputs.as_slice()[0].0
            );
        }
    }

    #[test]
    fn verify_extends_each_step_root_with_the_buffer_before_its_mini_block() {
        let (_, note_sets) = state();
        // An earlier Operation of the transaction deposited commitment(7) into
        // the channel.
        let mut tx_notes = TxNoteState::new();
        tx_notes.buffer_outputs(sample_set(), &[commitment(7)]);
        let mini_blocks = vec![
            vec![
                step(&[nullifier(2)], &[commitment(3)], root()),
                step(&[nullifier(4)], &[commitment(5)], root()),
            ],
            vec![step(&[nullifier(6)], &[commitment(8)], root())],
        ];
        let deferred = verify_in_tx(
            with_mini_blocks(mini_blocks.clone()),
            sample_proofs(&mini_blocks.concat()),
            &note_sets,
            &tx_notes,
        )
        .unwrap();
        let extended = |commitments: &[u64]| {
            commitments
                .iter()
                .fold(
                    CommitmentMmr::new().push(commitment(1)).unwrap(),
                    |mmr, n| mmr.push(commitment(*n)).unwrap(),
                )
                .frontier_root()
        };
        // The steps of a mini-block cover the same buffer, whatever their
        // order.
        for zk_transfer in &deferred.zk_transfers[..2] {
            assert_eq!(
                zk_transfer.inputs().tx_commitment_root.into_inner(),
                extended(&[7])
            );
        }
        assert_eq!(
            deferred.zk_transfers[2]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            extended(&[7, 3, 5])
        );
    }

    #[test]
    fn a_second_inscription_of_the_transaction_covers_the_first_in_its_first_mini_block() {
        let (_, note_sets) = state();
        // The first inscription of the transaction added commitment(3) to the
        // buffer.
        let mut tx_notes = TxNoteState::new();
        tx_notes.buffer_outputs(sample_set(), &[commitment(3)]);
        let steps = vec![step(&[nullifier(4)], &[], root())];
        let deferred = verify_in_tx(
            with_steps(steps.clone()),
            sample_proofs(&steps),
            &note_sets,
            &tx_notes,
        )
        .unwrap();
        assert_eq!(
            deferred.zk_transfers[0]
                .inputs()
                .tx_commitment_root
                .into_inner(),
            CommitmentMmr::new()
                .push(commitment(1))
                .unwrap()
                .push(commitment(3))
                .unwrap()
                .frontier_root()
        );
    }

    #[test]
    fn an_empty_mini_block_does_not_decode() {
        // One mini-block of no step
        assert!(MiniBlocks::decode(&[1, 0, 0, 0], &(u32::MAX, ())).is_err());
        assert_eq!(
            MiniBlocks::decode(&[0, 0], &(u32::MAX, ())).unwrap().1,
            MiniBlocks::default()
        );
    }

    #[test]
    fn verify_rejects_a_note_consumed_by_an_earlier_step() {
        let (channels, note_sets) = state();
        let steps = vec![
            step(&[nullifier(2)], &[commitment(3)], root()),
            step(&[nullifier(2)], &[commitment(4)], root()),
        ];
        assert_eq!(
            verify_steps(steps, &channels, &note_sets).unwrap_err(),
            Error::StepDoubleSpend(nullifier(2))
        );
    }

    #[test]
    fn verify_rejects_a_spent_channel_note() {
        let (channels, note_sets) = state();
        let set = note_sets
            .channel(&sample_channel())
            .unwrap()
            .clone()
            .execute_spending(&[nullifier(2)])
            .unwrap();
        let note_sets = note_sets.update_channel(set);
        assert_eq!(
            verify_steps(
                vec![step(&[nullifier(2)], &[commitment(3)], root())],
                &channels,
                &note_sets
            )
            .unwrap_err(),
            Error::NoteSet(NoteSetError::AlreadySpent(nullifier(2)))
        );
    }

    #[test]
    fn execute_spends_adds_and_buffers_every_step_in_order() {
        let (channels, note_sets) = state();
        let operation = with_steps(vec![
            step(&[nullifier(2)], &[commitment(3)], root()),
            step(&[nullifier(4)], &[commitment(5)], root()),
        ]);
        let signed_operation: SignedOperation<_, _, StandardMode> = signed(
            operation,
            vec![ZkTransferProof::sample(), ZkTransferProof::sample()],
        )
        .into_state_trusted();

        let mut tx_notes = TxNoteState::new();

        let (context, _) = signed_operation
            .execute(InscriptionExecutionContext {
                channels,
                note_sets,
                tx_notes: &mut tx_notes,
                block_slot: Slot::from(1u64),
            })
            .expect("the channel has a note set");

        let set = context.note_sets.channel(&sample_channel()).unwrap();
        let expected = CommitmentMmr::new()
            .push(commitment(1))
            .unwrap()
            .push(commitment(3))
            .unwrap()
            .push(commitment(5))
            .unwrap()
            .frontier_root();
        assert!(
            set.contains_nullifier(&nullifier(2)).unwrap()
                && set.contains_nullifier(&nullifier(4)).unwrap()
        );
        assert_eq!(set.commitment_root(), expected);
        assert_eq!(
            context.tx_notes.commitment_root(set, &root(), 1),
            Ok(expected)
        );
    }

    #[test]
    fn step_proof_is_bound_to_its_step_id() {
        let secret_key = Fr::from(1u64);
        let input = notes::Note::new(
            1_000,
            Fr::from(11u64),
            ZkKey::new(secret_key).to_public_key(),
        );
        let (mmr, path) = CommitmentMmr::new()
            .push_with_paths(input.commitment(), &mut [])
            .unwrap();
        let (channels, note_sets) = empty_state();
        let set = note_sets
            .channel(&sample_channel())
            .unwrap()
            .clone()
            .execute_adding(&[input.commitment()])
            .unwrap();
        let note_sets = note_sets.update_channel(set).end_block(1, 1024).unwrap();
        let output = notes::Note::new(1_000, Fr::from(12u64), ZkPublicKey::new(Fr::from(2u64)));
        let step = step(
            &[derive_note_nullifier(
                &input.commitment(),
                &ZkKey::new(secret_key).nullifier_key(),
            )],
            &[output.commitment()],
            mmr.frontier_root(),
        );
        let proof = prove_transfer(
            &[(input, secret_key, path)],
            &[output],
            mmr.frontier_root(),
            step_id(&with_steps(vec![]).channel_id, &step),
        );

        let deferred = signed(with_steps(vec![step]), vec![ZkTransferProof::new(proof)])
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .unwrap()
            .verify(&InscriptionValidationContext {
                channels: &channels,
                note_sets: &note_sets,
                tx_notes: &TxNoteState::new(),
                block_slot: Slot::from(0u64),
                block_number: 2,
            })
            .unwrap();
        let deferred = &deferred.zk_transfers;
        assert!(lb_zktransfer::verify(deferred[0].proof(), deferred[0].inputs()).unwrap());

        let as_a_transfer = ZkTransferVerifierInput {
            msg: Groth16Input::new(*tx_hash_view().as_fr()),
            ..*deferred[0].inputs()
        };
        assert!(!lb_zktransfer::verify(deferred[0].proof(), &as_a_transfer).unwrap());
    }

    fn verify_in_tx(
        operation: InscriptionOp,
        proofs: Vec<ZkTransferProof>,
        note_sets: &NoteSets,
        tx_notes: &TxNoteState,
    ) -> Result<InscriptionDeferredProofs, Error> {
        let (channels, _) = state();
        signed(operation, proofs)
            .into_preverified(&InscriptionPreverificationContext {
                tx_hash_view: &tx_hash_view(),
            })
            .expect("preverify accepts the inscription")
            .verify(&InscriptionValidationContext {
                channels: &channels,
                note_sets,
                tx_notes,
                block_slot: Slot::from(0u64),
                block_number: 1,
            })
    }

    fn sample_proofs(steps: &[Step]) -> Vec<ZkTransferProof> {
        steps.iter().map(|_| ZkTransferProof::sample()).collect()
    }
}
