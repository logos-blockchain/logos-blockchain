use lb_blend_proofs::{quota::ProofOfQuota, selection::ProofOfSelection};
use lb_cryptarchia_engine::Epoch;
use lb_key_management_system_keys::keys::Ed25519PublicKey;
use nom::{IResult, Parser as _, combinator::map, error::Error};

use crate::{
    mantle::nom::{NomArray, NomDecode, NomEncode},
    sdp::{ActivityMetadata, DeclarationId, blend},
};

pub mod active;
pub mod declare;
pub mod withdraw;

impl NomEncode for DeclarationId {
    fn encode(&self) -> Vec<u8> {
        NomArray::<u8, 32>::from(&self.0).encode()
    }
}

impl NomDecode for DeclarationId {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        map(NomArray::<u8, 32>::decode, Self).parse(bytes)
    }
}

const ACTIVE_METADATA_BLEND_TYPE: u8 = 1;

impl NomEncode for ActivityMetadata {
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::Blend(blend_activity_proof) => {
                let mut bytes = vec![ACTIVE_METADATA_BLEND_TYPE];
                bytes.extend(blend_activity_proof.encode());
                bytes
            }
        }
    }
}

impl NomDecode for ActivityMetadata {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (bytes, metadata_type) = u8::decode(bytes)?;
        match metadata_type {
            ACTIVE_METADATA_BLEND_TYPE => {
                let (bytes, blend_activity_proof) = blend::ActivityProof::decode(bytes)?;
                Ok((bytes, Self::Blend(Box::new(blend_activity_proof))))
            }
            _ => Err(nom::Err::Error(Error::new(
                bytes,
                nom::error::ErrorKind::Fail,
            ))),
        }
    }
}

const BLEND_ACTIVE_METADATA_VERSION_BYTE: u8 = 1;

impl NomEncode for blend::ActivityProof {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![BLEND_ACTIVE_METADATA_VERSION_BYTE];
        bytes.extend(self.epoch.encode());
        bytes.extend(self.signing_key.encode());
        bytes.extend(self.proof_of_quota.encode());
        bytes.extend(self.proof_of_selection.encode());
        bytes
    }
}

impl NomDecode for blend::ActivityProof {
    type Output = Self;

    fn decode(bytes: &[u8]) -> IResult<&[u8], Self::Output> {
        let (bytes, proof_version) = u8::decode(bytes)?;
        if proof_version != BLEND_ACTIVE_METADATA_VERSION_BYTE {
            return Err(nom::Err::Error(Error::new(
                bytes,
                nom::error::ErrorKind::Fail,
            )));
        }
        let (bytes, epoch) = Epoch::decode(bytes)?;
        let (bytes, signing_key) = Ed25519PublicKey::decode(bytes)?;
        let (bytes, proof_of_quota) = ProofOfQuota::decode(bytes)?;
        let (bytes, proof_of_selection) = ProofOfSelection::decode(bytes)?;
        Ok((
            bytes,
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

    use crate::{
        mantle::nom::{NomDecode as _, NomEncode as _, sdp::BLEND_ACTIVE_METADATA_VERSION_BYTE},
        sdp::{ActivityMetadata, blend::ActivityProof},
    };

    #[test]
    fn activity_proof_roundtrip() {
        let proof = ActivityProof {
            epoch: 10.into(),
            signing_key: new_signing_key(0),
            proof_of_quota: new_proof_of_quota_unchecked(0),
            proof_of_selection: new_proof_of_selection_unchecked(1),
        };

        let bytes = proof.encode();
        let (_, decoded) = ActivityProof::decode(&bytes).unwrap();

        assert_eq!(proof, decoded);
    }

    #[test]
    fn activity_proof_invalid_version() {
        let proof = ActivityProof {
            epoch: 10.into(),
            signing_key: new_signing_key(0),
            proof_of_quota: new_proof_of_quota_unchecked(0),
            proof_of_selection: new_proof_of_selection_unchecked(1),
        };
        let mut bytes = proof.encode();
        bytes[0] = 0x99; // Invalid version

        let result = ActivityProof::decode(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Parsing Error"));
    }

    #[test]
    fn activity_proof_too_short() {
        let bytes = vec![BLEND_ACTIVE_METADATA_VERSION_BYTE, 0x01, 0x02]; // Only 3 bytes

        let result = ActivityProof::decode(&bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Eof"));
    }

    #[test]
    fn activity_metadata_roundtrip() {
        let proof = ActivityProof {
            epoch: 10.into(),
            signing_key: new_signing_key(0),
            proof_of_quota: new_proof_of_quota_unchecked(0),
            proof_of_selection: new_proof_of_selection_unchecked(1),
        };
        let metadata = ActivityMetadata::Blend(Box::new(proof.clone()));

        let bytes = metadata.encode();
        let (_, decoded) = ActivityMetadata::decode(&bytes).unwrap();

        assert_eq!(metadata, decoded);

        let ActivityMetadata::Blend(decoded_proof) = decoded;
        assert_eq!(proof, *decoded_proof);
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
