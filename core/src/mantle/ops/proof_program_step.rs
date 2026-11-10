use lb_binary_codec::canonical::{BinaryCodec, BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof, Fr, serde::serde_fr};
use lb_key_management_system_keys::keys::{PK0, ZkPublicKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::mantle::{
    Value,
    notes::{Note, ProgramId},
    ops::{ZkTransferProof, channel::inscribe::Step},
};

// A compressed Groth16 proof (`Groth16Proof` in the encoding), with the JSON
// and bincode shape of a `ZkTransferProof`.
macro_rules! compressed_proof {
    ($name:ident, $proof:ty) => {
        impl $name {
            #[must_use]
            pub const fn new(proof: $proof) -> Self {
                Self(proof)
            }

            #[must_use]
            pub const fn as_proof(&self) -> &$proof {
                &self.0
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                ZkTransferProof::new(self.0).serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                ZkTransferProof::deserialize(deserializer).map(|proof| Self(*proof.as_proof()))
            }
        }

        impl BinaryEncode for $name {
            fn encoded_length(&self) -> usize {
                COMPRESSED_PROOF_SIZE
            }

            fn encode_into(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.0.to_bytes());
            }
        }

        impl BinaryDecode for $name {
            type Context = ();

            fn decode<'input>(
                input: &'input [u8],
                (): &Self::Context,
            ) -> Result<(&'input [u8], Self), DecodeError> {
                let (rest, bytes) = <[u8; COMPRESSED_PROOF_SIZE]>::decode(input, &())?;
                let proof = CompressedGroth16Proof::try_from_bytes(&bytes)
                    .map_err(|error| DecodeError::invalid_value::<Self>(error.to_string()))?;
                Ok((rest, Self(proof)))
            }
        }

        #[cfg(any(test, feature = "test-utils"))]
        impl crate::mantle::ops::op_proof::samples::SampleProof for $name {
            fn sample() -> Self {
                Self::new(CompressedGroth16Proof::from_bytes(
                    &[0u8; COMPRESSED_PROOF_SIZE],
                ))
            }
        }
    };
}

// The ZkProgramSpend of a program step (`ZkProgramSpend = Groth16Proof`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZkProgramSpendProof(lb_program_spend::ZkProgramSpendProof);
compressed_proof!(ZkProgramSpendProof, lb_program_spend::ZkProgramSpendProof);

// The proof of a program step under the verification key of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProgramProof(CompressedGroth16Proof);
compressed_proof!(ProgramProof, CompressedGroth16Proof);

// Value (8 bytes), NoteNonce, NoteStakePk and ProgramNoteData (32 bytes each).
const PROGRAM_NOTE_OPENING_SIZE: usize = 104;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ProgramNoteOpening {
    pub value: Value,
    #[serde(with = "serde_fr")]
    pub nonce: Fr,
    // The staking key of the note.
    pub stake_public_key: ZkPublicKey,
    #[serde(with = "serde_fr")]
    pub data: Fr,
}

impl ProgramNoteOpening {
    #[must_use]
    pub fn note(&self, program: ProgramId) -> Note {
        Note {
            value: self.value,
            nonce: self.nonce,
            public_key: *PK0,
            stake_public_key: self.stake_public_key,
            program,
            data: self.data,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStepProof {
    pub spend_proof: ZkProgramSpendProof,
    pub program_proof: ProgramProof,
    // The opening of each output of the program, in the order of the outputs.
    pub program_notes: Vec<ProgramNoteOpening>,
}

impl BinaryEncode for ProgramStepProof {
    fn encoded_length(&self) -> usize {
        StepProofShape::ProgramStep {
            program_notes: self.program_notes.len(),
        }
        .encoded_length()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.spend_proof.encode_into(out);
        self.program_proof.encode_into(out);
        for opening in &self.program_notes {
            opening.encode_into(out);
        }
    }
}

impl BinaryDecode for ProgramStepProof {
    // The program outputs of the step.
    type Context = StepProofShape;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let StepProofShape::ProgramStep { program_notes } = *context else {
            return Err(DecodeError::invalid_value::<Self>(
                "a program step proof needs the shape of a program step",
            ));
        };
        let (rest, spend_proof) = ZkProgramSpendProof::decode(input, &())?;
        let (mut rest, program_proof) = ProgramProof::decode(rest, &())?;
        let mut openings = Vec::with_capacity(program_notes);
        for _ in 0..program_notes {
            let (next, opening) = ProgramNoteOpening::decode(rest, &())?;
            openings.push(opening);
            rest = next;
        }
        Ok((
            rest,
            Self {
                spend_proof,
                program_proof,
                program_notes: openings,
            },
        ))
    }
}

// What a step proof holds, given by its step: a ZkTransfer for a user step or
// a program deposit, the proofs and openings of a program step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepProofShape {
    ZkTransfer,
    ProgramStep { program_notes: usize },
}

// The shape of the proof of a step is given by the step: its kind, and for a
// program step the notes of the program it creates.
impl From<&Step> for StepProofShape {
    fn from(step: &Step) -> Self {
        match step {
            Step::User(_) | Step::ProgramDeposit(_) => Self::ZkTransfer,
            Step::Program(step) => Self::ProgramStep {
                program_notes: step.program_outputs.len(),
            },
        }
    }
}

impl StepProofShape {
    #[must_use]
    pub const fn encoded_length(self) -> usize {
        match self {
            Self::ZkTransfer => COMPRESSED_PROOF_SIZE,
            // The program spend and the program proof, then the openings; cannot
            // overflow for 8 openings.
            Self::ProgramStep { program_notes } => {
                let openings = PROGRAM_NOTE_OPENING_SIZE
                    .checked_mul(program_notes)
                    .expect("the openings of a step fit a usize");
                (2 * COMPRESSED_PROOF_SIZE)
                    .checked_add(openings)
                    .expect("the proof of a step fits a usize")
            }
        }
    }
}

// A ZkTransfer for a user step or a program deposit, a `ProgramStepProof` for
// a program step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepProof {
    ZkTransfer(ZkTransferProof),
    ProgramStep(ProgramStepProof),
}

impl StepProof {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample_for(shape: StepProofShape) -> Self {
        use lb_groth16::AdditiveGroup as _;

        use crate::mantle::ops::op_proof::samples::SampleProof as _;

        match shape {
            StepProofShape::ZkTransfer => Self::ZkTransfer(ZkTransferProof::sample()),
            StepProofShape::ProgramStep { program_notes } => Self::ProgramStep(ProgramStepProof {
                spend_proof: ZkProgramSpendProof::sample(),
                program_proof: ProgramProof::sample(),
                program_notes: vec![
                    ProgramNoteOpening {
                        value: 0,
                        nonce: Fr::ZERO,
                        stake_public_key: ZkPublicKey::zero(),
                        data: Fr::ZERO,
                    };
                    program_notes
                ],
            }),
        }
    }
}

// No kind byte: the step gives it.
impl BinaryEncode for StepProof {
    fn encoded_length(&self) -> usize {
        match self {
            Self::ZkTransfer(proof) => proof.encoded_length(),
            Self::ProgramStep(proof) => proof.encoded_length(),
        }
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        match self {
            Self::ZkTransfer(proof) => proof.encode_into(out),
            Self::ProgramStep(proof) => proof.encode_into(out),
        }
    }
}

impl BinaryDecode for StepProof {
    type Context = StepProofShape;

    fn decode<'input>(
        input: &'input [u8],
        context: &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        match context {
            StepProofShape::ZkTransfer => ZkTransferProof::decode(input, &())
                .map(|(rest, proof)| (rest, Self::ZkTransfer(proof))),
            StepProofShape::ProgramStep { .. } => ProgramStepProof::decode(input, context)
                .map(|(rest, proof)| (rest, Self::ProgramStep(proof))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_step_proof_reads_its_shape_from_the_step() {
        let proof = StepProof::sample_for(StepProofShape::ProgramStep { program_notes: 1 });
        let bytes = proof.encode();
        assert_eq!(bytes.len(), 128 + 128 + 104);
        assert_eq!(
            StepProofShape::ProgramStep { program_notes: 1 }.encoded_length(),
            bytes.len()
        );
        assert_eq!(
            StepProof::decode(&bytes, &StepProofShape::ProgramStep { program_notes: 1 }).unwrap(),
            (&[][..], proof)
        );
        assert_eq!(StepProofShape::ZkTransfer.encoded_length(), 128);
        assert!(
            StepProof::decode(&bytes, &StepProofShape::ProgramStep { program_notes: 2 }).is_err()
        );
    }
}
