use lb_blend_proofs::{quota::ProofOfQuota, selection::ProofOfSelection};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use lb_wire::{DecodeError, WireDecode, WireEncode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ActivityProof {
    pub epoch: Epoch,
    pub signing_key: Ed25519PublicKey,
    pub proof_of_quota: ProofOfQuota,
    pub proof_of_selection: ProofOfSelection,
}

const BLEND_ACTIVE_METADATA_VERSION_BYTE: u8 = 1;

impl WireEncode for ActivityProof {
    fn encoded_length(&self) -> usize {
        BLEND_ACTIVE_METADATA_VERSION_BYTE
            .encoded_length()
            .checked_add(self.epoch.encoded_length())
            .and_then(|len| len.checked_add(self.signing_key.encoded_length()))
            .and_then(|len| len.checked_add(self.proof_of_quota.encoded_length()))
            .and_then(|len| len.checked_add(self.proof_of_selection.encoded_length()))
            .unwrap()
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        BLEND_ACTIVE_METADATA_VERSION_BYTE.encode_into(out);
        self.epoch.encode_into(out);
        self.signing_key.encode_into(out);
        self.proof_of_quota.encode_into(out);
        self.proof_of_selection.encode_into(out);
    }
}

impl WireDecode for ActivityProof {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (input, proof_version) = u8::decode(input, &())?;
        if proof_version != BLEND_ACTIVE_METADATA_VERSION_BYTE {
            return Err(DecodeError::invalid_value::<Self>(
                "Unsupported activity proof version",
            ));
        }
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
    use lb_blend_proofs::{
        quota::{ProofOfQuota, VerifiedProofOfQuota},
        selection::{ProofOfSelection, VerifiedProofOfSelection},
    };
    use lb_key_management_system_keys::keys::{Ed25519Key, Ed25519PublicKey};
    use lb_wire::{DecodeError, WireDecodeExt as _, WireEncode as _};

    use crate::sdp::blend::{ActivityProof, BLEND_ACTIVE_METADATA_VERSION_BYTE};

    #[test]
    fn activity_proof_invalid_version() {
        let proof = ActivityProof {
            epoch: 10.into(),
            signing_key: new_signing_key(0),
            proof_of_quota: new_proof_of_quota_unchecked(0),
            proof_of_selection: new_proof_of_selection_unchecked(1),
        };
        let mut bytes = proof.encode_to_vec();
        bytes[0] = 0x99; // Invalid version

        let err = ActivityProof::decode(&bytes).unwrap_err();
        assert!(matches!(err, DecodeError::InvalidValue { .. }));
    }

    #[test]
    fn activity_proof_too_short() {
        let bytes = vec![BLEND_ACTIVE_METADATA_VERSION_BYTE, 0x01, 0x02]; // Only 3 bytes

        let err = ActivityProof::decode(&bytes).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
    }

    fn new_signing_key(byte: u8) -> Ed25519PublicKey {
        Ed25519Key::from_bytes(&[byte; _]).public_key()
    }

    fn new_proof_of_quota_unchecked(byte: u8) -> ProofOfQuota {
        VerifiedProofOfQuota::from_bytes_unchecked([byte; _]).into()
    }

    fn new_proof_of_selection_unchecked(byte: u8) -> ProofOfSelection {
        VerifiedProofOfSelection::from_bytes_unchecked([byte; _]).into()
    }
}
