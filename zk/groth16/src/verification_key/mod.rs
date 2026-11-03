pub mod deserialize;
use core::fmt::{self, Debug, Formatter};

use ark_ec::{AffineRepr, pairing::Pairing};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize as _, SerializationError};
pub use deserialize::VerificationKeyJsonDeser;
use generic_array::{GenericArray, typenum::Unsigned as _};

use crate::{
    from_json_error::FromJsonError,
    proof::CompressSize,
    protocol::Protocol,
    utils::{StringifiedG1, StringifiedG2},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerificationKey<E: Pairing> {
    pub alpha_1: E::G1Affine,
    pub beta_2: E::G2Affine,
    pub gamma_2: E::G2Affine,
    pub delta_2: E::G2Affine,
    pub ic: Vec<E::G1Affine>,
}
impl TryFrom<VerificationKeyJsonDeser> for VerificationKey<ark_bn254::Bn254> {
    type Error = FromJsonError;
    fn try_from(value: VerificationKeyJsonDeser) -> Result<Self, Self::Error> {
        if !matches!(value.protocol, Protocol::Groth16) {
            return Err(Self::Error::WrongProtocol(
                value.protocol.as_ref().to_owned(),
            ));
        }
        let VerificationKeyJsonDeser {
            alpha_1,
            beta_2,
            gamma_2,
            delta2,
            ic,
            ..
        } = value;
        let alpha_1 = StringifiedG1(alpha_1)
            .try_into()
            .map_err(Self::Error::G1PointConversionError)?;
        let beta_2 = StringifiedG2(beta_2)
            .try_into()
            .map_err(Self::Error::G2PointConversionError)?;
        let gamma_2 = StringifiedG2(gamma_2)
            .try_into()
            .map_err(Self::Error::G2PointConversionError)?;
        let delta_2 = StringifiedG2(delta2)
            .try_into()
            .map_err(Self::Error::G2PointConversionError)?;
        let ic: Vec<ark_bn254::G1Affine> = ic
            .into_iter()
            .map(StringifiedG1)
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()
            .map_err(Self::Error::G1PointConversionError)?;

        Ok(Self {
            alpha_1,
            beta_2,
            gamma_2,
            delta_2,
            ic,
        })
    }
}
pub struct PreparedVerificationKey<E: Pairing> {
    vk: ark_groth16::PreparedVerifyingKey<E>,
}

impl<E: Pairing> PreparedVerificationKey<E> {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        self.vk
            .serialize_compressed(&mut bytes)
            .expect("vk failed to serialize");

        bytes
    }

    pub const fn alpha_g1(&self) -> &E::G1Affine {
        &self.vk.vk.alpha_g1
    }

    pub const fn beta_g2(&self) -> &E::G2Affine {
        &self.vk.vk.beta_g2
    }

    pub const fn delta_g2(&self) -> &E::G2Affine {
        &self.vk.vk.delta_g2
    }

    pub const fn gamma_abc_g1(&self) -> &Vec<E::G1Affine> {
        &self.vk.vk.gamma_abc_g1
    }

    pub const fn alpha_g1_beta_g2(&self) -> &E::TargetField {
        &self.vk.alpha_g1_beta_g2
    }

    pub const fn gamma_g2_neg_pc(&self) -> &E::G2Prepared {
        &self.vk.gamma_g2_neg_pc
    }

    pub const fn delta_g2_neg_pc(&self) -> &E::G2Prepared {
        &self.vk.delta_g2_neg_pc
    }
}

impl<E: Pairing> From<VerificationKey<E>> for ark_groth16::VerifyingKey<E> {
    fn from(value: VerificationKey<E>) -> Self {
        let VerificationKey {
            alpha_1,
            beta_2,
            gamma_2,
            delta_2,
            ic,
        } = value;

        Self {
            alpha_g1: alpha_1,
            beta_g2: beta_2,
            gamma_g2: gamma_2,
            delta_g2: delta_2,
            gamma_abc_g1: ic,
        }
    }
}

impl<E: Pairing> VerificationKey<E> {
    pub fn into_prepared(self) -> PreparedVerificationKey<E> {
        let vk: ark_groth16::VerifyingKey<E> = self.into();
        PreparedVerificationKey { vk: vk.into() }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CompressedVerificationKey<E: CompressSize> {
    pub alpha_1: GenericArray<u8, E::G1CompressedSize>,
    pub beta_2: GenericArray<u8, E::G2CompressedSize>,
    pub gamma_2: GenericArray<u8, E::G2CompressedSize>,
    pub delta_2: GenericArray<u8, E::G2CompressedSize>,
    pub ic: Vec<GenericArray<u8, E::G1CompressedSize>>,
}

impl<E> Debug for CompressedVerificationKey<E>
where
    E: CompressSize,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompressedVerificationKey")
            .field("alpha_1", &hex::encode(&self.alpha_1))
            .field("beta_2", &hex::encode(&self.beta_2))
            .field("gamma_2", &hex::encode(&self.gamma_2))
            .field("delta_2", &hex::encode(&self.delta_2))
            .field("ic", &self.ic.iter().map(hex::encode).collect::<Vec<_>>())
            .finish()
    }
}

impl CompressedVerificationKey<ark_bn254::Bn254> {
    // Alpha, Beta, Gamma, Delta, then the IC points.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        [
            &self.alpha_1[..],
            &self.beta_2,
            &self.gamma_2,
            &self.delta_2,
        ]
        .into_iter()
        .chain(self.ic.iter().map(|point| &point[..]))
        .flatten()
        .copied()
        .collect()
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let g1 = <ark_bn254::Bn254 as CompressSize>::G1CompressedSize::to_usize();
        let g2 = <ark_bn254::Bn254 as CompressSize>::G2CompressedSize::to_usize();
        let ic = bytes.get(g1 + 3 * g2..)?;
        if !ic.len().is_multiple_of(g1) {
            return None;
        }
        Some(Self {
            alpha_1: *GenericArray::from_slice(&bytes[..g1]),
            beta_2: *GenericArray::from_slice(&bytes[g1..g1 + g2]),
            gamma_2: *GenericArray::from_slice(&bytes[g1 + g2..g1 + 2 * g2]),
            delta_2: *GenericArray::from_slice(&bytes[g1 + 2 * g2..g1 + 3 * g2]),
            ic: ic
                .chunks_exact(g1)
                .map(|point| *GenericArray::from_slice(point))
                .collect(),
        })
    }
}

impl<E: CompressSize> TryFrom<&VerificationKey<E>> for CompressedVerificationKey<E> {
    type Error = SerializationError;
    fn try_from(value: &VerificationKey<E>) -> Result<Self, SerializationError> {
        let VerificationKey {
            alpha_1,
            beta_2,
            gamma_2,
            delta_2,
            ic,
        } = value;
        let mut alpha = GenericArray::default();
        let mut beta = GenericArray::default();
        let mut gamma = GenericArray::default();
        let mut delta = GenericArray::default();
        alpha_1.serialize_compressed(alpha.as_mut_slice())?;
        beta_2.serialize_compressed(beta.as_mut_slice())?;
        gamma_2.serialize_compressed(gamma.as_mut_slice())?;
        delta_2.serialize_compressed(delta.as_mut_slice())?;
        let ic = ic
            .iter()
            .map(|point| {
                let mut bytes = GenericArray::default();
                point.serialize_compressed(bytes.as_mut_slice())?;
                Ok(bytes)
            })
            .collect::<Result<_, SerializationError>>()?;
        Ok(Self {
            alpha_1: alpha,
            beta_2: beta,
            gamma_2: gamma,
            delta_2: delta,
            ic,
        })
    }
}

// Deserialization checks every point is on its curve and in its prime order
// subgroup, the point at infinity is rejected here.
impl<E: CompressSize> TryFrom<&CompressedVerificationKey<E>> for VerificationKey<E> {
    type Error = SerializationError;
    fn try_from(value: &CompressedVerificationKey<E>) -> Result<Self, SerializationError> {
        fn finite<P: AffineRepr>(point: P) -> Result<P, SerializationError> {
            if point.is_zero() {
                return Err(SerializationError::InvalidData);
            }
            Ok(point)
        }
        let CompressedVerificationKey {
            alpha_1,
            beta_2,
            gamma_2,
            delta_2,
            ic,
        } = value;
        let alpha = <E::G1Affine as CanonicalDeserialize>::deserialize_compressed(&alpha_1[..])?;
        let beta = <E::G2Affine as CanonicalDeserialize>::deserialize_compressed(&beta_2[..])?;
        let gamma = <E::G2Affine as CanonicalDeserialize>::deserialize_compressed(&gamma_2[..])?;
        let delta = <E::G2Affine as CanonicalDeserialize>::deserialize_compressed(&delta_2[..])?;
        let ic = ic
            .iter()
            .map(|point| {
                finite(<E::G1Affine as CanonicalDeserialize>::deserialize_compressed(&point[..])?)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            alpha_1: finite(alpha)?,
            beta_2: finite(beta)?,
            gamma_2: finite(gamma)?,
            delta_2: finite(delta)?,
            ic,
        })
    }
}

impl VerificationKey<ark_bn254::Bn254> {
    #[must_use]
    pub fn alpha_beta(&self) -> ark_bn254::Fq12 {
        ark_bn254::Bn254::pairing(self.alpha_1, self.beta_2).0
    }

    // The line coefficients of `-γ` and `-δ` are computed here, `e(α, β)` comes
    // from the ledger.
    #[must_use]
    #[expect(
        clippy::large_types_passed_by_value,
        reason = "moved into the prepared key"
    )]
    pub fn prepare_with_alpha_beta(
        &self,
        alpha_beta: ark_bn254::Fq12,
    ) -> PreparedVerificationKey<ark_bn254::Bn254> {
        PreparedVerificationKey {
            vk: ark_groth16::PreparedVerifyingKey {
                vk: self.clone().into(),
                alpha_g1_beta_g2: alpha_beta,
                gamma_g2_neg_pc: (-self.gamma_2).into(),
                delta_g2_neg_pc: (-self.delta_2).into(),
            },
        }
    }
}

impl<E: Pairing> AsRef<ark_groth16::PreparedVerifyingKey<E>> for PreparedVerificationKey<E> {
    fn as_ref(&self) -> &ark_groth16::PreparedVerifyingKey<E> {
        &self.vk
    }
}

#[cfg(test)]
mod tests {
    use ark_bn254::{Fr, G1Affine, G2Affine};
    use ark_ec::AffineRepr as _;

    use crate::{
        CompressedGroth16VerificationKey, Groth16VerificationKey, groth16_verify,
        test_utils::{off_subgroup_g2, three_input_prove, three_input_setup},
    };

    fn round_trip(key: &Groth16VerificationKey) -> Option<Groth16VerificationKey> {
        let compressed = CompressedGroth16VerificationKey::try_from(key).unwrap();
        Groth16VerificationKey::try_from(&compressed).ok()
    }

    #[test]
    fn compressed_key_round_trips_and_rejects_invalid_points() {
        let (key, _) = three_input_setup(1);
        assert_eq!(round_trip(&key), Some(key.clone()));
        let mut ic = key.ic.clone();
        ic[2] = G1Affine::zero();
        assert_eq!(round_trip(&Groth16VerificationKey { ic, ..key }), None);
        assert_eq!(
            round_trip(&Groth16VerificationKey {
                delta_2: G2Affine::zero(),
                ..key.clone()
            }),
            None
        );
        assert_eq!(
            round_trip(&Groth16VerificationKey {
                gamma_2: off_subgroup_g2(),
                ..key
            }),
            None
        );
    }

    #[test]
    fn a_key_prepared_from_its_stored_alpha_beta_verifies() {
        let (key, proving_key) = three_input_setup(1);
        let inputs = [Fr::from(3u64), Fr::from(5u64), Fr::from(7u64)];
        let proof = three_input_prove(&proving_key, inputs, 2);
        let alpha_beta = key.alpha_beta();
        assert_eq!(&alpha_beta, key.clone().into_prepared().alpha_g1_beta_g2());
        let prepared = key.prepare_with_alpha_beta(alpha_beta);
        assert!(groth16_verify(&prepared, &proof, &inputs).unwrap());
        assert!(
            !groth16_verify(
                &prepared,
                &proof,
                &[Fr::from(3u64), Fr::from(5u64), Fr::from(8u64)]
            )
            .unwrap()
        );
    }
}
