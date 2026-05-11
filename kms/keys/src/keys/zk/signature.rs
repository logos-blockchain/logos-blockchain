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
            use generic_array::{GenericArray, typenum::$size};
            use serde::{Deserialize as _, Deserializer, Serializer};

            pub fn serialize<S: Serializer>(
                bytes: &GenericArray<u8, $size>,
                serializer: S,
            ) -> Result<S::Ok, S::Error> {
                if serializer.is_human_readable() {
                    serializer.serialize_str(&hex::encode(&bytes))
                } else {
                    serializer.serialize_bytes(bytes)
                }
            }

            pub fn deserialize<'de, D: Deserializer<'de>>(
                deserializer: D,
            ) -> Result<GenericArray<u8, $size>, D::Error> {
                if deserializer.is_human_readable() {
                    let s = String::deserialize(deserializer)?;
                    Ok(GenericArray::from_iter(
                        hex::decode(s)
                            .map_err(serde::de::Error::custom)?
                            .into_iter(),
                    ))
                } else {
                    GenericArray::<u8, $size>::deserialize(deserializer)
                }
            }
        }
    };
}

declare_serde_generic_array!(serde_generic_array_u32, U32);
declare_serde_generic_array!(serde_generic_array_u64, U64);
