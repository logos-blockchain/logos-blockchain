use std::sync::LazyLock;

use lb_binary_codec::canonical::BinaryCodec;
use lb_groth16::{Fr, fr_from_bytes, serde::serde_fr};
use lb_poseidon2::Digest as _;
use serde::{Deserialize, Serialize};

use super::{
    ChannelId,
    inscribe::{Inscription, PROGRAM_DEPOSIT_KIND, serde_inscription},
    step_id,
};
use crate::{
    crypto::ZkHasher,
    mantle::{
        Value,
        notes::{InputNullifiers, OutputCommitments, ProgramId},
    },
};

static PROGRAM_DEPOSITS_V1: LazyLock<Fr> = LazyLock::new(|| {
    fr_from_bytes(b"PROGRAM_DEPOSITS_V1").expect("BigUint should load from constant string")
});

// A step paying a program from the user notes of its holder, consumed by the
// next program step of its mini-block.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ProgramDeposit {
    #[serde(with = "serde_fr")]
    pub program_id: ProgramId,
    pub inputs: InputNullifiers,
    // The change, user notes of the channel.
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    // The excess of the ZkTransfer, paid to the program.
    pub value: Value,
    #[serde(with = "serde_fr")]
    pub intent: Fr,
    // The zone message of the program deposit, signed by its holder through
    // `program_deposit_id`.
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
}

impl ProgramDeposit {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            program_id: Fr::from(0x34u64),
            inputs: [crate::mantle::notes::NoteNullifier(Fr::from(0x35u64))].into(),
            outputs: [crate::mantle::notes::NoteCommitment(Fr::from(0x36u64))].into(),
            commitment_merkle_root: Fr::from(0x37u64),
            value: 0x38,
            intent: Fr::from(0x39u64),
            inscription: b"deposit-inscription".into(),
        }
    }
}

// A step executing a program: it consumes the program deposits after the
// previous program step of its mini-block.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, BinaryCodec)]
pub struct ProgramStep {
    #[serde(with = "serde_fr")]
    pub program_id: ProgramId,
    // The notes of the program, possibly none.
    pub inputs: InputNullifiers,
    // The notes of the program the step creates.
    pub program_outputs: OutputCommitments,
    // The user notes the step pays.
    pub outputs: OutputCommitments,
    #[serde(with = "serde_fr")]
    pub commitment_merkle_root: Fr,
    #[serde(with = "serde_fr")]
    pub program_step_id: Fr,
    // The zone message of the step, bound by the `op_id` like the rest of the
    // step.
    #[serde(with = "serde_inscription")]
    pub inscription: Inscription,
}

impl ProgramStep {
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn sample() -> Self {
        Self {
            program_id: Fr::from(0x34u64),
            inputs: [crate::mantle::notes::NoteNullifier(Fr::from(0x3au64))].into(),
            program_outputs: [crate::mantle::notes::NoteCommitment(Fr::from(0x3bu64))].into(),
            outputs: OutputCommitments::default(),
            commitment_merkle_root: Fr::from(0x3cu64),
            program_step_id: Fr::from(0x3du64),
            inscription: b"program-step-inscription".into(),
        }
    }
}

// The message the ZkTransfer of a program deposit is bound to.
#[must_use]
pub fn program_deposit_id(channel_id: &ChannelId, deposit: &ProgramDeposit) -> Fr {
    step_id(
        b"PROGRAM_DEPOSIT_V1",
        channel_id,
        PROGRAM_DEPOSIT_KIND,
        deposit,
    )
}

// The second public input of the program proof: the program deposits a program
// step consumes, in their order in the mini-block.
#[must_use]
pub fn program_deposits_hash(deposits: &[&ProgramDeposit]) -> Fr {
    let mut elements = Vec::with_capacity(2 + 2 * deposits.len());
    elements.push(*PROGRAM_DEPOSITS_V1);
    elements.push(Fr::from(deposits.len() as u64));
    for deposit in deposits {
        elements.push(Fr::from(deposit.value));
        elements.push(deposit.intent);
    }
    ZkHasher::digest(&elements)
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
                        stake_public_key: *note.stake_public_key.as_fr(),
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
                            stake_public_key: *note.stake_public_key.as_fr(),
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
    use ark_ff::PrimeField as _;
    use lb_binary_codec::canonical::BinaryEncode as _;
    use lb_groth16::{AdditiveGroup as _, Field as _};

    use super::*;
    use crate::crypto::{Digest as _, Hasher};

    #[test]
    fn program_deposit_id_hashes_the_channel_and_the_encoded_program_deposit() {
        let deposit = ProgramDeposit::sample();
        let channel = ChannelId::from([0x33u8; 32]);
        let mut preimage = b"PROGRAM_DEPOSIT_V1".to_vec();
        preimage.extend_from_slice(&[0x33u8; 32]);
        preimage.push(0x01);
        preimage.extend(deposit.encode());
        let digest: [u8; 32] = Hasher::digest(&preimage).into();
        assert_eq!(
            program_deposit_id(&channel, &deposit),
            Fr::from_le_bytes_mod_order(&digest)
        );
        // every field of the program deposit is covered, its value included
        let other_value = ProgramDeposit {
            value: 1,
            ..deposit.clone()
        };
        assert_ne!(
            program_deposit_id(&channel, &other_value),
            program_deposit_id(&channel, &deposit)
        );
    }

    #[test]
    fn program_deposits_hash_lists_each_value_and_intent() {
        let dst = fr_from_bytes(b"PROGRAM_DEPOSITS_V1").unwrap();
        let deposit = ProgramDeposit::sample();
        assert_eq!(
            program_deposits_hash(&[&deposit]),
            ZkHasher::digest(&[dst, Fr::ONE, Fr::from(0x38u64), Fr::from(0x39u64)])
        );
        assert_eq!(
            program_deposits_hash(&[]),
            ZkHasher::digest(&[dst, Fr::ZERO])
        );
    }
}
