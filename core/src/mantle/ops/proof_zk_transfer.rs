use lb_binary_codec::canonical::{BinaryDecode, BinaryEncode, DecodeError};
use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// The ZkTransfer of an Operation, a compressed Groth16 proof (`ZkTransfer =
// Groth16` in the encoding).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZkTransferProof(lb_zktransfer::ZkTransferProof);

impl ZkTransferProof {
    #[must_use]
    pub const fn new(proof: lb_zktransfer::ZkTransferProof) -> Self {
        Self(proof)
    }

    #[must_use]
    pub const fn as_proof(&self) -> &lb_zktransfer::ZkTransferProof {
        &self.0
    }
}

// Serialized as `{pi_a, pi_b, pi_c}`, each a hex string when
// human readable, a fixed size tuple otherwise (128 bytes in bincode).
#[derive(Serialize, Deserialize)]
struct ProofSerde {
    #[serde(with = "bytes_array")]
    pi_a: [u8; 32],
    #[serde(with = "bytes_array")]
    pi_b: [u8; 64],
    #[serde(with = "bytes_array")]
    pi_c: [u8; 32],
}

mod bytes_array {
    use serde::{Deserializer, Serializer};

    pub fn serialize<const N: usize, S: Serializer>(
        bytes: &[u8; N],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        lb_utils::serde::serialize_bytes_array(*bytes, serializer)
    }

    pub fn deserialize<'de, const N: usize, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; N], D::Error> {
        lb_utils::serde::deserialize_bytes_array_or_seq::<N, D>(deserializer)
    }
}

impl Serialize for ZkTransferProof {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let CompressedGroth16Proof { pi_a, pi_b, pi_c } = self.0;
        ProofSerde {
            pi_a: pi_a.into(),
            pi_b: pi_b.into(),
            pi_c: pi_c.into(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ZkTransferProof {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let ProofSerde { pi_a, pi_b, pi_c } = ProofSerde::deserialize(deserializer)?;
        Ok(Self(CompressedGroth16Proof::new(
            pi_a.into(),
            pi_b.into(),
            pi_c.into(),
        )))
    }
}

impl BinaryEncode for ZkTransferProof {
    fn encoded_length(&self) -> usize {
        COMPRESSED_PROOF_SIZE
    }

    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.0.to_bytes());
    }
}

impl BinaryDecode for ZkTransferProof {
    type Context = ();

    fn decode<'input>(
        input: &'input [u8],
        (): &Self::Context,
    ) -> Result<(&'input [u8], Self), DecodeError> {
        let (rest, bytes) = <[u8; COMPRESSED_PROOF_SIZE]>::decode(input, &())?;
        Ok((rest, Self(CompressedGroth16Proof::from_bytes(&bytes))))
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod samples {
    use lb_groth16::{COMPRESSED_PROOF_SIZE, CompressedGroth16Proof};

    use crate::mantle::ops::{ZkTransferProof, op_proof::samples::SampleProof};

    impl SampleProof for ZkTransferProof {
        fn sample() -> Self {
            Self::new(CompressedGroth16Proof::from_bytes(
                &[0u8; COMPRESSED_PROOF_SIZE],
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zk_transfer_proof_is_its_128_proof_bytes() {
        let proof = ZkTransferProof::new(CompressedGroth16Proof::from_bytes(&[1u8; 128]));

        assert_eq!(proof.encode().to_vec(), vec![1u8; 128]);
        assert_eq!(
            ZkTransferProof::decode(&[1u8; 129], &()).unwrap(),
            (&[1u8][..], proof)
        );
        assert!(ZkTransferProof::decode(&[1u8; 127], &()).is_err());

        let json = serde_json::to_value(proof).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "pi_a": "01".repeat(32),
                "pi_b": "01".repeat(64),
                "pi_c": "01".repeat(32),
            })
        );
        assert_eq!(
            serde_json::from_value::<ZkTransferProof>(json).unwrap(),
            proof
        );

        let bytes = bincode::serialize(&proof).unwrap();
        assert_eq!(bytes, vec![1u8; 128]);
        assert_eq!(
            bincode::deserialize::<ZkTransferProof>(&bytes).unwrap(),
            proof
        );
    }
}
