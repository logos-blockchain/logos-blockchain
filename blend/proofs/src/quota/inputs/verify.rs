use groth16::fr_from_bytes;
use poq::{PoQVerifierInput, PoQVerifierInputData};

use crate::{
    ZkHash,
    quota::inputs::{prove::PublicInputs, split_ephemeral_signing_key},
};

/// Set of inputs required to verify a Proof of Quota.
///
/// It includes the inputs used to generate the proof (which must be fetched
/// from the verifier's context), and the proof key nullifier, which is part of
/// the Proof of Quota that is included in a Blend header.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    pub prove_inputs: PublicInputs,
    pub key_nullifier: ZkHash,
}

impl Inputs {
    #[must_use]
    pub const fn from_prove_inputs_and_nullifier(
        prove_inputs: PublicInputs,
        key_nullifier: ZkHash,
    ) -> Self {
        Self {
            prove_inputs,
            key_nullifier,
        }
    }
}

impl From<Inputs> for PoQVerifierInput {
    fn from(value: Inputs) -> Self {
        let (signing_key_first_half, signing_key_second_half) =
            split_ephemeral_signing_key(value.prove_inputs.signing_key);
        PoQVerifierInputData {
            core_quota: value.prove_inputs.core.quota,
            core_root: value.prove_inputs.core.zk_root,
            k_part_one: fr_from_bytes(&signing_key_first_half[..])
                .expect("First half of signing public key does not represent a valid `Fr` point."),
            k_part_two: fr_from_bytes(&signing_key_second_half[..])
                .expect("Second half of signing public key does not represent a valid `Fr` point."),
            key_nullifier: value.key_nullifier,
            leader_quota: value.prove_inputs.leader.message_quota,
            pol_epoch_nonce: value.prove_inputs.leader.pol_epoch_nonce,
            pol_ledger_aged: value.prove_inputs.leader.pol_ledger_aged,
            session: value.prove_inputs.session,
            total_stake: value.prove_inputs.leader.total_stake,
        }
        .into()
    }
}
