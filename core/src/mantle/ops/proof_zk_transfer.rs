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
        let proof = CompressedGroth16Proof::new(pi_a.into(), pi_b.into(), pi_c.into());
        proof.check_encoding().map_err(serde::de::Error::custom)?;
        Ok(Self(proof))
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
        let proof = CompressedGroth16Proof::try_from_bytes(&bytes)
            .map_err(|error| DecodeError::invalid_value::<Self>(error.to_string()))?;
        Ok((rest, Self(proof)))
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

    /// The modulus of the base field of BN254, little-endian.
    const BASE_FIELD_MODULUS: [u8; 32] = [
        0x47, 0xfd, 0x7c, 0xd8, 0x16, 0x8c, 0x20, 0x3c, 0x8d, 0xca, 0x71, 0x68, 0x91, 0x6a, 0x81,
        0x97, 0x5d, 0x58, 0x81, 0x81, 0xb6, 0x45, 0x50, 0xb8, 0x29, 0xa0, 0x31, 0xe1, 0x72, 0x4e,
        0x64, 0x30,
    ];

    /// `[1]G1`, `[1]G2`, `[1]G1`, real points whose flags are clear.
    fn generator_proof_bytes() -> [u8; 128] {
        hex::decode(concat!(
            "0100000000000000000000000000000000000000000000000000000000000000",
            "edf692d95cbdde46ddda5ef7d422436779445c5e66006a42761e1f12efde0018",
            "c212f3aeb785e49712e7a9353349aaf1255dfb31b7bf60723a480d9293938e19",
            "0100000000000000000000000000000000000000000000000000000000000000",
        ))
        .unwrap()
        .try_into()
        .unwrap()
    }

    fn with_coordinate(start: usize, coordinate: &[u8; 32]) -> [u8; 128] {
        let mut bytes = generator_proof_bytes();
        bytes[start..start + 32].copy_from_slice(coordinate);
        bytes
    }

    fn assert_rejected(bytes: &[u8; 128], case: &str) {
        assert!(
            matches!(
                ZkTransferProof::decode(bytes, &()),
                Err(DecodeError::InvalidValue { .. })
            ),
            "{case}"
        );
        let CompressedGroth16Proof { pi_a, pi_b, pi_c } = CompressedGroth16Proof::from_bytes(bytes);
        assert!(
            serde_json::from_value::<ZkTransferProof>(serde_json::json!({
                "pi_a": hex::encode(pi_a),
                "pi_b": hex::encode(pi_b),
                "pi_c": hex::encode(pi_c),
            }))
            .is_err(),
            "{case}, from its serde form"
        );
    }

    #[test]
    fn decoder_accepts_real_points() {
        let bytes = generator_proof_bytes();
        let (rest, proof) = ZkTransferProof::decode(&bytes, &()).unwrap();
        assert!(rest.is_empty());
        assert_eq!(proof.encode().to_vec(), bytes.to_vec());
    }

    /// Mantle Transaction Encoding: a decoder MUST reject a point with bit 6
    /// set, with or without bit 7.
    #[test]
    fn decoder_rejects_the_infinity_flag_on_every_point() {
        for last_byte in [31, 95, 127] {
            for flags in [0b01 << 6, 0b11 << 6] {
                let mut bytes = generator_proof_bytes();
                bytes[last_byte] |= flags;
                assert_rejected(&bytes, &format!("flags {flags:b} at byte {last_byte}"));
            }
        }
    }

    /// Mantle Transaction Encoding: a decoder MUST also reject a coordinate not
    /// less than the modulus. `pi_b` holds two, and only the second carries the
    /// flags.
    #[test]
    fn decoder_rejects_a_coordinate_not_less_than_the_modulus() {
        for start in [0, 32, 64, 96] {
            assert_rejected(
                &with_coordinate(start, &BASE_FIELD_MODULUS),
                &format!("the modulus at byte {start}"),
            );
            assert_rejected(
                &with_coordinate(start, &[0xFF; 32]),
                &format!("the largest value at byte {start}"),
            );
        }
        // The sign flag is not part of the coordinate, so it cannot lower it below
        // the modulus, nor raise one below the modulus above it.
        for start in [0, 64, 96] {
            let mut flagged = BASE_FIELD_MODULUS;
            flagged[31] |= 1 << 7;
            assert_rejected(
                &with_coordinate(start, &flagged),
                &format!("the flagged modulus at byte {start}"),
            );
        }
        let mut below = BASE_FIELD_MODULUS;
        below[0] -= 1;
        for start in [0, 32, 64, 96] {
            assert!(
                ZkTransferProof::decode(&with_coordinate(start, &below), &()).is_ok(),
                "the largest coordinate at byte {start}"
            );
        }
        let mut flagged_below = below;
        flagged_below[31] |= 1 << 7;
        for start in [0, 64, 96] {
            assert!(
                ZkTransferProof::decode(&with_coordinate(start, &flagged_below), &()).is_ok(),
                "the largest coordinate, flagged, at byte {start}"
            );
        }
    }

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
