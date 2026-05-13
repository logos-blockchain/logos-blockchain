use generic_array::{
    GenericArray,
    typenum::{U32, U64},
};
use lb_zksign::ZkSignProof;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(remote = "lb_zksign::ZkSignProof")]
struct SignatureSerde {
    #[serde(with = "serde_generic_array_u32")]
    pi_a: GenericArray<u8, U32>,
    #[serde(with = "serde_generic_array_u64")]
    pi_b: GenericArray<u8, U64>,
    #[serde(with = "serde_generic_array_u32")]
    pi_c: GenericArray<u8, U32>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Signature(#[serde(with = "SignatureSerde")] ZkSignProof);

impl Signature {
    #[must_use]
    pub const fn new(proof: ZkSignProof) -> Self {
        Self(proof)
    }

    #[must_use]
    pub const fn as_proof(&self) -> &ZkSignProof {
        &self.0
    }
}

macro_rules! declare_serde_generic_array {
    ($mod_name:ident, $size:ident) => {
        pub mod $mod_name {
            use generic_array::{
                GenericArray,
                typenum::{Unsigned, $size},
            };
            use serde::{Deserialize as _, Deserializer, Serializer};

            pub fn serialize<S: Serializer>(
                bytes: &GenericArray<u8, $size>,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                if serializer.is_human_readable() {
                    serializer.serialize_str(&hex::encode(bytes))
                } else {
                    serializer.serialize_bytes(bytes)
                }
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(
                deserializer: D,
            ) -> Result<GenericArray<u8, $size>, D::Error> {
                if deserializer.is_human_readable() {
                    #[derive(serde::Deserialize)]
                    #[serde(untagged)]
                    enum StringOrSeq {
                        Hex(String),
                        Seq(Vec<u8>),
                    }

                    let bytes = match StringOrSeq::deserialize(deserializer)? {
                        StringOrSeq::Hex(s) => hex::decode(&s).map_err(serde::de::Error::custom)?,
                        StringOrSeq::Seq(b) => b,
                    };

                    if bytes.len() != $size::USIZE {
                        return Err(serde::de::Error::custom(format!(
                            "expected {} bytes, got {}",
                            $size::USIZE,
                            bytes.len()
                        )));
                    }

                    GenericArray::try_from_iter(bytes)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))
                } else {
                    let bytes = <Vec<u8>>::deserialize(deserializer)?;

                    if bytes.len() != $size::USIZE {
                        return Err(serde::de::Error::custom(format!(
                            "expected {} bytes, got {}",
                            $size::USIZE,
                            bytes.len()
                        )));
                    }

                    GenericArray::try_from_iter(bytes)
                        .map_err(|e| serde::de::Error::custom(e.to_string()))
                }
            }
        }
    };
}

declare_serde_generic_array!(serde_generic_array_u32, U32);
declare_serde_generic_array!(serde_generic_array_u64, U64);

#[cfg(test)]
mod tests {
    use lb_groth16::Fr;
    use lb_poseidon2::{Digest as _, Poseidon2Bn254Hasher};
    use lb_zksign::{ZkSignPrivateKeysData, ZkSignWitnessInputs, prove, verify};
    use num_bigint::BigUint;
    use rand_core::RngCore as _;

    use crate::keys::zk::Signature;

    #[test]
    fn signature_rejects_wrong_pi_a_hex_length_json() {
        let json = r#"
        {
            "pi_a": "00",
            "pi_b": "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "pi_c": "0000000000000000000000000000000000000000000000000000000000000000"
        }
        "#;

        let err = serde_json::from_str::<Signature>(json).unwrap_err();
        assert!(err.to_string().contains("expected 32 bytes"));
    }

    #[test]
    fn signature_rejects_wrong_pi_b_hex_length_yaml() {
        let yaml = r#"
            pi_a: "0000000000000000000000000000000000000000000000000000000000000000"
            pi_b: "00"
            pi_c: "0000000000000000000000000000000000000000000000000000000000000000"
            "#;

        let err = serde_yaml::from_str::<Signature>(yaml).unwrap_err();
        assert!(err.to_string().contains("expected 64 bytes"));
    }

    #[test]
    fn zk_signature_json_roundtrip() {
        let sig = sig_generator();

        let encoded = serde_json::to_string(&sig).unwrap();
        let decoded: Signature = serde_json::from_str(&encoded).unwrap();

        assert_eq!(sig, decoded);
    }

    #[test]
    fn zk_signature_yaml_roundtrip() {
        let sig = sig_generator();

        let encoded = serde_yaml::to_string(&sig).unwrap();
        let decoded: Signature = serde_yaml::from_str(&encoded).unwrap();

        assert_eq!(sig, decoded);
    }

    fn sig_generator() -> Signature {
        let mut rng = rand::thread_rng();
        let sks: [Fr; 32] = std::iter::repeat_with(|| BigUint::from(rng.next_u64()).into())
            .take(32)
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let sks: ZkSignPrivateKeysData = sks.into();
        let msg_hash = Poseidon2Bn254Hasher::digest(&[BigUint::from_bytes_le(b"foo_bar").into()]);
        let input = ZkSignWitnessInputs::from_witness_data_and_message_hash(sks, msg_hash);
        let (proof, verifier_inputs) = prove(&input).unwrap();
        assert!(verify(&proof, &verifier_inputs).unwrap());
        Signature::new(proof)
    }
}
