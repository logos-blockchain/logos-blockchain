use std::sync::LazyLock;

use ark_ff::{AdditiveGroup as _, PrimeField as _};
use lb_binary_codec::canonical::BinaryCodec;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_key_management_system_keys::keys::ZkPublicKey;
use lb_poseidon2::Digest as _;
use lb_utils::bounded::UpperBoundedVec;
use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Hash, ZkHasher},
    mantle::ledger::Value,
};

pub type ProgramId = Fr;

static NOTE_CM_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NOTE_CM_V1").expect("BigUint should load from constant string")
});

static NOTE_NF_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NOTE_NF_V1").expect("BigUint should load from constant string")
});

static NOTE_NONCE_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"NOTE_NONCE_V1").expect("BigUint should load from constant string")
});

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, BinaryCodec,
)]
#[serde(transparent)]
pub struct NoteCommitment(#[serde(with = "serde_fr")] pub Fr);

impl AsRef<Fr> for NoteCommitment {
    fn as_ref(&self) -> &Fr {
        &self.0
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, BinaryCodec,
)]
#[serde(transparent)]
pub struct NoteNullifier(#[serde(with = "serde_fr")] pub Fr);

impl AsRef<Fr> for NoteNullifier {
    fn as_ref(&self) -> &Fr {
        &self.0
    }
}

// The count of `Inputs` and `Outputs` is a byte in the encoding.
pub const MAX_NOTE_LIST_LEN: usize = u8::MAX as usize;
pub type InputNullifiers = UpperBoundedVec<NoteNullifier, MAX_NOTE_LIST_LEN>;
pub type OutputCommitments = UpperBoundedVec<NoteCommitment, MAX_NOTE_LIST_LEN>;

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
pub struct Note {
    pub value: Value,
    #[serde(with = "serde_fr")]
    pub nonce: Fr,
    pub public_key: ZkPublicKey,
    // The staking key, whose secret the Proof of Leadership takes.
    pub stake_public_key: ZkPublicKey,
    #[serde(with = "serde_fr")]
    pub program: ProgramId,
    #[serde(with = "serde_fr")]
    pub data: Fr,
}

impl Note {
    // A user note under one key, its public key being also its staking key: no
    // program governs it and it carries no data. It is the default of a wallet
    // and of a transfer output, and the leader reward note, the Genesis notes,
    // the service notes, the service reward notes and the proof of work claim
    // notes have their one key as both.
    #[must_use]
    pub const fn new(value: Value, nonce: Fr, public_key: ZkPublicKey) -> Self {
        Self {
            value,
            nonce,
            public_key,
            stake_public_key: public_key,
            program: Fr::ZERO,
            data: Fr::ZERO,
        }
    }

    #[must_use]
    pub fn commitment(&self) -> NoteCommitment {
        // constants and structure as defined in the Mantle spec:
        // https://lip.logos.co/blockchain/raw/bedrock-v1.1-mantle-specification.html
        NoteCommitment(ZkHasher::digest(&[
            *NOTE_CM_V1,
            Fr::from(self.value),
            self.nonce,
            *self.public_key.as_fr(),
            *self.stake_public_key.as_fr(),
            self.program,
            self.data,
        ]))
    }
}

// The nullifier of a note, from the nullifier key of its holder.
#[must_use]
pub fn derive_note_nullifier(commitment: &NoteCommitment, nullifier_key: &Fr) -> NoteNullifier {
    NoteNullifier(ZkHasher::digest(&[
        *NOTE_NF_V1,
        commitment.0,
        *nullifier_key,
    ]))
}

/// The nonce the `ZkProgramSpend` enforces on the output `output_index` of a
/// step consuming a note of its program, derived from the nullifier of its
/// first input.
#[must_use]
pub fn derive_program_output_nonce(
    first_input: &NoteNullifier,
    output_index: usize,
    value: Value,
    public_key: &ZkPublicKey,
) -> Fr {
    ZkHasher::digest(&[
        *NOTE_NONCE_V1,
        first_input.0,
        Fr::from(output_index as u64),
        Fr::from(value),
        *public_key.as_fr(),
    ])
}

#[must_use]
pub fn derive_note_nonce(
    op_id: &Hash,
    output_number: usize,
    value: Value,
    public_key: &ZkPublicKey,
) -> Fr {
    ZkHasher::digest(&[
        *NOTE_NONCE_V1,
        Fr::from_le_bytes_mod_order(op_id),
        Fr::from(output_number as u64),
        Fr::from(value),
        *public_key.as_fr(),
    ])
}

#[cfg(test)]
mod test {
    use lb_groth16::fr_from_bytes_unchecked;
    use lb_key_management_system_keys::keys::ZkKey;

    use super::*;

    #[test]
    fn test_user_note_commitment_has_its_key_as_staking_key_and_zero_program_and_data() {
        let note = Note::new(100, Fr::from(7u64), ZkPublicKey::from(Fr::from(456u64)));
        assert_eq!(note.stake_public_key, note.public_key);
        assert_eq!(
            note.commitment().0,
            ZkHasher::digest(&[
                fr_from_bytes_unchecked(b"NOTE_CM_V1"),
                Fr::from(100u64),
                Fr::from(7u64),
                Fr::from(456u64),
                Fr::from(456u64),
                Fr::ZERO,
                Fr::ZERO
            ])
        );
        // the staking key is part of the commitment
        let restaked = Note {
            stake_public_key: ZkPublicKey::from(Fr::from(789u64)),
            ..note
        };
        assert_ne!(restaked.commitment(), note.commitment());
    }

    #[test]
    fn test_zktransfer_outputs_match_note_derivations() {
        let data = lb_zktransfer::test_utils::sample_witness_data();
        let (_, public) = lb_zktransfer::prove(data.clone().into()).unwrap();
        for (index, input) in data.wallet.inputs.iter().enumerate() {
            let expected = if data.chain.inputs_active[index] {
                let key = ZkKey::new(input.secret_key);
                let commitment = Note {
                    stake_public_key: ZkPublicKey::new(input.stake_public_key),
                    ..Note::new(input.value, input.nonce, key.to_public_key())
                }
                .commitment();
                derive_note_nullifier(&commitment, &key.nullifier_key()).0
            } else {
                Fr::ZERO
            };
            assert_eq!(public.inputs[index].into_inner(), expected);
        }
        for (index, output) in data.wallet.outputs.iter().enumerate() {
            let expected = if data.chain.outputs_active[index] {
                Note {
                    stake_public_key: ZkPublicKey::new(output.stake_public_key),
                    ..Note::new(
                        output.value,
                        output.nonce,
                        ZkPublicKey::new(output.public_key),
                    )
                }
                .commitment()
                .0
            } else {
                Fr::ZERO
            };
            assert_eq!(public.outputs[index].into_inner(), expected);
        }
        assert_eq!(public.excess_value.into_inner(), Fr::from(50u64));
    }

    #[test]
    fn test_mantle_note_is_the_private_note() {
        use crate::mantle::Note as MantleNote;

        let note = MantleNote::new(100, Fr::from(7u64), ZkPublicKey::from(Fr::from(456u64)));
        assert_eq!(
            note.commitment(),
            Note::new(100, Fr::from(7u64), ZkPublicKey::from(Fr::from(456u64))).commitment()
        );
    }
}
