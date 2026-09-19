use lb_blend_proofs::{quota::ProofOfQuota, selection::ProofOfSelection};
use lb_codec::{BinaryDecode, BinaryEncode, DecodeError};
use lb_cryptarchia_engine::{Epoch, Slot};
use lb_groth16::Fr;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use serde::{Deserialize, Serialize};

use crate::header::HeaderId;

/// Chain-derived state shared by the Chain Leader and Blend `PoL` APIs.
pub struct PolEpochState {
    pub nonce: Fr,
    pub aged_utxo_root: Fr,
    pub lottery_0: Fr,
    pub lottery_1: Fr,
    pub source: PolEpochStateSource,
}

/// Tip/LIB provenance for shared chain-derived `PoL` state.
pub struct PolEpochStateSource {
    pub tip_id: HeaderId,
    pub tip_slot: Slot,
    pub lib_id: HeaderId,
    pub lib_slot: Slot,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ActivityProof {
    pub epoch: Epoch,
    pub signing_key: Ed25519PublicKey,
    pub proof_of_quota: ProofOfQuota,
    pub proof_of_selection: ProofOfSelection,
}

impl BinaryEncode for ActivityProof {
    fn encoded_length(&self) -> usize {
        self.epoch
            .encoded_length()
            .checked_add(self.signing_key.encoded_length())
            .and_then(|len| len.checked_add(self.proof_of_quota.encoded_length()))
            .and_then(|len| len.checked_add(self.proof_of_selection.encoded_length()))
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        self.epoch.encode_into(out);
        self.signing_key.encode_into(out);
        self.proof_of_quota.encode_into(out);
        self.proof_of_selection.encode_into(out);
    }
}

impl BinaryDecode for ActivityProof {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, epoch) = Epoch::decode(input, &())?;
        let (input, signing_key) = Ed25519PublicKey::decode(input, &())?;
        let (input, proof_of_quota) = ProofOfQuota::decode(input, &())?;
        let (input, proof_of_selection) = ProofOfSelection::decode(input, &())?;
        Ok((
            input,
            Self {
                epoch,
                signing_key,
                proof_of_quota,
                proof_of_selection,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use lb_codec::{BinaryDecodeExt as _, DecodeError};

    use crate::sdp::blend::ActivityProof;

    #[test]
    fn activity_proof_too_short() {
        let bytes = vec![0x00, 0x01, 0x02]; // Only 3 bytes

        let err = ActivityProof::decode(&bytes).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }
}
