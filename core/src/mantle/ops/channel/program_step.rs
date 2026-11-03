use std::sync::LazyLock;

use bitvec::{array::BitArray, order::Lsb0};
use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::Digest as _;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use super::{ChannelId, MsgId};
use crate::{
    crypto::{Digest as _, Hash, Hasher, ZkHasher},
    mantle::{
        Value,
        notes::{InputNullifiers, NoteCommitment, OutputCommitments, ProgramId, derive_note_nonce},
    },
};

static PROGRAM_PAYMENT_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"PROGRAM_PAYMENT_V1").expect("BigUint should load from constant string")
});

static PROGRAM_PAYMENTS_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"PROGRAM_PAYMENTS_V1").expect("BigUint should load from constant string")
});

pub const MAX_PAYMENTS: usize = u8::MAX as usize; // PaymentCount = Byte
pub type Payments = UpperBoundedVec<Payment, MAX_PAYMENTS>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct Payment {
    pub inputs: InputNullifiers,
    // The change, user notes of the channel.
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    pub buffered: u16,
    // The excess of the ZkTransfer, paid to the program.
    pub paid_value: Value,
    #[serde(with = "serde_fr")]
    pub intent: Fr,
}

impl Payment {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            inputs: [crate::mantle::notes::NoteNullifier(Fr::from(0x35u64))].into(),
            outputs: [NoteCommitment(Fr::from(0x36u64))].into(),
            commitment_merkle_root: Fr::from(0x37u64),
            buffered: 0x47,
            paid_value: 0x38,
            intent: Fr::from(0x39u64),
        }
    }
}

// Bit i (least significant first) set when output i is a note of the program.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
pub struct ProgramOutputs(BitArray<u8, Lsb0>);

impl From<u8> for ProgramOutputs {
    fn from(byte: u8) -> Self {
        Self(BitArray::new(byte))
    }
}

impl From<ProgramOutputs> for u8 {
    fn from(outputs: ProgramOutputs) -> Self {
        outputs.0.into_inner()
    }
}

impl BinaryEncode for ProgramOutputs {
    fn encoded_length(&self) -> usize {
        1
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        u8::from(*self).encode_into(out);
    }
}

impl BinaryDecode for ProgramOutputs {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, byte) = u8::decode(input, &())?;
        Ok((rest, byte.into()))
    }
}

impl ProgramOutputs {
    #[must_use]
    pub fn is_program_output(self, index: usize) -> bool {
        self.0.get(index).is_some_and(|bit| *bit)
    }

    #[must_use]
    pub fn count(self) -> usize {
        self.0.count_ones()
    }

    // No bit at or above `outputs`.
    #[must_use]
    pub fn fits(self, outputs: usize) -> bool {
        self.0.iter_ones().all(|index| index < outputs)
    }

    #[must_use]
    pub fn flags(self, outputs: usize) -> Vec<bool> {
        (0..outputs)
            .map(|index| self.is_program_output(index))
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ProgramStep {
    #[serde(with = "serde_fr")]
    pub program_id: ProgramId,
    pub payments: Payments,
    // The notes of the program, possibly none.
    pub inputs: InputNullifiers,
    // The notes of the program and the user notes it pays.
    pub outputs: OutputCommitments,
    pub program_outputs: ProgramOutputs,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    #[serde(with = "serde_fr")]
    pub spend_notes_hash: Fr,
}

impl ProgramStep {
    // The outputs of the program, with their index.
    pub fn program_commitments(&self) -> impl Iterator<Item = (usize, &NoteCommitment)> {
        self.outputs
            .iter()
            .enumerate()
            .filter(|(index, _)| self.program_outputs.is_program_output(*index))
    }

    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            program_id: Fr::from(0x34u64),
            payments: [Payment::sample()].into(),
            inputs: [crate::mantle::notes::NoteNullifier(Fr::from(0x3au64))].into(),
            outputs: [NoteCommitment(Fr::from(0x3bu64))].into(),
            program_outputs: ProgramOutputs::from(0b1),
            commitment_merkle_root: Fr::from(0x3cu64),
            spend_notes_hash: Fr::from(0x3du64),
        }
    }
}

impl BinaryEncode for ProgramStep {
    fn encoded_length(&self) -> usize {
        self.program_id.encoded_length()
            + self.payments.encoded_length()
            + self.inputs.encoded_length()
            + self.outputs.encoded_length()
            + self.program_outputs.encoded_length()
            + self.commitment_merkle_root.encoded_length()
            + self.spend_notes_hash.encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.program_id.encode_into(out);
        self.payments.encode_into(out);
        self.inputs.encode_into(out);
        self.outputs.encode_into(out);
        self.program_outputs.encode_into(out);
        self.commitment_merkle_root.encode_into(out);
        self.spend_notes_hash.encode_into(out);
    }
}

impl BinaryDecode for ProgramStep {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, program_id) = ProgramId::decode(input, &())?;
        let (rest, payments) = Payments::decode(rest, &())?;
        let (rest, inputs) = InputNullifiers::decode(rest, &())?;
        let (rest, outputs) = OutputCommitments::decode(rest, &())?;
        let (rest, program_outputs) = ProgramOutputs::decode(rest, &())?;
        let (rest, commitment_merkle_root) = Fr::decode(rest, &())?;
        let (rest, spend_notes_hash) = Fr::decode(rest, &())?;
        // A step has a single encoding.
        if outputs.len() > lb_program_spend::MAX_OUTPUTS {
            return Err(DecodeError::invalid_value::<Self>(
                "a program step has at most 8 outputs",
            ));
        }
        if !program_outputs.fits(outputs.len()) {
            return Err(DecodeError::invalid_value::<Self>(
                "a program output flag is set beyond the outputs",
            ));
        }
        Ok((
            rest,
            Self {
                program_id,
                payments,
                inputs,
                outputs,
                program_outputs,
                commitment_merkle_root,
                spend_notes_hash,
            },
        ))
    }
}

// The message the ZkTransfer of a payment is bound to.
#[must_use]
pub fn payment_msg(program_id: &ProgramId, payment: &Payment) -> Fr {
    let mut elements = Vec::with_capacity(5 + payment.inputs.len() + payment.outputs.len());
    elements.push(*PROGRAM_PAYMENT_V1);
    elements.push(*program_id);
    elements.push(payment.intent);
    elements.push(Fr::from(payment.inputs.len() as u64));
    elements.extend(payment.inputs.iter().map(|note_nullifier| note_nullifier.0));
    elements.push(Fr::from(payment.outputs.len() as u64));
    elements.extend(
        payment
            .outputs
            .iter()
            .map(|note_commitment| note_commitment.0),
    );
    ZkHasher::digest(&elements)
}

// The second public input of the program proof.
#[must_use]
pub fn payments_hash(step: &ProgramStep) -> Fr {
    let mut elements = Vec::with_capacity(2 + 2 * step.payments.len());
    elements.push(*PROGRAM_PAYMENTS_V1);
    elements.push(Fr::from(step.payments.len() as u64));
    for payment in step.payments.iter() {
        elements.push(Fr::from(payment.paid_value));
        elements.push(payment.intent);
    }
    ZkHasher::digest(&elements)
}

// The `op_id` of the inscription cannot serve, since it covers the output
// commitments and thus their nonces. The channel keeps the first inscriptions
// of channels, whose parent is ZERO, apart. No program can enforce this
// derivation.
#[must_use]
pub fn recommended_program_output_nonce(
    channel_id: &ChannelId,
    parent: &MsgId,
    step_index: usize,
    output_index: usize,
    value: Value,
    public_key: &ZkPublicKey,
) -> Fr {
    let anchor: Hash =
        Hasher::digest([channel_id.as_ref().as_slice(), parent.as_ref().as_slice()].concat())
            .into();
    let output_number = step_index
        .checked_mul(8)
        .and_then(|number| number.checked_add(output_index))
        .expect("the output number of a step fits a usize");
    derive_note_nonce(&anchor, output_number, value, public_key)
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils {
    use lb_groth16::Fr;
    use lb_mmr::MerklePath;
    use lb_program_spend::{
        ZkProgramSpendChainInputsData, ZkProgramSpendInputNoteData, ZkProgramSpendOutputNoteData,
        ZkProgramSpendVerifierInput, ZkProgramSpendWalletInputsData,
        ZkProgramSpendWitnessInputsData,
    };

    use crate::mantle::{
        Value,
        notes::{Note, ProgramId},
    };

    // Proves a ZkProgramSpend from note openings and their commitment MMR paths,
    // as `prove_transfer`. Each input is a note, its secret key and its path in the
    // commitment MMR; the flags missing past `outputs_of_program` are false.
    #[must_use]
    pub fn prove_program_spend(
        inputs: &[(Note, Fr, MerklePath)],
        outputs: &[Note],
        outputs_of_program: &[bool],
        tx_commitment_root: Fr,
        program_id: ProgramId,
        paid_value: Value,
        msg: Fr,
    ) -> (
        lb_program_spend::ZkProgramSpendProof,
        ZkProgramSpendVerifierInput,
    ) {
        let wallet = ZkProgramSpendWalletInputsData {
            inputs: core::array::from_fn(|i| {
                inputs.get(i).map_or_else(
                    ZkProgramSpendInputNoteData::inactive,
                    |(note, secret_key, path)| ZkProgramSpendInputNoteData {
                        secret_key: *secret_key,
                        nonce: note.nonce,
                        value: note.value,
                        data: note.data,
                        path_and_selectors: core::array::from_fn(|h| {
                            (path.siblings()[h], (path.leaf_index() >> h) & 1 == 1)
                        }),
                    },
                )
            }),
            outputs: core::array::from_fn(|j| {
                outputs
                    .get(j)
                    .map_or_else(ZkProgramSpendOutputNoteData::inactive, |note| {
                        ZkProgramSpendOutputNoteData {
                            public_key: *note.public_key.as_fr(),
                            nonce: note.nonce,
                            value: note.value,
                            program: note.program,
                            data: note.data,
                        }
                    })
            }),
        };
        let chain = ZkProgramSpendChainInputsData {
            inputs_active: core::array::from_fn(|i| i < inputs.len()),
            outputs_active: core::array::from_fn(|j| j < outputs.len()),
            outputs_of_program: core::array::from_fn(|j| {
                outputs_of_program.get(j).copied().unwrap_or(false)
            }),
            tx_commitment_root,
            program_id,
            paid_value,
            msg,
        };
        lb_program_spend::prove(
            ZkProgramSpendWitnessInputsData::from_chain_and_wallet_data(chain, wallet).into(),
        )
        .expect("the witness proves")
    }
}

#[cfg(test)]
mod tests {
    use lb_groth16::{AdditiveGroup as _, Field as _};

    use super::*;

    #[test]
    fn payment_msg_commits_to_the_program_the_intent_and_the_notes() {
        let dst = fr_from_bytes(b"PROGRAM_PAYMENT_V1").unwrap();
        let payment = Payment::sample(); // nullifier 0x35, commitment 0x36, root 0x37, buffered 0x47, paid 0x38, intent 0x39
        let program = Fr::from(0x34u64);
        assert_eq!(
            payment_msg(&program, &payment),
            ZkHasher::digest(&[
                dst,
                program,
                Fr::from(0x39u64),
                Fr::ONE,
                Fr::from(0x35u64),
                Fr::ONE,
                Fr::from(0x36u64)
            ])
        );
        // the root, the buffered count and the paid value are not part of the message
        let moved = Payment {
            commitment_merkle_root: Fr::ONE,
            buffered: 0,
            paid_value: 1,
            ..payment.clone()
        };
        assert_eq!(
            payment_msg(&program, &moved),
            payment_msg(&program, &payment)
        );
        assert_ne!(
            payment_msg(&Fr::from(0x35u64), &payment),
            payment_msg(&program, &payment)
        );
    }

    #[test]
    fn payments_hash_lists_each_paid_value_and_intent() {
        let dst = fr_from_bytes(b"PROGRAM_PAYMENTS_V1").unwrap();
        let step = ProgramStep::sample();
        assert_eq!(
            payments_hash(&step),
            ZkHasher::digest(&[dst, Fr::ONE, Fr::from(0x38u64), Fr::from(0x39u64)])
        );
        let without = ProgramStep {
            payments: Payments::default(),
            ..step
        };
        assert_eq!(payments_hash(&without), ZkHasher::digest(&[dst, Fr::ZERO]));
    }

    #[test]
    fn program_step_decode_rejects_more_than_eight_outputs_and_flags_beyond_the_outputs() {
        let step = ProgramStep::sample();
        assert_eq!(
            ProgramStep::decode(&step.encode(), &()).unwrap(),
            (&[][..], step.clone())
        );
        let beyond = ProgramStep {
            program_outputs: ProgramOutputs::from(0b10),
            ..step.clone()
        };
        assert!(matches!(
            ProgramStep::decode(&beyond.encode(), &()),
            Err(DecodeError::InvalidValue { .. })
        ));
        let nine = ProgramStep {
            outputs: (1..=9)
                .map(|n| NoteCommitment(Fr::from(n)))
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
            program_outputs: ProgramOutputs::default(),
            ..step
        };
        assert!(matches!(
            ProgramStep::decode(&nine.encode(), &()),
            Err(DecodeError::InvalidValue { .. })
        ));
    }

    #[test]
    fn recommended_nonce_hashes_the_channel_and_parent_and_counts_eight_outputs_per_step() {
        let public_key = ZkPublicKey::new(Fr::from(3u64));
        let anchor: Hash = Hasher::digest([[1u8; 32], [2u8; 32]].concat()).into();
        let nonce = recommended_program_output_nonce(
            &ChannelId::from([1u8; 32]),
            &MsgId::from([2u8; 32]),
            2,
            5,
            40,
            &public_key,
        );
        assert_eq!(nonce, derive_note_nonce(&anchor, 21, 40, &public_key));
        assert_ne!(
            nonce,
            recommended_program_output_nonce(
                &ChannelId::from([3u8; 32]),
                &MsgId::from([2u8; 32]),
                2,
                5,
                40,
                &public_key
            )
        );
    }
}
